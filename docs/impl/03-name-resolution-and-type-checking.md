# 03. Name Resolution and Type Checking

**Status:** Normative for the reference implementation (see README §2)
**Stage:** Stage 0–2 (refined types and contracts Stage 3, resources and interfaces Stage 4, `async` Stage 5; marked inline)
**Related:** [spec/04](../spec/04-modules-and-names.md), [spec/05](../spec/05-types.md), [spec/06](../spec/06-expressions-and-statements.md), [spec/07](../spec/07-ownership-and-borrowing.md), [spec/08](../spec/08-effects-and-capabilities.md), [spec/09](../spec/09-contracts-and-refinements.md), [spec/10](../spec/10-errors-and-panics.md), [spec/11](../spec/11-async-and-concurrency.md), [spec/15](../spec/15-attributes-and-lints.md), [spec/16](../spec/16-core-library.md), [impl/01](01-architecture.md), [impl/02](02-frontend-ast-hir.md), [impl/04](04-mir.md), [impl/05](05-borrow-checker.md), [impl/08](08-effect-and-capability-analysis.md), [impl/11](11-diagnostics.md), [decisions.md](../design/decisions.md)

This chapter specifies how the reference compiler turns the HIR of [impl/02](02-frontend-ast-hir.md) into **typed HIR**: the module tree, item collection and the import fixpoint, the resolution of every path, the semantic type representation, the collection of item signatures and their well-formedness, bidirectional type inference with unification, trait solving and coherence, method resolution, operators, coercions, closures, pattern checking and exhaustiveness, refined and distinct types, generics, and the `TypeckResults` tables that MIR construction ([impl/04](04-mir.md)) consumes. The language rules are owned by [spec/04](../spec/04-modules-and-names.md) (names), [spec/05](../spec/05-types.md) (types) and [spec/06](../spec/06-expressions-and-statements.md) (expressions, patterns, exhaustiveness). This chapter implements them, fixes the data structures and the order of work, and decides the questions those chapters leave to the implementation. It never changes which programs are accepted.

## 3.1 Overview

### 3.1.1 Position in the pipeline

```text
impl/02: parse → AST → HIR owners, ItemTree, ValidatedAttrs
   │
   ▼  this chapter
module_tree ─► package_scopes (Phases 1–3 of spec/04 §4.5.4) ─► resolve_owner (Phase 4)
   │
   ▼
item signatures: generics_of, predicates_of, type_of, fn_sig, adt_def, trait_def, impl_header, variances_of
   │
   ▼
coherence (orphan, overlap)  ─►  typeck(owner)  ─►  check_match(owner)  ─►  typed HIR = HIR + TypeckResults
   │
   ▼
impl/04: MIR building ─► impl/05 borrowck, impl/08 effects, SFC ─► … ─► Mono MIR (uses resolve_instance)
```

The type checker never builds MIR, and MIR building never runs inference. Every fact that MIR building needs about an expression (its type, its adjustments, the method or operator it calls, the variant an implicit member names, the kind of a `?`, the captures of a closure) is recorded in `TypeckResults` (§3.15).

### 3.1.2 Queries

All work is done by queries of the query system of [impl/01](01-architecture.md). The queries of this chapter are:

| Query | Key | Value | Section |
|---|---|---|---|
| `module_tree` | `PackageId` | `Arc<ModuleTree>` with diagnostics | §3.2 |
| `lang_items` | `()` | `Arc<LangItems>` | §3.3.6 |
| `declared_requirements` | `PackageId` | `Arc<[DeclaredRequirement]>`: the `requires` and `requires test` entries of the package's `sai.package` | §3.3.3 |
| `package_scopes` | `PackageId` | `Arc<PackageScopes>`: `D(M)`, `X(M)`, `S(M)` of every module, with diagnostics | §3.3 |
| `traits_in_scope` | `ModuleId` | `Arc<[DefId]>`, sorted by `DefPathHash` | §3.9.2 |
| `resolve_owner` | `OwnerId` | `Arc<ResolvedOwner>`: `Res` of every path, label targets, local bindings | §3.4 |
| `unused_imports` | `ModuleId` | diagnostics | §3.4.8 |
| `generics_of` | `DefId` | `Arc<Generics>` | §3.6.2 |
| `predicates_of` | `DefId` | `Arc<Predicates>` (declared, not elaborated) | §3.6.2 |
| `param_env` | `DefId` | `ParamEnv` (elaborated predicates in scope) | §3.14.1 |
| `type_of` | `DefId` | `Ty` (of a const, a field, an alias expansion, an impl self type) | §3.6.3 |
| `fn_sig` | `DefId` | `PolyFnSig` | §3.6.3 |
| `adt_def` | `DefId` | `Arc<AdtDef>` (records, enums, distinct, refined, resources) | §3.6.4 |
| `trait_def` | `DefId` | `Arc<TraitDef>` | §3.6.5 |
| `impl_header` | `DefId` | `ImplHeader` (self type, trait reference, polarity of nothing: SAI has no negative impls) | §3.8.1 |
| `impls_of_trait` | `DefId` (trait) | `Arc<TraitImpls>`, indexed by simplified self type | §3.8.1 |
| `inherent_impls` | `SimplifiedType` | `Arc<InherentItems>` | §3.9.3 |
| `variances_of` | `DefId` | `Arc<[Variance]>` for impl/05 | §3.6.8 |
| `adt_sized_check` | `DefId` | diagnostics (`INFINITE_SIZE_TYPE`) | §3.6.6 |
| `wf_check_item` | `OwnerId` | diagnostics | §3.6.7, §3.6.9 |
| `panic_hooks` | `PackageId` | `Arc<[DefId]>`: the `@panic_hook` functions of the package, with diagnostics | §3.6.9 |
| `core_export_check` | `PackageId` (the root) | diagnostics; computed only for a core-module artifact | §3.6.9 |
| `entry_point` | `PackageId` (the root) | `Option<DefId>`, with diagnostics; computed in every build and check | §3.6.9 |
| `entry_point_note` | `PackageId` (the root) | diagnostics; computed only for a core-module artifact outside test builds | §3.6.9 |
| `attr_lints` | `OwnerId` | diagnostics (`UNUSED_ATTRIBUTE`, `MISSING_DOCS`) | §3.6.10 |
| `coherence` | `DefId` (trait) | diagnostics (`ORPHAN_IMPL`, `OVERLAPPING_IMPLS`) | §3.8.3 |
| `typeck` | `OwnerId` (body owner) | `Arc<TypeckResults>` with diagnostics | §3.7, §3.15 |
| `check_match` | `OwnerId` | `Arc<MatchCheckResults>` with diagnostics | §3.12 |
| `const_value` | `DefId` (const or explicit discriminant) | `Result<ConstValue, ErrorGuaranteed>` | §3.7.15 |
| `evaluate_goal` | `Canonical<(ParamEnv, Goal)>` | `EvalResult` (global solver cache) | §3.8.4 |
| `resolve_instance` | `(DefId, GenericArgs)` | `Option<Instance>` (monomorphic call target for impl/04) | §3.8.7 |

The queries of interfaces, worlds, SAI views of WIT and flags types (`interface_impls`, `export_world`, `flags_def`, `interface_check`, `world_check` and the others) are specified by [16](16-interfaces-and-wit.md) §16.1.3. They live in `sai_resolve` and `sai_ty` beside the queries of this chapter.

Every query value that carries diagnostics returns them inside the value, as [impl/02](02-frontend-ast-hir.md) §2.1 does for its queries, and the driver collects them ([impl/11](11-diagnostics.md)). A query never emits a diagnostic as a side effect.

### 3.1.3 Assumptions about impl/01

This chapter relies on the following properties of the query system of [impl/01](01-architecture.md):

1. Queries are pure functions of their key and of the values of the queries they call. Values are memoized and, between builds, reused when their inputs are unchanged (red-green incrementality). The value of a query does not depend on the order in which queries run, or on whether they run in parallel.
2. The inputs `source_text(FileId)` and `package_files(PackageId)` exist, as used by [impl/02](02-frontend-ast-hir.md). `package_files` yields the source-relative paths and kinds (file, directory, symbolic link) of every entry below the package's source directory, or the fact that the directory is missing.
3. Every package other than the one being built (including `sai:core` and `sai:alloc`) is visible to this chapter through the same queries, keyed by `DefId`s of that package. Whether their values are computed from source in the same session or decoded from a precompiled artifact is owned by impl/01. The queries of this chapter return equal values in both cases.
4. A cycle between queries is an internal error (`INTERNAL_COMPILER_ERROR`, [impl/11](11-diagnostics.md) §11.4.6). Every cycle that the language rejects (`REEXPORT_CYCLE`, `ALIAS_CYCLE`, `SUPERTRAIT_CYCLE`, `CONST_CYCLE`, `INFINITE_SIZE_TYPE`) is detected by an explicit algorithm inside one query, never by the query system (N-E-25).

### 3.1.4 General requirements

1. **Determinism.** Every iteration that can affect a diagnostic, a recorded result or a later stage runs in a fixed order: source order within a file, file path order (byte order of the source-relative path) across files of a package, and `DefPathHash` order ([impl/02](02-frontend-ast-hir.md) §2.8.2) across packages. Hash maps are used only for lookup, never iterated to produce output.
2. **Owners are independent.** `typeck` of one owner reads only signatures, scopes and impls, never another owner's `TypeckResults` (spec/05 §5.16.1). The only exceptions are the queries that aggregate per-owner results by design: `unused_imports` (§3.4.8) and `const_value`, which reads the typed HIR of the const it evaluates.
3. **Interned types.** Types, generic argument lists, predicates and trait references are interned in a session-wide arena (§3.5.1). Equality of interned values is pointer equality.
4. **No source rewriting.** Elaboration (implicit members, method calls, operators, coercions) is recorded in `TypeckResults`. The HIR is never mutated.
5. **Diagnostics.** Every diagnostic follows [impl/11](11-diagnostics.md): code, severity and primary location. Declaration problems point at the name ([impl/11](11-diagnostics.md) §11.4.2). Diagnostics whose condition involves the error type are suppressed (§3.16).

### 3.1.5 Stage map

| Stage | This chapter |
|---|---|
| 0 | §3.2–§3.4 (resolution of the root package, `sai:core`, `sai:alloc`), §3.5–§3.7 for non-generic programs, §3.10 coercions C-Never, C-Reborrow, C-MutToShared, C-SliceMut and C-Region, §3.12, §3.15, §3.16. User generic declarations are `FEATURE_NOT_IMPLEMENTED` (§3.14.4). |
| 1 | Unchanged; code generation consumes the Stage 0 results. |
| 2 | Generics, traits, impls, coherence, the trait solver, `@derive`, trait method resolution, operator traits, closures, fn pointers and C-FnPtr (§3.6–§3.11, §3.14). |
| 3 | Dependencies (`use` of declared packages), f-strings (§3.7.13), refined types and distinct types with a refinement clause (§3.13), contracts (§3.7.14), effect sets in fn pointer types and `Fn` bounds (resolved here, compared by [impl/08](08-effect-and-capability-analysis.md)). |
| 4 | Resources, interfaces and worlds (§3.3.2, §3.6.4), and the SAI views of WIT, interface implementations, world references and flags types of [16](16-interfaces-and-wit.md). |
| 5 | `async fn`, `async` blocks, `.await`, `for await` (§3.7.11). |

A construct of a higher stage than the toolchain implements is reported as `FEATURE_NOT_IMPLEMENTED` (spec/01 §1.4.5): f-strings, `async` and attributes of a later stage by [impl/02](02-frontend-ast-hir.md) (§2.9.11, §2.9.15, §2.10.2), and the type-system constructs of the table in §3.14.4 (including those only recognizable after resolution, such as a `use` of a declared dependency or a refined alias) by this chapter at the location given there (N-E-39).

## 3.2 The module tree

### 3.2.1 Data structures

```rust
pub struct ModuleId(u32);
pub struct NamespaceId(u32);
pub struct ModuleTree {
    pub package: PackageId,
    pub modules: Vec<ModuleData>,                  // ModuleId(0) is the root; remaining IDs follow the depth-first file walk
    pub namespaces: Vec<NamespaceData>,
    pub diagnostics: Vec<Diagnostic>,
}
pub struct ModuleData {
    pub path: Vec<Symbol>,                         // empty for the root
    pub file: FileId,                              // the `.sai` file of the module
    pub parent_dir: Option<ModuleId>,              // nearest enclosing module by path prefix, for display only (spec/04 §4.2.5)
    pub children: BTreeMap<String, ChildEntry>,    // source-spelling order: next segment → module and/or namespace directory
}
pub struct NamespaceData {
    pub path: Vec<Symbol>,
    pub children: BTreeMap<String, ChildEntry>,    // source-spelling order
}
pub enum ChildEntry { Module(ModuleId), Namespace(NamespaceId), Both(ModuleId, NamespaceId) }
```

`ModuleId` and `NamespaceId` are local to a package and index their respective vectors. A module's global identity is its `DefId`, whose `DefPath` consists of one `Module` component per path segment ([impl/02](02-frontend-ast-hir.md) §2.8.2). Child maps use source spellings rather than session-local `Symbol` IDs: this keeps map iteration in byte order regardless of interning history. A consumer starting with a `Symbol` resolves it to its spelling before looking up a child. `children` and namespace entries exist only to produce the `UNKNOWN_MODULE` listing of spec/04 §4.3.2 step 2.3. They never make a submodule a member of its parent.

### 3.2.2 Algorithm

`module_tree(P)` reads `package_files(P)` and applies spec/04 §4.2.3 steps 1–11 exactly:

1. If the source directory is missing or is not a directory, report `MISSING_ROOT_MODULE` and return a tree with no modules.
2. Walk the entries depth-first. `package_files` yields them in ascending byte order of their names within each directory, which is the order of step 2. Hidden entries (step 3) are skipped with their contents.
3. A symbolic link is `SYMLINK_IN_SOURCE` and is skipped (step 4). Non-`.sai` regular files are skipped silently (step 5).
4. For a directory, its name is validated as a segment (step 8) before descending. If it is invalid, the diagnostic of §3.2.3 is reported once and nothing below it is visited.
5. For a `.sai` file, every segment of its path and its stem are validated (step 8). A file whose own stem is invalid is reported. A valid file becomes a module; `lib.sai` directly in the source directory becomes the root (step 6).
6. Segment validation checks, in this order: ASCII only (`NON_ASCII_PUBLIC_NAME`), then the pattern `[a-z][a-z0-9_]*`, the strict keywords and reserved words of spec/02, and `lib` outside the root position (`INVALID_MODULE_NAME`). The first failing check is reported, and only one diagnostic is reported per entry.
7. After the walk, if no root was found, report `MISSING_ROOT_MODULE` (step 9).
8. Directories without a sibling file become namespaces (step 10). Empty directories produce no entry (step 11).

The root is `ModuleId(0)`. Other modules are numbered in depth-first file-system entry order, with entries in each directory visited in byte order. A directory is visited before a later sibling file whose name extends the directory name; for example, when `a/` and `a.sai` are siblings, modules below `a/` are numbered before module `a`. Because every valid segment is lowercase ASCII, two valid modules never differ only by case, and the numbering is the same on every file system.

### 3.2.3 Diagnostic locations (N-E-23)

Module-tree diagnostics concern files and directories, not source ranges. Their primary locations are fixed as follows, in line with [impl/11](11-diagnostics.md) §11.4.3:

| Diagnostic | Primary location | `data` |
|---|---|---|
| `INVALID_MODULE_NAME`, `NON_ASCII_PUBLIC_NAME` on a file | the empty span at offset 0 of that file | `path`: the source-relative path; `segment`: the offending segment |
| the same on a directory | the empty span at offset 0 of the first `.sai` file below the directory in walk order; null if the directory contains no `.sai` file | `path`: the directory's source-relative path; `segment` |
| `SYMLINK_IN_SOURCE` | null (the link has no source text) | `path` |
| `MISSING_ROOT_MODULE` | the `package` field of `sai.package` | `path`: the expected `lib.sai` path |

A file inside an invalid directory or with an invalid name is still parsed by impl/02 if a client asks for it (for example an editor), but it belongs to no module, so its items are never collected and no diagnostic other than the module-tree one is reported for it during a build.

### 3.2.4 Other packages

For `sai:core`, `sai:alloc` and (from Stage 3) every declared dependency, `module_tree` is computed in the same way from that package's files, or decoded from its artifact (§3.1.3 item 3). The modules of the core library are listed by [spec/16](../spec/16-core-library.md) §16.2. A toolchain whose core library lacks one of them reports `INTERNAL_COMPILER_ERROR`.

The package identity used for `use` resolution comes from the `package` field of `sai.package` (spec/04 N-A-1). The set of declared dependencies and the package selected for each are provided by the package resolver ([package/02](../package/02-resolver.md)) through impl/01.

**Foreign module path primitive (IR-495).** `resolve_foreign_module_target`
resolves the exact module suffix of a selected foreign SAI package, including
the empty suffix for its root. It shares local module-tree traversal and
unknown-path candidate behavior but deliberately omits the
`USE_OF_CURRENT_MODULE` comparison: `ModuleId` values are package-local, so
comparing a target package's module ID with the importing package's current
module ID is invalid. Package selection and scope composition still belong to
the later package-aware resolver.

**Active source dependency scopes (IR-496).** The locked CLI collects active
source packages in dependency-first order. For each package, it passes only
finalized scopes selected from that package's active direct graph edges.
Foreign paths use the exact edge target's module tree and completed `members`
tables, so aliases, item imports and `pub use` chains retain the original
definition's canonical `Res`/`DefId`. Ordinary foreign imports check public
visibility; public re-exports require public visibility. The consumer's tracked
scope identity includes each direct dependency key and scope fingerprint, so
changes to dependency inputs or transitive re-exports invalidate consumers.
Foreign interface paths retain their Stage 4 gate, with private interfaces
diagnosed separately. Artifact packages, WIT scopes, foreign interface-member
resolution, the canonical prelude and Phase 4 remain outside this slice.

**CLI integration update (IR-459).** `sai check` records each visible child entry's kind in a `PackageFilesSnapshot` during its capability-based directory walk. The inventory and source bytes are applied through one `VfsInputSnapshot` with `RootDatabase::replace_vfs_snapshot`, then passed to `RootDatabase::module_tree`. The walk records child symlinks without following them, skips dot-prefixed entries, and preserves the current parser diagnostics for visible `.sai` files. A preflight rejects a symlink in any component of the declared source path without canonicalizing or traversing it; the snapshot API models entries below a source directory and cannot represent the declared root path itself, so this one diagnostic is emitted at the CLI boundary. The module-tree walker opens the source path component-by-component without following links and keeps the declared path for diagnostics. The adapter renders the tree's diagnostics in check JSON and human output. `MISSING_ROOT_MODULE` is produced by the module-tree query for an absent or non-directory source root or missing regular `lib.sai`; the CLI no longer synthesizes a second copy. An incomplete root-directory scan suppresses only that missing-root diagnostic because the inventory cannot prove absence. The CLI keeps its package-relative `path` value for the missing root while the tree query's path remains source-relative. Module-name and source-symlink validation now run in `sai check`; Phase 1 collection is described in IR-460.

**CLI integration update (IR-460, IR-461).** `RootDatabase::collect_phase1_package_scopes` now assembles snapshot-bound parse, AST-ID, item-tree and target-filter inputs for every module, then runs the existing Phase 1 declaration collector. The database retains one `DefPathInterner` per package for its lifetime, so returned `DefId`s remain resolvable. `sai check` applies the explicit `--target`, or the first manifest target, and derives check's effective target capabilities: core targets provide `host`, wasm64 targets provide `memory64`, native targets provide `native_ffi` and `process`, and WASI32 provides no target predicates. It tracks the package inventory and successfully read `.sai` bytes in one VFS snapshot and creates the workspace snapshot from the exact module-tree file IDs. A source read failure makes that snapshot incomplete and skips Phase 1; a package without a usable target also has no target-filtered declaration pass. The collector reports target-filter and Phase 1 declaration diagnostics, including the previous declaration as `DeclaredHere` in JSON and human output, and reports `MODULE_INTERFACE_CONFLICT` on an interface declaration whose exact spelling matches a direct module-file child. Parser diagnostics and module-tree diagnostics remain owned by their existing passes. At this point the initial CLI bridge stopped after Phase 1; IR-464 through IR-469 below record the later temporary local-target, alias, item-import, and interface-member extensions.

**CLI integration update (IR-463).** Phase 1 validates every surviving `@export_name` against each item's actual syntactic target, including non-export functions, nested entries and component-target checks; invalid targets therefore produce `INVALID_ATTRIBUTE_TARGET` instead of the Stage 0 feature-gate diagnostic. Explicit root-core and test-build context controls export-table collection, not target validation. Ordinary root-package core-module checks build the export-name table and report duplicate/reserved names. Component builds and test analysis do not create that table or run package export-name uniqueness checks. The narrow HIR API does not change the general Stage 1 attribute gate. Core-module lowering does not yet consume the table.

**CLI integration update (IR-464).** After Phase 1 declaration collection, the driver walks only target-filtered `UseEntry`s and runs the local module-target adapter. `sai check` reports `USE_OF_CURRENT_MODULE` and `UNKNOWN_MODULE` at the full `PKG_PATH` span. It leaves malformed paths to parser diagnostics, missing roots to the module-tree pass, and foreign identities to package selection. The adapter suppresses an unknown-module diagnostic when the final local segment names an interface declared in its exact parent module; IR-466 below records the Phase 3 binding of that local interface alias. Unknown local paths carry the longest matched module or namespace prefix and every direct child module in spelling order, as required by spec/04 §4.3.2; a note names the matched prefix even when it has no child modules. `lib` and kebab segments receive their specified help, and the CLI preserves each help child when a diagnostic has more than one. That target-diagnostic slice did not bind imports or compute the Phase 2 fixpoint; IR-466 below records the later, narrower alias binding step.

## 3.3 Item collection and imports

This section implements Phases 1–3 of spec/04 §4.5.4. All three phases run in the single query `package_scopes(P)`, because the lazy `lookup` function of Phase 2 must see `D(M)` for every module of the package.

### 3.3.1 `@target` filtering

Before Phase 1, every module-level item and `use` is filtered by its `@target` predicate. The predicate is decoded from the item tree's `RawAttr` by the function that [impl/02](02-frontend-ast-hir.md) §2.10.2 shares with the `attrs` query, and evaluated here against the build's target and its capability set (spec/15, [targets](../targets/targets-and-profiles.md)), which the driver provides as the input `target_cfg(PackageId) -> Arc<TargetCfg>`. An item whose predicate is false is not collected, together with everything nested in it: it gets no `DefId` and no HIR owner (impl/02 §2.10.2), no binding, no impl entry and no body check, and its uses are resolved as if it did not exist. `@target` on fields, variants, trait or impl members, interface or extern items is invalid under spec/15 §15.11.3.

`@target` filtering is applied before every duplicate check, so two items with the same name are allowed when at most one survives (spec/04 §4.4.3).

### 3.3.2 Phase 1: declared items `D(M)`

```rust
pub struct PackageScopes {
    pub modules: IndexVec<ModuleId, ModuleScopes>,
    pub exports: BTreeMap<SmolStr, LocalDefId>,     // root package only (§3.3.5)
    pub interfaces: BTreeMap<LocalDefId, InterfaceScope>,  // members of local interfaces (impl/16 §16.4.8)
    pub diagnostics: Vec<Diagnostic>,               // includes WIT_ITEM_SHADOWED (impl/16 §16.4.5)
}
pub struct ModuleScopes {
    pub module_def_id: LocalDefId,                  // canonical module path; root is the package root
    pub declared: HashMap<Symbol, Binding>,         // D(M); lookup only, never iterated for output
    pub members: BTreeMap<Symbol, Binding>,         // X(M): D(M) plus `pub use` bindings
    pub scope: BTreeMap<Symbol, Binding>,           // S(M) = D(M) ∪ U(M)
    pub imports: Vec<ImportBinding>,                // every binding introduced by a `use`, in source order
}
pub struct Binding {
    pub res: Res, pub name_span: Span, pub origin: BindingOrigin,
    pub visibility: Visibility,                    // effective visibility for access checks
}
pub enum BindingOrigin { Decl, Use { import: ImportIndex }, PubUse { import: ImportIndex } }
pub struct ImportBinding {
    pub use_ast_id: ErasedAstId,
    pub item_index: Option<u32>,                    // None: the module alias of the declaration
    pub name: Symbol, pub name_span: Span,
    pub res: Res,                                   // Res::Err if the import failed (already diagnosed)
    pub is_pub: bool,
    pub test_only: bool,                            // introduced by a test-only use (§3.3.3)
}
```

`declared` is keyed by session-local `Symbol` for direct lookup. Its hash-map
iteration order is never used to assign identities or emit diagnostics;
diagnostics are sorted by source-relative file path and offset.

The current Phase 1 collector interns a canonical `DefPath` for every module
tree entry and stores its `LocalDefId` as `module_def_id`. The package root
uses the empty `DefPath`; each child segment is a `DefPathData::Module` with
disambiguator zero. Module paths are unique by module-tree construction, so
their canonical IDs can serve as module-alias resolutions in Phase 3.

For each module in `ModuleId` order, Phase 1 walks the module's `ItemTree` ([impl/02](02-frontend-ast-hir.md) §2.8.7) entries in source order and, for each item that survives `@target` and introduces a binding (spec/04 §4.4.1), inserts `name ↦ Res::Def(kind, def_id)` into `declared`. Extern functions are bound individually and always receive package-private effective visibility, even when the parser retained and diagnosed a visibility marker; the extern block, impls, invariants, tests and properties bind nothing. A named item whose `NAME` is missing retains a DefPath component containing `Symbol::ERROR` and its source-order disambiguator so its body can still be lowered and checked, but it is never bound. Test and property paths use the decoded string token; when the token is absent, no name is fabricated.

- A second declaration of a bound name is `DUPLICATE_BINDING` on the later name, with a related location on the earlier one (spec/04 §4.4.3). The earlier binding stays.
- A declaration named like a reserved type name is `RESERVED_TYPE_NAME` (spec/04 §4.4.4). The item is collected but not bound.
- A first declaration with any other name from the shadowable prelude table (spec/16 §16.4) is `SHADOWS_PRELUDE` at its name. Reserved names take precedence, and a later duplicate receives only `DUPLICATE_BINDING`. During the temporary Phase 3 merge, if an earlier successful local alias or item import already owns that name, the declaration's Phase 1 shadow warning is removed; the source-order winner alone receives the prelude warning.
- An interface declaration whose exact spelling matches a direct module-file child is `MODULE_INTERFACE_CONFLICT` on the interface name (spec/04 §4.3.2). A namespace directory without a module file at that path does not conflict.
- **Test names** ([spec/14](../spec/14-testing.md) §14.2.3). Each `Test` and `Property` entry that survives `@target` is checked when the walk reaches it, in every build, with or without test analysis. Its name is the string of the `Test(name)` or `Property(name)` component of its `DefPath` ([impl/02](02-frontend-ast-hir.md) §2.8.2), which is the value of its `STRING` token. A name that is empty or contains a character of general category `Cc` is `INVALID_TEST_NAME`, at the string. Otherwise the name is compared, by code points, with the names of the earlier tests and properties of the same module that passed the first check. An equal name is `DUPLICATE_TEST_NAME`, at the later string, with a note at the earlier one. A `test` and a `property` are compared with each other. An invalid name is not compared, so one declaration gets at most one of the two codes. The check reads only the item tree, so its result does not depend on test analysis (spec/14 §14.2.3 item 5).

Member namespaces of items (variants, associated functions, fields, trait items) are not built here. They are part of `adt_def`, `trait_def` and `inherent_impls` (§3.6, §3.9), because their duplicate rules are owned by spec/05 and some of them depend on impls. Local interface members are the separate exception described in IR-469; their names do not enter the containing module's member namespace.

**Temporary CLI Phase 3 slice (IR-467).** The driver extends these Phase 1
scopes with local module and interface aliases, local module item imports, and
local `pub use` re-exports. `Phase1ModuleScopes` now includes `declared`,
`members`, `scope`, and an `imports` list. Module aliases still use canonical
module `DefId`s as `Res::Def(DefKind::Module, def_id)`; a local interface alias
uses the declaring interface's `DefId`. Explicit aliases keep their spelling;
otherwise the last path segment or package name is used, with hyphens changed
to underscores. For parseable paths, binding-name keyword/reserved-name checks
precede target selection, and `PUB_USE_WITHOUT_ITEMS` is reported for each
retained `pub use` without an item list, including malformed or missing paths.
Item-only uses do not bind their target. An alias on a `pub use` remains a
private module-local binding; only its listed items can enter `members`.

Each local module's `members` is initialized from `declared` and extended only
by successful local `pub use` items. Ordinary module aliases and item imports
remain private to their source module and never become members. Item imports
resolve through direct declarations and local re-export chains, preserve the
original `Res`/`DefId`, and retain one `ImportBinding` per alias or listed item
with its source item index and binding-name span. Ordinary same-package imports
may resolve package-private declarations; `pub use` requires the resolved
entity to have exact `pub` visibility. The local lookup memo is keyed by
`(ModuleId, Symbol)`, detects re-export cycles once, and keeps missing,
deferred, failed, and cyclic lookups distinct. Scope bindings are inserted
after item resolution in source order, so a later module alias or listed item
receives `DUPLICATE_BINDING` and the earlier binding remains. Same-entity
imports produce `REDUNDANT_IMPORT`; unresolved local items include up to three
nearby names from the resolved member namespace.

**Prelude shadow diagnostics (IR-468).** Phase 1 checks declarations against
the exact shadowable-name list in spec/16 §16.4. Reserved type names and
`Send`/`Share` are excluded; the reserved-name diagnostic takes precedence.
The prelude provider's declaration at its canonical item path does not warn;
other declarations get `SHADOWS_PRELUDE` unless an earlier source-order
binding already owns the name. Later duplicates get `DUPLICATE_BINDING`
alone. Successfully inserted local module and interface aliases use the same
list, as do local item imports that denote a different entity from the prelude
binding. The grammar permits only lowercase aliases for these module targets,
so only `panic` and `unreachable` can
collide in this alias form.

When compiling the exact `sai:core` or `sai:alloc` package, local definition
paths are compared with the canonical prelude item path: an exact reimport
produces `REDUNDANT_IMPORT` and leaves the implicit prelude binding in place.
Re-export chains retain that identity through their original `DefId`.
Foreign-interface targets and members remain deferred, as do canonical
prelude identities through non-local re-export chains; source-package module
and item imports are resolved by the current `sai check` path below.

**Foreign source-package scopes in `sai check`.** The CLI collects Phase 1
scopes for active source packages in dependency-first order and supplies each
package with finalized scopes for its active direct source dependencies.
Foreign module aliases, item imports, and public item re-exports resolve
through those scopes, preserve canonical `DefId`s, and apply visibility
checks. The locked-source integration test covers a root-to-middle-to-leaf
re-export chain. This is a `check`-specific assembly path, not a general
tracked `package_scopes(PackageId)` query.

When a validated active source scope contains a public interface binding but
its generated WIT view is unavailable, a successful public re-export is kept
in a separate deferred-source map. A validated WIT-view closure also marks
foreign source interfaces without a matching generated-view origin as
unavailable, including interfaces reached through a module alias. These
interfaces do not enter ordinary foreign member lookup or enumeration, and
ordinary imports remain deferred. Qualified-path diagnostic traversal may
inspect their source suffixes to report missing or inaccessible members, but
a path that traverses one always returns `Res::Err`; the unavailable source
interface or member cannot be consumed as a resolved name. Duplicate aliases
still receive normal source-order conflict diagnostics and do not leave a
deferred fallback. Test-only re-exports never enter the sidecar. WIT-only
targets retain their separate deferred lookup path, and the
incomplete-analysis diagnostic remains.

Source-only type-path queries use the validated source closure directly and
do not infer WIT-view omissions. Queries that explicitly receive an active
WIT-view closure treat source interfaces without a matching generated origin
as unavailable; that closure must cover the source scopes used by the query.

When a valid manifest is available, Phase 1 collection compares its target-
and test-filtered requirements with the set of active outgoing dependency
identities. A supplied foreign scope is checked against an active direct edge
by visible identity, selected `PackageKey`, and source form; duplicate
identities and mismatched module/scope counts are rejected. The graph
projection tracks the selected target version in its query identity. These
checks validate graph inputs and supplied scopes; they do not assemble
missing scopes.

Source `Phase1PackageScopes` already stores direct interface member maps, but
foreign interface targets and their member imports/re-exports are not wired
into the import resolver: public interface targets are Stage-4-gated, while
private targets receive a visibility diagnostic. The current incremental
owner adapters cover top-level functions, extern functions, consts,
invariants, refined types, refinement-bearing distinct types, direct
trait/impl headers, and associated trait/impl declarations including method
bodies and contracts, and direct interface declarations including resource
method signatures and recovery-only interface body/contract syntax. IR-538
adds top-level structs, aliases, enums, and resources, including generic
bounds, `where` clauses, alias targets, fields, and enum payloads. IR-539 adds
test-only diagnostics for effect `covers` declarations, and IR-540 adds them
for direct world import/export/include module paths and world-function
signatures, including parser-recovered syntax. Package-qualified `WorldRef`
resolution remains with the WIT resolver; spec/14 does not yet define how a
direct `requires test` package path is diagnosed. The grammar exposes no
additional extern-child form beyond function heads, which IR-532 already
covers; IR-541 adds partial first-head type-path resolution for the existing
top-level type owners, and IR-542 connects qualified module/interface prefixes
to a validated active-source scope closure. Artifact and WIT member suffixes
remain deferred. The property-parameter exception is preserved during test
analysis.
The complete canonical prelude and general tracked `package_scopes(PackageId)`
query also remain deferred.

**Function type paths over validated source closures (IR-564).** The driver
now exposes `resolve_function_owner_analyses_with_member_scopes` for free
function signatures and body type paths. It accepts only a
`Phase1ActiveSourceMemberScopes` value validated against the same package
collection, indexes the complete transitive active source closure, applies
foreign visibility through canonical module/interface definitions, and
records the closure fingerprint on the result. The existing
`resolve_function_owner_analyses` entry point remains package-local. The
closure-aware function bridge retains private and missing member failures as
typed path errors and additionally maps them to snapshot-bound diagnostics.
`sai check` consumes these diagnostics alongside top-level type-owner
diagnostics; equivalent mapping for the other owner families remains later
work.

**Const type paths over validated source closures (IR-565).** The driver
provides the same validated-closure entry point for const declared types and
initializer type paths. It preserves the package-local API and records closure
provenance in `ConstOwnerResolutions`. The const regression covers transitive
public resolution, foreign private and missing member errors, and rejection
when the closure belongs to another package collection. Expression lookup and
diagnostic construction remain separate.

**Predicate-owner type paths over validated source closures (IR-566).**
Invariant targets and body annotations plus refined and refinement-bearing
distinct type paths now have closure-aware entry points. Their shared result
records the closure fingerprint, while package-local entry points and
predicate/expression channels keep their existing behavior. Regressions cover
transitive public, foreign private, and missing members for both adapter
families; diagnostic mapping remains separate.

**Extern-function type paths over validated source closures (IR-567).**
Extern signatures and contract/body type paths use the same validated
active-source closure as function, const, and predicate owners. The closure
fingerprint is retained in the shared predicate-owner result; the package-local
entry point and expression/predicate channels remain unchanged. A bodyless
extern signature regression covers transitive public lookup, foreign private
and missing member errors, closure provenance, and collection mismatch.

**Trait/impl header type paths over validated source closures (IR-568).**
Trait and impl header type paths now use the validated active-source closure
and retain its fingerprint in their result wrapper. Package-local behavior
remains available. Regressions cover a trait generic bound, an impl trait
target and self type, an interface-impl target, and a `where` bound through a
transitive-only package, including foreign private and missing member errors.

**Trait/impl associated-member type paths over validated source closures (IR-569).**
Trait and impl associated type and method type paths now use the validated
active-source closure. The aggregate result retains the closure fingerprint,
while each child owner keeps its own type-path and diagnostic results.
Regressions cover transitive public paths in associated types and method
signatures for both trait and impl owners, plus foreign private and missing
member errors.

**Interface/resource member type paths over validated source closures (IR-570).**
Direct interface declaration and resource-method type paths now accept the
validated active-source closure and record its fingerprint on the aggregate
result. Package-local resolution remains available. Regressions cover an
interface alias, function signature, and resource-method signature through a
transitive-only re-export, exact foreign identity, private and missing-member
errors, and requester mismatch rejection.

**World function signature type paths over validated source closures (IR-571).**
World import/export function signature types now accept the validated
active-source closure and record its fingerprint on the aggregate result.
Package-local resolution remains available. The regression checks transitive
public, private, and missing type paths, while module/interface/import/export
references and world includes remain outside the type-path result channel.

When a foreign-interface re-export candidate precedes a known local candidate
with the same name, this bridge uses the successful local candidate while
retaining the foreign-interface item as deferred; a complete package resolver
must replace this temporary recovery rule with final ordered lookup. Type
checking, lints, fixes, and successful `check` result records remain
unavailable and are represented by `FEATURE_NOT_IMPLEMENTED`.

**Local interface member tables (IR-469).** The temporary Phase 1 bridge
collects each local interface's direct type declarations, enums, resources,
and function heads into an `InterfaceScope` keyed by the interface's canonical
`DefId`. Member DefPaths use the same `TypeNs` or `ValueNs` components and
source-order disambiguators as HIR lowering, through shared ItemTree helpers.
Member visibility is inherited from the containing interface. Interface
members remain outside the module's `members` map; a local
`use interface.{member}` can resolve them directly, and a successful public
`pub use` may add that member to the re-exporting module's `members`. A
private interface member cannot be publicly re-exported. Duplicate member
names keep the first binding and diagnose the later name. Missing members of
local interfaces are `UNRESOLVED_IMPORT`; foreign interface member tables
remain deferred.

**Top-level function owner descriptors (IR-510).** `Phase1PackageScopes`
retains an ordered `function_owners` list alongside its module scopes. Each
descriptor pairs the canonical `OwnerId` from the Phase 1 definition-path
walk with its containing module file and the entry's erased AST ID. Collection
happens before duplicate-binding recovery, so duplicate functions and
`Symbol::ERROR` recovery entries retain distinct owner identities even when
they are omitted from the module scope. The list contains only direct function
entries that survived target filtering. An erased AST ID is file-local and
must be resolved against the current snapshot's `AstIdMap` before lowering;
descriptors carry no spans, syntax pointers, or arena-borrowed HIR.

At IR-510 this was only a source index for top-level functions, not a
`hir_owner` or `resolve_owner` query.

**Direct top-level owner index (IR-519).** `Phase1PackageScopes` now also
retains `top_level_owners`, an ordered list for direct top-level functions,
types, traits, impls, constants, invariants, effects, interfaces, worlds,
extern blocks, tests, and properties that have a canonical definition path.
Each entry pairs its `OwnerId`, `FileId`, erased AST ID, and owner kind from
the same Phase 1 path-interning pass. Descriptors are collected from the
target-filtered item tree before duplicate-binding recovery, so duplicate and
recovery-name declarations keep their distinct paths. A test or property with
no recovered name has no canonical path and receives no descriptor. Extern
functions remain in their existing child descriptors, which also retain the
parent extern-block identity.

Consumers must rebind each file and AST ID against the current snapshot before
lowering. The table is a source index, not a `hir_owner` or `resolve_owner`
query. The driver has an exhaustive direct-owner kind-to-lowerer dispatcher;
the function, invariant, refinement, and extern-block adapters now validate
their specialized descriptors against this common index before dispatching.
The current bridges do not yet lower every indexed owner through one
package-wide adapter or cache owner results. Associated, world, interface, and
extern child owners, fields, variants, generic parameters, closures, async
blocks, and synthetic derived owners are not in this direct top-level index.

**Tracked local query adapter (IR-470).** The driver now tracks this local
Phase 1 and Phase 3 slice through an interim `package_scopes` derived query,
keyed by canonical `PackageKey`. Its input identity covers the package's
module and namespace topology, source paths and bytes, document versions,
session-local file IDs, target facts, and build context. File IDs are included
because source locations in the memo use them. This is deliberately not the
normative final query: dependency package scopes, declared requirements, WIT
inputs, the complete prelude, and the final `PackageId` key remain deferred.

Target filtering still runs at the driver boundary because the prepared AST
contains non-`Send` syntax nodes. Its diagnostics are appended from the
current request after applying the same `@export_name` stage-gate suppression
as Phase 1. Workspace-bound diagnostics produced during scope collection are
detached from the memo and rebound to the caller's snapshot.
The current bridge fails closed if a snapshot-bound diagnostic carries a fix;
such fixes need a snapshot-independent representation before this query can
cache them. The staged input handoff is safe only while query evaluation
remains synchronous and single-threaded. A parallel query runtime must replace
that handoff with stable query inputs.

### 3.3.3 Phase 2: the import fixpoint

Phase 2 resolves every `use` declaration of every module and computes the member namespaces `X(M)` with the memoized `lookup(M, x)` of spec/04 §4.5.4. The implementation:

`sai_resolve::use_path::parse_use_path` parses the `UseEntry.pkg_path` text into its exact versionless package identity and interned path segments. Since HIR does not retain the lexer's `PKG_PATH` error bit, this boundary checks the same package-identity and path-segment spelling rules as the lexer. It accepts implicit-use identities such as `sai:core` and preserves hyphens and underscores without normalization.

`sai_resolve::use_target::resolve_use_target` accepts the current `PackageKey` and its `PackageInterner`, verifies that the key exactly names `ModuleTree.package`, and derives the versionless identity from that canonical key. `resolve_parsed_use_target` applies the same check to a parsed path so callers can retain its segments. Ordinary SAI keys lose their `#compatibility_line` suffix for path comparison; `sai:core` and `sai:alloc` are retained as raw identities, while WIT keys are rejected by this SAI-module adapter. An exact identity match delegates the suffix to the module resolver; a different identity is returned as a `ForeignPackage` with its parsed suffix intact for package selection.

The module-only path step is implemented by `sai_resolve::module_target::resolve_module_target`. Given path symbols after the package identity, it performs exact spelling lookup through modules and namespace directories, maps an empty path to the package root, and rejects the current module. Unknown paths carry the longest matched prefix and byte-ordered direct module candidates; malformed child IDs fail as an invalid module tree.

The temporary CLI bridge is `sai_resolve::use_target_diagnostics::collect_local_use_target_diagnostics`, called after Phase 1 by the driver. It uses each filtered use's AST ID to recover the whole `PKG_PATH` token range, then converts only local current-module and unknown-module errors to diagnostics. It suppresses malformed-token duplicates, the module-tree-owned missing-root case, foreign package identities and unresolved paths whose final segment names an interface declared by the parent module. The Phase 3 slice also resolves such a local interface alias to its declaring interface `DefId`. Candidate data uses `matched_prefix` and `module_candidates`. For `UNKNOWN_MODULE`, spec/04 §4.3.2 step 2.3 requires the full direct-child module list; this exact list takes precedence over impl/11 N-H-5's edit-distance heuristic for general name suggestions.

The §4.3.3 binding-name step is implemented by `sai_resolve::use_binding::derive_use_binding_name`. Explicit aliases retain their symbol; an unaliased target derives its final path segment or package name and maps hyphens to underscores. An item-only `UseItems` list has no module binding; an empty list is rejected by the parser under spec/03 §3.1.1 and §3.4, and HIR still preserves its presence for error recovery in the stable item-tree shape. Derived strict keywords and reserved words return `RequiresAlias`; the temporary Phase 3 pass checks the binding name before resolving its target and inserts successful local module or interface aliases. Item-list imports are resolved from local scopes and supplied foreign source-package scopes; IR-472 classifies declared inactive and test-only requirements before local lookup. Foreign interface targets and member tables remain deferred.

The §4.3.4 item-alias case-class check is implemented by `sai_resolve::use_item_binding::derive_use_item_binding`. It preserves the source and binding symbols and returns whether their lexer-defined case classes differ; Phase 3 owns the `ALIAS_CASE_MISMATCH` diagnostic and binding insertion. The shared `sai_syntax::identifier_case_kind` helper keeps this check aligned with lexing, including Unicode uppercase and titlecase letters. The Phase 3 bridge resolves item lists against local module/interface tables and supplied foreign source-package scopes, retaining canonical entity identity through module and item re-exports. Foreign interface targets/member lookup, a general tracked package-scope query, and test-only diagnostics for owner kinds not yet covered remain deferred.

```rust
enum LookupState { InProgress, Done(Option<Binding>) }   // None: no member `x`; Some(Binding{res: Res::Err}) = ⊥
struct ImportResolver<'a> {
    scopes: &'a mut IndexVec<ModuleId, ModuleScopes>,
    memo: HashMap<(ModuleId, Symbol), LookupState>,
    stack: Vec<(ModuleId, Symbol, ImportIndex)>,      // the chain of `pub use` bindings being followed
    reported_cycles: HashSet<ImportIndex>,
}
```

1. **Use targets.** `resolve_pkg_path(M, path)` implements spec/04 §4.3.2 for each `UseEntry` in source order, and records the result (a module `DefId`, an interface `DefId`, or an error) on the entry. For a WIT package, the result is the view interface of a WIT unit, and an interface of another source package is replaced by its generated view ([16](16-interfaces-and-wit.md) §16.4.7, §16.4.8). Diagnostics: `UNKNOWN_PACKAGE`, `UNKNOWN_MODULE`, `USE_OF_CURRENT_MODULE`, `INVALID_USE_TARGET`, `UNKNOWN_WIT_ITEM`, `PRIVATE_ITEM`, `INACTIVE_DEPENDENCY` (below); before Stage 3, a declared dependency is `FEATURE_NOT_IMPLEMENTED`. `MODULE_INTERFACE_CONFLICT` is reported in Phase 1 of the defining package, on the interface declaration. Package and member-name suggestions follow the edit-distance rule of [impl/11](11-diagnostics.md) (N-H-5) where spec/04 calls for those suggestions, except `UNKNOWN_PACKAGE`, whose fixed distance-2 cutoff is in spec/04 §4.3.2 step 1.4. `UNKNOWN_MODULE` is another exception: spec/04 §4.3.2 step 2.3 requires every direct child module below the longest matched prefix.
2. **Foreign targets.** For a target in another package, `X(T)` is the `members` table of that package's `package_scopes`, which is final (spec/04 §4.5.4 Phase 2).
3. **Local targets.** For a target in the current package, `lookup(T, y)` is evaluated with the memo. `InProgress` on entry means a cycle: every `pub use` binding on `stack` from the first occurrence of `(T, y)` to the top forms the cycle. If none of them was reported, `REEXPORT_CYCLE` is reported once, on the binding of the cycle that comes first in source order (file path order, then offset), with every other binding of the cycle as a related location. Every binding on the cycle gets `Res::Err`.
4. **Item lists.** For each `UseItem` `x as y`: a missing member is `UNRESOLVED_IMPORT` (with the help of N-A-20 when `x` names a variant, field, method or associated item of a type in `X(T)`), an invisible member is `PRIVATE_ITEM`, a `pub use` of a non-`pub` entity is `PRIVATE_REEXPORT`, and a case mismatch between `x` and `y` is `ALIAS_CASE_MISMATCH`.
5. **Fixed order.** Use entries are processed module by module in `ModuleId` order and, within a module, in source order. `lookup` calls made on behalf of another module's `pub use` are memoized, so each `(M, x)` is computed once regardless of which use triggered it, and the diagnostics of a `pub use` are attached to the `pub use` itself. The resulting diagnostic set is therefore independent of the processing order; only their emission order follows it, and the driver sorts them ([impl/11](11-diagnostics.md)).

`pub use` without items is `PUB_USE_WITHOUT_ITEMS` and binds nothing.

**Declared requirements.** The input `package_graph` contains only the active edges of a package ([impl/01](01-architecture.md) §1.7.3 item 3), which is what step 1 resolves against. Two rules of step 1 also need the requirements that are not active edges, so `declared_requirements(P)` reads them from the package's own `sai.package`, which the queries of [package/01](../package/01-package-contract.md) parse like any other file of the package (impl/01 §1.7.3):

```rust
pub struct DeclaredRequirement {
    pub identity: SmolStr,                 // `ns:pkg`
    pub declaration_order: u32,            // first source occurrence among requirements
    pub only: Option<Arc<[Symbol]>>,       // the target names of `only { .. }`, in contract order
    pub test: bool,                        // an entry of `requires test`
}
```

The entries are sorted by identity bytes, while `declaration_order` preserves the first source occurrence among requirement entries. An entry that the contract queries reject (for example `DUPLICATE_REQUIREMENT`, package/01 §1.10.4) keeps its first occurrence only; the contract diagnostics are reported by package/01, never here. A change of `sai.package` that does not change this list leaves `package_scopes` green.

**Tracked requirement query (IR-471).** `RootDatabase` now exposes
`declared_requirements(PackageId)` over the exact manifest bytes and
`PackageRole` supplied for that package. The underlying query uses the stable
`PackageKey` and tracks both values as inputs. It sorts by identity bytes,
keeps the first source-order occurrence of a repeated identity, preserves
`only` targets in manifest order, and retains `requires test` entries for
dependency manifests. Each normalized entry carries the ordinal of its first
requirement occurrence so equal-ranked package suggestions can follow manifest
declaration order without changing the query's identity-sorted result. It
parses with a zero retained-diagnostic limit, rejects recovered entries from
syntactically invalid manifests, and verifies that a valid manifest header
matches the package identity and compatibility line.
Requirement identities and target names stay as strings in the cached value
so it does not retain session-local symbols; the resolver can intern target
names when it consumes this value. Manifest diagnostics remain owned by the
package-contract validation pass.

**Manifest-backed import classification (IR-472).** The tracked
`package_scopes` adapter includes the normalized requirements in its identity.
The current CLI passes the root package's exact manifest bytes and
`PackageRole::Root` to `collect_phase1_package_scopes_with_manifest`; an
invalid manifest or a package-key/header mismatch supplies no requirements,
distinct from a valid empty list. `collect_local_module_alias_bindings` uses
the declared identity, build target, and test-analysis flag to classify local
import syntax. The adapter still has no active package graph or foreign
package scopes, so this is only a manifest-backed prepass.

**Inactive dependencies** ([spec/04](../spec/04-modules-and-names.md) §4.3.2 step 1.3). After rules 1.1 and 1.2 found no target, let `K` be the `ns:pkg` part of the `PKG_PATH`. If `declared_requirements(P)` has an ordinary requirement `r` with `r.identity = K` and `r.only = Some(T)` where `T` does not contain the target of `target_cfg(P)` (§3.3.1), the use is `INACTIVE_DEPENDENCY`, at the `PKG_PATH`. The help lists the targets of `T` in contract order and suggests `@target` on the `use` and on the items that need it ([spec/15](../spec/15-attributes-and-lints.md) §15.11). The entry records an error, its bindings get `Res::Err`, and step 4 reports nothing for its items. A `use` that `@target` removed was never collected (§3.3.1), so it is never checked. An entry with `r.test = true` is a test-only use and follows the next paragraph instead, so a test dependency with an `only` list that excludes the target is also never `INACTIVE_DEPENDENCY` in a build without test analysis.

After current-package and toolchain-package handling, package identity
classification follows the manifest ordering in spec/04 §4.3.2: a
target-allowed declared dependency reaches the Stage 3 gate, and only an
undeclared dependency identity is `UNKNOWN_PACKAGE`. If a valid active package
graph is also supplied, the driver checks that its direct edges match the
requirements active for the current target and test-analysis context. A
missing edge for a declared active requirement, or an edge without a matching
active requirement, is a typed graph/manifest input inconsistency before
Phase 2; it must not be reclassified as `UNKNOWN_PACKAGE` or
`INACTIVE_DEPENDENCY`. Implicit toolchain packages (`sai:core`,
`sai:alloc`, and the bundled WASI packages) are excluded because they are not
ordinary active SAI dependency edges. Lock-backed graph activation is the
primary owner of this validation when it is integrated.

The current Stage 0 adapter may have no package graph, so the manifest
prepass classifies a target-allowed declared dependency as
`FEATURE_NOT_IMPLEMENTED` and a target-excluded ordinary dependency as
`INACTIVE_DEPENDENCY`. Both diagnostics suppress item lookup while
syntax-derived alias and item bindings retain `Res::Err` for later references
and still participate in duplicate checks. An available graph adds a
consistency check, not a different manifest-to-diagnostic precedence.

**Other foreign package identities (IR-473).** After the current-package,
toolchain-package, and test-only cases above, a valid manifest permits the
temporary adapter to classify an external `PKG_PATH` by its identity. A
target-allowed declared dependency is `FEATURE_NOT_IMPLEMENTED` at the package
identity span, with `data.stage = 3` and `data.feature = "dependencies"`.
Before this diagnostic, a declared ordinary requirement whose `only` list
excludes the target remains `INACTIVE_DEPENDENCY` on the complete path. If no
requirement declares the identity, it is `UNKNOWN_PACKAGE` at the identity
span. The seven toolchain WASI identities (`wasi:cli`, `wasi:clocks`,
`wasi:filesystem`, `wasi:http`, `wasi:io`, `wasi:random`, and `wasi:sockets`)
remain known Stage 4 WIT packages, along with `sai:core` and `sai:alloc`;
their scopes are still deferred.

`UNKNOWN_PACKAGE` stores the written identity as `data.name` and its candidate
identities as `data.candidates`, including when that list is empty. Candidates
are declared requirements (including `requires test`) within
Damerau–Levenshtein distance 2, ordered by distance and then first declaration
order, with byte spelling as a deterministic final tie-breaker and at most 10
entries. This fixed distance cutoff from spec/04 §4.3.2 step 1.4 takes
precedence over impl/11 §11.4.5's general small-candidate-set rule: a distant
identity is not listed just because the manifest has few requirements. A
nonempty list is also shown in a help child.

The adapter suppresses suffix, module, and member lookup after either
classification error. It retains the syntax-derived alias and item names as
`Res::Err` bindings for `DUPLICATE_BINDING`, without treating them as
`REDUNDANT_IMPORT` or `SHADOWS_PRELUDE`. An absent or invalid manifest remains
distinct from a valid empty requirement list: with no valid list, foreign
identities stay deferred; with a valid empty list, an undeclared identity is
`UNKNOWN_PACKAGE`.

**Test-only uses** ([spec/14](../spec/14-testing.md) §14.2.5, N-E-65). A `UseEntry` is **test-only** when the `ns:pkg` part of its `PKG_PATH` is the identity of an entry with `test = true` in `declared_requirements(P)`. This is read in every build, also for a dependency, whose test edges are never in the graph. Test analysis is **active for `P`** when `build_config` has test code active ([impl/01](01-architecture.md) §1.1.1) and `P` is one of the packages selected for test analysis (the `tests` list of [impl/08](08-effect-and-capability-analysis.md) §8.7.1). Before step 1, Phase 2 classifies every test-only entry in source order:

1. A test-only `pub use` is `TEST_DEPENDENCY_OUTSIDE_TEST`, at the `PKG_PATH`, and is otherwise treated as item 2 or item 4. It still binds its names (Phase 3), so a duplicate binding is still reported.
2. If test analysis is active for `P` and `P` is the root package, the entry is resolved by steps 1 to 4 against the test edges that the build activates, like any other entry, including `INACTIVE_DEPENDENCY` for an `only` list that excludes the target.
3. If test analysis is active for `P` and `P` is not the root package, the entry is `TEST_DEPENDENCY_UNAVAILABLE`, at the `PKG_PATH` ([impl/13](13-cli.md) §13.2.2), and is then treated as item 4.
4. Otherwise the entry is not resolved: no package lookup is made and no diagnostic of steps 1 to 4 is reported for it. Each of its bindings (spec/04 §4.3.3, §4.3.4, which depend only on the syntax) is an import binding with `Res::TestOnly(import)`, a resolution that is never equal to any other entity, so Phase 3 reports `DUPLICATE_BINDING` against it but never `REDUNDANT_IMPORT`.

The current adapter implements classification for the root manifest, the
no-test-analysis sentinel in item 4, the non-root diagnostic in item 3, and
foreign identity classification for a valid requirement list. For root test
analysis, a target-allowed declared test dependency reaches the Stage 3
dependency gate; an excluded test edge remains `INACTIVE_DEPENDENCY`.
`Phase1BuildContext::is_root_package` records whether package graph edges are
available to this package; it is separate from manifest `PackageRole` and the
artifact flags, and is part of the tracked package-scope query identity. The
root CLI sets it for its selected package. With test analysis enabled for a
non-root package, each test-only use gets `TEST_DEPENDENCY_UNAVAILABLE`
before target-restriction checks, while its syntax-derived names retain unique
`Res::TestOnly(import)` resolutions. The separate
`Phase1BuildContext::analyzes_tests` input controls test-edge target
restrictions independently of `is_test_build`; `sai check --tests` enables
test analysis but does not mark a test-build artifact. For root test analysis,
the adapter classifies a target-allowed declared test dependency at the Stage 3
gate, but does not verify its edge or resolve its module/member tables. A
test-only `pub use` is diagnosed at its
`PKG_PATH` and excluded from local re-export lookup in either mode. Every
binding introduced by a test-only entry has `ImportBinding::test_only = true`.
Checking ordinary references to these bindings at their use site, including
the note back to the import, remains unimplemented (§3.4.10).

### 3.3.4 Phase 3: module scopes `S(M)`

For each module, `scope` starts as a copy of `declared`. Then each import binding is inserted in source order:

1. The binding name is computed by spec/04 §4.3.3. A keyword or reserved word is `USE_BINDING_IS_KEYWORD`, and the declaration binds nothing.
2. A name that is a reserved type name is `RESERVED_TYPE_NAME`.
3. If `scope` already binds the name to the **same** entity (same `Res` after following re-exports), the warning is `REDUNDANT_IMPORT` and the earlier binding stays. If the prelude binds the name to the same entity, the warning is also `REDUNDANT_IMPORT`.
4. If `scope` binds the name to a different entity, the error is `DUPLICATE_BINDING` on the later binding, with a related location on the earlier one.
5. Otherwise the binding is inserted. If the prelude has a different binding of that name, the warning `SHADOWS_PRELUDE` is reported on the import. Phase 1 reports `SHADOWS_PRELUDE` the same way for declarations.

Module alias bindings carry `Res::Module(def_id)` (a module or an interface), which the resolver uses for the `SHADOWS_MODULE` rule (§3.4.3).

### 3.3.5 Export names

For the root package of a core-module build only (spec/04 §4.6.6 and spec/15 §15.7), Phase 1 also collects every surviving `export fn`. Its export name is the validated string of `@export_name` if present, otherwise the NFC-normalized function identifier with any raw prefix removed. Candidates are ordered by canonical module-path bytes and then source position, independent of `ModuleId` traversal order. Each later function with an already-used name is `DUPLICATE_EXPORT_NAME` at its `@export_name` string or, without one, at its function name, with a related location on the first export. The name `memory` is reserved and receives the same diagnostic with a note that memory uses that name. Export-name comparisons use UTF-8 bytes without normalization. Invalid `@export_name` arguments remain attribute diagnostics and do not supply an export name. In any non-root package, `export` functions are treated as `pub`; component builds do not export these functions and perform no export-name uniqueness check. The table is the export list consumed by [impl/06](06-wasm-lowering-and-layout.md) and [impl/07](07-component-lowering.md); whether the signatures are valid for export is checked by [impl/06](06-wasm-lowering-and-layout.md) and spec/12.

### 3.3.6 Lang items

Lang items are identified by canonical path (spec/16 §16.3.1). The query `lang_items()` builds:

```rust
pub struct LangItems {
    items: [Option<DefId>; LangItemKind::COUNT],   // indexed by LangItemKind
}
impl LangItems {
    pub fn get(&self, k: LangItemKind) -> DefId;   // missing: INTERNAL_COMPILER_ERROR
}
```

`LangItemKind` extends the `LangItem` enum of [impl/02](02-frontend-ast-hir.md) §2.9.1 with the items that only later stages refer to. Each kind maps either to one canonical path from the tables of spec/16 §16.3.2–§16.3.4 (for example `Option ↦ sai:core/option.Option`, `ArenaRegionNew ↦ sai:alloc/arena.Arena.region_new`), or, for the "trait items" group, to a member of a lang-item trait found by name in that trait's member namespace (for example `FuturePoll ↦ poll` in `Future`). Trait items are not lang items of their own; they are a lookup cache. The full list of kinds this chapter and [impl/04](04-mir.md) use is:

| Group | Kinds |
|---|---|
| types | `Str`, `Slice`, `SliceMut`, `Option`, `OptionSome`, `OptionNone`, `Result`, `ResultOk`, `ResultErr`, `Ordering` (and its three variants), the six range types, `Ptr`, `PtrMut`, `MaybeUninit`, `FormatSpec`, `Poll`, `PollReady`, `PollPending`, `Context`, `CStr`, `Out`, `Gen`, `String`, `Vec`, `Bytes`, `Box`, `Arena` |
| traits | `Copy`, `Clone`, `Drop`, `Default`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Hasher`, the ten operator traits, `Index`, `IndexMut`, `Fn`, `FnMut`, `FnOnce`, `IntoIterator`, `Iterator`, `Format`, `FmtWriter` (`Writer`), `FormatPad`, `FormatInt`, `FormatFloat`, `Future`, `Stream`, `Arbitrary` |
| trait items | `CloneClone`, `DropDrop`, `PartialEqEq`, `PartialOrdLt`, `PartialOrdLe`, `PartialOrdGt`, `PartialOrdGe`, the ten operator methods, `IndexIndex`, `IndexMutIndexMut`, `IntoIteratorIntoIter`, `IteratorNext`, `FormatFormat`, `WriterWriteStr`, `FuturePoll`, `FutureOutput`, `StreamPollNext`, `StreamItem`, `IteratorItem`, `IndexOutput`, the operator `Output` types |
| functions | `Panic`, `Unreachable`, `ArrayFilled`, `StringNew`, the numeric conversion intrinsics (`WrappingTo`, `SaturatingTo`, `ToF32`, `ToF64`, `TruncTo`, `CheckedTruncTo`, one kind per declaring type), the range `Index` impls, `SizeOf`, `AlignOf`, `Transmute`, `Zeroed`, `Forget`, the `core.ptr` intrinsics, `Allocate`, `Deallocate`, `Reallocate`, `ArenaRegionNew` |
| compiler-internal | `StreamNext` (§3.7.11), `FnCall`, `FnMutCallMut`, `FnOnceCallOnce` (§3.11.4) |

The three compiler-internal function-trait methods have no source declaration: spec/16 §16.6.3 says that the function traits have no nameable methods. They are synthesized by `trait_def` for the lang-item traits `Fn`, `FnMut` and `FnOnce` with the receivers of spec/05 §5.5.4, so that a closure call has an ordinary trait-method callee in MIR. Their names `call`, `call_mut` and `call_once` are internal and can never be written in source; they appear only in MIR dumps and symbol names (N-E-26). `StreamNext` is likewise internal (§3.7.11).

Resolution of a canonical path walks the module tree of `sai:core` or `sai:alloc` and the member namespaces, without visibility checks (package-private items such as `Arena.region_new` are lang items, spec/16 §16.3.4). A missing item, or an item of the wrong kind, is `INTERNAL_COMPILER_ERROR` (spec/16 §16.3.1 rule 4). Lang-item signatures are not compared with a reference at run time of the compiler; the conformance suite checks them ([impl/15](15-conformance-and-testing.md)).

### 3.3.7 The prelude

The prelude is a fixed table `prelude: BTreeMap<Symbol, Res>` built once from the list of spec/16 §16.4, by canonical path as for lang items. The reserved type names map to `Res::PrimTy(..)`: `Int ↦ I64`, `UInt ↦ U64` (D-TYPE-01), `Never ↦ PrimTy::Never`. `Array` maps to the built-in array constructor `Res::PrimTy(PrimTy::Array)`, because `Array<T, N>` is a structural type (spec/05 §5.1.3) even though it has a library module for its associated functions (§3.9.3).

**Canonical prelude provider (IR-530), primitive resolutions (IR-533), and early primitive lookup (IR-546–549).** `sai_resolve::prelude` defines all 50 canonical standard-library item paths and the 19 primitive type and constructor names, and builds the complete set of 69 names from a caller-supplied canonical-item resolver. The builder checks each item’s expected kind and standard-package identity. `Res::PrimTy(PrimTy)` represents primitive resolutions, while `PreludeBinding` remains explicit so callers can distinguish built-in bindings from canonical library items. The provider uses a symbol-keyed hash map for lookup plus an entry slice sorted by spelling, since `Symbol` does not implement `Ord`. Top-level struct, alias, enum, resource, const, free-function, and invariant type heads consult a primitive-only table after generic parameters, rejected test-only references, and the exact file's module scope. Const, function, and invariant type-head results are kept separate from expression `ModuleResolution`; qualified suffixes and other misses stay available to later resolution. The static table participates in the tracked type-owner query identity. Non-primitive canonical entries such as `Option` and `String` remain deferred because this checkout does not yet supply the `sai:core` and `sai:alloc` package scopes needed to construct their real `DefId`s.

## 3.4 Resolving paths in owners

### 3.4.1 Resolutions

```rust
pub enum Res {
    Def(DefKind, DefId),          // items, variants, fields are not here (fields are resolved by typeck)
    Module(DefId),                // a module alias target: module or interface
    PrimTy(PrimTy),               // I8 … USize, F32, F64, Bool, Byte, Char, Never, Array
    SelfTy { owner: DefId },      // `Self` in the trait or impl `owner`
    SelfValue,                    // `self`
    Local(BindingId),
    TestOnly(ImportIndex),        // a binding of an unresolved test-only use (§3.3.3); never equal to another entity
    Err,                          // already diagnosed
}
pub struct PathRes {
    pub base: Res,                // resolution of the longest prefix that spec/04 resolves
    pub resolved_segments: u32,   // number of segments covered by `base`
    pub segment_count: u32,       // path/qualified-chain length; a local receiver counts its Path only
}
pub struct ResolvedOwner {
    pub paths: HashMap<HirId, PathRes>,          // key: the HirId of a Path, or of the outermost Field of a qualified chain
    pub chain_heads: HashMap<HirId, HirId>,      // inner Field / Path of a qualified chain → its outermost node
    pub labels: HashMap<HirId, Result<HirId, ()>>, // Break/Continue → target Loop
    pub binding_names: IndexVec<BindingId, (Symbol, Span, BindingKind)>,
    pub diagnostics: Vec<Diagnostic>,
}
pub enum BindingKind { Let, Var, Param, VarParam, Pattern { var: bool }, ClosureParam { var: bool }, Synthetic }
```

`resolve_owner(owner)` resolves every path in the owner's HIR: generic parameters and bounds, signature types, the body, contracts, invariant and refinement predicates, and attribute targets. `DefKind` distinguishes `Fn`, `AssocFn`, `Struct`, `Enum`, `Variant`, `Distinct`, `Refined`, `Alias`, `Resource`, `Trait`, `TyParam`, `ConstParam`, `Const`, `Effect`, `Interface`, `World`, `WorldFn`, `ExternFn`, `Closure`, `AsyncBlock`, `AssocTy`. `WorldFn` is a function item of a world ([16](16-interfaces-and-wit.md) §16.8.2).

In a package that implements interfaces, `resolve_owner` also applies the redirect of [16](16-interfaces-and-wit.md) §16.7.4: a path to an implemented interface function or a bound resource resolves to the implementation, and `ResolvedOwner` gains the field `redirected: HashMap<HirId, DefId>` with the original resolution. World references and world members are resolved by [16](16-interfaces-and-wit.md) §16.8.1 and §16.8.2.

### 3.4.2 Qualified chains

HIR keeps `a.b.C` as nested `Field` expressions over `Path(a)` ([impl/02](02-frontend-ast-hir.md) §2.8.5, N-E-18). The resolver re-interprets such a chain (N-E-27):

1. For each maximal chain `Field(…Field(Path(s1), s2)…, sn)` in expression position, with each `Field` either bare or the callee of a `Call`, resolve `s1` by spec/04 §4.5.1. A `Field` node belongs to the chain only if its span starts at the start of the span of `Path(s1)`. impl/02 removes parentheses from HIR, and this test restores the distinction of spec/03 §3.13.4, where a path starts at an `Ident` primary expression: in `(model).Shape` the `Field` span starts at `(`, so `.Shape` is a field access on the value of the parenthesized expression `(model)`, which is `EXPECTED_VALUE_FOUND_TYPE` (spec/06 §6.4.1), and not a path segment.
2. If `s1` is a local or `self`, the chain is not a path: `paths[Path(s1)] = Local(..)`, and the `Field` nodes are field accesses or method calls for the type checker.
3. Otherwise follow spec/04 §4.5.2 steps 2–5: consume segments while the current entity is a module alias (`UNRESOLVED_NAME` for a missing member, `PRIVATE_ITEM` for an invisible one), then stop at the first entity that is not a module alias. The node of the last consumed segment `sk` becomes the **head** of the path: `paths[head] = PathRes { base, resolved_segments: k, segment_count: k }`, and every inner node maps to the head in `chain_heads`.
4. If `base` is a type, type parameter, `Self` or trait, the following `Field` nodes are **type-relative segments**. They are resolved by the type checker (§3.9.5), which may continue with field accesses and method calls once a segment has produced a value.
5. If `base` is a constant, the following `Field` nodes are field accesses and method calls on its value, as in item 2 (spec/04 §4.5.2 step 3, N-A-67): `model.ORIGIN.x` reads the field `x` of the constant `ORIGIN`. If `base` is a function, extern function, effect or world and a chain segment follows, the error is `WRONG_NAME_KIND` (spec/04 §4.5.2 step 5), reported on the first remaining segment. The chain then resolves to `Res::Err`. The exception is a world of the current package followed by exactly one segment, which names an imported world function ([16](16-interfaces-and-wit.md) §16.8.2). A view interface is a module alias: its next segment is looked up in its members ([16](16-interfaces-and-wit.md) §16.4.8 step 5).

Type paths and pattern paths have explicit segments in HIR (`TyKind::Path`, `PatKind::Ctor`) and are resolved by the same procedure without the field-chain step.

**Current module-prefix walker (IR-499).** `sai_resolve::module_path::resolve_module_prefix`
implements the segment-consumption step after a caller has resolved the first
name. It follows module/interface members, stops before a suffix whose current
resolution is no longer module-like, and distinguishes a missing member, an
inaccessible member, and a module target whose package scope was not supplied.
The returned segment count is relative to the supplied suffix. Generic
arguments remain attached to the input segments and are not resolved by this
walker. `PathRoot` handling, unqualified-name/rib lookup, expression `Field`
chain recognition, type-relative lookup, and diagnostic construction remain
outside this helper.

**Top-level type-owner prefix lookup and access diagnostics (IR-542–IR-543).**
The driver builds a temporary
`ModuleMemberIndex` from the current package and a validated closure of every
reachable active source package. The closure is separate from Phase 1's
direct-import scopes: it is checked against the same package graph, snapshot,
target configuration, module trees, and exact direct-source-edge fingerprints
before qualified lookup. Direct-edge inputs are opaque adapters derived from
retained finalized collections, so callers cannot pair detached scopes with a
claimed collection fingerprint; the CLI retains those collections in
dependency-first order. Top-level struct, alias, enum, and resource type paths
resolve module and interface segments through that index, then stop
before the first non-module type so type-relative tails remain for type
checking. Missing members and inaccessible members are retained as typed
errors with the reference segment and declaration span. Missing source scopes
fail closure construction; artifact and WIT member suffixes remain deferred
to their later resolvers. An erroneous head continues to suppress suffix
lookup. For current top-level type owners, an inaccessible member becomes a
snapshot-bound `PRIVATE_ITEM` diagnostic at the reference segment with the
defining declaration as a related location, including when that declaration
is in a foreign source file. IR-545 maps missing source members to
`UNRESOLVED_NAME`; missing target scopes remain closure errors rather than
source diagnostics.

**Tracked owner path query and `sai check` integration (IR-544).** Each
top-level type owner is resolved through a tracked query keyed by requester
package and owner index. Its identity includes the requester collection, the
validated active-source closure, and the concrete lookup maps used by module
and interface traversal. The query owns its staged paths and returns only
owned path results and errors; snapshot-bound `Diagnostic` values are rebuilt
at the caller boundary. `sai check` retains finalized dependency collections,
builds the active-source closure for each package, and emits the resulting
`PRIVATE_ITEM` and `UNRESOLVED_NAME` diagnostics with workspace-wide source
lookup for primary and foreign declaration locations. The command still reports
`FEATURE_NOT_IMPLEMENTED` and does not claim complete analysis or produce a
success result record.

**Function type-path member diagnostics (IR-573).** The closure-aware
free-function adapter maps typed private and missing source-member errors to
snapshot-bound diagnostics through the same mapper used by top-level type
owners. `FunctionOwnerResolution` exposes this optional diagnostic channel
separately from label, local, module, expression, and test-only results;
package-local calls retain their existing typed path errors for callers to map
with their own snapshot context. Driver and CLI regressions cover both
signature paths and body type annotations. `sai check` collects these function
diagnostics alongside the top-level type-owner diagnostics and passes both
through the existing package-aware source renderer. The command continues to
report `FEATURE_NOT_IMPLEMENTED`.

**Const type-path member diagnostics (IR-574).** The closure-aware const
adapter maps typed private and missing source-member errors from both declared
types and initializer annotations through the same snapshot-aware mapper.
`ConstOwnerResolution` exposes this optional channel independently of
initializer expression, local, module, and test-only results. Package-local
calls continue to expose typed errors without constructing diagnostics.
`sai check` renders const diagnostics alongside function and top-level
type-owner results; the command remains incomplete.

**Predicate-owner type-path member diagnostics (IR-575).** The closure-aware
invariant, refined/distinct, and extern-function adapters map typed private and
missing source-member failures through the same snapshot-aware mapper.
`PredicateOwnerResolution` exposes the result in a separate optional channel,
so predicate, local, module, and test-only results remain independent.
Package-local calls retain typed path errors without constructing mapped
diagnostics. `sai check` renders predicate-owner diagnostics alongside the
existing type, function, and const diagnostics. Locked CLI coverage exercises
transitive re-export paths from invariant/refined predicates and an extern
signature; the command remains incomplete.

**Trait/impl header type-path member diagnostics (IR-576).** The closure-aware
trait and impl header adapter maps typed private and missing member failures
through the same snapshot-aware mapper. `TraitImplHeaderResolution` exposes a
separate optional channel, leaving package-local path resolution and existing
test-only results unchanged. `sai check` collects diagnostics from trait and
impl headers alongside the previously mapped owner families. Associated
members retain child-owned path results and diagnostics, recorded in IR-577.

**Trait/impl associated-member type-path diagnostics (IR-577).** The
closure-aware trait/impl member adapter maps typed private and missing member
failures independently for each associated type or method child owner.
`TraitImplMemberTestOnlyResolution` exposes a separate optional diagnostic
channel, preserving the child's typed paths, test-only diagnostics, and
local/expression results. Package-local callers retain typed path errors
without snapshot-bound diagnostics. `sai check` collects each child result and
reports parent-owner analysis failures once. The command remains incomplete.

**Interface/resource member type-path diagnostics (IR-578).** The
closure-aware interface adapter maps typed private and missing member failures
independently for each direct interface declaration and nested resource-method
owner. Each child exposes a separate optional diagnostic channel; parent
interface errors, resource-method structure errors, test-only results, and
typed path results remain distinct. Package-local calls leave mapped
diagnostics absent. `sai check` collects member and resource-method diagnostics
independently and reports structural failures at their owning level.

**World function type-path diagnostics (IR-579).** The closure-aware world
adapter maps typed private and missing member failures independently for
import/export function signature owners. World module/interface references and
includes keep their world namespace handling and receive no type-member
diagnostics. Package-local calls leave mapped diagnostics absent. `sai check`
collects each item independently and reports a parent world analysis failure
once.

**Qualified missing-member diagnostics (IR-545).** The module path walker
retains the exact `ModuleTarget` when a member is missing. The driver emits a
snapshot-bound `UNRESOLVED_NAME` at that segment with the canonical package
identity and module path, up to three visible member names within bounded
Damerau–Levenshtein distance two, deterministic ranking, and the same names
in structured diagnostic data and a help child. Suggestion enumeration uses
the target package's effective visibility, so foreign private members are
excluded while same-package private members remain eligible. No `use` fix is
offered for a missing qualified suffix: importing the suffix into the current
scope cannot make the original `module.missing` path resolve. Unqualified
name fixes remain a separate resolver obligation.

**Primitive Prelude fallback for type-owner heads (IR-546).** Top-level
struct, alias, enum, and resource paths consult the fixed primitive binding
table after generic parameters and the exact file's module scope. A primitive
head resolves as `Res::PrimTy`; any type-relative suffix remains for the type
checker. The tracked path-query identity includes a fingerprint of the
primitive name-to-type mapping. Other misses, including canonical standard
items such as `Option` and `String`, remain deferred until real `sai:core` and
`sai:alloc` member scopes are provided. The checkout has no standard-library
source tree or provider for those scopes, so the complete Prelude must not be
assembled with placeholder definition identities.

**Free-function type heads (IR-547).** Top-level function signatures and
bodies expose a separate partial type-path result. The pass resolves function
type and const generic parameters first, then preserves rejected test-only
heads as `Res::Err`, consults the exact file's module scope, and finally
consults the primitive-only Prelude. Qualified paths retain their original
segment count while only the head is resolved; canonical non-primitive names
and other misses remain deferred. These type paths do not enter
`ModuleResolution`, which continues to represent local and module lookup for
expression names. The pass runs even for owners without bodies so signature
types are still available to later phases.

**Constant type heads (IR-548).** Top-level const owners expose the same
partial result for their declared type and type paths in initializer bodies.
Rejected test-only heads remain `Res::Err`; other heads use the exact file
module scope before the primitive-only Prelude, and unresolved canonical items
such as `Option` stay deferred. Type paths remain separate from module lookup
for initializer expressions. Const analysis computes this result before
checking whether an initializer body exists.

**Invariant type heads (IR-549).** Invariant owners expose partial type-head
results for their target and predicate-body types. The pass preserves rejected
test-only heads independently of diagnostic collection, uses the exact file
module scope before the primitive-only Prelude, and keeps its result separate
from expression `ModuleResolution`. Qualified suffixes and canonical names
without supplied standard-package scopes remain deferred.

**Extern-function type heads (IR-550).** Extern function owners expose the
same optional partial result for signature and contract/body type paths,
including bodyless signatures. Resolve function type and const generics before
independently rejecting test-only heads, then use the exact source file's
module scope and primitive-only Prelude. Keep the result separate from
expression module lookup and predicate diagnostics. Qualified suffixes and
canonical names without supplied standard-package scopes remain deferred.
Refined/distinct predicate owners were added in IR-551; other predicate
adapters still leave this channel absent.

**Refinement and distinct type heads (IR-551).** Refined and
refinement-bearing distinct owners expose partial type-head results for their
base types, generic bounds when present in HIR, and predicate-body type
paths. Distinct type and const generics resolve to their HIR definitions before
test-only rejection, exact-file module lookup, and the primitive-only Prelude.
Refined HIR deliberately omits generic parameters because refined types cannot
be generic; for invalid generic syntax, the ItemTree names shadow module
bindings as `Res::Err` without fabricating generic definition IDs. Generic
syntax paths omitted from HIR remain diagnostic-only and do not appear in the
ordinary path map. Expression `ModuleResolution` remains independent.
Qualified suffixes and canonical names without supplied standard-package
scopes remain deferred.

**Trait and impl header type heads (IR-552).** Direct trait and impl headers
expose partial type-head results for generic bounds and const-parameter types,
`where` clauses, supertraits, impl trait or interface targets, and impl self
types. Resolve exact type and const generic definitions first, then reject
test-only imports independently of diagnostic collection, and use the exact
source file's module scope before primitive Prelude entries. These results
remain separate from test-only diagnostic paths. Qualified suffixes and
associated declarations or methods are deferred; canonical non-primitive
names also remain deferred until standard-package definitions are available.

**Associated trait/impl member type heads (IR-553).** Each associated child
owner exposes partial type-head results for associated type bounds or
definitions and for method signatures, generic bounds, `where` clauses, body
annotations, casts, and contract bodies. Parent generics are included in each
member scope, with method generics taking precedence when names overlap. The
result stays independent from test-only diagnostics and expression lookup,
uses the parent's exact file scope, and resolves no qualified member suffixes.
Failure in one child does not stop type results for sibling declarations.

**Interface member type heads (IR-554).** Each retained direct interface
declaration and resource method exposes a partial type-head result under its
canonical child owner. The HIR walker covers declaration field, alias, variant,
refined-base, and signature types; exact type and const generic definitions
precede test-only rejection, file-local module bindings, and primitive
Prelude entries. Invalid refined generic syntax is retained as `Res::Err`
shadowing without inventing a generic definition. Recovery-only refinement
predicates, bodies, and contracts remain outside the HIR type result. Qualified
suffixes and non-primitive Prelude names remain deferred.

**World function signature type heads (IR-555).** Each retained world
function import or export exposes a partial type-head result for its HIR
signature. Resolve retained type and const generic definitions first; names
recovered from invalid generic syntax shadow module bindings as `Res::Err`
without fabricated definitions. Then reject test-only heads and use the exact
source file's module scope before the primitive-only Prelude. World module,
interface, and include references stay outside this channel even where the
diagnostic path walker classifies a module path as a type path. Invalid
function `where` clauses and body annotations omitted from HIR remain
diagnostic-only. Qualified suffixes and non-primitive Prelude names remain
deferred.

**Qualified local function type prefixes (IR-556).** Function type paths
continue through current-package module and interface members after resolving
their first head. Stop when a member is no longer module-like, retaining the
full path length and leaving its type-relative suffix for later resolution.
Missing local members are retained as typed path errors for diagnostic
mapping. A foreign module alias whose package scope is not in this local
index remains deferred; validated foreign source closure, visibility errors,
and WIT member scopes are later work. Generic, test-only, exact-file module,
and primitive Prelude precedence remains unchanged, and expression module
lookup stays separate.

**Qualified local const type prefixes (IR-557).** Const declared types and
initializer type paths continue through current-package module and interface
members. Stop at the first non-module member, retain the full path length and
type-relative suffix, and keep missing local members as typed path errors for
later diagnostic mapping. Rejected test-only heads remain `Res::Err` and do
not start member traversal. Foreign module aliases stay deferred until a
validated active-source closure is available.

**Qualified local predicate-owner type prefixes (IR-558).** Invariant,
refined, and refinement-bearing distinct owner type paths continue through
current-package module and interface members. Stop at the first non-module
member, retain the full path length and type-relative suffix, and keep missing
local members as typed path errors. Same-package private members remain
visible. Rejected test-only heads do not produce member lookup errors, and
expression lookup remains separate. Foreign scopes and WIT members stay
deferred.

**Qualified local extern type prefixes (IR-559).** Extern function signature
and contract type paths continue through current-package module and interface
members. Stop at the first non-module member, retain the full path length and
type-relative suffix, and keep missing local members as typed path errors.
Rejected test-only heads do not produce member lookup errors. Expression and
predicate resolution remain separate; foreign scopes and WIT members stay
deferred.

**Qualified local trait/impl header prefixes (IR-560).** Trait and impl header
type paths continue through current-package module and interface members.
Stop at the first non-module member, retain the full path length and
type-relative suffix, and keep missing local members as typed path errors.
Rejected test-only heads remain `Res::Err` without suffix lookup. Generic and
effect path candidates keep their existing classifications; foreign scopes
and WIT members stay deferred.

**Qualified local trait/impl associated-member prefixes (IR-561).** Associated
type bounds and definitions, method signatures, and retained method-body type
paths continue through current-package module and interface members. Each
child owner keeps its own result and generic scope. Stop at the first
non-module member, retain the full path length and type-relative suffix, and
keep missing local members as typed path errors. Rejected test-only heads
remain `Res::Err` without suffix lookup, even if diagnostic collection fails.
Expression lookup remains separate; foreign scopes and WIT members stay
deferred.

**Qualified local interface/resource prefixes (IR-562).** Direct interface
declarations and resource-method type paths continue through current-package
module and interface members. Stop at the first non-module member, retain the
full path length and type-relative suffix, and keep missing local members as
typed path errors. The shared resolver's world-item call still has no member
index, so world module, interface, and include paths keep their separate
namespace behavior. Foreign scopes and WIT members stay deferred.

**Qualified local world signature prefixes (IR-563).** Import and export
function type paths continue through current-package module and interface
members. Only function-signature candidates receive this type lookup; world
module, interface, and include references remain in their own namespace.
Stop at the first non-module member, retain the full path length and
type-relative suffix, and keep missing local members as typed path errors.
Foreign scopes and WIT members stay deferred.

### 3.4.3 Ribs

Local scopes are implemented with a stack of **ribs**, one per scope of spec/04 §4.7.1 below the module scope:

```rust
struct Rib { kind: RibKind, bindings: SmallVec<[(Symbol, BindingId); 4]> }
enum RibKind {
    ItemGenerics(DefId), FnGenerics(DefId),       // generic parameter names → Res::Def(TyParam | ConstParam)
    Params, Block, Arm, Condition, LoopPattern,   // local bindings
    ClosureBoundary(LocalDefId), AsyncBoundary(LocalDefId),  // label reset (§3.4.5)
    ContractScope { ensures: bool },              // `result` and `old` are HIR nodes, not bindings
}
```

1. Lookup of an unqualified value name scans the ribs from the top. Local ribs are searched first, then generic ribs, then `S(M)`, then the prelude (spec/04 §4.5.1). Closure and async boundaries do not stop value lookup: a local of an enclosing body is visible and becomes a capture (§3.11.2).
2. In type position, local ribs are skipped. In pattern position, a bare lowercase identifier is a binding and is never looked up; a bare uppercase identifier or a path skips local ribs, and a generic parameter found there is `WRONG_NAME_KIND`.
3. A `let` or `var` statement pushes its bindings after its initializer (and, for let-else, after its `else` block) has been resolved, into the current `Block` rib, so the bindings are visible from the next statement (spec/04 §4.7.1).
4. A name already bound in the **same** `Block` rib by an earlier statement is `DUPLICATE_BINDING` on the new binding, with a related location on the earlier one. The same applies to two parameters of one `Params` rib. Bindings within one pattern were already checked by impl/02 (§2.9.13). Shadowing across ribs is silent (D-MOD-03).
5. Every new local binding (parameter, closure parameter, pattern binding, `let`/`var`) whose name is a module alias of `S(M)` is `SHADOWS_MODULE`. The binding is still introduced, so later uses resolve to it and do not cascade.
6. A generic parameter of a method whose name is a generic parameter of the enclosing `impl` or `trait` is `SHADOWS_GENERIC_PARAM`; two generic parameters of one list are `DUPLICATE_BINDING`.
7. For `match` arms, the arm rib holds the pattern's bindings and covers the guard and the body. For `if let` and `while let`, which impl/02 lowers to `Match`, the user arm's rib covers its body only, which is the condition scope of spec/04 §4.7.1. For `for`, the user pattern is inside the `Some(..)` arm of the inner match, which gives the fresh-per-iteration loop pattern scope.

**Current owner-local resolution helper (IR-517).** `sai_resolve::owner::resolve_owner_locals` walks a borrowed `Body` without letting arena references escape. It registers parameters, closure parameters, `let`/`var` pattern bindings, match-arm bindings, region bindings, and generated loop-pattern bindings in lexical ribs; it resolves the local head of a plain value path to `Res::Local`, preserving the full path segment count, and records `Symbol::ERROR` paths as `Res::Err`. A local receiver such as `x.a.b` records the `Path(x)` node with one resolved segment; the following fields remain for later type/member resolution. Module alias names from the current `S(M)` are an explicit input and matching local declarations receive `SHADOWS_MODULE`. The walk covers the body value, function contracts, and invariant predicates. A local miss is left for generic, module, or prelude lookup, and non-expression type/pattern paths are not resolved here.

The helper takes the borrowed `Body`, its `HirSourceMap`, and the current `WorkspaceSnapshot`. It requires the Body and map to share the lowering-time `HirSourceId`, then checks owner identity and the map's file, path, package, document version, and raw bytes against that snapshot. This prevents an older Body from being paired with a new source map and permits an unchanged owner file to retain its source map when an unrelated workspace file changes. The owned `LocalResolution` records the file provenance, body-source identity, and module-alias set used for resolution; its provenance check takes the current map, snapshot, and aliases and rejects changed inputs without invalidating on unrelated workspace changes. This is an uncached local slice, not the `resolve_owner` query or the complete `ResolvedOwner`: type-relative suffixes, non-type signature/generic paths, chain heads, label results, and composed fallback lookup remain deferred. IR-547 and IR-548 add separate partial type-head results for top-level function and const owners; IR-549 adds the same for invariant targets and predicate-body types. Duplicate binding recovery keeps the first binding in a rib; the later duplicate receives `DUPLICATE_BINDING`.

**Function-owner driver bridge (IR-518).** `RootDatabase::resolve_function_owner_analyses` now runs the local pass alongside loop-label resolution for each current target-filtered top-level function descriptor. It uses the source map produced by that same lowering and selects aliases from the exact `S(M)` associated with the descriptor's file. A module alias is an imported module/interface binding with no item list; direct interface declarations are excluded. A syntax-known module binding from an unresolved test-only use remains an alias for `SHADOWS_MODULE` even when Phase 1 retains `Res::TestOnly`. Results retain the existing collection, snapshot, and target provenance, and failures remain isolated per function owner. The old label-named adapter remains as a compatibility entry point to the combined result. IR-527 adds test-only reference diagnostics for module-fallback body expressions and non-expression type, effect, and pattern paths in function signatures and bodies. IR-547 adds separate partial type-head resolution for free-function signatures and body type paths; type-relative suffixes, complete Prelude lookup, general owner kinds, and final unresolved-name diagnostics remain later stages.

**Function test-only reference diagnostics (IR-527).** After module lookup, the
top-level function adapter checks deferred body-expression paths against the
effective binding origin and diagnoses references to test-only imports. It
also walks parameter and return types, generic bounds and const parameter
types, where predicates, nested function-pointer/tuple/borrow types, generic
type arguments, body type annotations and casts, closure parameter and return
types, explicit generic type arguments, and constructor/range pattern paths.
Effect references are checked in function signatures, nested function-pointer
types, and bounds; built-in effect roots retain their spec/08 precedence over
module bindings. Type and pattern candidates are checked after function
generic parameters, while effect lookup does not use generic or local-value
shadowing. Local value ribs do not shadow types. A qualified path checks its
first module head and records one resolved segment, leaving the tail to later
member/effect lookup. Rejected type, effect, and pattern heads are kept in a
separate `FunctionTestOnlyPathResolution`, not expression
`ModuleResolution`. Both expression and non-expression references use the
originating use declaration's complete `PKG_PATH` as a related note, and
rejected results become `Res::Err` to suppress cascades. Diagnostics are
owner-scoped and sorted across the signature and body. This remains a partial
fallback: ordinary type/effect resolution, Prelude lookup, final
unresolved-name diagnostics, and non-function owner kinds are not handled
here.

### 3.4.4 Diagnostics of lookup

| Situation | Code | Notes |
|---|---|---|
| no binding found | `UNRESOLVED_NAME` | up to three candidates visible at the point of use (locals, generics, `S(M)`, prelude) within edit distance 2; a `use` fix when exactly one module of the current package or a declared dependency has a visible item of that exact name; the fix inserts `use <path>.{x}` after the last `use` of the file |
| kind does not fit the position | `WRONG_NAME_KIND`, or `EXPECTED_VALUE_FOUND_TYPE` (spec/06) for a type, module or trait in value position | names expected and found kinds |
| a record type or record variant used as a value, not called | `RECORD_CONSTRUCTOR_AS_VALUE` (spec/06) | reported by the type checker, which knows whether the path is a callee |
| `Self` outside a trait or impl, `self` outside a method or predicate | `SELF_OUTSIDE_IMPL` (spec/05) | |
| an invisible item or member | `PRIVATE_ITEM` | no fix outside the defining package |

`Err` nodes and `Symbol::ERROR` names are never reported ([impl/02](02-frontend-ast-hir.md) §2.8.6) and resolve to `Res::Err`.

### 3.4.5 Labels

Labels form one namespace per function, closure or `async` body (spec/04 §4.4.5). The resolver keeps a stack of `(label: Option<Symbol>, loop: HirId, source: LoopSource)` entries per body and saves and clears it at every `ClosureBoundary` and `AsyncBoundary` rib, restoring it afterwards (spec/06 §6.1.1: `Γ.loops` is reset).

1. Entering a `Loop` with a label already on the current stack is `LABEL_SHADOWED`, on the inner label, with a related location on the outer one. The loop is still entered with its label, and later references resolve to the innermost one.
2. `break 'l` and `continue 'l` resolve to the innermost loop on the stack with label `'l`. None is `UNDECLARED_LABEL`, with up to three labels of the current body within edit distance 2.
3. An unlabeled `break` or `continue` resolves to the top of the stack. An empty stack is `BREAK_OUTSIDE_LOOP` or `CONTINUE_OUTSIDE_LOOP` (spec/06).
4. The synthesized `break` of a `while`, `while let` or `for` desugaring ([impl/02](02-frontend-ast-hir.md) §2.9.2–§2.9.5) resolves like any unlabeled `break`; it is directly inside its own loop, so it always targets it.
5. A `return` in a const initializer is `RETURN_OUTSIDE_FUNCTION` (spec/06 §6.16.5). In a predicate (a refinement predicate, an invariant element, a `requires` or `ensures` clause), `return`, `break` and `continue` are `INVALID_CONTRACT_EXPRESSION` (spec/09 §9.1.2), and neither `RETURN_OUTSIDE_FUNCTION` nor a label error is reported for them (spec/06 §6.16.5, N-E-31). Both are reported here, because the owner kind and the `ContractScope` rib are known.
6. **`UNUSED_LABEL`** ([spec/15](../spec/15-attributes-and-lints.md) §15.18.4). Each stack entry also records whether a `break` or `continue` resolved to it by items 2 to 4. When a labeled `Loop` is left, an entry that no `break 'l` or `continue 'l` resolved to is `UNUSED_LABEL`, at the label, with a machine-applicable fix that removes the label and its `:` and the space that follows. Only labeled jumps count: an unlabeled `break` that targets a labeled loop does not use the label. A jump inside a nested closure or `async` block can never target the loop (the stack is saved and cleared at the boundary). The lint is not reported when the loop body contains an `Err` node (§3.16), because a jump may have been lost in recovery. The loop's own desugared `break` (item 4) is unlabeled and never counts. The level is applied by [impl/11](11-diagnostics.md) §11.3.

The result is `labels: Break/Continue HirId → Loop HirId`. `INVALID_BREAK_VALUE` needs the loop's `LoopSource` and is reported by the type checker (§3.7.10).

**Current implementation slice (IR-497).** `sai_resolve::owner::resolve_loop_labels`
now performs the loop-label portion as an iterative walk over an owner body. It
returns break/continue target mappings, duplicate-label, undeclared-label, and
outside-loop diagnostics, and unused-label candidates. It resets the visible
label stack at closure and async-block boundaries, suppresses an unused-label
candidate when an error node occurs in that loop body, and converts decoded HIR
spans through the supplied source snapshot before constructing diagnostics.
Unused-label candidates retain their HIR spans: lint-level application and the
machine-applicable removal fix remain deferred until the snapshot-aware syntax
fix planner is connected. This helper is not yet the tracked `resolve_owner`
query and does not handle owner-kind-specific contract/return errors; those
still require the owner context described above. Undeclared-label suggestions
use Damerau–Levenshtein distance over Unicode scalar values and return at most
three visible labels within distance two. Its tests cover labeled and unlabeled
jumps, shadowing, closure/async boundaries, error recovery, source offset
conversion, and bounded label suggestions.

**Driver integration (IR-511).** `RootDatabase` can collect Phase 1 package
scopes together with private snapshot, target, and module-tree provenance, then
resolve loop labels for the collected direct top-level function descriptors.
The adapter checks that provenance, rebinds current target-filtered inputs by
file, and lowers each function in a temporary owner-local HIR arena. Results
retain snapshot and target provenance and contain a per-owner success or
lowering error, so a failed owner does not stop the rest of the package. This
result carries the collection input fingerprint along with its snapshot and
target, and exposes a check for reuse with the matching package collection.
This integrates `Body::value` loop labels only; function contract control-flow
diagnostics are added in IR-512.

**Function contract control flow (IR-512).** `resolve_owner_control_flow`
combines ordinary label resolution for `Body::value` with an owner-aware scan
of function `requires` and `ensures` expressions. Source-written `return`,
`break`, and `continue` in those predicates receive
`INVALID_CONTRACT_EXPRESSION`; they are excluded from the label map and do not
also receive `RETURN_OUTSIDE_FUNCTION`, `BREAK_OUTSIDE_LOOP`,
`CONTINUE_OUTSIDE_LOOP`, or `UNDECLARED_LABEL`. The scan ignores synthetic
desugaring breaks and does not enter closure or async-block bodies, which are
separate owner bodies.

**Function contract restrictions (IR-513).** The same bridge also
reports `INVALID_CONTRACT_EXPRESSION` for source-written `?`, `.await`,
assignments, compound assignments, `var` statements, `unsafe` blocks, `region`
blocks, and closure expressions. It identifies `var` statements by their HIR
`VarStmt` desugaring context, leaving ordinary `let` statements allowed even
when a pattern binds a mutable local. It reports a closure expression without
walking its body. Async-block expressions are not in the prohibition list, and
their bodies remain separate owners.

**Invariant predicates (IR-514).** The same uncached bridge applies these
restrictions to `Body::invariant_preds` for direct top-level invariant owners.
Its Phase 1 descriptors preserve canonical owner IDs and source order; the
driver rebinds current target-filtered inputs and returns owned diagnostics
tagged with snapshot, target, and package-collection provenance. IR-521 also
runs the local-name pass over those predicate expressions using the same
lowered `Body` and `HirSourceMap`, so block bindings and their later uses are
resolved and module-alias shadowing is reported. This remains a partial local
pass: implicit `self` and other owner-specific lookup are deferred, as are
generic and prelude fallback, associated and other owner kinds, tracked
`hir_owner`, and full `resolve_owner`. IR-529 adds module fallback for
invariant expression paths only; it does not provide general type, member, or
module lookup for other owner kinds.

**Invariant test-only references (IR-529).** The invariant adapter also walks
the target type and non-expression paths in predicate HIR, then resolves
deferred expression paths through the exact module scope. It follows import
origins to diagnose test-only bindings independently of `Res`, changes only
rejected path heads to `Res::Err`, and returns separate module-resolution,
path-resolution, and diagnostic channels. Test-only diagnostics are source
ordered and preserve per-owner failures; path results are keyed by HIR path in
a map. The new channels are optional on the shared
predicate result because the refined/distinct and extern predicate adapters
adopted these channels in IR-531 and IR-532.

**Top-level refinement predicates (IR-515).** The bridge now also validates
`Body::value` for top-level `type R = B where p` and `distinct D = B where p`
owners. These values are scanned as predicates rather than executable bodies,
so prohibited jumps receive `INVALID_CONTRACT_EXPRESSION` without label or
outside-function cascades. Distinct declarations without a refinement clause
are not indexed. IR-521 also resolves block-local names in each predicate from
the same lowering result, while leaving implicit `self` for owner-aware
resolution. Generic, interface, and prelude fallback, ordinary type
resolution, nested interface type declarations, and other owner kinds remain
deferred. IR-531 adds exact-module and test-only path checks for top-level
refined and refinement-bearing distinct owners only.

**Refinement test-only references (IR-531).** The refined/distinct predicate
adapter resolves expression paths after local names and checks the base type,
predicate HIR types, generic bounds, const-parameter types, and recovered
generic `where` clauses against the exact module scope. Distinct generic names
shadow module bindings. Invalid refined declarations have no generic fields
in HIR, so their generic names come from the current filtered ItemTree and
their generic-syntax `PathType` and `PathExpr` heads, plus nested types and
effects inside function-sugar bounds, are diagnosed from the current AST.
Function-sugar names such as `Fn` remain bound to their core lang items. The
AST scan emits diagnostics
without fabricating HIR path results. HIR-backed paths retain separate
expression and non-expression results; test-only diagnostics are merged in
source order and use import origins with complete `PKG_PATH` notes. This pass
does not provide general type, effect, interface, Prelude, or final
unresolved-name resolution.

**Extern-function test-only references (IR-532).** The extern predicate
adapter checks signature parameter and return types, effect references,
generic bounds, const-parameter types, and generic where predicates against
the exact module scope. Generic parameters shadow same-named imports in
type-position paths; built-in effect roots retain their established
precedence.
Contracts on bodyless extern declarations still form a HIR body and receive
local resolution, module fallback, and test-only expression diagnostics;
ordinary bodyless signatures have no local or module result but retain
signature diagnostics. Expression paths resolve after contract locals.
Test-only references are classified through the originating import even when
the visible binding carries `Res::Def`, and rejected paths become `Res::Err`
with the source `PKG_PATH` note. Signature and contract diagnostics are merged
in source order. This remains an incremental extern adapter, not complete
extern or general type/effect resolution.

**Trait and impl header test-only references (IR-534).** The top-level header
adapter checks trait generic bounds, const-parameter types, `where` clauses,
and supertraits, plus impl generics, `where` clauses, self types, and trait or
interface targets. It selects the exact module scope for each current,
target-filtered owner. Generic parameters shadow same-named imports in
type-position paths. Function-trait sugar names are not looked up as imports,
while nested types and effects remain candidates; built-in effect roots retain
their precedence. The pass follows each binding's import origin even when its
`Res` is `Def`, reports at exact source spans with the complete `PKG_PATH`
note, stores rejected header heads as `Res::Err`, and isolates failures per
owner. Associated members and ordinary type/module resolution remain outside
this header-only increment.

**Associated trait/impl signature test-only references (IR-535).** For each
current, target-filtered direct trait or impl, the adapter matches associated
type and method entries from its filtered ItemTree with the child `OwnerId`s
in the lowered parent. Each child has its own path results and failure
channel. Trait associated-type bounds, impl associated-type definition types,
and method parameter/return types, effects, generic bounds, const-parameter
types, and `where` clauses are checked against the module scope for the
parent's exact file. Parent and method generic names both shadow type-position
imports; built-in effect roots retain their precedence. Test-only status
follows the originating import even when the visible binding has `Res::Def`;
diagnostics include the source span and complete `PKG_PATH` note, and rejected
path heads become `Res::Err`. Spec/15 §15.11.3 makes `@target` on an associated
member `INVALID_ATTRIBUTE_TARGET`; the filtered parent ItemTree supplies the
associated declarations present in this build.

**Associated method body and contract test-only references (IR-536).** The
adapter reuses each method's canonical `OwnerId`, lowered HIR, source map, and
the exact module scope selected for its parent file. The full owner walk checks
nested body and contract type, effect, and pattern paths with both parent and
method generic names and built-in effect precedence. Expression paths resolve
through lexical locals first, then generic parameters, then module bindings;
only paths claimed by module fallback are classified by `BindingOrigin` and
changed to `Res::Err` when their import is test-only. Qualified module-alias
heads stay on the module diagnostic path because they are not one-segment
local-value lookups. Methods with contracts but no written body use the HIR
body produced for those contracts; bodyless methods and associated types have
no local or module result. Test-only diagnostics retain exact source spans and
the complete `PKG_PATH` note, with resolution and diagnostic failures isolated
per member. This remains a diagnostic pass and does not add ordinary
type/effect/module-member lookup or command-level check integration.

**Interface declaration test-only references (IR-537).** For each
current-target interface, the adapter pairs its filtered ItemTree children
with canonical child `OwnerId`s from the lowered interface. It checks direct
struct fields, aliases, refined bases, enum payloads, interface function
signatures, and resource method signatures against the exact containing
file's module scope. Generic parameters shadow same-named type paths, and
built-in effect roots retain precedence in nested function-pointer types.
Resource methods are lowered through their resource owner and keep a separate
result channel. Interface functions and resource methods are signature-only
HIR owners, so they have no retained local or module body-path results. For
recovered interface function/resource syntax and invalid interface refinement
predicates, the adapter transiently lowers the body for lexical-local
resolution, then scans those syntax roots for test-only diagnostic paths.
Parameter and block-local bindings therefore shadow module aliases, including
the first segment of a qualified expression path. These recovery-only paths
never enter the retained HIR resolution results. The standalone refined-owner
syntax scan uses the owner's body-local resolution to apply the same shadowing
rule.

Interface refinements are rejected by spec/09 §9.2.1 and spec/13 §13.2.2, but
the current HIR keeps only the base type and refinement span. The adapter
scans a retained invalid refinement predicate from its current AST and emits
test-only diagnostics without manufacturing HIR path results. AST-only
predicate and recovered function scans include constructor and range-pattern
heads, and nested refined declarations use their ItemTree generic parameters
for shadowing. Parent, child, and resource-method failures stay isolated. This
remains a diagnostic pass; full ordinary type/effect/member lookup, full world/WIT
resolution (including package-qualified references), and command-level `check`
integration remain pending.

**Top-level type test-only references (IR-538).** The driver diagnoses
references to test-only imports in direct top-level structs, aliases, enums,
and resources. It checks declaration generic bounds and `where` clauses,
alias targets, struct/resource fields, and tuple/record enum payloads, keeping
field and variant paths under the enclosing type owner. It uses the exact
source-file scope, applies generic type-name shadowing, checks import origin
independently of `Res`, emits exact first-segment spans with the original
`PKG_PATH`, and stores rejected heads as `Res::Err` in a diagnostic-only
result channel. Target filtering and per-owner failure isolation remain in
force. Top-level resource field types are included at this stage because
resources already lower as ordinary top-level type owners; resources add no
generic parameter scope. IR-539 covers effect `covers` references, and IR-540
covers test-only module-binding heads in direct world items and boundary
signatures. IR-541 adds first-head resolution for these top-level type owners,
and IR-542 walks their qualified module/interface prefixes with validated
source scopes. Full ordinary type/member and Prelude resolution,
package-qualified world/WIT resolution, final diagnostics, and command-level
`check` integration remain pending.
The grammar exposes no further extern-child form beyond function heads, which
IR-532 covers.

**Effect `covers` test-only references (IR-539).** The driver diagnoses
test-only imports used as the first segment of a top-level effect's `covers`
paths, using that effect's exact file scope and the originating import's
`test_only` flag. Built-in effect roots keep precedence over module aliases.
Well-formed HIR paths retain `Res::Err` results with one resolved head and the
full segment count. When a malformed covers set is lowered as empty, a
recovery-only AST scan diagnoses valid references without creating HIR path
results. IR-540 covers test-only module-binding heads in local world items and
world-function boundary signatures. Ordinary effect lookup, complete
covers-graph validation, package-qualified world/WIT resolution, and
command-level `check` integration remain pending.

**World item test-only references (IR-540).** The driver checks first-segment
module-scope bindings used by direct world `import`, `export`, and `include`
items, plus nested parameter/result types and effects in world-function
signatures. It uses each child's exact file scope, classifies by import origin
and `test_only` rather than `Res`, emits a diagnostic at the reference with a
note at the originating `PKG_PATH`, and stores retained HIR heads as
`Res::Err`. Target-filtered imports are already absent from the scope.
Parser-recovered world-function generics, `where`, borrow/effect/contracts,
and bodies receive diagnostic-only AST coverage; generic and const-parameter
shadowing is retained, and a failed recovered-body walk does not suppress
signature diagnostics. Direct package-qualified `WorldRef` values remain with
world/package resolution: spec/14 §14.2.5 defines the restriction around
bindings introduced by `use` and the note at that use, not direct package
paths.

**Review required:** Confirm whether direct `WorldRef::Pkg` references whose
identity appears in `requires test` should be rejected outside tests, and if
so, which source span and related note the diagnostic should use. They do not
have a test-only `use` binding to point back to. Until spec/14 defines this
case, IR-540 leaves it outside the binding-based test-only pass. Ordinary
world/WIT name resolution and package availability validation remain pending.

**Top-level type-path resolution (IR-541, extended through IR-572).**
`resolve_type_owner_paths` returns type-path results and the existing
test-only diagnostics from one lowering pass over current-target top-level
structs, aliases, enums, distinct types, and resources. Distinct declarations
without a refinement clause use this pass; their base and generic-bound type
paths share the same lowering and lookup behavior. Refinement-bearing distinct
types remain in the predicate-owner adapter, which also resolves their type
paths.
Plain type-path heads resolve by generic parameter, then exact-file module
scope; generic type and const parameters retain their `DefKind` and `DefId`,
and the first source declaration wins duplicate-name recovery. Ordinary
bindings preserve their known `Res`, including known wrong-kind entities,
while test-only imports are diagnosed by import origin regardless of `Res`
and receive `Res::Err`. With a validated active-source closure, the additive
`resolve_type_owner_paths_with_member_scopes` entry point also walks available
module/interface prefixes across the requester and transitive source packages
and records closure provenance. Typed missing/private member failures remain
available to the matching diagnostic mapper. Misses without a resolved
member prefix, including Prelude-provided names, remain source-ordered
deferred candidates; this adapter does not emit final unresolved-name
diagnostics. `resolve_type_owner_test_only_paths` remains a compatibility
wrapper over the combined pass.

**Extern-function contracts (IR-516; implemented; hostile reviews complete).** The
adapter reuses Phase 1's extern-function descriptors and their canonical
block/child `OwnerId`s, rebinds both AST IDs from current target-filtered
inputs, lowers the extern block and function item in a temporary arena, and applies the
predicate-only restriction scan to retained `requires` and `ensures`
expressions. It includes bodyless extern declarations with contracts and
returns per-owner results. Per spec/09 §9.3.1, extern `requires` are checked at
call sites while `ensures` are trusted facts and are never checked at runtime;
this bridge handles syntax restrictions only and does not implement those
semantics or complete name/type resolution. IR-521 additionally resolves
parameter and block-local names in each retained contract from the function's
same lowered `Body` and `HirSourceMap`; module, generic, prelude, and type
fallback remain deferred. Test results and review findings are recorded in
[`design/implementation-review.md`](../design/implementation-review.md).
Hostile review found no remaining issues after the target-filtered block
disambiguator correction. Other owner kinds and tracked `hir_owner`/full `resolve_owner`
remain deferred.

**Predicate-owner local resolution (IR-521).** The invariant, top-level
refinement/distinct, and extern-function adapters retain the source map from
their existing lowering and run `resolve_owner_locals` against that exact map
and body. The new `PredicateOwnerResolution::local_resolution` result is
independent from predicate diagnostics, so a local-resolution error does not
hide control-flow or restriction diagnostics. It resolves only lexical locals
and reports `SHADOWS_MODULE` using aliases from the owner's exact file scope.
Implicit `self` remains owner-scoped and is not treated as a lexical local.
Const initializers and tests/properties are outside this predicate adapter.
IR-523 and IR-525 add their local/module path passes under the required
test-analysis or constant-owner context. At the time of IR-521,
constant-expression legality and test-only diagnostics remained deferred;
later entries document test-only path coverage added by IR-526 through IR-540.
Constant-expression legality remains pending.

**Function module-scope fallback (IR-522).** The top-level function adapter
passes local misses through generic-parameter lookup and then resolves
one-segment `PathRoot::Plain` expression paths in the `Phase1ModuleScopes`
belonging to the descriptor's exact file. Local results remain in their own
channel; names matching a function generic and module misses remain ordered
deferred candidates. Module bindings with `Res::Def`, `Res::Err`, or
`Res::TestOnly` become one-segment `PathRes` entries. The test-only marker is
retained for a later build-context-aware dependency diagnostic. A module alias
used as the head of a qualified expression is resolved while its member tail
remains for a later pass. This slice does not resolve prelude names, qualified
members, or non-function owner kinds, and does not emit final unresolved-name
diagnostics.

**Test and property owner resolution (IR-523–524).** The driver retains the
`Phase1BuildContext` on each package-scope collection and enables this adapter
only when `analyzes_tests` is true, independently of `is_root_package`.
Target-filtered test and property descriptors are lowered from current file
inputs, then `resolve_owner_locals` runs against each owner's paired `Body`
and `HirSourceMap`, using module aliases from that exact file scope. Results
retain collection provenance and isolate lowering/local-resolution failures
per owner. Test-only module aliases with no item list remain syntax-known for
`SHADOWS_MODULE` even when an inactive test-only import has `Res::Err`.
After local lookup, one-segment plain expression paths resolve through the
same file's module scope. Test-only and diagnosed module bindings retain their
`Res` values, while misses remain deferred. Test and property owners have no
generic parameters, so this bridge supplies an empty generic-name set. Prelude
lookup, qualified-member tails, type-position paths, test-only diagnostics for
other non-test owner kinds, tracked `hir_owner`/`resolve_owner` queries, and
type checking remain deferred.

**Constant-owner path analysis (IR-525).** For every target-filtered top-level
const descriptor, the driver lowers the current initializer and resolves its
plain one-segment expression paths through lexical locals and then the
`Phase1ModuleScopes` selected by the exact source file. Constants have no
generic parameters or body-local bindings in the accepted const-expression
grammar, so this bridge passes an empty generic set and leaves misses deferred.
IR-526 checks deferred initializer paths against the effective binding origin;
test-only references produce a diagnostic and become `Res::Err`, regardless
of the import's current `Res`. Other `Res::Err` and `Res::TestOnly` values are
preserved. This partial pass does not validate §6.27 constant-expression
legality, resolve type-position paths or qualified tails, perform prelude
lookup, or emit final unresolved-name diagnostics.

**Const test-only reference diagnostics (IR-526).** After local and module
lookup for a const initializer, the driver examines each deferred one-segment
expression path in source order. It follows the exact module binding's
`BindingOrigin` to its `ImportBinding` and checks `test_only`, independent of
the binding's `Res`. Local shadowing has already removed that path from the
deferred set. Each test-only reference gets `TEST_DEPENDENCY_OUTSIDE_TEST` at
the path and a note at the originating use's complete `PKG_PATH`; the path
result becomes `Res::Err` so later phases do not cascade. IR-526 covered const
initializer expressions only. IR-528 below extends the same diagnostic rule to
const declaration-type heads; IR-529, IR-531, and IR-532 add invariant,
refined/distinct, and extern owners, while IR-534 covers direct trait/impl
headers, IR-535–IR-536 cover associated declaration signatures, method bodies,
and contracts, IR-537 covers interfaces, and IR-538 covers top-level
struct/alias/enum/resource declarations. IR-539 and IR-540 add test-only
reference diagnostics for effects and world owners. IR-541 adds partial
first-head type-path resolution for top-level type declarations, and IR-542
adds qualified module/interface prefixes for those paths using a validated
active-source scope closure. Full ordinary type/effect/member lookup, canonical
Prelude fallback, package-qualified world/WIT resolution, and final
unresolved-name diagnostics remain for later integration.

**Const test-only type diagnostics (IR-528).** The const owner adapter walks
the declared type before its body-dependent local-resolution branch and also
walks the initializer HIR for non-expression paths. It checks direct and
nested generic type paths through the exact file's module scope and stores
rejected heads in a separate `OwnerTestOnlyPathResolution`, keeping this
diagnostic-only pass distinct from expression `ModuleResolution`. The
initializer walk covers local type annotations, casts, closure signatures,
and constructor/range pattern paths where present. Qualified types diagnose
the first module alias segment and leave the tail for later member resolution.
The declared-type walk runs even if the owner has no body. Test-only
declaration-type, initializer-body, and initializer-expression diagnostics
are merged in source order, use the binding's import origin regardless of
`Res`, include the complete original `PKG_PATH`, and mark only the rejected
path as `Res::Err`. Complete ordinary type resolution, canonical Prelude
fallback, qualified-tail lookup, and final unresolved-name diagnostics remain
later work; IR-548 supplies only the partial type-head and primitive fallback
slice for const owners.

### 3.4.6 Visibility

`visible(e, M)` (spec/04 §4.6.1) is evaluated for every path segment that names an item or a member, at the point where the segment is resolved: by the resolver for module members and `use` items, and by the type checker for type-relative members (variants never fail, since they have the enum's visibility; inherent associated functions and methods, spec/05 §5.13.5) and for fields (`PRIVATE_FIELD`, spec/04 §4.6.3, §4.6.4). The visibility of an entity is read from its `ItemTree` entry or from the dependency's metadata, never from HIR, so the check does not depend on the target's body.

**Current module-member lookup slice (IR-498).** `sai_resolve::module_member_lookup::ModuleMemberIndex`
indexes supplied package scopes by canonical module or interface `DefId`, then
looks up qualified members through the `members` maps while preserving each
binding's original `Res`. Same-package lookups may see private members;
cross-package lookups require effective `Visibility::Pub` or
`Visibility::Export`. The caller must supply every active source package that
can own a qualified target, including transitive packages reached through
re-exports. This index does not yet parse or resolve path text, apply
owner-level test-only rules, or emit path diagnostics; those remain part of the
tracked owner resolver.

Distinct types (spec/04 §4.6.4): the constructor `D(b)`, the pattern `D(b)` and `.value` are visible outside the defining package only if the declaration is `pub distinct D = pub B`. The check is done by the type checker where it resolves the constructor call, the pattern and the field (§3.13.3). `PUB_REPR_ON_PRIVATE_DISTINCT` is reported by `wf_check_item` on the `pub` marker of the base.

### 3.4.7 Deprecation

The lint `DEPRECATED_REFERENCE` ([spec/15](../spec/15-attributes-and-lints.md) §15.6.4) is reported by the query that resolves the reference: the resolver reports it for path segments, including those of `use` items, and the type checker for type-relative segments, method calls, field names and implicit variants. Both read the deprecated declarations and deprecated contexts of [spec/15](../spec/15-attributes-and-lints.md) §15.6.2 from `ValidatedAttrs`, skip the references that §15.6.4 excludes, among them every path that a desugaring of [02](02-frontend-ast-hir.md) creates (`PathRoot::Lang`), and take the message, the help and the fix from the attribute as §15.6.4 defines.

### 3.4.8 Unused imports

`unused_imports(M)` implements spec/04 §4.3.6:

1. Every `PathRes` whose first segment was found in `S(M)` through an import binding records that binding's `ImportIndex` in `ResolvedOwner::used_imports`.
2. Every method call, path call `Tr.m(..)` or associated-item path whose trait was found in scope through an import binding records it in `TypeckResults::used_trait_imports` (§3.9.2).
3. The query unions both sets over every owner of `M` that survived `@target`, including tests and properties, and reports `UNUSED_IMPORT` (spec/15) for every non-`pub` import binding that is not in the union, on its name. A module alias and each listed item of the same declaration are checked separately. Bindings that already failed (`Res::Err`) and `pub use` bindings are never reported.

The query depends on `typeck` of every owner of the module. It is run by the driver after type checking, so it never delays the diagnostics of a body.

### 3.4.9 Private names in public APIs

`wf_check_item(owner)` implements spec/04 §4.6.7. For a public item (`pub` or `export`), it walks the HIR positions of its public surface listed in the table of §4.6.7 and, for every path that resolves (per `resolve_owner`) to a type, trait or constant of the current package that is not `pub`, reports `PRIVATE_TYPE_IN_PUBLIC_API` on the path, naming the entity. Type parameters, `Self`, prelude names and entities of other packages are skipped. Each offending entity is reported once per item, at its first occurrence in source order. Effects are checked by [impl/08](08-effect-and-capability-analysis.md) (`PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE`).

### 3.4.10 Test-only bindings

`resolve_owner(owner)` implements [spec/14](../spec/14-testing.md) §14.2.5 item 2 for the bindings that §3.3.3 marks `test_only` (N-E-65):

1. **Permitted owners.** A test or property owner may refer to them anywhere: its body with the closures and `async` blocks in it, and the parameter types of a property. These owners are resolved only when test analysis is active for the package (spec/14 §14.2.4), so a reference there is resolved through the binding's ordinary `Res`.
2. **Other owners.** In every other owner, in every build, a path whose first segment is found in `S(M)` through a `test_only` binding, by the lookup of §3.4.3 item 1 or of §3.4.2 item 1, is `TEST_DEPENDENCY_OUTSIDE_TEST`, at the path, with a note at the `PKG_PATH` of the use that introduced the binding. The path resolves to `Res::Err`, so the type checker reports nothing more for it. The check needs only the binding's origin, never its resolution, so the result is the same with and without test analysis (spec/14 §14.2.5 item 4). Each reference is reported. Local bindings and generic parameters shadow a test-only binding by the ordinary order of §3.4.3, and then no reference to it exists.
3. **Traits in scope.** A trait bound through a `test_only` binding is in scope only for test and property owners: `traits_in_scope(M)` (§3.9.2) is computed in two versions, the module version without these bindings and the test version with them, and only test and property owners read the test version. A method call in another owner therefore never selects such a trait, and its result is the same with and without test analysis (spec/14 §14.2.5 item 2, N-I-52).
4. **`Res::TestOnly`.** In a build without test analysis, only non-test owners are resolved, and item 2 rejects every path that reaches a `test_only` binding, so `Res::TestOnly` never reaches the type checker. With test analysis active and `P` the root, the bindings carry their real resolution. A test owner that refers to a binding of an unavailable use (§3.3.3 item 3) gets `Res::Err` and no further diagnostic (spec/14 §14.2.5 item 5).

A `use` never refers to a binding (spec/04 has no relative paths), so the only "reference from another `use`" of spec/14 §14.2.5 item 2 is a `pub use` of the test dependency itself, which §3.3.3 item 1 reports at its `PKG_PATH`.

## 3.5 Type representation

### 3.5.1 Interned types

```rust
pub type Ty = Interned<TyKind>;                 // pointer-equal iff structurally equal
pub enum TyKind {
    Int(IntTy),                                 // I8 I16 I32 I64 ISize
    Uint(UintTy),                               // U8 U16 U32 U64 USize
    Float(FloatTy),                             // F32 F64
    Bool, Byte, Char, Never,
    Tuple(List<Ty>),                            // `()` is Tuple([])
    Array(Ty, Const),
    Ref(Region, Ty, Mutability),                // borrow ρ τ / borrow mut ρ τ
    Adt(DefId, GenericArgs),                    // every nominal type, including Str, Slice, SliceMut, String, Vec, Box, Option
    FnPtr(FnPtrSig),                            // fnptr(safety, inputs → output, effects)
    FnDef(DefId, GenericArgs),                  // function item type
    Closure(DefId, GenericArgs),                // closure type: parent generics + ClosureParts (§3.11.5)
    Future(DefId, GenericArgs),                 // anonymous future of an async fn or async block (Stage 5)
    Param(ParamTy),                             // { index: u32, name: Symbol }; `Self` of a trait is index 0
    Projection(ProjectionTy),                   // <τ as Tr<σ̄>>.A: { trait_ref: TraitRef, item: DefId }
    Infer(InferTy),                             // TyVar(TyVid) | IntVar(IntVid) | FloatVar(FloatVid)
    Error(ErrorGuaranteed),
}
pub enum GenericArg { Type(Ty), Const(Const), Region(Region) }
pub type GenericArgs = Interned<[GenericArg]>;
pub enum ConstKind { Value(ScalarValue), Param(ParamConst), Infer(ConstVid), Error(ErrorGuaranteed) }
pub struct FnPtrSig { pub safety: Safety, pub inputs_and_output: List<Ty>, pub effects: EffectSetRef }
pub struct TraitRef { pub def_id: DefId, pub args: GenericArgs }   // args[0] is Self
```

- `Int` and `UInt` resolve to `I64` and `U64`, and aliases are expanded during conversion (§3.6.1), so neither appears in `TyKind` (spec/05 §5.1.2).
- Distinct types, refined types and resources are `Adt`. `AdtDef::kind` (§3.6.4) tells them apart.
- `ErrorGuaranteed` is a zero-sized token that can only be created by emitting an error diagnostic (§3.16).
- `EffectSetRef` is the `EffectSetId` of [impl/08](08-effect-and-capability-analysis.md) §8.1.6, the interned effect set of spec/08. This chapter does not resolve effect paths: when it converts a fn pointer type (§3.6.1), it takes the set of `resolved_effect_set` of impl/08 §8.1.7 for the type's clause, and impl/08 reports the diagnostics of that resolution (`UNKNOWN_EFFECT`, `EFFECT_NOT_VISIBLE`, `DUPLICATE_EFFECT`) and interprets the set. Before Stage 3 the set is always empty.
- Interning uses a sharded hash set keyed on the structural value. The intern arena lives for the whole session; interned values are never freed during a session.

### 3.5.2 Regions (N-E-24)

Regions are never written in source and never affect type checking (spec/05 §5.14, spec/07). This chapter therefore carries them only as placeholders that [impl/05](05-borrow-checker.md) interprets:

```rust
pub enum Region {
    Static,                 // 'static: string and byte-string literal data (spec/06 §6.3.3)
    Sig(SigRegion),         // a region position of a signature or a type declaration, numbered per item
    Erased,                 // any region inside a body
}
pub struct SigRegion { pub item: DefId, pub index: u32 }
```

1. In `fn_sig`, `type_of` and impl headers, every region position gets its own `Sig` index in pre-order: parameters left to right (the receiver first), then the return type. A region position is a `borrow`/`borrow mut`, a view (`Str`, `Slice`, `SliceMut`), or a nominal type with an implicit region parameter (spec/07 §7.8.2).
2. In field types of a declaration with an implicit region parameter, every region position is `Sig { item: the declaration, index: 0 }`, because such a type has exactly one region parameter (spec/07 §7.8.2 rule 1). The type's `GenericArgs` start with that region.
3. Inside bodies, every region is `Erased`. Unification ignores regions entirely (spec/05 §5.14), so no region relation is ever recorded by this chapter.
4. Elision, the `borrows` clause and the relations between `Sig` regions are owned by spec/07 §7.7 and computed by [impl/05](05-borrow-checker.md), which replaces `Erased` regions of MIR local types by region variables.

`has_region_param(DefId) -> bool` is computed for every record, enum, distinct and refined type of a package by the least fixed point of spec/07 §7.8.2 in the query `region_params(PackageId) -> Arc<BitSet<DefIndex>>`. `Str`, `Slice` and `SliceMut` have one region parameter by definition (spec/07 §7.8.1).

### 3.5.3 Inference variables

`InferTy::TyVar` is the general variable `α`, `IntVar` the integer-literal variable `ι`, `FloatVar` the float-literal variable `φ`, and `ConstVid` the const variable `κ` (spec/05 §5.16.2). Variables are created only inside an `InferCtxt` (§3.7.2) and never escape it: writeback (§3.15.3) replaces every variable by its resolved type or reports it. Variables are numbered in creation order, which is the order of the type checker's traversal, so every diagnostic that names "the leftmost" variable is deterministic.

### 3.5.4 Generic arguments and substitution

A generic item's parameters are numbered from 0, parent first: for a method, the impl's or trait's parameters (with the trait's `Self` at index 0), then the method's own. Const and type parameters share the index space in declaration order. The implicit region parameter of a nominal type (§3.5.2), when present, is argument 0 and shifts the others. `subst(ty, args)` replaces `Param(i)` and `ConstKind::Param(i)` by `args[i]`; it is applied only to types of the item that owns the parameters. `instantiate_fresh(def)` returns a `GenericArgs` of fresh variables (`α` for types, `κ` for consts, `Erased` for the region).

### 3.5.5 Projections and normalization

`<τ as Tr<σ̄>>.A` is a projection. `normalize(ty)` replaces every projection whose trait reference can be selected (§3.8.4) by the associated type of the selected impl, substituted. A projection whose self type is a type parameter, `Self` or another rigid projection, and whose bound comes from the environment, stays **rigid** (spec/05 §5.8.5). A projection that contains unresolved variables is replaced by a fresh `α` together with the pending obligation `projection = α` (spec/05 §5.16.3), retried when its variables are resolved.

Normalization runs on every type read from a signature after substitution, so that types in `TypeckResults` never contain a normalizable projection.

### 3.5.6 The error type

`TyKind::Error` is the error type `{error}` named by [impl/02](02-frontend-ast-hir.md) §2.8.6 and [impl/11](11-diagnostics.md) class C2. It is produced only together with a reported error, which the `ErrorGuaranteed` token proves. The rules:

1. `{error}` unifies with every type without binding anything. Unifying a variable with `{error}` binds the variable to `{error}`, so that dependent expressions are also error-typed and do not report.
2. Every obligation whose goal mentions `{error}` succeeds silently.
3. Method lookup, field lookup, indexing, operators and calls on a receiver or operand of type `{error}` produce `{error}` without a diagnostic.
4. A pattern checked against `{error}` binds its bindings to `{error}`. `check_match` skips a `match` whose scrutinee type contains `{error}` (§3.12.2).
5. A body whose `TypeckResults` contain `{error}`, or for which any error was reported, is marked `tainted_by_errors`. Tainted bodies are not lowered to MIR ([impl/04](04-mir.md) §4.4.1), so no borrow, effect or contract diagnostic is derived from a body whose types are wrong; the unsafe check, which reads HIR, follows §3.16 item 6.

### 3.5.7 Printing types

Types in diagnostics are printed in source syntax with the shortest unambiguous name: the name under which the type is visible in the module of the diagnostic if exactly one binding of `S(M)` or the prelude denotes it, otherwise its canonical path (spec/04 §4.9.1). Regions are never printed. Variables print as `_`, an integer-literal variable as `{integer}`, a float-literal variable as `{float}`, the error type as `{error}`, a closure type as `closure at <file>:<line>:<col>`, a function item as `fn <path>`, and an anonymous future as `future of <path>` or `future of async block at <file>:<line>:<col>`. The `data` of a diagnostic carries the canonical form ([impl/11](11-diagnostics.md)).

## 3.6 Declarations and signatures

### 3.6.1 Converting HIR types

`lower_ty(cx, ty: &hir::Ty) -> Ty` turns a resolved HIR type into a semantic type (spec/05 §5.2.1). `cx` is either a **signature context** (item signatures, field types, impl headers, bounds, alias right-hand sides; `_` was already rejected by [impl/02](02-frontend-ast-hir.md) §2.5.12 with `INFER_IN_SIGNATURE`) or a **body context** (annotations and explicit generic arguments inside a body; `_` becomes a fresh `α`, or `κ` in a const argument position).

| HIR type | Result |
|---|---|
| path resolving to `Res::PrimTy(p)` | the primitive; `Int` → `I64`, `UInt` → `U64`; `Str` → `Adt(Str)` with region (§3.5.2) |
| path resolving to a nominal type | `Adt(def, args)` after the checks of §3.6.2 |
| path resolving to an alias | the expansion, substituted (§3.6.1.1) |
| path resolving to a type parameter | `Param` |
| `Self` | in an impl, the impl's self type (`impl_header(impl).self_ty`); in a trait, `Param(0)`; elsewhere `SELF_OUTSIDE_IMPL` and `{error}` |
| `Self.A`, `T.A`, `π.A` | `Projection` resolved by spec/05 §5.8.5: the unique trait among the elaborated bounds of the subject in `param_env` that declares `A` (`UNKNOWN_ASSOC_TYPE`, `AMBIGUOUS_ASSOCIATED_TYPE`); a concrete subject is `PROJECTION_ON_CONCRETE_TYPE`; then normalized (§3.5.5) |
| `borrow T`, `borrow mut T` | `Ref(r, T, m)`; `r` per §3.5.2 |
| `owned T` | `T` |
| tuple, unit, grouping | `Tuple`, `Tuple([])`, the inner type |
| `Array<T, c>` | `Array(T, c)` with `c` converted by §3.6.1.2 against `USize` |
| `fn(T̄) -> R`, `unsafe fn …`, `… effects { E }` | `FnPtr`; omitted return is `()`; the effect set is `resolved_effect_set` of [impl/08](08-effect-and-capability-analysis.md) §8.1.7 at Stage 3, which reports `UNKNOWN_EFFECT` and the other resolution errors, and is empty before |
| a path resolving to a function, module, trait, constant, effect or world | `WRONG_NAME_KIND` (reported by the resolver for type positions, spec/04 §4.5.2) and `{error}` |

#### 3.6.1.1 Aliases

The expansion of `type A<Ḡ> = T` is `type_of(A)`, computed by lowering `T` in a signature context. The query keeps an explicit expansion stack per package (N-E-25): when the expansion of `A` reaches `A` again, through any position including generic arguments, `ALIAS_CYCLE` is reported once per cycle, on the alias of the cycle that comes first in source order, with related locations on the others. Every alias of the cycle expands to `{error}`. A use `A<ā>` checks the argument count (all arguments are required in type position), substitutes, and registers the alias's bounds as obligations at the use (spec/05 N-A-26). In signatures these obligations are checked by `wf_check_item` (§3.6.7); in bodies they join the body's fulfillment context. The alias name is recorded in `TypeckResults::alias_uses` so that diagnostics can mention it (spec/05 §5.6.5).

`UNUSED_ALIAS_PARAM` is reported by `wf_check_item(alias)` for every parameter of `Ḡ` that does not occur in the lowered right-hand side.

#### 3.6.1.2 Generic and const arguments

For a reference with parameters `P1 … Pn` (implicit region parameter excluded) and written arguments `A1 … Am`, the checks of spec/05 §5.2.2 are applied in this order: count (`WRONG_NUMBER_OF_GENERIC_ARGS`, with m = 0 accepted only in expression position), kind (`GENERIC_ARG_KIND_MISMATCH`), const form (`CONST_ARG_NOT_SIMPLE`, `LITERAL_OUT_OF_RANGE`, `TYPE_MISMATCH` for a literal of the wrong kind), and associated-type bindings (`ASSOC_TYPE_BINDING_OUTSIDE_BOUND`, `UNKNOWN_ASSOC_TYPE`, `DUPLICATE_BINDING`). The first failing check of an argument list is reported and the whole reference becomes `{error}`, so that one wrong list gives one diagnostic. A const argument that is a const parameter has the parameter's type, which must equal `κ` (`TYPE_MISMATCH`).

### 3.6.2 Generics and predicates

`generics_of(def) -> Arc<Generics>` lists the parameters of `def` and its parent (§3.5.4), with names, kinds, the const parameter types and, for nominal types, whether the implicit region parameter is present. The parent of a method is its impl or trait; the parent of a closure or an async block is its enclosing body owner; nothing else has a parent. `INVALID_CONST_PARAM_TYPE` is reported here for a const parameter whose type is not an integer type, `Bool` or `Char`. `GENERIC_REFINED_TYPE` is reported here for a refined type or a distinct type with a refinement clause that declares any generic parameter (Stage 3).

`predicates_of(def) -> Arc<Predicates>` lowers the bounds on the parameter list and the `where` clause into predicates in source order:

```rust
pub enum Predicate {
    Trait(TraitPredicate),                 // τ : Tr<σ̄>   { trait_ref, span, origin, caps }
    Projection(ProjectionPredicate),       // <τ as Tr<σ̄>>.A = υ   (from `Tr<A = υ>`)
    FnSugar(TraitPredicate, Ty),           // τ : Fn(Ā) -> S: the trait predicate plus `FnOnce.Output = S`
}
```

- A bound path that does not resolve to a trait is `NOT_A_TRAIT` and is dropped.
- A `where` subject without any generic parameter of the item is `WHERE_WITHOUT_PARAM`. On a trait, a subject other than `Self` or `Self.A` is `INVALID_TRAIT_WHERE_SUBJECT`.
- `Fn(Ā) -> S` lowers to `τ : Fn<(Ā)>` plus the projection `<τ as FnOnce<(Ā)>>.Output = S`. The function traits have no nameable parameters, methods or associated types (spec/16 §16.6.3), so their internal shape is fixed here (N-E-28): each takes one type parameter, the tuple of argument types, and `FnOnce` has one internal associated type `Output`. An omitted `-> S` means `-> ()`.
- **Caps.** Any bound may carry an effect cap (`BoundEffects`, [spec/08](../spec/08-effects-and-capabilities.md) §8.8.3): a trait bound as well as an `Fn`-family bound. `TraitPredicate.caps` is the list of the HIR `EffectSet` nodes of the caps that apply to the predicate: its own written cap, if any, and, after elaboration (§3.14.1), the caps of the predicates it was elaborated from (spec/08 N-D-25). The `FnSugar` trait predicate carries the cap of the sugar. This chapter neither resolves nor compares caps: [impl/08](08-effect-and-capability-analysis.md) resolves them (§8.1.7 there) and checks them (§8.3.5 and §8.5.4 there, and the impl-method rule of §8.1.7 there).

Predicates are stored unelaborated. Elaboration (spec/05 §5.7.4) is done once per environment in `param_env` (§3.14.1).

`SUPERTRAIT_CYCLE` is detected by `trait_def` (§3.6.5) with an explicit stack over the supertrait graph of the package (N-E-25), reported on the trait of the cycle that comes first in source order.

### 3.6.3 `type_of` and `fn_sig`

`fn_sig(def) -> Arc<FnSig>` holds the lowered parameter types (the receiver first), the return type (`()` when omitted, `Never` for `-> Never`), safety, `async`, the `extern` ABI, and the HIR ids of the contract clauses. For an `async fn f(..) -> T`, `fn_sig.output` is the anonymous future type `Future(f, identity args)` and `fn_sig.async_output` is `T` (Stage 5, spec/11 §11.2). `type_of(def)` returns:

| Definition | `type_of` |
|---|---|
| function, method, associated function | `FnDef(def, identity args)` |
| record, enum, distinct, refined, resource | `Adt(def, identity args)` |
| enum variant | for a unit variant, the enum type; for a tuple variant, `FnDef(variant, identity args)` whose signature is the payload types to the enum; a record variant has no value type (spec/06 §6.4.1) |
| distinct type used as a value | `FnDef(def, ..)` of signature `fn(B) -> D` (spec/05 §5.6.3) |
| `const` | the annotated type |
| field | the lowered field type |
| alias | the expansion (§3.6.1.1) |
| associated type in an impl | its definition |

Record constructors have no `type_of` as a value (spec/06 `RECORD_CONSTRUCTOR_AS_VALUE`).

### 3.6.4 `adt_def`

`adt_def(def) -> Arc<AdtDef>` describes a nominal type:

```rust
pub struct AdtDef {
    pub def_id: DefId,
    pub kind: AdtKind,                    // Record | Enum | Distinct { refined: bool } | Refined | Resource | Flags (impl/16 §16.9.2)
    pub variants: IndexVec<VariantIdx, VariantDef>,   // records, distinct, refined and resources have exactly one
    pub discr_ty: IntTy,                  // I64 or the `@repr` type (enums only)
    pub has_region_param: bool,           // §3.5.2
    pub flags: AdtFlags,                  // IS_BOX, IS_VEC, IS_STRING, IS_STR, IS_SLICE, IS_SLICE_MUT, IS_OPTION, IS_RESULT, IS_POLL, IS_ARENA, HAS_DROP_IMPL, IS_UNINHABITED_ENUM, IS_HANDLE (impl/16 §16.7.5)
    pub repr: ReprOptions,                // validated `@repr` (spec/15), passed to impl/06
}
pub struct VariantDef { pub def_id: DefId, pub name: Symbol, pub discr: i128, pub shape: VariantShape, pub fields: IndexVec<FieldIdx, FieldDef> }
pub struct FieldDef { pub def_id: DefId, pub name: Symbol, pub vis: Visibility, pub ty: Ty /* in terms of the ADT's parameters */ }
```

- For a distinct type, the only field is `value`, with the base type, and the visibility given by spec/04 §4.6.4. For a refined type, the only field is the base, named `value`, which is not accessible as a field: field reads go to the base (§3.13.2).
- For a tuple variant the fields are named `0`, `1`, ….
- `DUPLICATE_FIELD` and `DUPLICATE_VARIANT` are reported here, on the later occurrence.
- Discriminants are computed by spec/05 N-A-23 in `i128` arithmetic, then checked against `discr_ty`: `DISCRIMINANT_OUT_OF_RANGE` on the variant whose value falls outside, `DUPLICATE_DISCRIMINANT` on the later of two variants with equal values. After an error, every discriminant keeps its computed value so that later diagnostics remain stable.
- `REFINED_BASE_IS_REFINED` and `RESOURCE_WITH_BORROW` (Stage 4) are reported here, on the base or the field type.
- **Interfaces and WIT** (Stage 4). An enum with `@flags` is checked by [16](16-interfaces-and-wit.md) §16.9.1 (`INVALID_FLAGS_ENUM`). A valid one has `AdtKind::Flags` with one variant and one hidden field ([16](16-interfaces-and-wit.md) §16.9.2). The `AdtDef`s of the types of SAI views are built from the WIT model ([16](16-interfaces-and-wit.md) §16.4.3). `IS_HANDLE` marks view resources and the resources of local interfaces.

### 3.6.5 `trait_def`

`trait_def(def) -> Arc<TraitDef>` lists the supertraits (lowered, with `Self := Param(0)`), the associated types with their bounds, the methods and associated functions with their receiver forms and whether they have a default, `unsafe` and sealed markers (spec/16), and the lang-item role. `DUPLICATE_TRAIT_ITEM` is reported here. For the function traits it synthesizes the internal methods of N-E-26 (§3.3.6).

### 3.6.6 Infinite size

`adt_sized_check(PackageId)` implements spec/05 §5.6.7 over all nominal types of the package at once. It builds the graph whose nodes are declarations and whose edges go from `N` to every declaration `M` whose application occurs as a by-value component of `N` (after substitution, skipping the indirections of spec/05 §5.6.7 rule 2). Every declaration in a strongly connected component with a cycle (including a self-loop) is `INFINITE_SIZE_TYPE`, reported on the declaration, with the fix "wrap the recursive field in `Box`" placed on the first field (in declaration order) that has an edge inside the component. The components are computed by Tarjan's algorithm over declarations in source order, so the output order is deterministic. Types of dependencies were checked when the dependency was compiled; an edge into a dependency is never part of a cycle.

### 3.6.7 Well-formedness of declarations

`wf_check_item(def)` checks one item:

1. every type written in the item is WF under the item's `param_env` (spec/05 §5.17): for every nominal type, alias and projection, the substituted predicates of the declaration hold (`UNSATISFIED_BOUND`, with origin "required by the bound on `P` of `N`"), and the referenced entity is visible (`PRIVATE_ITEM`, already reported by the resolver for paths);
2. the item-specific checks: spec/05 §5.6 for type declarations, §5.9.1 and §5.9.2 for impls (§3.8.2), `PRIVATE_TYPE_IN_PUBLIC_API` (§3.4.9), and the attribute checks that need types (spec/15);
3. for a function, every parameter and return type, the contract clause expressions are checked as bodies (§3.7.14), and `INVALID_SELF_TYPE` was already checked syntactically by [impl/02](02-frontend-ast-hir.md).

WF obligations are solved with the trait solver of §3.8.4 in a fresh inference context that contains no variables; an obligation that is ambiguous there is reported as `UNSATISFIED_BOUND`. A body owner's own types (annotations, explicit generic arguments, the instantiated predicates of every path) are checked by `typeck` (§3.7.4).

### 3.6.8 Variances

`variances_of(def) -> Arc<[Variance]>` gives the variance of every region and type parameter of a nominal type, computed by spec/07 §7.6.3 from the field types as a least fixed point over the package's nominal types (N-B-8: a type parameter used in no field is invariant). This chapter computes and stores variances but never uses them: unification ignores regions (§3.5.2). [impl/05](05-borrow-checker.md) consumes them when it relates the regions of two types.

### 3.6.9 Boundary declarations

This section implements the declaration rules of [spec/12](../spec/12-unsafe-and-ffi.md) §12.7.4, §12.8 and §12.9, of [spec/10](../spec/10-errors-and-panics.md) §10.6.1 (N-E-63), and the entry point of [targets](../targets/targets-and-profiles.md) §4.2 (N-E-68). A function **is a boundary scalar function** when every parameter type and the result type of its `fn_sig` satisfy the boundary-scalar table of [impl/06](06-wasm-lowering-and-layout.md) §6.6.7, with aliases expanded, and the result may also be `()`.

**Extern functions.** `wf_check_item(f)` of a function `f` of an `extern` block runs these checks after item 1 of §3.6.7, in this order. Each check reports every violation it finds, and a later check is skipped only where stated:

1. **Form** (spec/12 §12.7.4 item 2, §12.9.3 item 2). Each of the following that `f` has is one `INVALID_EXTERN_SIGNATURE`, at the construct, in source order: the `async` keyword, the generic parameter list, a `self` parameter, the `where` clause, the `borrows` clause. [impl/02](02-frontend-ast-hir.md) does not report `self` in an extern function (§2.5.12).
2. **Link name** (spec/12 §12.9.1 item 3), `unsafe extern "C"` blocks only. The symbol name is the string of `@import_name` if present, otherwise the function name without `r#`. If it does not match `[A-Za-z_][A-Za-z0-9_]*`, the error is `INVALID_LINK_NAME`, at the `@import_name` string, or at the name if there is no attribute. An `extern "wasm"` name has no restriction (spec/12 §12.7.6).
3. **Types.** Skipped if item 1 reported a generic parameter list, because the types are not monomorphic. For an `extern "wasm"` block, each parameter or result type that fails the boundary-scalar table is `EXPORT_FN_NON_SCALAR`, at the type. For an `unsafe extern "C"` block, each one that is not an FFI type of the table of spec/12 §12.9.3 is `INVALID_EXTERN_SIGNATURE`, at the type. A type that contains the error type (§3.5.6) is not reported.
4. **Panic hook** (spec/10 §10.6.1 item 3), when `attrs(f).panic_hook` is set. [impl/02](02-frontend-ast-hir.md) §2.10.2 already dropped the attribute on any target other than a function of an `extern "wasm"` block, with `INVALID_ATTRIBUTE_TARGET`. The check runs only if items 1 and 3 reported nothing for `f`, `f` is a boundary scalar function, and `effect_decl_check(f)` ([impl/08](08-effect-and-capability-analysis.md) §8.1.7) reports no `EXTERN_EFFECTS_MISMATCH`, so a declaration that fails a check of spec/12 §12.7.4 gets no second code. It reports `PANIC_HOOK_SIGNATURE_MISMATCH`, once, at the function name, if `f` does not have exactly two parameters whose types are the primitive `USize` after alias expansion (a distinct type over `USize` is not `USize`), its result type is not `()`, it is declared `unsafe fn`, or it has a `requires` or `ensures` clause. One secondary label is attached per violated condition, at the parameter list, the result type, the `unsafe` keyword or the clause.

**Panic hooks of a package.** `panic_hooks(P)` lists every function of `P` whose `attrs` has `panic_hook`, after `@target` filtering, ordered by the byte order of the source-relative path of its file, then by offset (spec/10 §10.6.1 item 5). This is not `ModuleId` order, which visits a directory before a file of the same stem (§3.2.2). Whether `P` is the root package is read from `package_graph` ([impl/01](01-architecture.md) §1.7.3 item 4):

1. If `P` is not the root package, every listed function is `PANIC_HOOK_OUTSIDE_ROOT`, at its `@panic_hook` attribute, and the value is empty. These declarations are not counted for item 2: the build has one root, and one diagnostic per declaration is enough (P8).
2. If `P` is the root package, the value is the first listed function. Every later one is `DUPLICATE_PANIC_HOOK`, at its `@panic_hook` attribute, with a note at the attribute of the first. A hook that fails a signature check still counts.

The result does not depend on the signature checks, so their order does not matter. [impl/08](08-effect-and-capability-analysis.md) §8.7.2 takes the `PanicHook` root from `panic_hooks(root)`, and [impl/10](10-runtime.md) §10.5.3 the reporting import.

**Core exports.** The checks of spec/12 §12.8 apply only to the `export fn` items that a build exports, those of the root package when the artifact is a core module. `core_export_check(root)` makes them, and the driver asks for it only when the artifact of the build is a core module ([impl/08](08-effect-and-capability-analysis.md) §8.7.1), so no query value depends on the artifact kind. For every function of `package_scopes(root).exports` (§3.3.5), in the order of that table:

1. An `async` function is `ASYNC_EXPORT_UNSUPPORTED` ([spec/11](../spec/11-async-and-concurrency.md) §11.10.4), at the `async` keyword, and items 2 and 3 are skipped for it.
2. **Form.** Each of the generic parameter list, the `where` clause and the `borrows` clause that it has is one `INVALID_EXTERN_SIGNATURE`, at the construct. A `self` parameter outside an impl is already `INVALID_SELF_TYPE` ([impl/02](02-frontend-ast-hir.md) §2.5.12).
3. **Types.** Skipped if item 2 reported a generic parameter list. Each parameter or result type that fails the boundary-scalar table is `EXPORT_FN_NON_SCALAR`, at the type.

`UNSAFE_EXPORT` does not depend on the build and is reported by [impl/08](08-effect-and-capability-analysis.md) §8.10.7 for every `export unsafe fn`.

**Entry point.** `entry_point(root)` implements [targets](../targets/targets-and-profiles.md) §4.2 items 1 to 5 (N-E-68). Its value is `Option<DefId>`, the valid entry point of the root package. The driver asks for it in every build and check of the root package, on every target and in test builds too, because whether a package is a valid program does not depend on the target (targets §4.2 item 5):

1. **Candidate.** The candidate is the function item named `main` in `D(M)` of the root module (§3.3.2), after `@target` filtering (§3.3.1). A function `main` in another module, in a dependency or in an impl is an ordinary function. Without a candidate, the value is `None` and nothing is reported.
2. **`async`.** A candidate declared `async` is `ASYNC_MAIN`, which `wf_check_item` reports at the `async` keyword (§3.7.11). The value is then `None`, and no `INVALID_ENTRY_POINT` is reported.
3. **Unresolved types.** If a parameter type or the result type of `fn_sig(main)` contains the error type (§3.5.6), the value is `None` and nothing more is reported. This covers a root package that does not depend on `sai:process`: `process.Env` is then an unresolved name, which is already reported (targets §4.2 item 4).
4. **Form.** Otherwise, `Env` and `ExitCode` are the designated items `sai:process.Env` and `sai:process.ExitCode`, found by canonical path in the package graph ([spec/16](../spec/16-core-library.md) §16.16.1 item 3, N-J-26). The candidate deviates when it is not `pub`, has a generic parameter list or a `where` clause, is `unsafe` or `export`, does not have exactly one parameter, has a parameter whose mode is not `owned` or whose type is not `Env` after alias expansion, or has a result type other than `ExitCode` after alias expansion. A distinct type over `Env` is not `Env`. Any deviation is one `INVALID_ENTRY_POINT` at the candidate's name, whose help shows the expected signature, and the value is `None`. When the only deviation is the missing `pub`, the diagnostic carries a machine-applicable fix that inserts `pub ` before `fn`.
5. **Valid.** Otherwise the value is `Some(main)`. [impl/08](08-effect-and-capability-analysis.md) §8.7.2 takes the `Main` root from it. A candidate with an error of step 2 or 4 stops the build, which produces no artifact (targets §4.2 item 3).

`entry_point_note(root)` is `ENTRY_POINT_NOT_APPLIED` at the name of `main` when `entry_point(root)` is `Some(main)`, with the help "build for a WASI target to run `main`". It is a session-level query, like `export_world_note` ([impl/16](16-interfaces-and-wit.md) §16.1.3 item 4). The driver asks for it exactly in a command that analyzes the root package for a core module (`sai check`, `sai build` and `sai size` for a core target), and never in a test build (targets §4.5 item 4).

### 3.6.10 Attribute and documentation lints

`attr_lints(owner)` reports the lints `UNUSED_ATTRIBUTE` ([spec/15](../spec/15-attributes-and-lints.md) §15.18.6) and `MISSING_DOCS` (spec/15 §15.18.7) for the declaration of an item owner and for the fields and variants that belong to it ([impl/02](02-frontend-ast-hir.md) §2.8.2; N-E-66). It reads `attrs` of each declaration ([impl/02](02-frontend-ast-hir.md) §2.10.2), the item tree, `fn_sig` and `adt_def`. It never reads a body, so it runs for every surviving owner in every build, after `@target` filtering (spec/15 §15.18: a lint is reported only for code that exists for the build). The driver applies the levels ([impl/11](11-diagnostics.md) §11.3), including the dependency cap.

**`UNUSED_ATTRIBUTE`.** For each declaration, in source order of its attributes, each case below is one occurrence at the attribute, with a machine-applicable fix that removes the attribute and its line if nothing else is on it:

1. `@must_use` on a function whose declared result type in `fn_sig` is `()`, `Never`, or a must-use type (spec/15 §15.5.1) that mentions no generic parameter in scope at the function (§3.14.1), so that no instantiation can change whether the result is must-use. A result type that contains the error type is not reported.
2. `@export_name(s)` or `@import_name(s)` where `s` is equal, by code points, to the function name without `r#`, which is the name the attribute would replace (spec/12 §12.7.6, §12.8 item 4).
3. `@repr` on an enum whose `adt_def` has no variants after `@target` filtering.
4. `@doc(hidden)` on a declaration that is not visible outside its package (the predicate of `MISSING_DOCS` below is false for it), or whose enclosing item carries `@doc(hidden)`: the trait of a trait item, the enum of a variant, the record of a field, the interface of an interface item.
5. **Level attributes** (spec/15 §15.16.8). For each `LintAttr` that `lint_levels(owner)` accepted ([impl/02](02-frontend-ast-hir.md) §2.10.2), the occurrence is at the code name, and the fix removes the name and its separator, or the whole attribute if it is the only name: a name that is `DUPLICATE_PACKAGE_LINE`, at any level; and a level `allow` or `warn` for a code that a `policy` item of the package sets to `deny` (the table of spec/15 §15.16.5, read from the parsed `sai.package` of the package as in §3.3.3). Group names are not checked by the second rule.

**`MISSING_DOCS`.** Only in the root package and the workspace members ([impl/13](13-cli.md) §13.2.2), read from `package_graph`. A declaration is **visible outside its package** when it is an item declared `pub` or `export`, a `pub` field of such a record or of a variant of such an enum, a variant of a `pub` enum, a `pub` function of an inherent impl, an item of a `pub` trait, or an item of an interface. `use` declarations, impl blocks, the functions and types of trait impls, tests and properties are never checked. Each visible declaration whose node has no `DOCS` child ([impl/02](02-frontend-ast-hir.md) §2.6.4) is one occurrence, at its name. The fix has applicability `has-placeholders` ([impl/11](11-diagnostics.md) §11.7.2): it inserts, before the first attribute of the declaration or before its first token if it has none, a line with the declaration's indentation that holds `/// ` and a placeholder. The default level is `allow`, so the driver drops the occurrences unless a level source raises the level at the name ([impl/11](11-diagnostics.md) §11.3.3).

## 3.7 Type checking bodies

### 3.7.1 Units of checking (N-E-22)

`typeck(owner) -> Arc<TypeckResults>` checks one **body owner**: a function or method with a body (including trait default methods, checked once under `Self: Tr`, spec/05 §5.9.1 item 7), a `const` initializer, a `test`, a `property`, the refinement predicate of a refined type or a distinct type with a refinement clause (with `self` of the base type), and the `invariant` predicates of a type (with `self: borrow T`, spec/09). Contract clauses (`requires`, `ensures`) are checked as part of their function's owner (§3.7.14).

Closures and `async` blocks are **not** owners. They are checked inside the `InferCtxt` of their enclosing owner, because their types are inferred together with it (spec/05 §5.16.1). Their nodes are recorded in the owner's `TypeckResults`, and `typeck(closure_def)` is answered by `typeck(enclosing owner)`. Rationale: one inference context per inference scope keeps defaulting and ambiguity decisions in one place (P8).

### 3.7.2 The inference context

```rust
pub struct InferCtxt {
    ty_vars:    UnificationTable<TyVid, TyVarValue>,    // union-find; value: Unknown { origin } | Known(Ty)
    int_vars:   UnificationTable<IntVid, Option<IntTy /* or UintTy */>>,
    float_vars: UnificationTable<FloatVid, Option<FloatTy>>,
    const_vars: UnificationTable<ConstVid, ConstVarValue>,
    trail:      Vec<UndoLog>,                           // every binding and union, for snapshots
    tainted:    Option<ErrorGuaranteed>,
}
pub struct FnCtxt<'a> {
    infcx: &'a InferCtxt,
    param_env: ParamEnv,                                 // §3.14.1
    fulfill: FulfillmentCtxt,                           // §3.7.4
    results: TypeckResultsBuilder,                      // §3.15
    ret_stack: Vec<RetCtx>,                             // Γ.ret per function boundary (fn, closure, async block)
    loop_stack: Vec<LoopCtx>,                           // break types per loop, from the resolver's label table
    diverges: Diverges,                                 // Maybe | Always { span } | WarnedAlways
    deferred_closures: Vec<HirId>,                      // closure arguments postponed by N-A-41
    deferred_casts_of_literals: Vec<(HirId, Ty)>,       // literals whose range is checked after defaulting
}
```

Every type variable has an **origin** (the span and the reason: omitted generic argument, `_`, unannotated `let`, closure parameter, …), used by `TYPE_ANNOTATIONS_NEEDED`. `snapshot()` records the length of the trail; `rollback_to(s)` undoes every entry after it; `commit(s)` keeps them. Obligations registered inside a snapshot that is rolled back are discarded with it. Snapshots nest.

### 3.7.3 Unification

`unify(expected, found, cause) -> Result<(), TypeError>` implements spec/05 §5.16.3 and §5.14:

1. Shallow-resolve both sides (follow the variable bindings at the head).
2. If either side is `{error}`, succeed (§3.5.6).
3. Two variables of the same kind are unioned. `α` with `ι` (or `φ`) binds `α` to the `ι` (or `φ`). `ι` with `φ` fails.
4. A variable with a type: `ι` accepts only the ten integer types, `φ` only `F32` and `F64`; `α` accepts any type after the **occurs check**, which fails with the note "the type would be infinite". Before binding `α`, every variable inside the type is resolved as far as possible, so the check sees the real structure.
5. A `Projection` that contains variables is not unified structurally: the obligation `projection = other side` is registered (§3.7.4) and unification succeeds. A rigid projection unifies only with an equal rigid projection.
6. Two known heads must be equal (`Adt` by `DefId`, `FnDef` and `Closure` and `Future` by `DefId`, `Ref` by mutability, `FnPtr` by safety, arity and effect set, `Array` with equal or unifiable consts) and their arguments are unified pairwise, left to right. Regions are skipped.
7. Const variables unify with a const of the same type or another `κ`. Two const values unify iff they are equal.

On failure, the `TypeError` carries the **outermost** expected and found types (resolved as far as possible) and the first mismatching pair, so that the `TYPE_MISMATCH` message names both levels when they differ. The `cause` names the origin of the expectation (spec/05 §5.14): an annotation, a parameter (with its position and the function), a return type, a previous branch, an operator operand, an array element, a field, or a pattern.

### 3.7.4 Obligations and fulfillment

```rust
pub struct Obligation { pub goal: Goal, pub cause: ObligationCause, pub depth: u32 }
pub enum Goal {
    Trait(TraitPredicate),               // τ : Tr<σ̄>
    Projection(ProjectionPredicate),     // π = τ
    WellFormed(Ty),                      // spec/05 §5.17, for types written in the body
    ClosureKind(DefId, ClosureKind),     // the closure implements at least this Fn kind (§3.11.3)
    Copy(Ty, CopyOrigin),                // `T: Copy` with a specific diagnostic (arenas, array repeat)
}
```

The `FulfillmentCtxt` keeps pending obligations in registration order. `select_where_possible()` runs the solver (§3.8.4) on each pending obligation in order and repeats until a full pass makes no progress. An obligation that is ambiguous records the variables it stalls on and is skipped until one of them is bound. `select_all_or_error()` is the end-of-body round of spec/05 §5.11.3: ambiguous goals become `TYPE_ANNOTATIONS_NEEDED`, failed ones `UNSATISFIED_BOUND`, both reported in registration order, one diagnostic per obligation cause span.

A failure found by `select_where_possible` is reported immediately, with the cause's span and the derive fix of spec/05 §5.11.2 step 6. The specific codes that replace `UNSATISFIED_BOUND` for particular causes are chosen from the cause, not from the goal: `NOT_ITERABLE` (cause `ForIterable`), `FORMAT_NOT_IMPLEMENTED` (cause `Interpolation`), `OPERATOR_NOT_SUPPORTED` (cause `Operator`), `NOT_INDEXABLE` (cause `Index`), `ARENA_REQUIRES_COPY` (cause `ArenaAlloc`, §3.7.12), `NOT_CALLABLE` (cause `CallCallee`).

**Must-know points.** `structurally_resolve(ty, span) -> Ty` implements spec/05 §5.16.4: it runs `select_where_possible()`, shallow-resolves `ty`, defaults a remaining `ι` to `I64` or `φ` to `F64` immediately (recording the defaulting in `TypeckResults::defaulted_at` for the fix of spec/05 §5.16.4), and reports `TYPE_ANNOTATIONS_NEEDED` for a remaining `α`, returning `{error}`. It is called exactly at the points listed in spec/05 §5.16.4, and nowhere else, so the set of programs that type-check does not depend on traversal details.

### 3.7.5 Expectations and coercion sites

Every expression is checked with `check_expr(e, expect) -> Ty`, where

```rust
pub enum Expectation {
    None,                      // synthesis: Γ ⊢ e ⇒ τ
    HasType(Ty),               // checking by unification: Γ ⊢ e ⇐ τ
    Coerce(Ty, CoercionSite),  // checking at a coercion site (spec/06 §6.1.3)
}
```

`Coerce` is used exactly at the sites of spec/06 §6.1.3 and spec/05 §5.15.3: annotated `let` initializers, call and method arguments other than the auto-referenced receiver, `return` operands and body tails, field initializers in constructions, right-hand sides of assignment, array elements, and `if` branches and `match` arms when an expected type exists. At such a site the form is first checked with the expected type as a hint (it flows into generic calls, closures, literals and implicit members), and the synthesized type is then passed to `coerce(found, expected, site)` (§3.10.2). Anywhere else the expected type is unified with the synthesized type.

The hint passed down is the expected type with every variable shallow-resolved. A hint is never an obligation: a form that cannot use it ignores it, and the mismatch is reported once, at the site.

**Generic calls** (spec/05 §5.16.6). When the callee is a generic function or constructor without explicit arguments and the call has an expected type, the instantiated return type is unified with the expected type inside a snapshot. If unification fails, the snapshot is rolled back and nothing is reported there.

### 3.7.6 Literals and defaulting

- An integer literal checked against a known integer type gets that type. Checked against a refined type whose base is an integer type, it gets the base type, and the coercion site then performs the B→R conversion (§3.10.2, spec/06 §6.3.1). It never gets a distinct type: against a distinct type the result is `TYPE_MISMATCH` with the fix "wrap it: `D(..)`" naming the distinct type (spec/05 §5.6.3). Synthesized, it gets a fresh `ι`. A float literal gets a known float type or a fresh `φ`. A negated literal (`Lit::Int { negated: true }`, [impl/02](02-frontend-ast-hir.md)) is one literal.
- An integer literal against an expected `F32` or `F64` is `TYPE_MISMATCH` with the fix "write `1.0`" (spec/06 §6.3).
- `Byte` literals, `Char` literals, `Bool`, `Str` literals (type `Str` with `Region::Static`), byte strings (`Slice<Byte>`, static) follow spec/06 §6.3.3.
- Range checks (`LITERAL_OUT_OF_RANGE`, spec/02) are done when the literal's type is known: immediately if it was known at the literal, otherwise after end-of-body defaulting (spec/05 §5.16.5 step 3). A literal whose value did not fit in `u128` ([impl/02](02-frontend-ast-hir.md) `value: None`) is always out of range.

**Current Stage 0 implementation slice (IR-750).** The reusable semantic kernel in `sai_ty::typeck::literal` checks one already-resolved scalar primitive type against a direct HIR literal:

```rust
pub enum PrimitiveLiteralResult {
    Match,
    TypeMismatch,
    OutOfRange,
}
pub fn check_primitive_literal(
    expected: PrimTy,
    literal: sai_hir::literals::Lit<'_>,
    symbols: &Interner,
    pointer_width: u8,
) -> PrimitiveLiteralResult;
```

The kernel accepts only the exact kind/type pairs: `Bool`, `Char`, and `Byte` literals with their corresponding primitive; integer literals with an integer primitive; and floating-point literals with `F32` or `F64`. It checks signed and unsigned integer bounds, including the direct negative-literal boundary and the explicitly supplied target width for `ISize` and `USize`. It resolves interned decimal float text through the interner and parses directly at the expected width, preserving IEEE 754 roundTiesToEven rounding. A failed parse or a rounded non-finite value returns `OutOfRange`; a nonzero float that rounds to zero remains accepted. The `TypeMismatch` result covers other literal kinds.

The driver remains responsible for resolving the expected type, selecting `pointer_width` from the current target, and constructing diagnostics using the expression span, owner, and `WorkspaceSnapshot`. It also retains canonical `Str` and unit handling outside this kernel, and turns `OutOfRange` into the existing integer or floating-point range diagnostic based on the HIR literal kind. The kernel is reused by the existing direct-literal checks, but this slice does not implement expression inference, literal variables, defaulting, coercions, `TypeckResults`, or general `typeck(owner)`. Those remain governed by the complete design above, including the end-of-body order below; implementing this leaf operation does not claim full Stage 0 type checking.

**End of body** (N-E-30). After the last expression of the owner, `typeck` runs, in order: `select_where_possible()`; defaulting of every remaining `ι` to `I64` and `φ` to `F64`, in creation order; `select_where_possible()` again; capture analysis and the closure kinds of every closure of the owner (§3.11.2, §3.11.3), which resolves the stalled `ClosureKind` goals and cannot bind any type variable; `select_where_possible()` once more; the literal range checks; the reports of `as` casts (§3.10.3); `select_all_or_error()`; and finally the report of every remaining `α` or `κ` (spec/05 §5.16.5 step 4): one `TYPE_ANNOTATIONS_NEEDED` per variable, at the leftmost expression whose recorded type mentions it, skipping variables unioned with a variable already reported. The fix inserts a type annotation on the `let` that introduced the variable, or explicit generic arguments on the path whose omitted argument created it, depending on the variable's origin.

### 3.7.7 Paths and implicit members

A path expression is typed from its `PathRes` (§3.4.1) by the table of spec/06 §6.4.1:

- **Locals and `self`** have the type recorded for their binding (§3.12.1).
- **Function items, tuple variants and distinct constructors** get `type_of(def)` instantiated with the explicit generic arguments (all or none, spec/05 N-A-21; `_` is a fresh variable) or with fresh variables, and the item's `predicates_of`, substituted, are registered with cause `ItemPredicate { item, span }`. The arguments are recorded in `TypeckResults::node_args`.
- **Constants** get their declared type. SAI has no `static` items: there is no global mutable state (D-CAP-02), and `static` is a reserved word ([spec/02](../spec/02-lexical-structure.md) §2.5.3).
- **Unit variants** get the enum type with fresh or explicit arguments.
- **Record types and record variants** used as values are `RECORD_CONSTRUCTOR_AS_VALUE`; types, modules and traits are `EXPECTED_VALUE_FOUND_TYPE`. Both give `{error}`.
- **Type-relative paths** (`T.f`, `E.V`, `Self.new`, `Tr.m`) are resolved by §3.9.5, which records the selected item in `type_dependent_defs`.

An **implicit member** `.V` ([impl/02](02-frontend-ast-hir.md) `ExprKind::ImplicitMember`) takes the expectation as it is (spec/06 N-C-3): with no expectation, or an expectation that is still an unresolved variable after `structurally_resolve`, the error is `IMPLICIT_MEMBER_NEEDS_TYPE` (with the fix "qualify as `E.V`" when exactly one enum in `S(M)` and the prelude has a variant `V`); a known non-enum head, including a flags type ([16](16-interfaces-and-wit.md) §16.9.3), is `IMPLICIT_MEMBER_NOT_ENUM`; a missing variant is `UNKNOWN_VARIANT`. The resolved variant is recorded in `TypeckResults::implicit_members`, and the expression is then checked exactly like `E<A>.V`, `E<A>.V(..)` or `E<A>.V(f: ..)`. The expectations that reach an implicit member are those of the coercion sites, the right operand of `==` and `!=`, and patterns (spec/06 §6.4.3); every other position passes `Expectation::None`.

### 3.7.8 Blocks and statements

- **Blocks** follow spec/06 §6.11.1 (N-C-14): the statements are checked in order, then the tail against the block's expectation. A block without a tail has type `()`, unless it diverges, in which case it has type `Never`. `unsafe` blocks are typed like other blocks: type checking does not track the unsafe context, which [impl/08](08-effect-and-capability-analysis.md) §8.10.2 computes from the HIR, including the unsafe-module rule of [spec/12](../spec/12-unsafe-and-ffi.md) §12.2.4; `region` blocks are §3.7.12.
- **`let p: T = e`** checks `e` at a coercion site against `lower_ty(T)` and then the pattern against `T`. Without an annotation, `e` is synthesized and the pattern is checked against the result. A refutable pattern is reported by `check_match` (§3.12.2). `var` is `let` with mutable bindings.
- **`let`-`else`** checks the initializer and the pattern as `let`, then the `else` block with no expectation; the block's type must be `Never` (`LET_ELSE_NOT_DIVERGING`, on the block). `LET_ELSE_IRREFUTABLE` is reported by `check_match`.
- **Assignment** `p = e`: the left side is checked as a place expression in a mutable place context (§3.7.12 for indexing), then `e` at a coercion site against its type. The left side must be a place (spec/06 §6.13.1): [impl/02](02-frontend-ast-hir.md) already rejected non-place syntax, and a path that does not resolve to a local binding, such as a constant or a function, is `INVALID_ASSIGN_TARGET` (spec/03) here; a place that is not mutable is reported by [impl/05](05-borrow-checker.md) (`ASSIGN_TO_IMMUTABLE`, spec/07 §7.2.4), which has the place's mutability from MIR.
- **Compound assignment** `p op= e` follows spec/06 §6.13.2: built-in when both sides are primitive numeric types accepted by the operator (§3.10.1), otherwise the `…Assign` trait of the operator, with the left side in a mutable place context. The result is recorded as `OpRes` (§3.10.1).
- **`assert c, m`** checks `c` against `Bool` and `m` (if present) against `Str` or `String`: `m` is synthesized, `structurally_resolve`d, and must be `Str` or `String` (`TYPE_MISMATCH` naming both). An `assert` in a const initializer is `NOT_CONST_EXPRESSION`.
- **Expression statements** are checked with no expectation. The discarded-value rules of spec/06 §6.25.2 are applied after the end-of-body processing, on resolved types: `UNUSED_MUST_USE` for a `@must_use` type or a call of a `@must_use` function, else `STATEMENT_HAS_NO_EFFECT` for a side-effect-free expression by the syntactic list of spec/06 §6.25.2. A statement beginning with an implicit member gets `IMPLICIT_MEMBER_NEEDS_TYPE` through the absent expectation.

**Initial Stage 0 body-checking slice (IR-751).** IR-751 accepted a non-generic, safe, synchronous function with no parameters whose body consists of exactly one immutable, single-name `let name: T = literal` statement followed by a tail path that resolves by `BindingId` to that binding. The binding name and tail-read spelling must be plain ASCII identifiers, and the binding name must not trigger the current function-name convention gate; names that could trigger `NAMING_CONVENTION`, non-ASCII names, and raw identifiers remain gated because the general lint checks are unavailable. The annotation and function return type must each resolve directly to a supported primitive type, and the initializer must be a direct scalar primitive literal. Check the initializer against the annotation using the primitive-literal semantics of §3.7.6, give the binding that annotated type, and check the tail read against the function return type. This exercises one source-ordered statement, an explicit expected type, and a resolved local read. It does not implement unannotated-local inference, general patterns, computed expressions, nested blocks, or general `typeck(owner)`.

That initial slice gated `var`, `let`-`else`, tuple, refutable or multi-binding patterns, multiple statements or local bindings, non-literal initializers, non-local or computed tails, function parameters, generic functions, aliases, refined and distinct types, and every other unsupported body shape.

**IR-752 body-checking slice.** The no-parameter form accepts one or more source-ordered immutable, single-name `let name: T = literal` statements before the tail path. Each binding name and the tail-read spelling must be a plain ASCII identifier, and each binding name must pass the current naming-convention gate; raw identifiers, non-ASCII names, and names that could trigger `NAMING_CONVENTION` remain gated because the general lint checks are unavailable. Every annotation and the function return type must resolve directly to a supported primitive type, and every initializer is a direct scalar primitive literal. Check each initializer against its annotation in source order, bind that resolved type to the local's `BindingId`, then check the plain tail path against the function result using the referenced binding's type. Report initializer diagnostics in statement order followed by any tail mismatch, with each diagnostic anchored to its initializer or tail.

**Current Stage 0 body-checking slice (IR-753).** In the IR-752 form, an annotated primitive initializer may also be a one-segment, non-generic plain local path that resolves by `BindingId` to an earlier local in the checked block. The earlier binding's annotated primitive type must exactly match the destination annotation; other primitive pairs do not coerce. The current local is added to the checked environment only after its initializer, so self-reference and forward reference remain gated. Raw or non-ASCII path spellings, paths that resolve to non-local declarations, qualified or generic paths, and all other statement, pattern, initializer, tail, and type forms remain gated. Local-copy mismatch diagnostics point to the initializer and are reported before any tail mismatch. Existing direct-literal and bounded conditional body slices remain separate; this is not general `typeck(owner)` or complete Stage 0 checking.

**IR-754 return-constrained local slice.** A no-parameter function with an explicit supported primitive return type may contain exactly one immutable, single-name, unannotated `let` whose initializer is a direct scalar primitive literal, followed by a plain path that resolves by `BindingId` to that local. An integer literal's `ι` or floating-point literal's `φ` is constrained by the tail expectation only when the return type belongs to that literal family; the existing primitive literal kernel then checks its exact range or target-width rounding at the initializer. A family mismatch is reported at the tail. If that leaves `ι` or `φ` unconstrained, it defaults to `I64` or `F64` at body end and its range is checked at the initializer. `Bool`, `Byte`, and `Char` literals already have concrete types and must exactly match the return type; a mismatch is reported at the tail. No numeric conversion is added. Multiple locals, computed or non-scalar initializers, mutable or destructured bindings, different tail bindings, parameters, aliases, and all other body forms remain gated. This does not implement general inference or `typeck(owner)`.

**IR-755 annotated-source copy slice.** A no-parameter function with an explicit supported primitive result may contain exactly two immutable, single-name `let` statements: first `let source: T = literal` with a directly supported primitive annotation and scalar literal, then `let copied = source` with a plain, one-segment, non-generic local path. Its tail must be a plain path that resolves by `BindingId` to `copied`. The source initializer is checked against its annotation using the existing primitive-literal rules. The unannotated copy inherits the source's exact concrete primitive type; this initializer is not a coercion site, so no numeric conversion is performed. A source-literal mismatch or range error points to that initializer, while a return-type mismatch points to the tail. Unannotated literal sources (whose `ι` or `φ` may still be constrained by the return type), additional copy chains, parameters, mutable or destructured bindings, nonlocal/computed/raw/qualified/generic paths, and all other body shapes remain gated. This bounded rule does not implement general local inference or `typeck(owner)`.

**IR-756 inferred-literal copy slice.** Accept the same two-local shape when the first local is unannotated and initialized by a direct scalar literal, and the second is `let copied = source` using a plain, one-segment, non-generic path resolving to the first binding. The tail must resolve by `BindingId` to `copied`. The copied value retains the source literal's type state: integer `ι` and float `φ` remain unresolved until the tail's expected return type constrains them, while Bool, Byte, and Char retain their concrete types. Family mismatch diagnostics point to the tail; an unconstrained numeric variable defaults to `I64` or `F64` at body end, and range or rounding errors point only to the original literal initializer. A same-family out-of-range literal is checked against the explicit return type at that initializer. Mutable or destructured locals, further copies, computed or nonlocal/raw/qualified/generic paths, and all other unsupported body forms remain gated. This does not implement general inference, longer copy chains, or complete `TypeckResults`.

**IR-757 flat primitive-local sequence slice.** Generalize the preceding forms to a non-empty, source-ordered sequence of immutable, single-name `let` statements and a plain tail path resolving by `BindingId` to any local in that sequence. Each local may have no annotation or a direct supported primitive annotation. Its initializer must be a direct scalar primitive literal or a plain, one-segment, non-generic path to a preceding local. Each unannotated integer or float literal creates a distinct inference variable; copying through one or more local paths preserves that variable's identity and original literal span. An annotation on a compatible numeric literal or copied literal variable constrains it at that initializer. An incompatible annotation reports the mismatch there, leaves the original literal variable unresolved for body-end defaulting, and still gives the annotated destination its declared type. The tail constrains an unresolved numeric variable only when the explicit result belongs to its literal family; family mismatches point to the tail, and any unresolved variable defaults to `I64` or `F64` after tail checking. Range and rounding errors are emitted once at the original literal. Bool, Byte, and Char remain concrete, and no numeric conversions are added. Preserve the existing safe, synchronous, non-generic, no-parameter owner and plain-ASCII/naming gates. Mutable or destructured bindings, self/forward/nonlocal/computed/raw/qualified/generic paths, nested or otherwise unsupported body forms, and all other owner/type shapes remain gated. This composes the bounded local forms; it is not general local inference or complete `TypeckResults`.

**IR-758 parameter-source local slice.** The IR-757 form may have exactly one named, immutable, non-`self` parameter whose type resolves directly to a supported primitive. The body parameter must have the same name and a `BindingId` classified as `Param`; that binding is inserted into the initial type environment as its concrete primitive type. A local initializer path may therefore read either the parameter or a preceding local, but the tail must still read a local declared in the block. An annotated copy of the parameter requires exact primitive equality; an unannotated copy inherits its concrete type. Do not add numeric conversions. Multiple parameters, mutable or `self` parameters, user-defined aliases, parameter tails, and all other forms excluded by the earlier local-slice gates remain unsupported.

**IR-759 multiple-parameter local sources.** Generalize IR-758 so a function may have any number of named, immutable, non-`self` parameters, provided every signature/body parameter pair has the same name, each body binding is a `Param`, and every parameter type resolves directly to a supported primitive. Seed every parameter binding into the initial type environment by `BindingId`. Local initializers may read any parameter or preceding local; the tail must still read a local declared in the block. Annotated copies require exact primitive equality, while unannotated copies preserve the source type. A mutable, `self`, unnamed, unsupported, or user-defined-alias parameter gates the whole form. This changes neither the local-sequence rules nor numeric-conversion behavior.

**IR-760 parameter tail after local statements.** In the parameter-source form, the tail may read any binding visible after the source-ordered local sequence: either an eligible function parameter from the initial environment or a local declared by one of its statements. Resolve the tail by `BindingId`, then check its concrete type against the explicit function result. A mismatch is reported at the tail; no primitive conversion is introduced. Self/forward paths and all other previously gated forms remain unsupported.

**IR-761 flat-local primitive comparison initializers.** An immutable, single-name local may also be initialized by one direct comparison (`==`, `!=`, `<`, `<=`, `>`, or `>=`) whose two operands are plain, one-segment, non-generic paths to eligible parameters or preceding locals. Resolve each operand by `BindingId`; both must already have the same concrete supported primitive type, and every comparison operator is supported for that primitive set (spec/06 §6.8.7). The expression's type is `Bool`, so its optional annotation must be `Bool`. If operand types differ, report `TYPE_MISMATCH` at the right operand path with the left and right primitive types; recover the comparison result as `Bool`. If an annotation is not `Bool`, report `TYPE_MISMATCH` on the whole initializer and still bind the local as `Bool`, so later reads and the tail use the comparison's actual result type. Do not infer unresolved literal variables from a comparison. Literal operands, arithmetic, logical combinations, nested expressions, nonlocal or qualified paths, aliases, conversions, unsupported operand types, mutable/destructured bindings, self/forward references, and other unsupported forms remain gated. No coercions or numeric conversions are introduced.

**IR-762 flat-local primitive logical initializers.** An immutable, single-name local may also be initialized by one direct top-level `and` or `or` whose two operands are plain, one-segment, non-generic paths to eligible parameters or preceding locals. Resolve both operands by `BindingId` and check each against `Bool` in written source order. Each non-`Bool` supported primitive operand gets its own `TYPE_MISMATCH` at that operand path. The expression recovers to `Bool`, so an optional annotation must be `Bool`; a different supported primitive annotation reports one mismatch at the whole initializer while subsequent local reads and the tail still see `Bool`. Typechecking does not rewrite the logical operator or its operands, preserving `and`/`or` short-circuit behavior (spec/06 §§6.8.8 and 6.23). Nested or chained logical forms, `not`, literal operands, arithmetic, comparisons, qualified or nonlocal paths, aliases, unresolved/self/forward paths, unsupported types, and other unsupported body forms remain gated.

**IR-763 flat-local unary `not` initializer.** An immutable, single-name local may also be initialized by exactly one unary `not` applied to a plain, one-segment, non-generic path to an eligible parameter or preceding local. Resolve the operand by `BindingId` and require its type to be `Bool`; a non-`Bool` supported primitive reports `TYPE_MISMATCH` at the operand path, while the initializer recovers as `Bool`. An omitted or `Bool` annotation is accepted; another supported primitive annotation reports a mismatch at the whole initializer, and later local reads and tail checking still use `Bool`. Preserve the unary operator and operand. Nested or double `not`, `not` applied to a comparison, logical operator, literal or computed expression, nonlocal/qualified/unresolved paths, user-defined aliases, and all other unsupported forms remain gated (spec/06 §§6.8.6 and 6.8.8).

**IR-764 flat-local path/literal comparison initializer.** An immutable, single-name local may also be initialized by one direct comparison (`==`, `!=`, `<`, `<=`, `>`, or `>=`) with exactly one plain path to an eligible parameter or preceding local and one direct scalar `Bool`, `Char`, `Byte`, integer, or floating-point literal, in either operand order. Resolve the path by `BindingId`; the comparison result is `Bool`, and an omitted or `Bool` annotation is accepted. Check a literal on the right against the path's primitive type. When the literal is on the left, check an integer against any integer primitive and a float against `F32` or `F64`; range-check the literal at its own span using the selected target width. If the literal family does not match the path type, report the inferred/default literal type (`I64` or `F64`) or fixed literal type (`Bool`, `Char`, or `Byte`) at the path. A right-side literal type/range error points to the literal. Any operand error still recovers as `Bool`; a non-`Bool` annotation reports at the initializer and subsequent reads and the tail use `Bool`. Preserve the written operator and operand order. Repeated and shadowed bindings are handled by resolved binding identity. Literal/literal, string and unit, computed or nested operands, logical/arithmetic forms, nonlocal/qualified/alias/unresolved paths (including unresolved numeric locals), self/forward references, unsupported types, and other unsupported forms remain gated. The transparent built-in aliases `Int` and `UInt` resolve to `I64` and `U64`; no numeric conversions are introduced (spec/06 §§6.3.1–6.3.3, 6.8.7, and 6.11.1).

**IR-765 flat-local addition initializer.** An immutable, single-name local may also be initialized by one direct `+` whose two operands are plain, one-segment, non-generic paths to eligible parameters or preceding locals. Resolve both operands by `BindingId`, and accept only the same concrete primitive integer type or the same `F32`/`F64` type. There is no widening. A mismatched supported numeric right operand reports `TYPE_MISMATCH` at its path and the expression recovers to the left operand's type. An omitted annotation or an annotation equal to the result type is accepted; a different supported primitive annotation reports `TYPE_MISMATCH` on the whole initializer, and the local recovers to its declared annotation. Preserve the direct addition expression. Numeric literal operands were gated at this IR-765 step; IR-767 and IR-768 later added direct right-hand numeric literals for `+` and for `-`/`*`, respectively. Unresolved numeric locals, user-defined aliases, nonlocal or qualified paths, nested or computed expressions, other operators, Byte/Char/Bool operands, mutable or destructured bindings, self/forward references, and every other unsupported shape remain gated. Transparent `Int`/`UInt` paths use their resolved `I64`/`U64` primitive identity (spec/06 §§6.8.2–6.8.3, 6.11.1, and 6.12.1).

**IR-767 flat-local addition with a right numeric literal.** Extend the direct local-initializer `+` form to accept a concrete primitive integer or float path to an eligible parameter or preceding local on the left and one direct integer or floating-point literal on the right. Follow left-directed operand typing: check the right literal against the left path's primitive type, report a literal-kind mismatch or target-width range error at the literal, and recover the expression as the left type. An omitted or matching annotation is accepted; a different supported primitive annotation reports at the whole initializer and the local recovers to its declared type. Preserve the written addition. At this step, the literal-left order remained deferred: §6.8.2 synthesizes the left literal to `ι`/`φ` and checks the right path against that variable (§5.16). IR-770 later implements that form and its family-mismatch recovery. Unresolved numeric locals, fixed-kind literals, nonnumeric left types, nested/computed expressions, and every other unsupported shape remain gated. In particular, integer literals are not implicitly accepted as floats; an integer literal checked against `F32` or `F64` reports the §3.7.6 mismatch. Right-literal subtraction and multiplication were deferred here and are covered by IR-768 below. Arithmetic evaluation and overflow checks remain unimplemented (spec/06 §§6.3.1, 6.8.2–6.8.3, 6.11.1, and 6.12.1).

**IR-770 flat-local arithmetic with a left numeric literal.** Accept one direct integer or floating-point literal on the left of `+`, `-`, or `*` and a concrete numeric path on the right to an eligible parameter or preceding local, resolved by `BindingId`. Constrain an integer literal's `ι` to any integer primitive and a float literal's `φ` to `F32` or `F64`; range-check at the full literal span (including a directly folded minus) with the selected target pointer width, and give the expression the right path type. For a cross-family mismatch, report `TYPE_MISMATCH` at the right path using `I64` or `F64` as the literal's default type, and retain the left literal variable as the expression/local type. A later matching annotation or tail can constrain it; an incompatible annotation reports the literal mismatch on the initializer and recovers the local to its annotation. Unconstrained variables default and range-check at body end. Fixed-kind literals, literal/literal arithmetic, unresolved numeric locals as arithmetic paths, nested/computed expressions, other operators, aliases, arithmetic evaluation, and overflow checking remain outside this slice.

**IR-771 flat-local division.** Extend the immutable single-name local arithmetic initializer to one direct `/` between two eligible concrete numeric paths, one path and one direct numeric literal on the right, or one direct numeric literal and path on the left. Resolve paths by `BindingId`; require exact type equality for two concrete paths, and reuse the established literal-family inference, range checks, mismatch spans, and annotation recovery for either literal order. Accept the ten integer types and `F32`/`F64`; do not widen or convert. An integer literal divisor of zero is accepted during type checking because division-by-zero is a runtime `DivByZero` panic; signed `MIN / -1` overflow and all arithmetic execution remain outside this slice. `%`, wrapping operators, literal/literal division, computed or nested expressions, user-defined aliases, and every other unsupported shape remain gated (spec/05 §5.16; spec/06 §§6.8.2–6.8.3, 6.11.1, and 6.12.1).

**Divergence.** `FnCtxt::diverges` tracks spec/06 §6.26: when a subexpression's synthesized type is `Never`, the flag becomes `Always { span }`. Before checking the next operand of the enclosing expression, or the next statement of the enclosing block, the checker tests the flag; if it is `Always`, the warning `UNREACHABLE_CODE` is reported on that operand or statement, the flag becomes `WarnedAlways` (so it is reported once per diverging expression), and the operand's or statement's `HirId` is appended to `TypeckResults::unreachable` (N-E-29). Unreachable code is checked normally. The flag is saved and restored around branches: an `if` diverges only if both branches do, a `match` only if every arm does (or it has no arms), a loop only if it is a `loop` without a `break` to it.

### 3.7.9 The `?` operator

`e?` ([impl/02](02-frontend-ast-hir.md) `ExprKind::Try`) is checked by spec/10 §10.3.2, in the order of that section so that each `?` gets at most one diagnostic:

1. `e` is synthesized and `structurally_resolve`d. Its head must be the lang item `Result` or `Option` (`TRY_INVALID_OPERAND`, with the `.as_ref()` help for a borrowed operand).
2. If `Γ.ret` (the top of `ret_stack`) is still a variable, it is unified with `Result<β, E>` or `Option<β>` with a fresh `β` (spec/10 N-C-70).
3. Otherwise `Γ.ret` is `structurally_resolve`d; a head that is neither `Result` nor `Option` is `TRY_INVALID_RETURN_TYPE`; a different one of the two is `TRY_OPTION_RESULT_MIX`.
4. For `Result`, the error types are unified with cause `TryErrorType`; failure is `TRY_ERROR_TYPE_MISMATCH` (never `TYPE_MISMATCH`), with the help and the structured fix of spec/10 §10.3.5.
5. `?` inside a contract, refinement or invariant expression is `INVALID_CONTRACT_EXPRESSION` (spec/09); inside a const initializer `NOT_CONST_EXPRESSION`. Both are detected from the owner kind and the `ContractScope` rib before step 1.

The result type is the payload type. `TypeckResults::try_kinds[e]` records `TryKind::Result { ok, err }` or `TryKind::Option { some }` with the resolved types, from which [impl/04](04-mir.md) builds the `SwitchInt`, the `Downcast` and the early return.

### 3.7.10 Conditionals, `match`, loops and jumps

- **`if c { a } else { b }`**: `c` against `Bool`. With an expectation, both branches are checked at coercion sites against it. Without one, the first non-diverging branch is synthesized and the other is checked against it by unification; a branch of type `Never` is skipped (spec/05 §5.15.3). An `if` without `else` has type `()`; checked against a non-`()` expectation, it is `IF_WITHOUT_ELSE` (spec/06 §6.14.1), on the `if` keyword.
- **`match`**: the scrutinee is synthesized; the arms' patterns are checked against its type (§3.12.1), guards against `Bool`, and the arm bodies like `if` branches. `structurally_resolve` is applied to the scrutinee type when any arm has a pattern other than a binding or `_` (spec/05 §5.16.4). `MatchSource` ([impl/02](02-frontend-ast-hir.md)) is kept; desugared matches (`if let`, `while let`, `for`, `?` is not one) report their errors in source terms.
- **Loops.** `loop_stack` gets one entry per `Loop`, with its `LoopSource` and a break type: for `LoopSource::Loop`, a fresh `α` coerced to by every `break e` (a coercion site against the loop's expectation if any, else unification with the first `break`'s type); for `While`, `WhileLet`, `ForLoop` and `ForAwait`, the break type is `()`. A `break e` that targets one of these is `INVALID_BREAK_VALUE`, on `e`. `loop` without any `break` to it has type `Never`. `break` and `continue` have type `Never`.
- **`return e`** checks `e` at a coercion site against `Γ.ret`; `return` alone checks `()` against it.
- **Ranges** construct the lang-item range types of spec/06 §6.19; the right end is checked against the left end's type.
- **`for`** was desugared by [impl/02](02-frontend-ast-hir.md) into `IntoIterator.into_iter` and `Iterator.next` calls; the iterable's type is `structurally_resolve`d first (spec/05 §5.16.4), and the `IntoIterator` obligation has cause `ForIterable` so that its failure is `NOT_ITERABLE`. `for await` is §3.7.11.

### 3.7.11 `async`, `.await` and `for await` (Stage 5)

Each entry of `ret_stack` is

```rust
pub struct RetCtx { pub ty: Ty, pub kind: BodyKind, pub span: Span }
pub enum BodyKind { Fn { is_main: bool, is_free_or_inherent: bool }, AsyncFn, Closure, AsyncBlock, ConstInit, Predicate, Test }
```

`Γ.async` (spec/11 §11.3.1) holds iff the top entry is `AsyncFn` or `AsyncBlock`; it is never inherited across a `Closure` entry.

- **`async fn f`.** Its HIR body is a block whose tail is a synthesized `AsyncBlock` ([impl/02](02-frontend-ast-hir.md) §2.9.15). The outer body is checked with `Γ.ret = fn_sig(f).output = Future(f, identity args)`. The synthesized block is recognized by its `def_id` being the async body of `f`, gets the type `Future(f, identity args)` (not a fresh future type), and is checked with a pushed `RetCtx { ty: async_output, kind: AsyncFn }`. `return`, the tail and `?` therefore use the declared output type (spec/11 §11.2.1).
- **`async` blocks.** Any other `AsyncBlock` pushes `RetCtx { ty: α, kind: AsyncBlock }` with a fresh `α` and an empty `loop_stack`, and has type `Future(block_def, parent identity args ++ [α, υ])`, where `υ` is the capture tuple filled in by capture analysis (§3.11.5). `α` is the future's output. Captures are computed with those of closures (§3.11.2).
- **`e.await`.** If `Γ.async` does not hold: inside a `Predicate` the error is `INVALID_CONTRACT_EXPRESSION`, inside a `ConstInit` `NOT_CONST_EXPRESSION`, and otherwise `AWAIT_OUTSIDE_ASYNC` at the `.await` token, with a note at the closure boundary when an enclosing entry is async and the fix "add `async` to the function" when the top entry is `Fn { is_main: false, is_free_or_inherent: true }`. `e` is then still checked, and the result is `{error}`. Otherwise `e` is synthesized and `structurally_resolve`d, the obligation `F: Future` is registered with cause `Await` (on failure `UNSATISFIED_BOUND` with the note "`.await` requires `Future`"), and the result is the normalized `<F as Future>.Output`. `TypeckResults::await_kinds[e] = AwaitKind::Future`.
- **`for await`.** The desugaring of [impl/02](02-frontend-ast-hir.md) §2.9.6 contains `Await(Call(lang::StreamNext, [Borrow(Mut, %stream)]))`. `StreamNext` is not a callable item: the type checker recognizes this exact shape, checks `Γ.async` as for `.await` (reported at the `await` token of `for await`), takes the type `S` of `%stream`, registers `S: Stream` with cause `ForAwaitStream` (on failure `UNSATISFIED_BOUND` with the note "`for await` requires `Stream`", plus "use a plain `for`" when `S: IntoIterator` holds in a probe), and gives the `Await` the type `Option<<S as Stream>.Item>`. It records `await_kinds[e] = AwaitKind::StreamNext` and does not record any call resolution for the inner `Call`. The scrutinee of the outer `match` is checked in a value context, so the stream is consumed (spec/11 §11.6.2 item 2).
- `ASYNC_FN_IN_TRAIT` and `ASYNC_MAIN` are reported by `wf_check_item` on the `async` keyword. Whether a future type contains itself (`ASYNC_RECURSION`) is decided on monomorphic MIR ([impl/04](04-mir.md) §4.13.4).

### 3.7.12 Places, constructions and calls

**Fields.** `Field { base, name }` that the resolver left as a value path (§3.4.2) is checked as follows: the base is synthesized, and the auto-deref chain of spec/06 §6.6.1 is walked with `structurally_resolve` at each step (a must-know point). The first record type with a field `name` visible here, a distinct type when `name` is `value` (visibility per §3.13.3), or a refined type over a record with that field, resolves the access. The result records `field_indices[e] = FieldIdx` and `adjustments[base] = [Deref; i]` (§3.10.2). Failures: `PRIVATE_FIELD` if the field exists but is not visible, `UNKNOWN_FIELD` otherwise (with the note "match on the variant" for an enum, and the fix "write `(r.f)(x)`" is the method-call case of §3.9.4). `TupleField` follows spec/06 §6.6.2 (`TUPLE_INDEX_OUT_OF_RANGE`, `UNKNOWN_FIELD`; `INVALID_TUPLE_INDEX` was reported by [impl/02](02-frontend-ast-hir.md)).

**Place contexts.** The checker passes a `PlaceCtx { Value, ImmPlace, MutPlace }` down to place expressions. `MutPlace` is used for the left side of `=` and `op=`, the operand of `borrow mut`, a receiver auto-referenced as `borrow mut` (known only after method selection), and the callee of an `FnMut` call. Mutability itself is checked by [impl/05](05-borrow-checker.md); this chapter uses the context only to choose between `Index` and `IndexMut`.

**Indexing** follows spec/06 §6.7 (N-C-12): no auto-deref; the base and the index are synthesized; built-in when the base is `Array`, `Slice` or `SliceMut` and the index is `USize` or an unresolved `ι` (unified with `USize`); otherwise `Index<I>` is selected with cause `Index` (`NOT_INDEXABLE`), unifying an unresolved `ι` index with `USize` first when `C: Index<USize>` holds in a probe. The result is a place of type `<C as Index<I>>.Output`. `TypeckResults::index_kinds[e]` records `IndexKind::Builtin` or `IndexKind::Overloaded { mutbl }`. The choice of `IndexMut` is made by a **fix-up** after the enclosing expression is checked (N-E-32): every `Overloaded` index that ends up in a `MutPlace` context, directly or as the base of a field, tuple field, deref or nested index of such a place, is switched to `mutbl: Mut`, and the obligation `C: IndexMut<I>` is registered with cause `Index`. The receiver case is fixed up after method selection, by the same walk over the receiver expression.

**Records and variants.** `Call { callee, args, base }` whose callee resolves (§3.7.7, §3.9.5) to a record type, a record variant, `Self` of record type, or an implicit member naming a record variant is a record construction (spec/06 §6.18.1). Generic arguments come from the path or are fresh, with the expected type unified in a snapshot (§3.7.5). For each argument in source order: `name: e` names a field; a nameless argument that is a single-segment value path `x` is punning; any other nameless argument is `RECORD_FIELD_NEEDS_NAME`. Then `DUPLICATE_FIELD`, `UNKNOWN_FIELD` and `PRIVATE_FIELD` are reported per argument, each initializer is checked at a coercion site against the substituted field type, the base is checked against the record type by unification, and `MISSING_FIELD` lists every field (declaration order) not covered when there is no base; `REDUNDANT_UPDATE_BASE` is reported on the base when every field is explicit. `TypeckResults::record_ctors[e]` records the variant, the field index of every argument and whether the base supplies the rest. A `base` in any other call is `FUNCTIONAL_UPDATE_OUTSIDE_RECORD`. A construction whose callee is an `IS_HANDLE` resource (an imported resource, or an unbound resource of a local interface) is `OPAQUE_RESOURCE_CONSTRUCTION` instead of the field checks ([16](16-interfaces-and-wit.md) §16.7.5).

**Tuple variants and distinct construction** are calls of the constructor's `FnDef` (§3.6.3). `D(e)` with a refinement clause is a B→R conversion recorded for [impl/04](04-mir.md) (§3.13.1). Unit variants with an argument list are `WRONG_ARGUMENT_COUNT`.

**Tuples and arrays.** A tuple with an expected tuple type of the same arity checks the elements against the element types; otherwise each element is synthesized. An array with an expected `Array<U, m>` checks the elements at coercion sites against `U` and reports `ARRAY_LENGTH_MISMATCH` when `m ≠ n` (with `m` an unresolved `κ`, the constant is unified with `n`); without an expectation the first element is synthesized and the rest are checked against it; `[]` without an expected element type is `TYPE_ANNOTATIONS_NEEDED`. `Array.filled(x)` is a call of the lang item `ArrayFilled`; its `T: Copy` bound is registered as `Goal::Copy(T, CopyOrigin::ArrayFilled)`, whose failure is `UNSATISFIED_BOUND` naming `Copy` and N-C-18.

**Calls.** `Call { callee, args }` that is not a construction and not a method call (§3.9) is checked by spec/06 §6.5.1:

1. The callee is synthesized and `structurally_resolve`d.
2. `callable(F)`: a `FnDef` or `FnPtr` gives its signature directly (instantiated, with the item's predicates registered, §3.7.7). A `Closure` gives its signature (§3.11.1). A `Param` or `Projection` is searched for a `Fn`, `FnMut` or `FnOnce` bound in the `ParamEnv` (§3.14.1); the first one found in the order `Fn`, `FnMut`, `FnOnce` supplies the argument tuple and `Output`. An unresolved variable is `TYPE_ANNOTATIONS_NEEDED`; anything else is `NOT_CALLABLE` (cause `CallCallee`), on the callee.
3. The argument count must match (`WRONG_ARGUMENT_COUNT`, naming both counts; extra arguments are still checked with no expectation). A named argument is `NAMED_ARGUMENT_ON_FUNCTION` (spec/03) on the argument, unless the callee ends with a LIDENT segment, in which case [impl/02](02-frontend-ast-hir.md) already reported it; the argument's value is still checked at its position.
4. Arguments are checked at coercion sites in source order, except that arguments whose expression is a closure are checked after all the others, in source order (spec/05 §5.16.6, N-A-41), so that the bounds of a generic callee are known when the closure's signature is deduced (§3.11.1).
5. **`MISSING_BORROW`.** When the coercion of argument `i` fails, the parameter type is `borrow U` or `borrow mut U`, and the argument's synthesized type unifies with `U` in a probe, the error is `MISSING_BORROW` (spec/07 §7.5.2) with the machine-applicable fix "insert `borrow`" or "insert `borrow mut`", instead of `TYPE_MISMATCH`.
6. `TypeckResults::call_kinds[e]` records `CallKind::Direct(def, args)`, `CallKind::FnPtr`, or `CallKind::FnTrait { kind, method }`. The kind of a closure call is decided after capture analysis (§3.11.4), so it is recorded as `CallKind::Pending` and replaced by `FnTrait` at the end of the body. An `unsafe fn` callee is recorded for [impl/08](08-effect-and-capability-analysis.md), which reports calls outside `unsafe` (spec/12).

**Deref and borrow.** `*e` follows spec/06 §6.9.1: `borrow T`, `borrow mut T` and `Box<T>` give a place of type `T`; `Ptr<T>` and `PtrMut<T>` also give `T`, with the requirement of `unsafe` checked by [impl/08](08-effect-and-capability-analysis.md); anything else is `CANNOT_DEREFERENCE`. `borrow p` and `borrow mut p` give `Ref(Erased, T, mutbl)`; a value operand is recorded as needing a temporary (`TypeckResults::temps`), which [impl/04](04-mir.md) materializes.

**Region blocks.** `Region { binding, block }` binds `binding` with type `borrow Arena` (lang item `Arena`) and checks the block against the region's expectation. The arena methods of spec/07 §7.15.2 are ordinary library methods; the obligation for their `T: Copy` bound gets the cause `ArenaAlloc` when the method is a method of the lang-item type `Arena`, so its failure is `ARENA_REQUIRES_COPY` with the fix "allocate a `Copy` representation, or use `Box`/`Vec` outside the arena" (and "use `r.alloc_str(..)`" when the argument has type `String`).

### 3.7.13 Format strings (Stage 3)

`FormatString(pieces)`: each `FmtPiece::Arg { expr, spec }` is synthesized in an `ImmPlace` context (a value operand needs a temporary). The obligation `T: Format` has cause `Interpolation` (`FORMAT_NOT_IMPLEMENTED`). The specifier is checked by spec/16 §16.14.3: an unknown name is `UNKNOWN_FORMAT_SPEC`; a missing, unexpected or out-of-range argument is `FORMAT_SPEC_ARGUMENT`; otherwise the required marker trait (`FormatInt`, `FormatFloat` or `FormatPad`) is registered with cause `FormatSpec`, whose failure is `FORMAT_SPEC_NOT_APPLICABLE`. When `T: Format` fails, the specifier obligation is dropped, so no specifier error is reported (rule 3). The type of the expression is `String`. `TypeckResults::format_specs[piece]` records the `FormatSpec` variant and argument that [impl/04](04-mir.md) passes to `Format.format`.

### 3.7.14 Contracts and predicates

Predicates (spec/09 §9.1.2) are checked against `Bool` with `RetCtx { kind: Predicate }` pushed, so that `return`, `break`, `continue`, `?`, `.await`, assignments, `var`, `unsafe` and `region` blocks and closures are `INVALID_CONTRACT_EXPRESSION`. In the eventual tracked pipeline, the resolver reports jump forms (§3.4.5) and this chapter reports the others. The temporary top-level owner bridge (IR-511–IR-515) currently reports the listed syntax restrictions for direct functions, invariants, refined types, and refinement-bearing distinct types; type-checker integration must coordinate with that bridge to avoid duplicate diagnostics. It does not yet resolve predicate names and types. Each predicate is checked in the scope of its declaration (N-C-43): `requires` and `ensures` clauses in their function's owner, after the body, in declaration order; refinement predicates and invariants in their own owners (§3.7.1).

- **`requires`** sees the parameters at their declared types. **`ensures`** sees the parameters and `ContractResult`, whose type is the declared return type (for an `async fn` the declared output, spec/09 §9.3.2). A function with an `ensures` clause and a parameter named `result` is `RESULT_NAME_CONFLICT`, on the parameter. `RESULT_OUTSIDE_ENSURES` and `OLD_OUTSIDE_ENSURES` were reported by the resolver.
- **`ContractOld { args, base }`**: validate that there is exactly one unnamed positional argument and no `base`, as required by spec/09 §9.3.2; preserving malformed call shapes in HIR lets this stage diagnose them without resolving `old` as a user function. For the valid shape, a nested `ContractOld` is `NESTED_OLD`; an operand that mentions `ContractResult` is `OLD_REFERS_TO_RESULT`; the operand type must be `Copy` (`Goal::Copy(T, CopyOrigin::Old)`, failure `OLD_REQUIRES_COPY` with the fix "copy the needed scalar"). The free locals of the operand must be parameters; the resolver's `ContractScope` rib already restricts them.
- **Contract ids** (spec/09 §9.1.3) are assigned here and stored in `contract_clauses(def) -> Arc<[ContractClause]>` with `{ id: ContractId { item: DefId, kind, index }, hir: HirId, mentions_old: Vec<HirId>, mentions_result: bool }`.
- **Trait methods.** A `requires` clause on a method of a trait impl is `CONTRACT_REQUIRES_ON_IMPL` (reported by `wf_check_item`). The `ensures` of the trait method apply to every impl and are instantiated with the impl's arguments by [impl/04](04-mir.md) (spec/09 §9.5.1).
- **Invariants.** `wf_check_item` on an `invariant T` reports `INVARIANT_ON_INVALID_TYPE` (refined type, alias, primitive, interface type), `INVARIANT_ON_GENERIC_TYPE`, and `DUPLICATE_INVARIANT` on the second declaration for the same type in source order (the first is kept). `INVARIANT_FIELD_WRITE` needs the place and the borrow kind and is reported by [impl/05](05-borrow-checker.md) from MIR, using `invariant_of(adt)` from this chapter.
- Effects of predicates (`EFFECTFUL_PREDICATE`) belong to [impl/08](08-effect-and-capability-analysis.md).

### 3.7.15 Constant evaluation

`const_value(def) -> Result<ConstValue, ErrorGuaranteed>` evaluates a `const` initializer or an explicit discriminant on its typed HIR (no MIR is built for consts; N-E-33). The checker first walks the initializer and reports every form outside spec/06 §6.27 as `NOT_CONST_EXPRESSION` (once per offending node, outermost first; `return` is `RETURN_OUTSIDE_FUNCTION`, §3.4.5). The interpreter then evaluates the allowed forms with the semantics of spec/06 §6.8 on exact `i128`/`u128` values checked against the type, IEEE `f32`/`f64` for floats, and structured values for constructions:

```rust
pub enum ConstValue { Int(i128 /* or u128 bits */, IntTy), Float(u64 /* bits */, FloatTy), Bool(bool), Char(char), Byte(u8),
                      Str(Symbol), ByteStr(Arc<[u8]>), Aggregate { variant: Option<VariantIdx>, fields: Arc<[ConstValue]> }, Array(Arc<[ConstValue]>) }
```

- A built-in check that would panic is `CONST_EVAL_PANIC`, naming the panic kind and the operation's span.
- A use of another const calls `const_value` of it. The query keeps a stack of consts under evaluation in the session; re-entering a const on the stack is `CONST_CYCLE`, reported once on the first const of the cycle in `DefId` order, with the cycle listed (N-E-25: the cycle is detected by the explicit stack, never by the query engine).
- B→R conversions and invariant constructions inside the initializer are SFC obligations proven on the typed HIR by the same procedure ([impl/04](04-mir.md) §4.12.4 describes the shared interface); failures are `REFINEMENT_NOT_PROVEN` and `CONST_INVARIANT_NOT_PROVEN`.
- `ConstValue`s are the inputs of [impl/06](06-wasm-lowering-and-layout.md) data segments and of MIR `Const` operands; every use of a const is a copy of its value (spec/06 §6.27).

## 3.8 Traits, impls and the solver (Stage 2)

### 3.8.1 Impl headers and the impl index

```rust
pub struct ImplHeader {
    pub def_id: DefId,
    pub trait_ref: Option<TraitRef>,     // None for an inherent impl
    pub self_ty: Ty,                     // in terms of the impl's own parameters
    pub predicates: Arc<Predicates>,     // bounds and `where` of the impl, unelaborated
    pub origin: ImplOrigin,              // Written | Derived { attr: Span }
}
pub enum SimplifiedType { Int(IntTy), Uint(UintTy), Float(FloatTy), Bool, Byte, Char, Never, Tuple(usize), Array, Ref(Mutability), Adt(DefId), FnPtr(usize) }
pub struct TraitImpls { pub blanket: Vec<DefId>, pub by_head: BTreeMap<SimplifiedType, Vec<DefId>> }
```

`impl_header(impl)` lowers the header in a signature context. A projection in the self type or the trait arguments is `PROJECTION_IN_IMPL_HEADER` and makes the header `{error}` (the impl then takes part in nothing). `impls_of_trait(trait)` collects the impls of the trait in the current package and in every transitive dependency (from the dependency's metadata, [impl/01](01-architecture.md)), whether or not anything was imported (spec/05 §5.11.2 step 5), and the derived impls (§3.8.6). An impl whose self type has a head goes into `by_head` under the simplified head; one whose self type is a parameter goes into `blanket`. Each list is sorted by `DefPathHash`, so the candidate order never depends on the order in which packages were loaded. Compiler-provided impls are not stored: the solver produces them from the structure of the type (§3.8.5).

### 3.8.2 Impl checks

`wf_check_item(impl)` applies spec/05 §5.9.1 items 1 to 9 to a trait impl, in that order, and §5.9.2 to an inherent impl. Item 9 is `coherence` (§3.8.3).

- **Item checks.** `MISSING_TRAIT_ITEM` lists every missing item in the trait's declaration order. Its fix inserts, for each missing function, the trait signature with `Self` and the trait parameters substituted, with the body `{ panic("not implemented") }` (spec/16 `panic`), and for each missing associated type `type A = ()`. `UNKNOWN_TRAIT_ITEM` and `DUPLICATE_TRAIT_ITEM` are reported on the later item.
- **Signature comparison** (N-A-32). `compare_impl_fn(impl_fn, trait_fn)` builds `sig_T` by substituting `Self := self_ty`, the trait parameters `:= ā` and every associated type by the impl's definition, and maps the trait function's own generic parameters to the impl function's by position. It then compares, in this order, and reports `TRAIT_ITEM_SIGNATURE_MISMATCH` on the first difference: the number of own generic parameters, their kinds, the elaborated predicate sets (as sets of predicates after the positional renaming, compared with type equality and without their `caps`), the receiver form, the number of parameters, each parameter type left to right, the return type, `unsafe`, and `async`. Types are compared by spec/05 §5.14 after normalization in the impl function's `param_env`; no inference variable is involved. The caps of the impl function's own bounds are compared by [impl/08](08-effect-and-capability-analysis.md) §8.1.7 `effect_decl_check`, which reports `TRAIT_ITEM_SIGNATURE_MISMATCH` at an impl cap that the trait's bound does not imply ([spec/08](../spec/08-effects-and-capabilities.md) §8.8.3, N-D-27) and runs only when this comparison reported nothing for the item (impl/08 N-D-87).
- **Particular traits.** `MANUAL_COPY_IMPL` and `MANUAL_FN_IMPL` are reported on the trait path of a written impl of `Copy` or of a function trait. For `Drop`: `DROP_IMPL_INVALID_TYPE` (not a local record, enum, distinct type or resource), `DROP_IMPL_NOT_GENERIC_ENOUGH` (the self type's arguments are not the declaration's parameters in order and pairwise distinct, or the elaborated predicate set of the impl differs from the declaration's), and `COPY_AND_DROP` (spec/07) when the type has a `Copy` impl. `ASYNC_FN_IN_TRAIT` covers `async fn drop` (spec/11).
- **Inherent impls.** `INHERENT_IMPL_FOREIGN_TYPE` (with the fix "declare a trait, or wrap the type in a `distinct` type"), `ASSOC_TYPE_IN_INHERENT_IMPL`, `UNCONSTRAINED_IMPL_PARAM`. `DUPLICATE_INHERENT_ITEM` is reported by `inherent_impls` (§3.9.3), which sees all inherent impls of a type at once: on the later item in source order, with a note on the first.
- Effects of impl items (`TRAIT_IMPL_EFFECTS_EXCEED_BOUND`) are checked by [impl/08](08-effect-and-capability-analysis.md), `borrows` clauses by [impl/05](05-borrow-checker.md), contracts by §3.7.14.

### 3.8.3 Coherence

`coherence(trait)` runs for every trait that has at least one impl in the current package (written or derived).

**Orphan check** (spec/05 §5.10.1, N-A-35). For each impl of the current package, `owner(self_ty)` strips `Ref` and `Box` from the outside, repeatedly, and maps the head by the table of §5.10.1; `sai:core` and `sai:alloc` are one package for this purpose. The impl is accepted iff the trait is declared in the current package or the owner is the current package; otherwise `ORPHAN_IMPL` on the self type, with both conditions in the message.

**Overlap check** (spec/05 §5.10.2). For each impl `I` of the current package in source order, and each other impl `J` of the same trait that precedes `I` in this order (impls of dependencies first, sorted by `DefPathHash`, then the current package's impls in source order, then derived impls in the order of their `@derive` attributes), `overlap(I, J)` is computed in a fresh `InferCtxt` in **coherence mode**: both headers are instantiated with fresh variables, then the trait arguments and the self types are unified, ignoring regions and bounds. Every pair that unifies is `OVERLAPPING_IMPLS`, reported on `I` with a related location on `J`. Candidates are pruned with `by_head`: `J` is compared only if its simplified head equals `I`'s or one of them is blanket.

The compiler-provided impls take part as **pseudo-headers**, one per row of spec/05 §5.11.4 that can be written in an impl header: primitives and `()` for `Copy` and `Clone`; `Never`, tuples of every arity that occurs in a user header, `Array<T, N>`, `borrow T`, `borrow mut T`, `Str`, `Slice<T>` and function pointer types for their traits; and, for every refined type `R` of the current package, `R` for `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` and `Format`. Because overlap ignores bounds, a written `impl PartialEq for R` overlaps the pseudo-header whether or not the base implements `PartialEq`, as spec/05 §5.6.4 states. A pseudo-header is reported as "the compiler-provided impl of spec/05 §5.11.4".

### 3.8.4 The solver

The solver implements spec/05 §5.11.2 exactly (N-A-36). Its entry points are `select(obligation)` inside a `FnCtxt` or a WF context, and the query `evaluate_goal` for goals without inference variables.

```rust
pub enum SelectionResult { Selected(ImplSource), Ambiguous { stalled_on: Vec<TyOrConstVid> }, Failed(SelectionError) }
pub enum ImplSource {
    Param(PredicateIdx),                        // an environment predicate of the ParamEnv
    Impl { impl_def: DefId, args: GenericArgs },
    Builtin(BuiltinImpl),                       // §3.8.5
}
pub enum SelectionError { Unsatisfied, Overflow }
pub enum PredicateIdx { Listed(u32), AssocBound { base: u32, index: u32 } }
pub enum EvalResult { Selected(ImplSource), Ambiguous, Failed(SelectionError) }   // the value of `evaluate_goal`
```

`EvalResult` is the selection of the root goal of `evaluate_goal`; the `args` of a `Selected(Impl { .. })` are expressed in the canonical variables of the goal, which the caller instantiates. A `PredicateIdx` names an environment candidate of step 4: `Listed(i)` is the `i`-th predicate of the elaborated list of the `ParamEnv` (§3.14.1), and `AssocBound { base, index }` is the `index`-th candidate of the bounds of an associated type (§3.14.1 step 5: the declared bounds, then their supertraits, in order), brought by the listed predicate `base`. `param_env_predicate(env, idx)` returns the predicate of either form with its `caps` (§3.6.2); for `AssocBound`, they are the caps of the declared bound followed by those of `base` (spec/08 N-D-25). [impl/08](08-effect-and-capability-analysis.md) §8.3.4 reads the selected source of a trait-method call and of a latent key from `EvalResult`.

1. `depth > 128` is `TRAIT_SOLVER_OVERFLOW`, reported once per root obligation on its cause span; the obligation fails.
2. The goal is shallow-resolved and normalized (§3.5.5).
3. A goal equal to one on the solving stack fails (induction).
4. **Environment candidates**: the elaborated predicates of the `ParamEnv` (§3.14.1) whose trait is the goal's and whose arguments unify with the goal's in a probe. One candidate, or several that are identical after resolution, is selected and its unification applied; several different ones make the goal ambiguous. Environment candidates are tried first and, if any exists, impl candidates are not considered.
5. **Impl candidates**: the impls of `impls_of_trait` under the goal's simplified head plus the blanket impls (all impls when the head is an unresolved variable), and the compiler-provided candidates of §3.8.5. Each is probed by instantiating the header with fresh variables and unifying it with the goal. Exactly one: it is selected, the unification is applied, and its predicates are registered as nested obligations at `depth + 1` (in a body, they join the fulfillment context; in `evaluate_goal`, they are solved recursively). Two or more: ambiguous. None: failed.
6. A failure reported to the user is `UNSATISFIED_BOUND` (or the cause-specific code of §3.7.4) with the origin, and the fix "add `@derive(Tr)`" when the type is a nominal type of the current package and `Tr` is derivable for its kind.

A goal whose self type is an unresolved `α` with no environment candidate is ambiguous without probing any impl. A goal whose self type is an unresolved `ι` probes only the impls whose head is an integer type, and is ambiguous when two of them match; the end-of-body defaulting resolves it.

**Projection goals** `<τ as Tr<σ̄>>.A = υ` select `τ: Tr<σ̄>` and then: for an impl, unify `υ` with the impl's definition of `A`, substituted; for an environment predicate, use an equality predicate of the `ParamEnv` for this projection if there is one, otherwise the projection is rigid and `υ` is unified with the rigid projection; for a compiler-provided impl, the built-in associated type (§3.8.5). Ambiguous selection leaves the goal pending.

**`Goal::Copy(T, origin)`** is the trait goal `T: Copy` with an origin-specific report (§3.7.4). **`Goal::WellFormed(T)`** expands into the obligations of spec/05 §5.17 for `T`. **`Goal::ClosureKind(c, k)`** is ambiguous until capture analysis has recorded the kind of `c` (N-E-30), then holds iff the recorded kind is at least as capable as `k`: a closure recorded as `Fn` satisfies every kind, one recorded as `FnMut` satisfies `FnMut` and `FnOnce`, and one recorded as `FnOnce` only `FnOnce` (spec/06 §6.17.4).

**Caching** (N-E-35). `evaluate_goal(Canonical<(ParamEnv, Goal)>)` is a query, so its results are shared across bodies and incremental sessions ([impl/01](01-architecture.md)). Only goals that contain no inference variables are canonicalized and cached. A result that depended on the inductive failure of a goal that is still on the stack (step 3) is not cached, except at the root of that cycle, where it is final. Overflow results are never cached. Selection inside a body caches nothing beyond the fulfillment context's own bookkeeping, so no result depends on the order in which bodies are checked.

### 3.8.5 Compiler-provided impls

`builtin_candidates(trait, self_ty)` returns the impls of spec/05 §5.11.4 for the lang-item trait and the structural type, each with its nested obligations:

| Self type | Traits and nested obligations |
|---|---|
| primitives, `()` | `Copy`, `Clone`; nothing nested |
| `Never` | `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` |
| `(τ1, …, τn)`, `n ≥ 1` | `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Default`, `Arbitrary`, each with `τi: Tr` for every `i` |
| `Array<τ, N>` | the same traits, with `τ: Tr` |
| `borrow τ` | `Copy`, `Clone`; `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` with `τ: Tr` |
| `borrow mut τ` | `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` with `τ: Tr` |
| `Str`, `Slice<τ>` | `Copy`, `Clone` |
| `FnDef(f, ḡ)` | `Copy`, `Clone`; for a safe item, `Fn`, `FnMut`, `FnOnce` with the argument tuple and `Output` of `fn_sig(f)` instantiated (for an `async fn`, `Output` is its `Future` type, spec/11 §11.2.2); none of the function traits for an `unsafe` item |
| `FnPtr` | `Copy`, `Clone`; `Fn`, `FnMut`, `FnOnce` when safe |
| `Closure(c, ḡ)` | `FnOnce` always; `FnMut` and `Fn` through `Goal::ClosureKind(c, k)`; `Output` is the closure's return type. `Copy` and `Clone` with `υ: Tr` for the capture tuple `υ` (N-E-34) |
| `Future(d, ḡ)` | `Future` with `Output` the async output type (spec/11 §11.3.2); never `Copy` or `Clone` |
| refined `R` with base `B` | `Copy` and `Clone` with `B: Copy`; `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Format` with `B: Tr` |
| a flags type `F` ([16](16-interfaces-and-wit.md) §16.9.4) | `Copy`, `Clone`, `PartialEq`, `Eq`, `Hash`, and `BitOr`, `BitAnd`, `BitXor` with `Rhs = F` and `Output = F`; nothing nested |
| a view enum without payloads ([16](16-interfaces-and-wit.md) §16.4.6) | `Copy`, `Clone`, `PartialEq`, `Eq`, `Hash`; nothing nested |
| a view record or variant ([16](16-interfaces-and-wit.md) §16.4.6) | `Copy`, `Clone`, `PartialEq`, `Eq`, each with `F: Tr` for every field and payload type `F` |

Any other impl, including `Box<F>: Future` and `Box<S>: Stream`, comes from a declaration in the library (spec/16) and is found as an ordinary impl candidate. A compiler-provided impl and a written impl can never both match a goal, because the overlap check of §3.8.3 rejects the written one.

**Closures and `Copy` (N-E-34).** A closure is `Copy` or `Clone` when every capture is (spec/05 §5.5.3, N-A-31). The capture types of §3.11.2 make this the goal `υ: Tr` on the capture tuple: a shared capture is `borrow τ`, which is `Copy`, a `mut` capture is `borrow mut τ`, which is neither, and a by-value capture has its own type. While capture analysis has not fixed `υ`, the goal is ambiguous and stalls like `Goal::ClosureKind` (§3.8.4). An anonymous future is never `Copy` or `Clone` (spec/11 §11.2.1, §11.2.3). `Clone.clone` of a closure resolves to the `StructuralClone` shim of [impl/04](04-mir.md) §4.3.3, which clones the captures in capture order.

### 3.8.6 Derived impls

For each `DerivedImpl` owner ([impl/02](02-frontend-ast-hir.md) §2.9.17), `impl_header` builds the generated header of spec/05 §5.12.1 (`origin: Derived`): `impl<Ḡ> Tr for N<Ḡ> where W, T1: Tr, …, Tk: Tr` for records and enums, `where W, B: Tr` for distinct types, `B: Tr<B, Output = B>` (or `B: Tr<Output = B>`) for the derived operator traits, and `B: Arbitrary` with no parameters for a refined type. `wf_check_item(derived)` then checks, in this order:

1. `DERIVE_NOT_SUPPORTED` by the table of spec/05 §5.12.4, on the trait name in the attribute; for a "no (provided)" entry, the note that the refined type already has the impl and the fix "remove it from the derive list".
2. `MISSING_SUPERTRAIT_IMPL` for each supertrait not implemented for the type (by a derive or an impl), with the fix "add `Tr` to the derive list".
3. The field check: for each field and payload type `F`, in declaration order and for variants in discriminant order, the obligation `F: Tr` under the generated predicates; a failure is `DERIVE_FIELD_MISSING_IMPL` on the attribute, with a note on the field. For a distinct type with a refinement clause, `Clone` requires `B: Copy` (the same code, with the note of §5.12.4). For distinct and refined types, an unsatisfiable `B: Tr` with a concrete `B` is reported the same way.

A derived impl is an ordinary impl for the solver and for coherence; its items have exactly the trait's signatures, so no signature comparison is done. The bodies are MIR shims built by [impl/04](04-mir.md) §4.3.3.

### 3.8.7 Instance resolution and drop queries

`resolve_instance(def, args) -> Option<Instance>` gives [impl/04](04-mir.md) the code to call for an item applied to **fully concrete** arguments; it returns `None` when `args` still mention parameters. It solves in the empty `ParamEnv` with all types revealed:

1. A function that is not a trait item resolves to `Instance::item(def, args)`.
2. For a trait method `Tr.m` with `Self` and the trait arguments taken from `args`, `Self: Tr<ā>` is selected. An `Impl` source gives the impl's item `m` with the impl's arguments followed by the method's own arguments, or, when the impl does not define `m`, the trait's default body with `args` unchanged. A `Builtin` source gives one of the shim instances of [impl/04](04-mir.md) §4.3.3 (structural `Clone` for tuples, arrays and closures, comparison and hashing for tuples and arrays, refined-type forwarding, function-trait calls on function items, function pointers and closures, `Future.poll` of an anonymous future, the operations of flags types and the derived traits of SAI views of [16](16-interfaces-and-wit.md) §16.9.4 and §16.4.6). The provided functions of a flags type (`empty`, `all`, `contains`, `is_empty`) resolve to `Shim(Flags { .. })` in item 1. A derived impl gives the derived shim for `m`.
3. Selection failure at this point is an internal compiler error: every obligation was proven when the generic body was checked, and the arguments satisfy the item's predicates.

`needs_drop(param_env, ty) -> NeedsDrop { No, Yes, Maybe }` implements spec/05 §5.18 rule 5: `Yes` for `String`, `Vec`, `Box`, resources, types with a `Drop` impl, closures and futures with a capture or a live slot of a `Yes` type, and aggregates with a `Yes` component; `Maybe` for a type that mentions a parameter or projection whose instantiation could have drop code; `No` otherwise. `has_drop_impl(adt)` and `drop_impl(adt) -> Option<DefId>` come from `impls_of_trait(Drop)`. [impl/04](04-mir.md) schedules drops for `Yes` and `Maybe`, and monomorphization turns every `Maybe` into `Yes` or `No` ([impl/04](04-mir.md) §4.13).

## 3.9 Method resolution

### 3.9.1 Entry

`r.m(args)` is `Call { callee: Field { base: r, name: m, args: generic_args }, args }` ([impl/02](02-frontend-ast-hir.md)); it is always a method call, never a call of a field (spec/06 N-C-10). `check_method_call` synthesizes `r` in a place context, calls `structurally_resolve` on its type (an `ι` or `φ` receiver is defaulted here, an `α` receiver is `TYPE_ANNOTATIONS_NEEDED`; spec/05 §5.13.1), runs the probe of §3.9.4, and then checks the call as in §3.7.12 with the selected signature: the receiver is not a coercion site, the other arguments are, closure arguments are deferred (N-A-41), and explicit generic arguments instantiate only the method's own parameters, all or none (`WRONG_NUMBER_OF_GENERIC_ARGS`).

### 3.9.2 Traits in scope

`traits_in_scope(M)` is the sorted list of traits bound in the scope table of `M` (by declaration or by `use`, under any name) followed by the prelude traits that are not already in it. Each entry remembers the import binding that put it there, if any. For a particular call, the traits of the elaborated `ParamEnv` predicates whose subject is a step of the receiver that is a type parameter, `Self` or a rigid projection are added (spec/05 §5.13.3 rule 3). When a trait found through an import is selected, the import is recorded in `TypeckResults::used_trait_imports`, so that `UNUSED_IMPORT` (spec/04 §4.3.6, spec/15) is not reported for an import that is used only as a trait in scope. The compiler-internal function-trait methods (N-E-26) are never method candidates.

### 3.9.3 Inherent impls

`inherent_impls(SimplifiedType) -> Arc<InherentItems>` collects the inherent impls of a head from the package that may declare them: the package of the declaration for `Adt`, and `sai:core` together with `sai:alloc` for primitives, `Array`, tuples, views and the other built-in heads (spec/05 §5.9.2 permits only these). `InherentItems` maps each item name to its `DefId` and impl; two items with the same name across the impls of one head are `DUPLICATE_INHERENT_ITEM` (§3.8.2), and the first in source order is kept, so lookup stays deterministic after the error. Inherent impls are found regardless of imports. Two kinds of heads also have items without an impl declaration, recorded with the type's own `DefId` in the impl position: the members of a view resource ([16](16-interfaces-and-wit.md) §16.4.4), and the provided functions `empty`, `all`, `contains` and `is_empty` of a flags type, which come before the written items, so a written item of the same name is the duplicate ([16](16-interfaces-and-wit.md) §16.9.4).

### 3.9.4 The probe

The probe implements spec/05 §5.13.2 to §5.13.5 (N-A-37):

1. **Steps.** `τ0` is the receiver type; `τ(i+1)` is the referent of a `Ref` or the argument of `Box`, each step `structurally_resolve`d and normalized. A step that is an unresolved `α` is `TYPE_ANNOTATIONS_NEEDED`. If the last step is a refined type, its base is appended as the **base step**.
2. **Adjustments.** For each step, in order: by value, `borrow`, `borrow mut`; at the base step only by value and `borrow`.
3. **Candidates at (step, adjustment).** With `A` the adjusted type: inherent candidates are the methods named `m` of `inherent_impls(head(τi))` whose receiver, with fresh impl arguments, unifies with `A` in a probe. Trait candidates are the methods named `m` of the traits in scope whose receiver, with `Self := σ` fresh, unifies with `A`, and for which the goal `σ: Tr<fresh>` is not `Failed` (an ambiguous goal is kept as an obligation of the pick).
4. **Selection.** An inherent candidate is selected first. Otherwise, the trait candidates of exactly one trait are selected. Trait candidates of two or more traits are `AMBIGUOUS_METHOD`, listing the traits by canonical path, with the fix `Tr.m(r, ..)` for each. Otherwise the probe moves to the next adjustment, then to the next step.
5. **Refined base step.** If no candidate was found before the base step and a method of the base `B` with receiver `borrow mut Self` would match `borrow mut B`, the error is `REFINED_PLACE_MUTATION` with the fix of spec/09 §9.2.2 item 4.
6. **Visibility.** If the selected inherent method is not visible from `M`, the error is `PRIVATE_ITEM` and the probe does not continue (spec/05 §5.13.5). A selected `Drop.drop` is `EXPLICIT_DROP_CALL`.

The pick is recorded as:

```rust
pub struct MethodPick { pub item: DefId, pub source: PickSource /* Inherent(impl) | Trait(trait) */, pub autoderefs: usize,
                        pub autoref: Option<Mutability>, pub base_step: bool }
```

The receiver's adjustments (§3.10.2) are `autoderefs` times `Deref` (with `DerefKind::Box` for a `Box` step), then `RefinedToBase` at the base step, then `Borrow(m)` for an auto-reference, the last two combining into `RefinedBorrowToBase` for the `borrow` adjustment at the base step. A receiver without an auto-reference whose adjusted type is `borrow mut T` gets `Reborrow` as its last adjustment, so it is reborrowed and not moved (spec/07 §7.5.3 rule 2, last item). [impl/04](04-mir.md) §4.5.2 lowers both a `borrow mut` auto-reference and this `Reborrow` as two-phase borrows (spec/07 §7.10 case 1). A `borrow mut` auto-reference puts the receiver in a `MutPlace` context, which triggers the `IndexMut` fix-up of §3.7.12 and the `mut` capture mode of §3.11.2. A by-value adjustment at a step reached through a dereference moves out of the referent unless the step type is `Copy`; that move is checked by [impl/05](05-borrow-checker.md) (`MOVE_OUT_OF_BORROW`, spec/07). `TypeckResults::type_dependent_defs[call]` gets the item and its instantiated arguments; an `unsafe` method is recorded for [impl/08](08-effect-and-capability-analysis.md).

**Failure** (spec/05 §5.13.5). When nothing matches, the probe is rerun with every trait of the package graph that has an item `m` (the index `traits_with_item(name)`, built per package from `trait_def`s and dependency metadata) instead of the traits in scope, considering only traits nameable from `M`. If some match, the error is `TRAIT_NOT_IN_SCOPE`, listing them by canonical path, with the fix `use <path>.{Tr}`, machine-applicable when exactly one trait matched. Otherwise it is `METHOD_NOT_FOUND`, naming the receiver type and `m`, with the additions of §5.13.5 in this order: the fix "write `(r.m)(..)`" when a step has a visible field `m` whose type is callable (§3.7.12 `callable`), the note "call it as `T.m(..)`" when an associated function `m` without `self` exists on a step, and the note on distinct types when a step is a distinct type whose base has a method `m`.

### 3.9.5 Type-relative paths

A value path whose prefix resolves to a type, `Self`, a type parameter, a projection or a trait (§3.4.2 item 4) is resolved here, segment by segment, by spec/05 §5.13.6:

1. A UIDENT segment on an enum is the variant (`UNKNOWN_VARIANT` with "did you mean" suggestions from the variant names); on any other type it is `UNKNOWN_VARIANT` with the note "`P` is not an enum". On a flags type it is the flag constant `TypeDependentDef::FlagsConst`, or `UNKNOWN_VARIANT` with suggestions from the flag names ([16](16-interfaces-and-wit.md) §16.9.3).
2. A LIDENT segment searches `inherent_impls(head(P))` for methods and associated functions alike; then the traits in scope (with rule 3 for `P` itself) for an item whose trait `P: Tr` does not fail; one trait selects it, two or more are `AMBIGUOUS_METHOD`. `R.try_from` on a refined type or a distinct type with a refinement clause is found at the inherent step, as the compiler-provided function `fn(B) -> Option<R>` (spec/09 N-C-41), recorded as `TypeDependentDef::RefinedTryFrom(R)`.
3. `Tr.name` names the trait item; `Self` is a fresh `σ` with the obligation `σ: Tr<fresh>`, inferred from the arguments. A path `T.name` with `T` a type parameter searches only the traits of `T`'s bounds.
4. Nothing found is `METHOD_NOT_FOUND` with the additions of §3.9.4. A `Drop.drop` path is `EXPLICIT_DROP_CALL`.

Path calls perform no auto-deref and no auto-reference; every argument, the receiver included, is an ordinary argument at a coercion site (spec/05 §5.13.6). The selected item and its arguments are recorded in `type_dependent_defs`. Further `Field` nodes after a resolved value segment are field accesses or method calls on that value.

## 3.10 Operators and coercions

### 3.10.1 Operators

Binary operators other than `and`, `or` and the comparisons follow spec/06 §6.8.2 (N-C-2):

1. `a` is synthesized and shallow-resolved to `A`. A refined `A` is erased to its base, with the adjustment `RefinedToBase` on `a` (spec/09 §9.2.2 item 3).
2. If `A` is a primitive numeric type, `Byte`, `Bool`, `ι` or `φ`, then `b` is checked against `A`, or against `U32` for `<<` and `>>`; a refined `b` whose base equals the target is erased in the same way. The operation is **built-in** iff the tables of spec/06 §6.8.3 to §6.8.5 define it for `A` (an `ι` qualifies for every integer operation, a `φ` for every float operation, and the choice is final once the variable is resolved). Otherwise it is `OPERATOR_NOT_SUPPORTED`, with the fixes of the spec (`%` on floats, `Bool` bit operators, `-` on unsigned types).
3. Otherwise `b` is synthesized to `B` and the obligation `A: Tr<B>` for the lang-item trait of the operator is registered with cause `Operator` (`OPERATOR_NOT_SUPPORTED` names the trait and both types, with the string-concatenation fixes of spec/06 §6.8.9 for `Str + String` and `String + String`). The result type is the normalized `<A as Tr<B>>.Output`.
4. `+%`, `-%` and `*%` are built-in on the primitive integer types only (spec/06 §6.8.4); everything else, including `ι` defaulted later to a non-integer, is `OPERATOR_NOT_SUPPORTED`. An unresolved `ι` is accepted and stays integer.

Unary `-` and `~` use the same scheme with `Neg` and `BitNot`; `not` checks against `Bool`; `*` is §3.7.12. A `-` directly before a literal was folded by [impl/02](02-frontend-ast-hir.md).

**Comparisons** (spec/06 §6.8.7): `a` is synthesized to `A` (erased if refined), and `b` is checked against `A` (so `s == .Active` and `x < 10` work); built-in for the primitive types, `Str` and `()`; otherwise `A: PartialEq` or `A: PartialOrd` with cause `Operator`. The operands are in `ImmPlace` contexts and are passed as implicit shared borrows. **Logical operators** check both operands against `Bool`.

**Compound assignment** `p op= e` is typed as `p' op t` followed by `p' = …` (spec/06 §6.13.2; there are no compound-assignment traits): built-in when `p`'s type is primitive and the operation is built-in for it (with `e` checked as the right operand of step 2); otherwise the obligation `A: Tr<B>` of the operator's trait with cause `Operator`, as in step 3. In both cases the result type must equal `A` (`TYPE_MISMATCH` on the operator otherwise), and `p` is in a `MutPlace` context. A non-`Copy` `A` is read by take-and-restore ([impl/04](04-mir.md) §4.8.3).

```rust
pub enum OpRes {
    Builtin(BuiltinBinOp /* Add Sub Mul Div Rem BitAnd BitOr BitXor Shl Shr WrapAdd WrapSub WrapMul Eq Ne Lt Le Gt Ge */),
    Overloaded { method: DefId, args: GenericArgs },   // e.g. Add.add with [A, B]; comparisons pass borrows
}
```

`TypeckResults::op_res[e]` records it. This chapter does not decide whether a built-in arithmetic operation is checked for overflow; [impl/04](04-mir.md) §4.9 specifies the MIR checks required for each built-in operation by type.

The **IR-765 flat-local addition slice** is narrower than general operator checking: it accepts one direct `+` in an immutable single-name local initializer only when both operands are plain paths resolved to parameters or preceding locals by binding identity. Both resolved operand types must be the same primitive integer type or the same `F32`/`F64` type. A right-operand mismatch points to the right path and recovers with the left type; a bad annotation points to the whole initializer and leaves the local at its declared type. At IR-765, literal operands, nested expressions, nonnumeric primitives, custom aliases, and other forms were gated; IR-767/768 later added one direct numeric literal on the right for `+` and for `-`/`*`. Nested expressions, nonnumeric primitives, custom aliases, and other unsupported forms remain gated.

**IR-766 flat-local subtraction and multiplication.** Extend the IR-765 initializer form to one direct `-` or `*` between two plain, one-segment, non-generic paths resolving by `BindingId` to eligible parameters or preceding locals. Accept only identical concrete primitive integer types or identical `F32`/`F64` types, with no widening; the result retains that operand type. If the right path has a different supported numeric type, report `TYPE_MISMATCH` on that path and recover the expression to the left type. An omitted or matching annotation is accepted; a different supported primitive annotation reports on the full initializer and the local recovers to its declared type. Preserve the original HIR operator and expression. At the IR-766 step, right numeric literals remained gated; IR-768 subsequently added direct right-hand literals for `-` and `*`. Unresolved numeric locals, other operators, `Byte`/`Char`/`Bool`, custom aliases, nonlocal or qualified paths, nested/computed expressions, mutable or destructured locals, and self/forward references remain gated. Arithmetic evaluation and overflow checking are not implemented in this type-checking slice; they remain future work (spec/06 §§6.8.2–6.8.3).

**IR-767 right-literal addition.** The direct `+` initializer may pair one eligible concrete numeric path on the left with one direct integer or floating-point literal on the right. The literal is checked against the path's type using the primitive literal checker, preserving literal-span type/range diagnostics and target pointer width; the expression recovers to the left type. Literal-left arithmetic was deferred at the IR-767 step and was later added by IR-770. Under spec/06 §6.8.2, the left literal synthesizes `ι`/`φ` and the right path is checked against that variable (§5.16); IR-770 carries that variable through arithmetic path checking and family-mismatch recovery. Integer-to-float literal checking remains a type mismatch under §3.7.6. Fixed literals, nonnumeric path types, unresolved literal-variable locals, nested forms, and all other shapes remain gated. Right-literal `-` and `*` were subsequently added by IR-768.

**IR-768 right-literal subtraction and multiplication.** The direct `-` and `*` initializers also accept one eligible concrete numeric path on the left and one direct integer or floating-point literal on the right. Use the path type as the literal expectation, preserving literal-span type/range diagnostics, target pointer-width checks, and the left-type result after operand errors. A different supported primitive annotation reports on the whole initializer and recovers the local to its declared type. HIR folds a unary `-` directly preceding an integer token into one `Lit::Int { negated: true }` expression whose span includes the sign and digits; the existing range checker accepts `-128` for `I8`, rejects `-129`, accepts `-0` for `U8`, and rejects `-1` for `U8`. A direct unary minus before a float is likewise folded into `Lit::Float { negated: true }`, so negative decimal float literals use the existing F32/F64 parser and range check as direct literals. Literal-left inference was outside the IR-768 step and was subsequently added by IR-770. Unresolved numeric locals, fixed-kind literals, `/`, `%`, wrapping operators, custom aliases, nested/computed operands, and other unsupported forms remain gated. Preserve the operator expression; do not evaluate arithmetic or add overflow checks. Integer-to-float checking remains a type mismatch under §3.7.6 (spec/02 §2.6.1; spec/06 §§6.3.1, 6.8.2–6.8.3, 6.11.1, and 6.12.1).

**IR-770 literal-left arithmetic.** In an immutable single-name local initializer, accept one direct integer or floating-point literal on the left of `+`, `-`, or `*` and one plain path on the right to an eligible parameter or preceding local with a concrete supported numeric type. Resolve the path by `BindingId`. An integer literal's `ι` variable may unify with any integer primitive; a float literal's `φ` variable may unify with `F32` or `F64`. On a family match, check the literal at its complete HIR span (including a directly folded negative sign), use the selected target's pointer width, and give the expression the path type. If the literal and path families differ, report `TYPE_MISMATCH` at the path using the literal family's default type (`I64` or `F64`), and retain the left literal variable unresolved as the expression and local type. A later matching local annotation or function tail may constrain that variable; a mismatched annotation reports the literal-kind error at the initializer and recovers the local to its annotation. Otherwise, body-end defaulting selects `I64`/`F64` and performs the range check at the literal. Fixed-kind literal operands, literal/literal, unresolved numeric locals used as arithmetic paths, other operators, aliases, nested/computed operands, arithmetic evaluation, and overflow checks remain gated (spec/05 §5.16.2; spec/06 §§6.3.1, 6.8.2–6.8.3, 6.11.1, and 6.12.1).

**IR-771 flat-local division.** Accept one direct `/` between eligible concrete numeric paths, a path and direct numeric literal on the right, or a direct numeric literal and path on the left. Resolve path operands by `BindingId`; concrete path types must match exactly with no widening. Reuse the arithmetic literal checker for family constraints, literal ranges, primary spans, and annotation recovery in either literal order. The supported operand families are the ten integer types and `F32`/`F64`. A zero integer divisor is accepted during type checking; runtime `DivByZero`, signed `MIN / -1` overflow, and arithmetic execution are not implemented by this slice. `%`, wrapping arithmetic, literal/literal, nested/computed operands, aliases, and overflow behavior remain gated (spec/06 §§6.8.2–6.8.3).

**IR-772 flat-local remainder.** Accept one direct `%` between matching concrete integer paths, an integer path and one direct integer literal on the right, or one direct integer literal and an integer path on the left. Resolve paths by `BindingId`; accept all ten integer primitives without widening. Reuse literal-family inference, range checks, target pointer width, diagnostic spans, and annotation recovery in either literal order. Integer `% 0` passes type checking; runtime `RemByZero`, signed `MIN % -1`, and remainder evaluation are not implemented here. For a supported flat shape whose left operand is `F32` or `F64`, check the right operand against that type, then report `OPERATOR_NOT_SUPPORTED` on the whole expression with help to call the float `rem` method. A float literal on the left also selects this unsupported operator family; while its width is unresolved, the operator diagnostic names floating-point values generically, and an incompatible right path retains the existing F64 default in its type-mismatch diagnostic. A supported right-operand mismatch can therefore produce both diagnostics. Literal/literal, unresolved numeric locals, Byte/Bool/Char operands, aliases, wrapping arithmetic, and nested/computed expressions remain gated.

**IR-773 flat-local wrapping operators.** Accept one direct `+%`, `-%`, or `*%` between plain paths to parameters or preceding locals, resolved by `BindingId`. Matching path types are accepted only for the ten integer primitives; no widening is performed. A different right path reports `TYPE_MISMATCH` at that path and recovers the expression to the left type. A different supported annotation reports at the full initializer and recovers the local to the annotation. Known non-integer primitive paths report `OPERATOR_NOT_SUPPORTED` at the full expression. Following spec/06 §6.8.2, non-integer primitive numeric, `Byte`, or `Bool` left operands also report any right-path mismatch; `Char` follows the unsupported-operator path without a synthetic mismatch. Integer wrapping is well-typed here only; MIR execution and modulo-2^w behavior are not checked. Literal operands, unresolved numeric locals, custom aliases, nested/computed forms, and all other unsupported body shapes remain gated.

**IR-774 right integer literal for flat-local wrapping operators.** The IR-773 form also accepts one direct integer literal on the right of `+%`, `-%`, or `*%`, when the left path resolves by `BindingId` to an integer primitive parameter or preceding local. Check the literal using the existing annotated-integer-literal path, including target pointer width and the full signed literal span; report type/range errors at the literal and recover the expression to the left type. A mismatched supported annotation reports at the full initializer and recovers the local to its annotation. The left operand must remain an integer path; a floating literal, a literal on the left, literal/literal, unresolved numeric local, custom alias, nested/computed form, or unsupported owner remains gated. These checks establish types only and do not verify modulo-2^w runtime results (spec/06 §§6.3.1, 6.8.2, 6.8.4).

**IR-775 left integer literal for flat-local wrapping operators.** The IR-773 form also accepts one direct integer literal on the left of `+%`, `-%`, or `*%` and one eligible primitive path on the right. If the path is an integer primitive, constrain the literal's `ι` to that exact type and apply target-width range checking at the literal's full source span. If the path has another supported primitive type, report `TYPE_MISMATCH` at the path with the integer-literal default `I64` as the expected type, and retain the unresolved left literal variable for later annotation, tail, or body-end defaulting; do not add `OPERATOR_NOT_SUPPORTED` for that right-family mismatch. An annotation or tail that constrains the variable to an integer type still checks range at the original literal; an incompatible float annotation/tail reports the existing integer-literal mismatch. Float literals, literal/literal, unresolved numeric locals, custom aliases, nested/computed forms, and unsupported owners remain gated. These checks establish types only and do not verify modulo-2^w runtime results (spec/05 §§5.16.2, 5.16.5; spec/06 §§6.3.1, 6.8.2, 6.8.4).

**IR-776 flat-local bitwise path pairs.** Accept one direct `&`, `|`, or `^` initializer whose operands are plain paths resolving by `BindingId` to eligible parameters or preceding locals with concrete supported primitive types. Matching integer and `Byte` pairs are accepted, with no widening. When the left type is an integer or `Byte`, a different supported primitive on the right reports `TYPE_MISMATCH` at the right path and the expression recovers to the left type. With `Bool`, `F32`, or `F64` on the left, a differing supported right type reports that mismatch first, then `OPERATOR_NOT_SUPPORTED` on the whole expression; matching operands report only `OPERATOR_NOT_SUPPORTED`. A `Char` left operand follows spec/06 §6.8.2 rule 3: report `OPERATOR_NOT_SUPPORTED` naming the operator trait and both operand types, without a synthetic mismatch, even when the types differ. Annotation checking follows the recovered expression type, and the original operator is preserved. Logical `and`/`or` checking is unchanged. Literals (including unresolved numeric locals), `~`, shifts, aliases, self/forward references, computed/nested operands, and unsupported owners remain gated. This slice performs type checking only; it does not implement bitwise lowering or runtime execution (spec/06 §§6.8.2, 6.8.5, 6.11.1, and 6.12.1).

**IR-777 direct right integer literal for flat-local bitwise operators.** Extend IR-776's direct bitwise initializer to accept an eligible concrete primitive path on the left and one direct integer literal on the right for `&`, `|`, and `^`. The left path must resolve by `BindingId` to a parameter or preceding local. For any of the ten integer primitives, check the literal against the exact left type with the existing annotated-literal checker, including signed-span parsing and target pointer width; after a literal type or range error, the expression still recovers to the left type. A `Byte` left path is recognized but an integer literal is not a `Byte` literal, so report `TYPE_MISMATCH` at the literal and recover to `Byte`. `Bool`, `F32`, and `F64` left paths likewise check the integer literal against the left type, then report `OPERATOR_NOT_SUPPORTED` on the full expression. The body checker records the operand mismatch before the unsupported-operator error; CLI diagnostics are sorted by source span, so the full-expression diagnostic can be printed before the nested literal diagnostic. Keep `Char` with a direct integer literal gated until operator-trait inference can select its right operand. A literal on the left, literal/literal, non-integer literal, unresolved numeric local, alias, self/forward reference, nested/computed operand, `~`, shifts, and unsupported body owner remain gated. This is type checking only; no bitwise lowering or runtime execution is added (spec/06 §§6.3.1, 6.8.2, 6.8.5, 6.11.1, and 6.12.1).

### 3.10.2 Coercions and adjustments

```rust
pub enum Adjust {
    NeverToAny,                         // C-Never
    Deref(DerefKind),                   // Builtin (borrow, borrow mut) | Box: auto-deref of fields and receivers
    Borrow(Mutability),                 // receiver auto-reference
    Reborrow,                           // C-Reborrow: borrow mut *x instead of moving x
    MutToShared,                        // C-MutToShared
    SliceMutToSlice,                    // C-SliceMut
    ReifyFnPointer,                     // C-FnPtr
    RefinedToBase,                      // C-RefinedToBase and refinement erasure
    RefinedBorrowToBase,                // C-RefinedBorrow
    BaseToRefined(RefinementObligationId),  // C-BaseToRefined (§3.13.1)
}
pub struct Adjustment { pub kind: Adjust, pub target: Ty }
```

`TypeckResults::adjustments[expr]` holds the list applied to an expression, in order. `coerce(found, expected, site)` is spec/05 §5.15.3 (N-A-40):

1. Both sides are shallow-resolved. `Never` found: accept with `NeverToAny` (the expected side is not constrained).
2. If either side is an inference variable, unify them; no coercion.
3. Unify in a snapshot; on success commit. If both sides are `borrow mut` and the expression is a place, record `Reborrow`, so that passing a mutable borrow to a parameter of type `borrow mut T` reborrows rather than moves it (C-Reborrow; spec/07 decides the region).
4. Otherwise roll back and choose the rule by the heads: `borrow mut τ` to `borrow υ` (`MutToShared`, then unify `τ`, `υ`); `SliceMut` to `Slice` (`SliceMutToSlice`); `FnDef` to `FnPtr` (`ReifyFnPointer`, after the checks of spec/05 §5.15.2: not `async`, `Self` resolved, safety compatible, equal parameter and return types after instantiation; effects are compared by [impl/08](08-effect-and-capability-analysis.md) from Stage 3); `borrow R` to `borrow` of a non-`R` type (`RefinedBorrowToBase`); `R` to a non-`R` type (`RefinedToBase`); a non-`R` type to `R` (`BaseToRefined`, recording a refinement obligation, §3.13.1).
5. If no rule applies or the inner unification fails, `TYPE_MISMATCH` with the explicit-conversion fixes of spec/05 §5.15.4 chosen from the two heads, and `MISSING_BORROW` instead when §3.7.12 step 5 applies. Closures never coerce to function pointers (spec/05 §5.15.2, spec/06 §6.17.1), so a closure against a function pointer type is `TYPE_MISMATCH` with the fix "use a function item or a generic parameter bounded by `Fn`".

At most one coercion other than C-Region is applied per site, and never below a type constructor. A `ReifyFnPointer` of a function item whose instantiation is not known yet keeps its `FnDef` arguments as variables; writeback resolves them.

### 3.10.3 `as` casts

SAI has no cast expression; [impl/02](02-frontend-ast-hir.md) §2.5.6 parses `e as T` only to recover, and lowers it to `ExprKind::AsCast { operand, ty }` without a diagnostic. `NO_AS_CAST` ([spec/06](../spec/06-expressions-and-statements.md) §6.10) is reported here, because the conversion to suggest depends on the operand's type (N-E-64). The check is part of `typeck(owner)` and therefore runs for every body that is type-checked, which includes the bodies of tests and properties only under test analysis ([spec/14](../spec/14-testing.md) §14.2.4).

1. **When the expression is checked.** `ty` is converted as a type in the body's scope (§3.6.1); its own errors are reported as usual and make it `{error}`. The operand is synthesized with no expectation and shallow-resolved to `A`; a refined `A` is erased to its base for the rest of this section. If `A` is an `ι` and the converted type `T` is a primitive integer type, or `A` is a `φ` and `T` is a float type, `A` is unified with `T`. The expression's type is `T`, so that the rest of the body is checked as if the intended conversion had been written; it is `{error}` if `T` is. The cast is recorded with its span and with a flag that tells whether `A` was a variable.
2. **At the end of the body.** After the literal range checks and before `select_all_or_error()` (§3.7.6), every recorded cast is reported in source order: one `NO_AS_CAST` whose primary span runs from `as` to the end of the type, with a secondary label on the operand that shows its resolved type. Reporting sets the owner's `tainted_by_errors`, so the body is not lowered to MIR (§3.16 item 6) and no later stage sees an `AsCast` node.
3. **The fix** is chosen from the resolved `A` and `T`. `x` stands for the operand's source text, parenthesized when it is not a primary or postfix expression and the fix is a method call; `T` is the type's source text.

| `A` and `T` | Fix | Applicability |
|---|---|---|
| `A` is `{error}`, an unresolved variable, or `T` is `{error}` | none; the help lists the table of spec/06 §6.10 | — |
| `A` equals `T`, and `A` was not a variable | delete ` as T` | machine-applicable |
| `A` equals `T` after the unification of item 1 | delete ` as T` | maybe-incorrect: the context may now infer another type |
| an impl `T: From<A>` exists ([spec/16](../spec/16-core-library.md) §16.8.5) | `T.from(x)` | machine-applicable |
| two integer types without `From` | `T.try_from(x)`, then the alternatives `x.wrapping_to<T>()` and `x.saturating_to<T>()` | maybe-incorrect: the result type of the first is `Option<T>`, and the others choose a behavior |
| a float type to an integer type | `x.trunc_to<T>()`, then the alternative `x.checked_trunc_to<T>()` | maybe-incorrect |
| an integer type to a float type without `From`, or `F64` to `F32` | `x.to_f32()` or `x.to_f64()`, by `T` | maybe-incorrect: the value is rounded |
| a distinct type `D` whose base is `T` | `x.value` | machine-applicable |
| `T` is a distinct type whose base is `A` | `T(x)` | machine-applicable |
| any other pair | none; the help says that the types have no conversion and names `From` and `TryFrom` | — |

The functions are those of spec/16 §16.8.2, §16.8.4 and §16.8.5. A cast that names a type parameter or another non-primitive type falls in the last row unless one of the earlier rows applies.

## 3.11 Closures (Stage 2)

### 3.11.1 Signatures

A closure expression is checked by spec/06 §6.17.1 and §6.17.2 with the expectation it receives:

1. **Deduction.** The expected type is shallow-resolved. If it is a type variable or a parameter `F`, the solver collects the pending obligations and `ParamEnv` predicates `F: Fn*<(Ā)>` and `<F as FnOnce<(Ā)>>.Output = S`; the strongest kind found gives the expected signature `Ā -> S`. If it is a function pointer type, its parameters and return type are the expected signature (spec/06 §6.17.2 rule 2; N-E-36). Otherwise there is no expected signature.
2. **Parameters.** An annotated parameter is lowered in a body context and, when an expected signature exists, unified with the corresponding `Ai`. An unannotated parameter takes `Ai`, or a fresh `α` with origin "closure parameter" (reported at the end of the body as `TYPE_ANNOTATIONS_NEEDED` with the fix "annotate the parameter"). An arity different from the expected signature's is `WRONG_ARGUMENT_COUNT` on the closure, naming both counts.
3. **Return.** The annotation, else the expected `S`, else a fresh `α`. The body is checked against it with `RetCtx { kind: Closure }` pushed and an empty `loop_stack`. Its unsafe context is that of [impl/08](08-effect-and-capability-analysis.md) §8.10.2 rule 3: it starts again as safe, except in an unsafe module ([spec/12](../spec/12-unsafe-and-ffi.md) §12.2.4).
4. **Type.** The closure has type `Closure(c, parent identity args ++ [sig, υ])` (§3.11.5). Against an expected function pointer type, the coercion then fails with `TYPE_MISMATCH` (§3.10.2), so N-E-36 changes only the diagnostics: the parameters get the pointer's types and no `TYPE_ANNOTATIONS_NEEDED` is added to the mismatch.

Closures have no name, so they are neither recursive nor generic. Closure arguments of a call are deferred until the other arguments have been checked (N-A-41).

### 3.11.2 Capture analysis

Capture analysis runs at the end of the owner (N-E-30), when every type is known up to unresolved variables, because the mode of a use depends on whether the read type is `Copy`. It visits each closure and `async` block of the owner in post-order, a nested one before the one that contains it and siblings in source order, so the capture tuple of a closure is fixed before a later closure reads a value of its type (a read of a closure type whose tuple is not fixed yet is classified as not `Copy`). It uses the **use classifier** below, which walks the typed HIR with the place contexts of §3.7.12 and the recorded adjustments:

| Use of a place rooted at `x` | Class |
|---|---|
| read in a value context of a type that is not `Copy` (after adjustments) | `move` |
| `MutPlace` contexts: left side of `=` and `op=`, operand of `borrow mut`, receiver auto-referenced as `borrow mut`, callee of an `FnMut` call, scrutinee place under a `borrow mut` pattern binding | `mut` |
| every other use, including a `Copy` read, `borrow`, a shared auto-reference, comparison operands and interpolations | `shared` |
| a capture of `x` by a nested closure or `async` block, with mode `m` | `m`, or `shared` when the nested capture is by value of a `Copy` type |

The **captured variables** are the locals of enclosing bodies (parameters and `self` included) used in the body, each captured as a whole variable (spec/06 N-C-17), in the order of their first use in source order. For a closure without `move`, the mode is the maximum class of its uses in the order `shared < mut < move`; with `move fn`, every capture is by value. An `async` block with `CaptureBy::Move` captures by value; with `CaptureBy::Infer` it uses the closure rule (spec/11 §11.2.3). The async body of an `async fn` captures **every** parameter by value in parameter order, whether or not it is used (spec/11 §11.2.1 and §11.5.5); this is the rule [impl/02](02-frontend-ast-hir.md) §2.9.15 refers to.

The capture types are `Ref(Erased, T, Not)` for `shared`, `Ref(Erased, T, Mut)` for `mut` and `T` by value. Their tuple is unified with the `υ` of the closure or future type, and `TypeckResults::closure_captures[c]` records `CapturedVar { var: HirId, mode: CaptureMode { ByRef(Mutability) | ByValue }, ty, first_use: Span }`. Whether a `mut` capture of an immutable local is allowed (`BORROW_MUT_OF_IMMUTABLE`) and whether a by-value capture is a valid move are borrow-checker questions ([impl/05](05-borrow-checker.md)): MIR building turns every capture into an ordinary `borrow`, `borrow mut` or move of the local at the closure expression.

### 3.11.3 Closure kinds

The kind of each closure follows spec/06 §6.17.4 from the classes of its body's uses of its own captures: `FnOnce` if some use moves out of a capture (a `move` class use of a by-value capture of a non-`Copy` type); otherwise `FnMut` if some capture is `mut`, or some by-value capture is used in a `MutPlace` context; otherwise `Fn`. `TypeckResults::closure_kinds[c]` records it. The stalled `Goal::ClosureKind` obligations are then resumed (§3.8.4), so a closure passed to a parameter bounded by `Fn` whose body mutates a capture fails with `UNSATISFIED_BOUND` naming `Fn`, with the note "this closure implements `FnMut` because it mutates `x`", pointing at the first such use.

### 3.11.4 Calls through the function traits

A call whose callee is a closure, or a value of a parameter type with a function-trait bound, is resolved to one of the internal methods of N-E-26:

| Callee | Kind used | Internal method | Callee passing |
|---|---|---|---|
| closure of kind `Fn` | `Fn` | `FnCall` | implicit shared borrow |
| closure of kind `FnMut` | `FnMut` | `FnMutCallMut` | implicit `borrow mut` (a `MutPlace` use of the callee) |
| closure of kind `FnOnce` | `FnOnce` | `FnOnceCallOnce` | by value |
| parameter `F` | the strongest bound of `F` in the order `Fn`, `FnMut`, `FnOnce` (spec/06 §6.5.1) | as above | as above |

The arguments are packed into one tuple, the single type parameter of the function traits (N-E-28). `TypeckResults::call_kinds[e]` gets `CallKind::FnTrait { kind, method }`. Because a closure's kind is known only at the end of the body, a closure call is recorded as `CallKind::Pending` while the body is checked and filled in after §3.11.3; a call of an `FnMut` closure that is a captured variable of an enclosing closure contributes a `mut` use to that capture, which is why the innermost closures are analyzed first. Function items and function pointers are called directly (`CallKind::Direct`, `CallKind::FnPtr`), never through the traits; the trait impls of §3.8.5 are used only when they are passed to a function-trait-bounded parameter.

### 3.11.5 Representation

The arguments of `Closure(c, args)` are the identity arguments of the enclosing owner followed by two **parts**: the signature as a `FnPtr` type (safe, with the closure's latent effects left to [impl/08](08-effect-and-capability-analysis.md)), and the capture tuple `υ`. An anonymous future of an `async` block has the parts `[output, υ]`. The future of an `async fn f` is `Future(f, identity args)` without parts: its output is `fn_sig(f).async_output` and its captures are the parameters. The queries `closure_sig(c)`, `closure_kind(c)` and `closure_captures(c)` read the owner's `TypeckResults`. A closure type is `Copy` or `Clone` exactly when `υ` is, a future type never is (N-E-34), and neither coerces to a function pointer.

## 3.12 Patterns and `check_match`

### 3.12.1 Pattern typing

`check_pat(p, expected: Ty, origin)` implements spec/06 §6.21.2 to §6.21.4. The expected type always comes from the scrutinee; it is `structurally_resolve`d first whenever `p` is not a binding or `_` (spec/05 §5.16.4).

- **Refined scrutinee.** If the expected type is a refined `R`, a top-level binding or `_` matches at `R`; any other pattern is checked against the base, and `TypeckResults::pat_adjustments[p] = [RefinedToBase]`.
- **Bindings** record `BindingInfo { mode: Move | Borrow(Mutability), mutable: bool, ty }` in `TypeckResults::bindings`; the local's type is `T`, `borrow T` or `borrow mut T`. There are no default binding modes.
- **Or-patterns** check every alternative against the same type; the names and modes were compared by [impl/02](02-frontend-ast-hir.md), and a name bound with types that do not unify is `OR_PATTERN_BINDING_MISMATCH` here.
- **Literals** follow spec/06 §6.21.3 (N-C-33). An integer literal against an `ι` scrutinee unifies nothing and leaves the variable to end-of-body defaulting; the literal's range is checked once the type is known. String literals match `Str` only, byte strings `Slice<Byte>` only.
- **Ranges** require an integer type, `Char` or `Byte`, ends of that type, and a nonempty range (`EMPTY_RANGE_PATTERN`), evaluated with `const_value` for const ends.
- **Const patterns** require the const's type to equal the expected type and to be an integer type, `Bool`, `Char`, `Byte` or `Str` (`CONST_PATTERN_TYPE`).
- **Slice patterns** follow the table of spec/06 §6.21.3: `SLICE_PATTERN_LENGTH_MISMATCH` for arrays, rest types per scrutinee, `REDUNDANT_BINDING_MODE` for `borrow mut r` on a `SliceMut`, and `TYPE_MISMATCH` on any other type with the fix `match v.as_slice()` for a `Vec`.
- **Constructor patterns** take their generic arguments from the expected type (unifying the constructor's type with it), and apply the arity and field rules (`PATTERN_ARITY_MISMATCH`, `DUPLICATE_FIELD`, `MISSING_FIELD`, `UNKNOWN_FIELD`, `PRIVATE_FIELD`, `RECORD_FIELD_NEEDS_NAME`, `EXPECTED_PATTERN`); distinct patterns check visibility (§3.13.3). Implicit members require an enum expected type (`IMPLICIT_MEMBER_NOT_ENUM`). A pattern that names a flag of a flags type is `FLAGS_PATTERN`, and `check_match` treats a flags type as a type without constructors ([16](16-interfaces-and-wit.md) §16.9.5).
- **`x @ P`** applies the four rules of spec/06 §6.21.4 with the `Copy` test of the solver; a violation is `AT_PATTERN_BINDING_CONFLICT` on `x`.
- **`match` on a borrow.** If the scrutinee type is `borrow U` or `borrow mut U` and an arm has a pattern other than `_`, a binding or `x @ _`, the error is `MATCH_ON_BORROW` on the scrutinee, with the machine-applicable fix "write `match *e`" (spec/06 §6.15.1); the arms are then not checked further.

`TypeckResults::pat_kinds[p]` records the resolved constructor (variant index, distinct type, record type) for every constructor pattern, so that `check_match` and [impl/04](04-mir.md) need no name lookup.

### 3.12.2 `check_match`

`check_match(owner) -> Arc<MatchCheckResults>` runs after `typeck(owner)` on the typed HIR and before MIR building; [impl/04](04-mir.md) does not build the MIR of an owner whose `check_match` reported an error (§3.16). It implements spec/06 §6.22 without modification: the algorithm, the constructor sets, the splitting rules, the order of witnesses and their rendering are normative there, and every conforming implementation must give the same results.

```rust
pub enum Constructor { Variant(VariantIdx), Bool(bool), IntRange(IntRange), Str(Symbol), ByteStr(Arc<[u8]>), Single, Slice(SliceKind), Opaque, Or, Wildcard }
pub struct IntRange { pub lo: MaybeInfinite, pub hi: MaybeInfinite }   // inclusive; MaybeInfinite = NegInf | Finite(i128) | PosInf
pub enum SliceKind { Fixed(u64), Var(u64, u64) }
pub struct DeconstructedPat<'p> { pub ctor: Constructor, pub fields: &'p [DeconstructedPat<'p>], pub ty: Ty, pub span: Span }
```

- Deconstruction follows spec/06 §6.22.1, using `pat_kinds` and `pat_adjustments` (refined scrutinees use their base). `Char` values are their scalar values; `USize` and `ISize` domains have the symbolic infinities of spec/06 §6.22.2, so the result never depends on the target's pointer width.
- A `match` whose scrutinee type contains `{error}`, or one of whose patterns contains an error-typed node, is skipped.
- Usefulness is computed per arm in source order on a growing matrix, exactly as in the reachability procedure of spec/06 §6.22.4: rows of guarded arms are never added. There is no complexity limit: a limit would make acceptance depend on the implementation, which spec/06 §6.22 forbids.
- Results: `NON_EXHAUSTIVE_MATCH` on the scrutinee with up to three witnesses and one "add arm" fix per witness, inserting `<witness> => panic("not implemented")` before the closing brace; `UNREACHABLE_PATTERN` (warning) on alternatives or arms. Desugared matches report in source terms by `MatchSource`: `IfLet` and `WhileLet` report `IRREFUTABLE_CONDITION_PATTERN` (warning) when the pattern is irrefutable, and never `NON_EXHAUSTIVE_MATCH`; `ForLoop` and `ForAwait` report `REFUTABLE_PATTERN_IN_FOR` on the loop pattern; `Try` matches are generated with exhaustive arms and are not reported.
- `let` statements are checked here too: without `else`, a refutable pattern is `REFUTABLE_PATTERN_IN_LET` with the witnesses; with `else`, an irrefutable pattern is `LET_ELSE_IRREFUTABLE`.
- **Guards** (N-E-38). For every guard, the use classifier of §3.11.2 is run over the guard expression; a `move` or `mut` class use of a place rooted at a binding of the arm, or at the scrutinee place, is `GUARD_MOVE_OR_MUTATION` on the use (spec/06 §6.15.2, N-C-23). Interference through other aliases cannot arise, because [impl/04](04-mir.md) holds a shallow shared borrow of the scrutinee across every guard, which the borrow checker enforces.

`MatchCheckResults` records, per `match`, whether each arm is reachable ([impl/04](04-mir.md) does not lower the guard and body of an arm whose every alternative is unreachable, and records them in `Body.unlowered` like the unreachable code of N-E-29) and the irrefutability of each `let` pattern.

## 3.13 Refined and distinct types (Stage 3 for refinements)

This section implements spec/09 §9.2 and spec/05 §5.6.3–§5.6.4 on the type-checking side. The type checker never decides whether a refinement predicate holds: it records every B→R conversion as an obligation, and the Standard Fact Checker decides it on MIR ([impl/04](04-mir.md) §4.12).

### 3.13.1 B→R conversions

A **B→R conversion** is created in exactly these places, in the order the checker reaches them, which is source order:

1. the coercion C-BaseToRefined at a coercion site whose expected type is a refined `R` (or a distinct `D` with a refinement clause used as a type) and whose expression has the base type `B`, including a literal checked against `B` first (§3.7.6, spec/06 §6.3.1);
2. the construction `D(e)` of a distinct type with a refinement clause (§3.7.12): the argument is checked at a coercion site against `B`, and the conversion is recorded on the call;
3. an assignment `p = e` or `p op= e` whose place `p` has a refined type (spec/09 §9.2.5): the right-hand side of `=` is a coercion site against `R`, so rule 1 applies; `p op= e` is checked as `p = p op e` (the operands are erased, §3.13.2, the result has the base operation's type), and the result is converted to `R` as in rule 1, with the site `CompoundAssign` so that the diagnostic shows the operator.

```rust
pub struct RefinementObligation {
    pub id: RefinementObligationId,       // dense, in creation order within the owner
    pub expr: HirId,                      // the converted expression (the argument for `D(e)`)
    pub target: DefId,                    // the refined type or the refined distinct type
    pub base_ty: Ty,                      // B
    pub site: RefinementSite,             // Coercion | DistinctCtor | CompoundAssign
    pub span: Span,                       // primary location of `REFINEMENT_NOT_PROVEN`
}
```

`TypeckResults::refinement_obligations` holds them; `Adjust::BaseToRefined(id)` on the expression (or `CallKind::Direct` of the distinct constructor together with the id) links the expression to its obligation. [impl/04](04-mir.md) emits one `ProofObligation` statement per obligation at the point where the converted value exists, and the SFC reports `REFINEMENT_NOT_PROVEN` with the fixes of spec/09 §9.2.3. Inside a const initializer, `const_value` (§3.7.15) asks the SFC through the interface of [impl/04](04-mir.md) §4.12.4 instead, so a failed proof is still `REFINEMENT_NOT_PROVEN`.

**Predicates.** The refinement predicate of `R` is a body owner (the `where` expression of [impl/02](02-frontend-ast-hir.md) §2.8) checked by `typeck` with `RetCtx { kind: Predicate }` against `Bool`. Its only local is `self` of type `B` (N-C-38), an immutable place: [impl/04](04-mir.md) §4.3.3 lowers the predicate to an instance whose single parameter has type `borrow B`, with `self` denoting the dereferenced parameter, so a move out of `self` is `MOVE_OUT_OF_BORROW` from [impl/05](05-borrow-checker.md) and a non-`Copy` `self` can still be borrowed and used as a receiver. `.await`, `result` and `old` are rejected by §3.7.11 and §3.7.14. Effects in the predicate are checked by [impl/08](08-effect-and-capability-analysis.md) (`EFFECTFUL_PREDICATE`).

**`R.try_from`.** The path `R.try_from` resolves by §3.9.5 to `TypeDependentDef::RefinedTryFrom(R)`, a function of signature `fn(value: B) -> Option<R>` with the empty effect set (N-C-41). It is not a `TryFrom` impl. Its body is the shim of [impl/04](04-mir.md) §4.3.3, which evaluates the predicate at run time. A call of `R.try_from` creates no refinement obligation.

### 3.13.2 Refinement erasure

Every use of a value of type `R` that needs a `B` sees the value as `B` (spec/09 §9.2.2, N-C-39). The checker records the erasure as the adjustment `RefinedToBase` on the operand (or, for a pattern, in `pat_adjustments`) at these points, and nowhere else:

| Use | Where | Result type |
|---|---|---|
| coercion site expecting a non-`R` type | §3.10.2 step 4 | the expected type |
| `borrow R` at a coercion site expecting `borrow` of a non-`R` type | §3.10.2 step 4 (`RefinedBorrowToBase`) | `borrow B` |
| operand of an arithmetic, bitwise, logical, comparison or range operator | §3.10.1 step 1 | the base operation's |
| indexing base on a read | §3.7.12 | the element type |
| interpolation of an f-string | §3.7.13 | not applicable |
| `match` scrutinee, unless every arm is a top-level binding or `_` | §3.12.1 | the base |
| field read through `R` | §3.7.12 | the field type |
| receiver at the refined base step of method lookup | §3.9.4 step 5 | `B` or `borrow B` |

`borrow mut R` never coerces to `borrow mut B`. A base method with a `borrow mut Self` receiver is `REFINED_PLACE_MUTATION` (§3.9.4 step 5). The result type of an erased operation is never `R`: `p + 1` with `p : Percentage` has type `I32`, and converting it back is a new B→R conversion (§3.13.1).

In MIR, `RefinedToBase` on a place is the projection `Field(0)` of the refined type, whose only field is the base (§3.6.4); on a value it is a copy or move of that projection; `RefinedBorrowToBase` is a shared reborrow of the projection ([impl/04](04-mir.md) §4.5). A place reached through a `Field(0)` projection of a refined type, or through `.value` of a distinct type with a refinement clause, is immutable (spec/06 N-C-34): this chapter never puts such a place in a `MutPlace` context without reporting, and [impl/05](05-borrow-checker.md) reports `ASSIGN_TO_IMMUTABLE` or `BORROW_MUT_OF_IMMUTABLE` on any write or mutable borrow through it, as for every other immutable place (spec/07 §7.2.4).

The compiler-provided trait implementations of `R` (forwarding `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash` and `Format` to `B`) are the builtin candidates of §3.8.5; their bodies are shims of [impl/04](04-mir.md) §4.3.3.

### 3.13.3 Distinct types

`distinct D = B` without a refinement clause is Stage 0; with one it is Stage 3 and is also refined (spec/05 §5.6.3).

- **Construction** `D(e)` is a call of the constructor `FnDef` (§3.6.3) with signature `fn(B) -> D`; the argument is a coercion site against `B`. A literal is never checked against `D` itself (§3.7.6).
- **Patterns** `D(p)` check `p` against `B` and record `pat_kinds[p] = Distinct(D)`.
- **`.value`** is the field of §3.6.4; its read gives a place of type `B` whose mutability is that of the base place, except for a refined distinct type (§3.13.2).
- **Visibility** (spec/04 §4.6.4). Outside the defining package, unless the declaration is `pub distinct D = pub B`: the constructor call and the pattern are `PRIVATE_ITEM` on the path, and `.value` is `PRIVATE_FIELD` on the field name. The constructor call is then typed as `D` and the field as `B`, so no further diagnostic follows. Inside the defining package all three are visible. `PUB_REPR_ON_PRIVATE_DISTINCT` is reported by `wf_check_item` (§3.6.7) on the `pub` marker of the base.
- **No coercions.** `D` and `B` never coerce into each other (D-TYPE-03); `D` has no operator, method or trait of `B` unless it is declared or derived for `D` (spec/05 §5.12).

## 3.14 Generics (Stage 2)

### 3.14.1 Parameter environments

`param_env(def) -> ParamEnv` is the elaborated predicate set `Δ` of spec/05 §5.7.4 for the item `def`. Closures and `async` blocks use the `ParamEnv` of their enclosing body owner. Elaboration is deterministic, so that environment candidates and their diagnostics appear in the same order everywhere:

1. Inside a trait and its items, `Self: Tr<Ḡ>` comes first (spec/05 §5.7.4 item 5).
2. The written predicates follow, parent first: the predicates of the enclosing impl or trait, then those of the item. Within one item, the bounds of the parameter list come before the `where` clause, each in source order, and the bounds of one parameter left to right (`FnSugar` contributes its trait predicate, then its projection).
3. A worklist then visits the list from the front. For each `Trait` predicate `τ: Tr<σ̄>` it appends, in this order: the supertrait predicates of `Tr` in declaration order, substituted with `Self := τ`; the `where` predicates of `Tr` on `Self`, substituted likewise; the equalities `<τ as Tr>.A = υ` of the bindings on the bound (already present from step 2 as `Projection` predicates). Appended predicates are visited in turn.
4. A predicate structurally equal to an earlier one is dropped, so the first occurrence keeps its position and its span. Equality ignores `caps`; the caps of the dropped predicate are appended to the `caps` of the kept one, because every cap that applies to a bound restricts it (§3.6.2).
5. **Bounds of associated types** (spec/05 §5.7.4 item 3) are not added to the list: their elaboration could be infinite (`trait Tr { type A: Tr }`). Instead the solver treats them as environment candidates on demand (N-E-37): when a goal's self type is a rigid projection `<τ as Tr>.A` and `τ: Tr` is in `Δ`, the bounds `Bs` declared on `A` in `Tr`, substituted, and their supertraits are candidates of step 4 of §3.8.4, after the predicates of the list. The set of predicates that hold is exactly that of spec/05 §5.7.4; only the representation is lazy.

Step 3 terminates because supertrait cycles are rejected (`SUPERTRAIT_CYCLE`, §3.6.2) and the substitution only ever puts the subject of an existing predicate in `Self` position. Nothing else is implied (N-A-28): mentioning `Vec<T>` in a signature adds no bound on `T`.

The `ParamEnv` also records the mode `Reveal::UserFacing`, under which projections whose self type is a parameter stay rigid. Instance resolution (§3.8.7) uses `Reveal::All` with an empty predicate list.

### 3.14.2 Checking generic items once

A generic item is checked once, with its identity arguments, against `Δ` only (spec/05 §5.7.1). `Param` types are rigid: they unify only with themselves, and the only facts about them are the predicates of `Δ`.

- Methods on a value of type `Param(T)` or a rigid projection are found by the probe of §3.9.4 from the traits of `Δ`'s bounds on that type, whether or not those traits are in scope (spec/05 §5.13).
- A path to a generic item is instantiated with fresh variables, or with the explicit arguments (`WRONG_NUMBER_OF_GENERIC_ARGS`, `GENERIC_ARG_KIND_MISMATCH`, `CONST_ARG_NOT_SIMPLE`, §3.6.1), and the item's predicates are registered as obligations with the cause `ItemBound { item, index, span }`, so a failure names the bound and the use.
- Trait default methods are checked once, with `Self` as a parameter and the trait's `Δ`. An impl that does not override them uses the default body, instantiated at monomorphization.
- An instantiation that satisfies the bounds is valid. Monomorphization reports no type error except `INSTANTIATION_DEPTH_EXCEEDED` (spec/05 §5.7.6, N-A-30), which [impl/04](04-mir.md) §4.13 reports at the call that produces the instantiation.

### 3.14.3 Const generics

A const parameter `const N: κ` is `ConstKind::Param` in types and, as a value expression, a path of type `κ` that [impl/04](04-mir.md) lowers to a constant operand substituted at monomorphization. A const argument is an integer literal, possibly negated, a `CHAR`, `true` or `false`, or a const parameter of type `κ` in scope (spec/05 §5.2.2 item 3); anything else, including the name of a `const` item, is `CONST_ARG_NOT_SIMPLE` (§3.6.1), and `_` in expression position is a fresh `κ`. A literal argument becomes a `ScalarValue` of type `κ` directly: `LITERAL_OUT_OF_RANGE` if its value does not fit, `TYPE_MISMATCH` if its kind does not match `κ`. Const arguments are compared by value (spec/05 §5.14). A const variable `κ` unifies only with a const of the same type; at the end of the body an unresolved `κ` is `TYPE_ANNOTATIONS_NEEDED`, like `α`.

### 3.14.4 Staging of type-system constructs (N-E-39)

A toolchain whose implemented stage range does not include the stage of a construct below reports `FEATURE_NOT_IMPLEMENTED` (spec/01 §1.4.5) for it. Spec/01 §1.4.5 names the features of Stages 5 to 7 and takes the names of the language constructs of the earlier stages from this table. Each construct is reported once, at the location given, with the data `stage` and `feature`; the construct is then treated as erroneous, so that its uses are silently `{error}`-typed and do not cascade.

| Construct | Stage | Location | `feature` |
|---|---|---|---|
| a generic parameter list on a declaration of the program | 2 | the list, from `<` to `>` | `generics` |
| a `trait` declaration | 2 | the `trait` keyword | `traits` |
| an `impl` block, inherent or trait | 2 | the `impl` keyword | `impls` |
| a closure expression | 2 | its `fn` or `move` keyword | `closures` |
| a function pointer type | 2 | the type | `function-pointers` |
| the `unsafe` marker of a block, a function (the functions of `extern` blocks, trait, impl and interface items among them), a trait, an impl, a module header or an `extern "wasm"` block; a trait or impl that the rows above report is not reported again | 2 | the `unsafe` keyword | `unsafe` |
| an f-string, reported by [impl/02](02-frontend-ast-hir.md) §2.9.11 | 3 | `FSTRING_START` | `f-strings` |
| a refinement clause, on a `type` or `distinct` declaration | 3 | the `where` keyword of the clause | `refined-types` |
| a `requires`, `ensures` or `invariant` clause | 3 | the keyword | `contracts` |
| an effect set in a function pointer type, or an effect cap on a bound | 3 | the effect list | `effects` |
| a `use` of a declared dependency | 3 | the package name of the path (§3.2) | `dependencies` |
| a `resource`, interface or world declaration | 4 | the keyword | `resources`, `interfaces`, `worlds` |
| an `unsafe extern "C"` block (spec/12 §12.9.2 item 4) | 7 | the ABI string | `extern-c` (spec/01 §1.4.5) |

An erroneous `unsafe extern "C"` block makes the calls of its functions `{error}`-typed, so the feature `extern-c` is reported once per block and not per call. Raw pointers and `sai:core/mem` are library items (spec/12 §12.1.4) and are gated by their paths, as below. Attributes, `@derive` among them, are gated by their own stage with the feature `attribute @name` (spec/15 §15.2.4 step 2, [impl/02](02-frontend-ast-hir.md) §2.10.2), and library items by theirs with their canonical path (spec/16 §16.1.6), not by this table. A Stage 0 program may use `Option` and `Result` with inferred arguments (spec/05 §5.20). A construct that uses a lang item of a later stage is reported at its first token with the path of that item (spec/16 §16.1.6): before Stage 2, a `for` loop is reported with `sai:core/iter.IntoIterator` and a range expression with the path of its range type. The solver, the method probe and the impls of `sai:core` and `sai:alloc` are present at every stage, because the library items of Stage 1 are declared with them; the table only restricts what the program itself may declare.

## 3.15 `TypeckResults`

### 3.15.1 Contents

`TypeckResults` is the complete interface between this chapter and MIR building. [impl/04](04-mir.md) reads the HIR and these tables, and performs no lookup, inference or trait selection of its own except `resolve_instance` and `needs_drop` (§3.8.7).

```rust
pub struct TypeckResults {
    pub owner: OwnerId,
    // Types (§3.7, §3.10.2, §3.12.1)
    pub node_types: ItemLocalMap<Ty>,                    // every expression, pattern and binding
    pub node_args: ItemLocalMap<GenericArgs>,            // instantiation of every value path to a generic item
    pub adjustments: ItemLocalMap<Vec<Adjustment>>,      // in application order
    pub pat_adjustments: ItemLocalMap<Vec<Adjustment>>,  // only `RefinedToBase` (§3.12.1)
    // Resolutions
    pub type_dependent_defs: ItemLocalMap<Result<TypeDependentDef, ErrorGuaranteed>>,  // method calls, type-relative paths
    pub implicit_members: ItemLocalMap<DefId>,           // the variant named by `.V` (§3.7.7)
    pub field_indices: ItemLocalMap<(VariantIdx, FieldIdx)>,  // field reads and named fields of patterns
    pub record_ctors: ItemLocalMap<RecordCtor>,          // variant, argument field indices, base (§3.7.12)
    pub call_kinds: ItemLocalMap<CallKind>,              // Direct | FnPtr | FnTrait { kind, method } (§3.7.12, §3.11.4)
    pub index_kinds: ItemLocalMap<IndexKind>,            // Builtin | Overloaded { mutbl } (§3.7.12)
    pub op_res: ItemLocalMap<OpRes>,                     // unary, binary and compound assignment (§3.10.1)
    pub try_kinds: ItemLocalMap<TryKind>,                // §3.7.9
    pub await_kinds: ItemLocalMap<AwaitKind>,            // Future | StreamNext (§3.7.11)
    pub format_specs: ItemLocalMap<FormatSpecRes>,       // per interpolation (§3.7.13)
    pub temps: ItemLocalSet,                             // value operands of borrows and place contexts (§3.7.12)
    // Patterns (§3.12.1)
    pub bindings: ItemLocalMap<BindingInfo>,
    pub pat_kinds: ItemLocalMap<PatRes>,                 // Variant(adt, idx) | Record(adt) | Distinct(adt) | Tuple | Slice
    // Closures and futures (§3.11)
    pub closure_captures: IndexMap<LocalDefId, Vec<CapturedVar>>,   // in first-use order
    pub closure_kinds: IndexMap<LocalDefId, ClosureKind>,
    // Obligations for later phases (§3.13.1)
    pub refinement_obligations: IndexVec<RefinementObligationId, RefinementObligation>,
    // Bookkeeping
    pub unreachable: Vec<HirId>,                         // N-E-29, in source order
    pub defaulted_at: Vec<(HirId, Ty)>,                  // literal defaulting, for fixes and `sai explain`
    pub alias_uses: ItemLocalMap<DefId>,                 // the alias a written type came from (§3.6.1.1)
    pub used_trait_imports: BTreeSet<UseId>,             // §3.9.2
    pub tainted_by_errors: Option<ErrorGuaranteed>,
}
pub enum TypeDependentDef {
    Assoc { def: DefId, args: GenericArgs, pick: Option<MethodPick> },  // method or associated item
    Variant(DefId),                                      // `E.V` through a type-relative path
    RefinedTryFrom(DefId),                               // §3.13.1
    FlagsConst { flags: DefId, bit: u32 },               // `F.X` of a flags type (impl/16 §16.9.3)
}
```

`ItemLocalMap` is keyed by the `ItemLocalId` of a `HirId` of the owner and iterates in id order, which is the HIR's pre-order ([impl/02](02-frontend-ast-hir.md) §2.8), so every traversal of the tables is deterministic. Nodes of closures and `async` blocks of the owner are in the same tables (§3.7.1).

### 3.15.2 Invariants after writeback

1. No type in any table contains an inference variable. A variable that could not be resolved was reported (§3.7.6) and is replaced by `{error}`.
2. No type contains a normalizable projection (§3.5.5); rigid projections remain.
3. Every region is `Erased`, except `Static` for literal data and the `Sig` regions of types read from signatures, which are also erased in bodies (§3.5.2).
4. Every expression whose meaning depends on types has its entry: every method call and type-relative path in `type_dependent_defs`, every binary, unary and compound-assignment operator in `op_res`, every index in `index_kinds`, every call in `call_kinds` (none still `Pending`), every `?` in `try_kinds`, every `.await` and `for await` in `await_kinds`, every record construction in `record_ctors`, every field access in `field_indices`, every closure and `async` block in `closure_captures`, and every closure in `closure_kinds`.
5. If `tainted_by_errors` is `None`, no table contains `{error}` and no `Err` HIR node is reachable from the owner's body.

[impl/04](04-mir.md) §4.4.1 checks invariant 4 for untainted owners and reports a violation as an internal compiler error; invariants 1 to 3 are checked by the MIR validator through the local types ([impl/04](04-mir.md) §4.11).

### 3.15.3 Writeback

`writeback(fcx) -> TypeckResults` runs after the end-of-body steps of §3.7.6. It walks the owner's HIR in pre-order and, for every entry of the builder, fully resolves the stored types (`resolve_vars_if_possible`, then `normalize`), replaces every region by `Erased` (keeping `Static`), and copies the entry into the final table. An entry whose resolution meets an unresolved variable is replaced by `{error}` without a new diagnostic (the variable was reported at the end of the body). Pending call kinds are replaced from `closure_kinds`. The builder's `tainted` flag and the `InferCtxt`'s are merged into `tainted_by_errors`.

## 3.16 Error recovery

The goal is one diagnostic per mistake: an error is reported where it is detected and never again in code that depends on it ([impl/11](11-diagnostics.md) class C2).

1. **The error type.** Every failed check that cannot produce a meaningful type produces `{error}` with the `ErrorGuaranteed` of the reported diagnostic, under the rules of §3.5.6. A failed lookup produces `{error}` for the expression, an `Err` resolution for the path, and an `Err` entry in `type_dependent_defs`.
2. **Checking continues.** After an error, the remaining subexpressions are still checked, with no expectation where the error removed it, so that independent mistakes in the same body are all reported. Arguments of a call to an unknown function are synthesized; the arms of a `match` on an `{error}` scrutinee are checked with their bindings typed `{error}`.
3. **One diagnostic per construct.** A construct reports at most one error from this chapter. When several rules fail for one construct (for example, both the arity and an argument type of a call), only the first in the order of the rule's section is reported. A mismatch on a coercion site is reported once, at the site; its expected and found types are not reported again by enclosing expressions, because the expression takes the expected type after a failed coercion.
4. **Obligations.** An obligation that mentions `{error}` holds (§3.5.6 rule 2). An obligation whose failure was reported is removed. At the end of the body, at most one diagnostic is reported per cause span (§3.7.4), and an unresolved variable unioned with an `{error}` is not reported.
5. **Declarations.** A declaration whose signature contains an error keeps the rest of its signature: a parameter of type `{error}` makes every call's corresponding argument an unchecked coercion site, and a missing return type annotation after an error is `{error}`. A trait or impl with an error is kept in the indexes with its error-free items, so that uses of its other items do not report spurious `METHOD_NOT_FOUND`.
6. **Tainted bodies.** An owner with `tainted_by_errors` set, or whose `check_match` reported an error, is not lowered to MIR ([impl/04](04-mir.md) §4.4.1). No borrow-checking, effect, contract or code-generation diagnostic is derived from it, because each of them would be a consequence of an error already reported. The unsafe check is the exception, because it reads the HIR and `TypeckResults` and not MIR: [impl/08](08-effect-and-capability-analysis.md) §8.10 also walks a tainted owner, checks only the operations whose types are known (§8.10.3 item 6 there) and their declaration rules, reports `UNSAFE_OPERATION_OUTSIDE_UNSAFE` for them, and reports no `UNUSED_UNSAFE` for a block of the owner or for the `unsafe module` header of its file. Warnings already reported for the owner are kept. Other owners are unaffected: a call to a function whose body is tainted, but whose signature is valid, is checked normally.
7. **Determinism.** Recovery never depends on the order in which owners are checked: each owner's diagnostics depend only on its own body and on the signatures it uses, and the driver reports them in the order of [impl/11](11-diagnostics.md) §11.11.1.

## 3.17 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) and the language chapters leave open. They are provisional until merged into the decision record. IDs continue from [02](02-frontend-ast-hir.md) and continue in [04](04-mir.md).

| ID | Rule | Rationale |
|---|---|---|
| N-E-22 | The unit of type checking is the body owner: a function or method with a body, a `const` initializer, a test, a property, a refinement predicate, an invariant. Closures and `async` blocks are checked inside the inference context of their enclosing owner and share its `TypeckResults`; contract clauses belong to their function's owner (§3.7.1). | spec/05 §5.16.1 makes the enclosing body the inference scope; P8: defaulting and ambiguity are decided in one place. |
| N-E-23 | Module-tree diagnostics, which concern files and directories, have the fixed primary locations and `data` of the table in §3.2.3. | P3 and P8: diagnostics without a source range still have deterministic, machine-usable locations. |
| N-E-24 | Regions are placeholders in this chapter: `Static` for literal data, `Sig { item, index }` numbered in pre-order in signatures and declarations, `Erased` in bodies. Unification ignores them; [impl/05](05-borrow-checker.md) interprets them (§3.5.2). | spec/05 §5.14: regions never affect type checking. Keeping them out of inference keeps the checker independent of spec/07. |
| N-E-25 | Every cycle that the language rejects (`REEXPORT_CYCLE`, `ALIAS_CYCLE`, `SUPERTRAIT_CYCLE`, `CONST_CYCLE`, `INFINITE_SIZE_TYPE`) is found by an explicit algorithm inside one query; a cycle between queries is an internal compiler error (§3.1.3). | P8: which declaration of a cycle is reported must not depend on the order in which queries are forced. |
| N-E-26 | The function-trait methods `call`, `call_mut` and `call_once`, and `StreamNext`, are compiler-internal: synthesized by `trait_def` or recognized by shape, never nameable in source, visible only in MIR dumps and symbols (§3.3.6). | spec/16 §16.6.3 gives the function traits no nameable methods, but MIR needs ordinary callees. |
| N-E-27 | A `Field` node belongs to a path chain only if its span starts where the span of the chain's first segment starts, so that `(m).S` is a field access while `m.S` is a path (§3.4.2). | HIR has no parentheses (N-E-18); the span test restores the distinction without a new HIR node (P8). |
| N-E-28 | The function traits each take one type parameter, the tuple of argument types; `FnOnce` has one internal associated type `Output`. `Fn(Ā) -> S` lowers to `τ: Fn<(Ā)>` plus `<τ as FnOnce<(Ā)>>.Output = S`; an omitted return type is `()` (§3.6.2). | spec/16 §16.6.3 leaves the internal shape open; it is unobservable in source, so the simplest shape is chosen (P8). |
| N-E-29 | Code after a diverging expression is checked normally, warned once with `UNREACHABLE_CODE`, and recorded in `TypeckResults::unreachable`. [impl/04](04-mir.md) does not lower it and lists it in `Body.unlowered`, so that analyses that must see every source construct can still visit it (§3.7.8). | P2: unreachable code is validated like reachable code; MIR contains no dead blocks. |
| N-E-30 | The end-of-body steps run in the fixed order of §3.7.6: selection, literal defaulting, selection, capture analysis and closure kinds, selection, literal range checks, final selection, report of unresolved variables. | spec/05 §5.16.5 fixes the defaulting step but not its position relative to capture analysis; P8: acceptance does not depend on traversal details. |
| N-E-31 | In a predicate, `return`, `break` and `continue` are reported once, as `INVALID_CONTRACT_EXPRESSION`, during resolution, and no `RETURN_OUTSIDE_FUNCTION` or label error is added (§3.4.5). | spec/06 §6.16.5 defers predicates to spec/09 §9.1.2; one diagnostic per mistake (P3). |
| N-E-32 | An overloaded index is first resolved as `Index`; a fix-up after the enclosing expression switches it to `IndexMut` when the indexed place ends up in a mutable place context, including a `borrow mut` auto-referenced receiver (§3.7.12). | The context of an index is known only after its enclosing expression is checked; the fix-up makes the choice independent of checking order (P8). |
| N-E-33 | Constants and explicit discriminants are evaluated on typed HIR by `const_value`; no MIR is built for them. Their proof obligations are passed to the SFC through the interface of [impl/04](04-mir.md) §4.12.4 (§3.7.15). | spec/06 §6.27 restricts const expressions to a small pure subset; a tree interpreter needs no MIR and has no cycles through MIR building (P8). |
| N-E-34 | A closure type is `Copy` or `Clone` through the goal `υ: Tr` on its capture tuple, which stalls until capture analysis; anonymous future types are never `Copy` or `Clone` (§3.8.5). | spec/05 §5.5.3 (N-A-31) owns the rule; the capture types of §3.11.2 already encode "a `borrow mut` capture makes it neither", so no special case is needed. |
| N-E-35 | Solver caching: only goals without inference variables are canonicalized and cached; a result that depends on an inductive cycle still on the stack is cached only at the root of that cycle; overflow is never cached (§3.8.4). | P8: no result depends on the order in which bodies or goals are evaluated. |
| N-E-36 | An expected function pointer type supplies the expected signature of a closure. The coercion of the closure still fails with `TYPE_MISMATCH`, so only the diagnostics change (§3.11.1). | spec/06 §6.17.2 rule 2 contradicts spec/05 §5.16.6; the choice has no effect on acceptance and gives one precise diagnostic instead of two (P3). |
| N-E-37 | Bounds on associated types are not added to the elaborated `ParamEnv`; the solver uses them as environment candidates on demand for rigid projections (§3.14.1). | Eager elaboration of spec/05 §5.7.4 item 3 does not terminate for `trait Tr { type A: Tr }`; the lazy form yields exactly the same set of predicates. |
| N-E-38 | `GUARD_MOVE_OR_MUTATION` is decided in `check_match` by the use classifier on typed HIR, for places rooted at the arm's bindings or the scrutinee; [impl/04](04-mir.md) holds a shallow shared borrow of the scrutinee across every guard, so mutation through other aliases is a borrow error (§3.12.2). | spec/06 N-C-23 states the rule but not where it is checked; a syntactic check gives the precise code, the borrow covers what syntax cannot see. |
| N-E-39 | The type-system constructs of Stages 2 to 4 are gated by this chapter, once per construct, at the locations and with the feature names of §3.14.4. | spec/01 §1.4.5 fixes the form of `FEATURE_NOT_IMPLEMENTED`, names the features of Stages 5 to 7 and takes the names of the earlier language constructs from §3.14.4 (P8). |
| N-E-63 | Boundary declarations are checked in `wf_check_item` in the order form, link name, types, panic hook signature, and the hook signature is checked only when no code of spec/12 §12.7.4 or `EXTERN_EFFECTS_MISMATCH` applies. `panic_hooks(P)` orders hooks by source-relative path bytes and offset; hooks outside the root are each `PANIC_HOOK_OUTSIDE_ROOT` and do not count as duplicates. The core-export checks are a separate query that the driver asks for only when the artifact is a core module (§3.6.9). | P8: one code per mistake, the most specific first; the order of spec/10 §10.6.1 item 5 is independent of module numbering, and no query value depends on the artifact kind. |
| N-E-64 | `e as T` is parsed into a recovery node without a diagnostic and reported as `NO_AS_CAST` by the type checker at the end of the body, with a fix chosen from the resolved operand and target types. The expression has type `T`, so that no consequential error follows (§3.10.3). | spec/06 §6.10 requires a conversion that fits both types, which only the type checker knows; P3: the fix is computed from types, not from text. P8: one diagnostic per mistake (§3.16). |
| N-E-65 | Bindings of test-only uses are marked in item collection from the `requires test` entries of the package's own contract, in every build. Outside tests and properties every reference to one is `TEST_DEPENDENCY_OUTSIDE_TEST` from its origin alone, and traits bound through one are in scope only for test owners, through a second version of `traits_in_scope`. `INACTIVE_DEPENDENCY` reads the same declared requirements (§3.3.3, §3.4.10). | spec/14 §14.2.5 item 4: the result of a build must not depend on test analysis, which is the only case in which these bindings resolve (P8). |
| N-E-66 | `UNUSED_ATTRIBUTE` and `MISSING_DOCS` are computed per item owner by `attr_lints`, from attributes and signatures only, in every build after `@target` filtering; `MISSING_DOCS` is computed only for the root package and the workspace members (§3.6.10). | P8: a lint that reads no body cannot depend on test analysis or on the order of checking. The lints of a dependency are capped ([impl/11](11-diagnostics.md) §11.3), so computing `MISSING_DOCS` for it would be wasted work. |
| N-E-68 | `entry_point(root)` checks the candidate `main` of the root module in every build and check, on every target and in test builds: `async` is `ASYNC_MAIN` only, a signature that contains the error type reports nothing more, and every other deviation is one `INVALID_ENTRY_POINT` at the name. `ENTRY_POINT_NOT_APPLIED` is the session-level query `entry_point_note`, asked only for a core-module artifact outside test builds (§3.6.9). | targets §4.2 item 5: whether a package is a valid program does not depend on the target (P8). One diagnostic per candidate, and none on top of an unresolved type, avoids cascades (P9). |

## 3.18 Diagnostics

This chapter defines no diagnostic code of its own. Every code below is defined in the document and section named at the start of its description ("defined in"). A reference of the form §3.x is the section of this chapter that reports the code, and other section references belong to the defining document. Codes that this chapter only relies on are marked with the document that reports them.

| Code | Severity | Description |
|---|---|---|
| `ALIAS_CASE_MISMATCH` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.4; an item alias has a different case class from the item |
| `ALIAS_CYCLE` | error | defined in [spec/05](../spec/05-types.md) §5.6.5; a type alias that expands to itself |
| `AMBIGUOUS_ASSOCIATED_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.8.5; `T.A` where several bound traits of `T` have an associated type `A` |
| `AMBIGUOUS_METHOD` | error | defined in [spec/05](../spec/05-types.md) §5.13.4; methods or associated functions of two or more traits match at the same step |
| `ARENA_REQUIRES_COPY` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.15.2; a non-`Copy` value allocated in an arena (§3.7.12) |
| `ARRAY_LENGTH_MISMATCH` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.18.4; an array literal's length differs from the expected `Array<T, N>` |
| `ASSIGN_TO_IMMUTABLE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.2.4; assignment to an immutable place; reported by [impl/05](05-borrow-checker.md) |
| `ASSOC_TYPE_BINDING_OUTSIDE_BOUND` | error | defined in [spec/05](../spec/05-types.md) §5.2.2; `A = T` in a trait reference that is not a bound |
| `ASSOC_TYPE_IN_INHERENT_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.9.2; an associated type in an inherent impl |
| `ASYNC_EXPORT_UNSUPPORTED` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.10.4; an `async` function exported from a core module, reported by `core_export_check` (§3.6.9) |
| `ASYNC_FN_IN_TRAIT` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.6; an `async fn` declared in a trait or implemented in a trait impl |
| `ASYNC_MAIN` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.6; the entry point `main` is declared `async` |
| `ASYNC_RECURSION` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.4; the future type of an async function contains itself without a `Box`; reported by [impl/04](04-mir.md) §4.13.4 |
| `AT_PATTERN_BINDING_CONFLICT` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.4; `x @ P` combines binding modes that conflict |
| `AWAIT_OUTSIDE_ASYNC` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.3.1; `.await` or `for await` where the innermost body is not an `async fn` or `async` block |
| `BORROW_MUT_OF_IMMUTABLE` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.2.4; `borrow mut` of an immutable place, including `mut` captures; reported by [impl/05](05-borrow-checker.md) |
| `BREAK_OUTSIDE_LOOP` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.4; `break` with no enclosing loop in the current body |
| `CANNOT_DEREFERENCE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.9.1; `*e` on a type that is not `borrow T`, `borrow mut T`, `Box<T>`, `Ptr<T>` or `PtrMut<T>`; a raw-pointer dereference outside an unsafe context is `UNSAFE_OPERATION_OUTSIDE_UNSAFE`, reported by [impl/08](08-effect-and-capability-analysis.md) §8.10.5 |
| `CONST_ARG_NOT_SIMPLE` | error | defined in [spec/05](../spec/05-types.md) §5.2.2; a const argument that is not a literal or a const parameter |
| `CONST_CYCLE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.27; a const depends on itself |
| `CONST_EVAL_PANIC` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.27; evaluating a const initializer would panic (overflow, division by zero, shift overflow) |
| `CONST_INVARIANT_NOT_PROVEN` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.4.5; a construction of a type with an invariant in a const initializer that the SFC does not prove; reported by the SFC ([impl/04](04-mir.md) §4.12.4) |
| `CONST_PATTERN_TYPE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.3; a const used as a pattern has a type other than an integer, `Bool`, `Char`, `Byte` or `Str` |
| `CONTINUE_OUTSIDE_LOOP` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.4; `continue` with no enclosing loop in the current body |
| `CONTRACT_REQUIRES_ON_IMPL` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.5.1; a `requires` clause on a trait impl method |
| `COPY_AND_DROP` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.13.6; a type that is both `Copy` and `Drop` (§3.8.2) |
| `DEPRECATED_REFERENCE` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.6.4; a reference to a deprecated declaration outside a deprecated context (§3.4.7) |
| `DERIVE_FIELD_MISSING_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.12.1; a field, payload or base type that does not implement the derived trait |
| `DERIVE_NOT_SUPPORTED` | error | defined in [spec/05](../spec/05-types.md) §5.12.4; a derivable trait that cannot be derived for this kind of declaration |
| `DISCRIMINANT_OUT_OF_RANGE` | error | defined in [spec/05](../spec/05-types.md) §5.6.2; an enum discriminant outside the discriminant type |
| `DROP_IMPL_INVALID_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.9.3; `impl Drop` for a type that is not a local record, enum, distinct type or resource |
| `DROP_IMPL_NOT_GENERIC_ENOUGH` | error | defined in [spec/05](../spec/05-types.md) §5.9.3; a `Drop` impl that is not exactly as generic as its type |
| `DUPLICATE_BINDING` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.4.3; two bindings of one name in one scope |
| `DUPLICATE_DISCRIMINANT` | error | defined in [spec/05](../spec/05-types.md) §5.6.2; two variants with the same discriminant |
| `DUPLICATE_EXPORT_NAME` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.7; two `export fn` items with the same export name, or the export name `memory` |
| `DUPLICATE_FIELD` | error | defined in [spec/05](../spec/05-types.md) §5.6.1; two fields with the same name in a record or record variant |
| `DUPLICATE_INHERENT_ITEM` | error | defined in [spec/05](../spec/05-types.md) §5.9.2; two inherent items with the same name across the inherent impls of one type |
| `DUPLICATE_INVARIANT` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.4.1; a second `invariant` declaration for the same type |
| `DUPLICATE_PANIC_HOOK` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.6.1; every `@panic_hook` of the root package after the first in file-path and source order, by `panic_hooks` (§3.6.9) |
| `DUPLICATE_TEST_NAME` | error | defined in [spec/14](../spec/14-testing.md) §14.2.3; a test or property with the name of an earlier one of the same module, in every build (§3.3.2) |
| `DUPLICATE_TRAIT_ITEM` | error | defined in [spec/05](../spec/05-types.md) §5.8.1; two items with the same name in one trait |
| `DUPLICATE_VARIANT` | error | defined in [spec/05](../spec/05-types.md) §5.6.2; two variants with the same name |
| `EFFECTFUL_PREDICATE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.11; a refinement, contract or invariant predicate has effects; reported by [impl/08](08-effect-and-capability-analysis.md) |
| `EMPTY_RANGE_PATTERN` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.3; a range pattern that matches no value |
| `ENTRY_POINT_NOT_APPLIED` | info | defined in [targets](../targets/targets-and-profiles.md) §4.5 item 4; a valid `main` in a command that analyzes the root package for a core module, from `entry_point_note` (§3.6.9) |
| `EXPECTED_PATTERN` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.3; a path in a pattern that is not a constructor, variant or const |
| `EXPECTED_VALUE_FOUND_TYPE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.4.1; a type, module or trait used as a value |
| `EXPLICIT_DROP_CALL` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.13.6; an explicit call of `Drop.drop` (§3.9.5) |
| `EXPORT_FN_NON_SCALAR` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.7.4, §12.8; a parameter or result type of an `extern "wasm"` function or of a core export that is not a boundary scalar (§3.6.9) |
| `EXTERN_EFFECTS_MISMATCH` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.1; reported by [impl/08](08-effect-and-capability-analysis.md) §8.1.7; §3.6.9 skips the panic-hook signature check when it applies |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; a staged construct that the toolchain does not implement (§3.14.4) |
| `FORMAT_NOT_IMPLEMENTED` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.20.3; an f-string interpolation whose type does not implement `Format` |
| `FORMAT_SPEC_ARGUMENT` | error | defined in [spec/16](../spec/16-core-library.md) §16.14.3; a format specifier with a missing or unexpected argument, or with an argument outside `0..=65535` |
| `FORMAT_SPEC_NOT_APPLICABLE` | error | defined in [spec/16](../spec/16-core-library.md) §16.14.3; a known format specifier applied to a type that does not implement the marker trait it requires (`FormatInt`, `FormatFloat` or `FormatPad`); data: `spec`, `type`, `required_trait` |
| `FUNCTIONAL_UPDATE_OUTSIDE_RECORD` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.5.1; `..e` as an argument of a call that is not a record construction |
| `GENERIC_ARG_KIND_MISMATCH` | error | defined in [spec/05](../spec/05-types.md) §5.2.2; a type argument for a const parameter, or the reverse |
| `GENERIC_REFINED_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.6.4; a refined type, or a distinct type with a refinement clause, with generic parameters (spec/03 §3.6) |
| `GUARD_MOVE_OR_MUTATION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.2; a match guard moves, assigns or mutably borrows a binding or the scrutinee |
| `IF_WITHOUT_ELSE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.14.1; an `if` without `else` used where a non-`()` value is required |
| `IMPLICIT_MEMBER_NEEDS_TYPE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.4.3; `.V` without a known expected type |
| `IMPLICIT_MEMBER_NOT_ENUM` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.4.3; `.V` whose expected type is not an enum |
| `INACTIVE_DEPENDENCY` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; a `use` of a declared dependency whose `only` list excludes the build target, from `declared_requirements` (§3.3.3) |
| `INFER_IN_SIGNATURE` | error | defined in [spec/03](../spec/03-grammar.md) §3.15; `_` in an item signature; reported by [impl/02](02-frontend-ast-hir.md) |
| `INFINITE_SIZE_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.6.7; a nominal type that contains itself by value |
| `INHERENT_IMPL_FOREIGN_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.9.2; an inherent impl for a type not declared as a nominal type in the current package |
| `INSTANTIATION_DEPTH_EXCEEDED` | error | defined in [spec/05](../spec/05-types.md) §5.7.6; monomorphization nests a type argument deeper than 64; reported by [impl/04](04-mir.md) §4.13 |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11](11-diagnostics.md) §11.4.6; an internal invariant was violated, including a query cycle (§3.1.3) |
| `INVALID_ASSIGN_TARGET` | error | defined in [spec/03](../spec/03-grammar.md) §3.12; assignment to a non-place expression; non-place syntax is reported by [impl/02](02-frontend-ast-hir.md), a path that is not a local binding by §3.7.8 |
| `INVALID_BREAK_VALUE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.2; `break` with a value targeting `while`, `while let` or `for` |
| `INVALID_CONST_PARAM_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.7.1; a const parameter whose type is not an integer type, `Bool` or `Char` |
| `INVALID_CONTRACT_EXPRESSION` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.1.2; a predicate contains `return`, `break`, `continue`, `?`, `.await`, an assignment, `var`, `unsafe`, `region` or a closure |
| `INVALID_ENTRY_POINT` | error | defined in [targets](../targets/targets-and-profiles.md) §4.2; the candidate `main` of the root module does not have the entry-point form, from `entry_point` (§3.6.9) |
| `INVALID_EXTERN_SIGNATURE` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.7.4; the `async`, generic, `self`, `where` or `borrows` form of an extern function or core export, or a non-FFI type in an `extern "C"` signature (§3.6.9) |
| `INVALID_LINK_NAME` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.9.1; an `extern "C"` symbol name that is not a C identifier (§3.6.9) |
| `INVALID_MODULE_NAME` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.2.3; a directory or file name in `<source>` is not a valid module segment, or is `lib` outside the root file |
| `INVALID_SELF_TYPE` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; a receiver type other than `Self`, `borrow Self`, `borrow mut Self`; reported by [impl/02](02-frontend-ast-hir.md) |
| `INVALID_TEST_NAME` | error | defined in [spec/14](../spec/14-testing.md) §14.2.3; an empty test name or one with a `Cc` character, in every build (§3.3.2) |
| `INVALID_TRAIT_WHERE_SUBJECT` | error | defined in [spec/05](../spec/05-types.md) §5.7.3; a `where` predicate on a trait whose subject is not `Self` or `Self.A` |
| `INVALID_TUPLE_INDEX` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.2; a malformed tuple index; reported by [impl/02](02-frontend-ast-hir.md) |
| `INVALID_USE_TARGET` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; a WIT `PKG_PATH` names a package or a world instead of an interface |
| `INVARIANT_FIELD_WRITE` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.4.2; a field of a type with an invariant is written or mutably borrowed outside its defining module; reported by [impl/05](05-borrow-checker.md) from MIR |
| `INVARIANT_ON_GENERIC_TYPE` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.4.1; an `invariant` on a type with generic parameters |
| `INVARIANT_ON_INVALID_TYPE` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.4.1; an `invariant` on a refined type, alias, primitive or interface type |
| `IRREFUTABLE_CONDITION_PATTERN` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.14.2; `if let` or `while let` with an irrefutable pattern |
| `LABEL_SHADOWED` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.4; a loop label that repeats an enclosing label in the same body |
| `LET_ELSE_IRREFUTABLE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.12.2; `let`-`else` with an irrefutable pattern |
| `LET_ELSE_NOT_DIVERGING` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.12.2; the `else` block of `let`-`else` does not have type `Never` |
| `LITERAL_OUT_OF_RANGE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.1; literal out of range for its type |
| `MANUAL_COPY_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; a hand-written `impl Copy` |
| `MANUAL_FN_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.5.4; a hand-written impl of `Fn`, `FnMut` or `FnOnce` |
| `MATCH_ON_BORROW` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.1; a non-trivial pattern on a scrutinee of borrow type (fix: `match *e`) |
| `METHOD_NOT_FOUND` | error | defined in [spec/05](../spec/05-types.md) §5.13.5; no method or associated function of that name for the type |
| `MISSING_BORROW` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.5.2; an argument passed by value where the parameter is a borrow (§3.7.12) |
| `MISSING_DOCS` | warning (lint), default `allow` | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.18.7; a declaration visible outside the package without a doc comment, in the root package and the workspace members, by `attr_lints` (§3.6.10) |
| `MISSING_FIELD` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.18.1; a record construction or record pattern misses fields |
| `MISSING_ROOT_MODULE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.2.3; `<source>` or `<source>/lib.sai` does not exist |
| `MISSING_SUPERTRAIT_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; an impl (or derive) of a trait whose supertrait is not implemented for the type |
| `MISSING_TRAIT_ITEM` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; a trait impl that lacks a required item |
| `MODULE_INTERFACE_CONFLICT` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; an interface and a module file have the same path |
| `MOVE_OUT_OF_BORROW` | error | defined in [spec/07](../spec/07-ownership-and-borrowing.md) §7.3.5; move out of a place behind a borrow, including `self` of a predicate; reported by [impl/05](05-borrow-checker.md) |
| `NAMED_ARGUMENT_ON_FUNCTION` | error | defined in [spec/03](../spec/03-grammar.md) §3.13.2; a named argument in a call that is not a record construction (§3.7.12) |
| `NESTED_OLD` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2; `old(..)` inside another `old(..)` |
| `NON_ASCII_PUBLIC_NAME` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.4; non-ASCII character in a public name or module name |
| `NON_EXHAUSTIVE_MATCH` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.1; a `match` that does not cover every value; lists witnesses |
| `NOT_A_TRAIT` | error | defined in [spec/05](../spec/05-types.md) §5.7.2; a bound or impl names something that is not a trait |
| `NOT_CALLABLE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.5.1; a call of a value whose type is not callable |
| `NOT_CONST_EXPRESSION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.27; a form not allowed in a const initializer |
| `NOT_INDEXABLE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.7.2; `e[i]` with no built-in indexing and no `Index` / `IndexMut` impl |
| `NOT_ITERABLE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.3; the `for` iterable does not implement `IntoIterator` |
| `NO_AS_CAST` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.10; reported at the end of the body with a fix chosen from the operand and target types (§3.10.3) |
| `OLD_OUTSIDE_ENSURES` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2; `old(..)` outside an `ensures` clause that does not resolve to a function |
| `OLD_REFERS_TO_RESULT` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2; the operand of `old(..)` mentions `result` |
| `OLD_REQUIRES_COPY` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2; the operand of `old(..)` has a type that is not `Copy` |
| `OPERATOR_NOT_SUPPORTED` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.8.2; an operator not defined for the operand types |
| `ORPHAN_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.10.1; a trait impl that violates the orphan rule |
| `OR_PATTERN_BINDING_MISMATCH` | error | defined in [spec/03](../spec/03-grammar.md) §3.14; alternatives of an or-pattern bind different names, modes or types (§3.12.1) |
| `OVERLAPPING_IMPLS` | error | defined in [spec/05](../spec/05-types.md) §5.10.2; two impls, derived impls or compiler-provided impls whose headers unify |
| `PANIC_HOOK_OUTSIDE_ROOT` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.6.1; every `@panic_hook` of a package that is not the root, by `panic_hooks` (§3.6.9) |
| `PANIC_HOOK_SIGNATURE_MISMATCH` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.6.1; a hook whose signature is not `(USize, USize) -> ()` or that is `unsafe` or has a contract, checked after the spec/12 checks (§3.6.9) |
| `PATTERN_ARITY_MISMATCH` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.2; wrong number of subpatterns for a tuple, tuple variant or unit variant |
| `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.4.2; a public signature names a non-`pub` user effect; reported by [impl/08](08-effect-and-capability-analysis.md) |
| `PRIVATE_FIELD` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.6.3; a field (or `.value` of an opaque distinct type) is not visible here, or construction/pattern rules of spec/04 §4.6.3 are violated |
| `PRIVATE_ITEM` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; an item, member, distinct constructor or pattern is not visible here |
| `PRIVATE_REEXPORT` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.5; a `pub use` lists an item that is not `pub` |
| `PRIVATE_TYPE_IN_PUBLIC_API` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.6.7; a public item's surface names a package-private type, trait or constant |
| `PROJECTION_IN_IMPL_HEADER` | error | defined in [spec/05](../spec/05-types.md) §5.10.2; a projection in the self type or trait arguments of an impl header |
| `PROJECTION_ON_CONCRETE_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.8.5; `τ.A` with a concrete `τ` |
| `PUB_REPR_ON_PRIVATE_DISTINCT` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.6.4; `distinct D = pub B` where `D` is not `pub` |
| `PUB_USE_WITHOUT_ITEMS` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.5; a `pub use` has no item list |
| `RECORD_CONSTRUCTOR_AS_VALUE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.4.1; a record type or record variant used as a function value |
| `RECORD_FIELD_NEEDS_NAME` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.18.1; an unnamed, non-punning argument or subpattern in a record construction or pattern |
| `REDUNDANT_BINDING_MODE` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.3; `borrow mut r` rest binding on a `SliceMut` |
| `REDUNDANT_IMPORT` | warning (lint) | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.6; a `use` binding that denotes what its name already denotes |
| `REDUNDANT_UPDATE_BASE` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.18.1; `..base` in a record construction that names every field |
| `REEXPORT_CYCLE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.5; a chain of `pub use` re-exports returns to itself |
| `REFINED_BASE_IS_REFINED` | error | defined in [spec/05](../spec/05-types.md) §5.6.4; the base of a refined type is itself refined |
| `REFINED_PLACE_MUTATION` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.2.2; a base method with a `borrow mut Self` receiver selected on a refined place |
| `REFINEMENT_NOT_PROVEN` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.2.3; a B→R conversion, or a refined distinct construction `D(e)`, that the SFC does not prove; reported by the SFC ([impl/04](04-mir.md) §4.12) |
| `REFUTABLE_PATTERN_IN_FOR` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.3; a refutable pattern in `for` |
| `REFUTABLE_PATTERN_IN_LET` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.12.1; a refutable pattern in `let` without `else`; lists witnesses |
| `RESERVED_TYPE_NAME` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.4.4; a declaration or import alias uses a primitive type name, `Int`, `UInt` or `Never` |
| `RESOURCE_WITH_BORROW` | error | defined in [spec/05](../spec/05-types.md) §5.6.6; a resource field whose type contains a view or borrow |
| `RESULT_NAME_CONFLICT` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2; a parameter named `result` in a function with an `ensures` clause |
| `RESULT_OUTSIDE_ENSURES` | error | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.3.2; `result` outside an `ensures` clause that does not resolve to a binding |
| `RETURN_OUTSIDE_FUNCTION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.5; `return` in a const initializer |
| `SELF_OUTSIDE_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.2.1; `Self` outside an impl or trait |
| `SHADOWS_GENERIC_PARAM` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.1; a method's generic parameter reuses a name of its impl's or trait's generic parameters |
| `SHADOWS_MODULE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.1; a local has the name of a module or interface binding |
| `SHADOWS_PRELUDE` | warning (lint) | defined in [spec/04](../spec/04-modules-and-names.md) §4.4.4; a module-level binding of a prelude name |
| `SLICE_PATTERN_LENGTH_MISMATCH` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.21.3; a slice pattern that cannot match the length of an `Array<U, N>` |
| `STATEMENT_HAS_NO_EFFECT` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.25.2; a discarded expression without side effects |
| `SUPERTRAIT_CYCLE` | error | defined in [spec/05](../spec/05-types.md) §5.8.1; a trait that is its own supertrait, directly or transitively |
| `SYMLINK_IN_SOURCE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.2.3; a symbolic link inside `<source>` |
| `TEST_DEPENDENCY_OUTSIDE_TEST` | error | defined in [spec/14](../spec/14-testing.md) §14.2.5; a test-only `pub use` (§3.3.3) or a reference to a test-only binding outside tests and properties (§3.4.10), in every build |
| `TEST_DEPENDENCY_UNAVAILABLE` | error | defined in [spec/14](../spec/14-testing.md) §14.2.5 item 5; a test-only use of a selected package that is not the root, under test analysis (§3.3.3) |
| `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.3; an impl method's effects are not permitted by the trait method's clause; reported by [impl/08](08-effect-and-capability-analysis.md) |
| `TRAIT_ITEM_SIGNATURE_MISMATCH` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; a trait impl item whose signature does not match the trait exactly |
| `TRAIT_NOT_IN_SCOPE` | error | defined in [spec/05](../spec/05-types.md) §5.13.5; the method exists only in a trait that is not in scope; the fix adds a `use` |
| `TRAIT_SOLVER_OVERFLOW` | error | defined in [spec/05](../spec/05-types.md) §5.11.2; trait solving exceeded depth 128 |
| `TRY_ERROR_TYPE_MISMATCH` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.3.2; the error type of a `Result` operand of `?` differs from the error type of the enclosing body's return type |
| `TRY_INVALID_OPERAND` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.3.2; the operand of `?` is not a `Result` or an `Option`, including a borrow of one and a future that is not awaited |
| `TRY_INVALID_RETURN_TYPE` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.3.2; `?` in a body whose return type is neither a `Result` nor an `Option` |
| `TRY_OPTION_RESULT_MIX` | error | defined in [spec/10](../spec/10-errors-and-panics.md) §10.3.2; `?` applied to an `Option` in a body returning a `Result`, or to a `Result` in a body returning an `Option` |
| `TUPLE_INDEX_OUT_OF_RANGE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.2; `e.N` with `N` not less than the tuple's arity |
| `TYPE_ANNOTATIONS_NEEDED` | error | defined in [spec/05](../spec/05-types.md) §5.16.4; a type that must be known (spec/05 §5.16.4), or remains unknown at the end of the body (spec/05 §5.16.5), or an obligation that stays ambiguous |
| `TYPE_MISMATCH` | error | defined in [spec/05](../spec/05-types.md) §5.14; two types that must be equal, or a coercion that does not apply; names the expected and found types and the origin of the expectation |
| `UNCONSTRAINED_IMPL_PARAM` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; an impl parameter that does not occur in the impl header |
| `UNDECLARED_LABEL` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.4; a `break` or `continue` label that names no enclosing loop |
| `UNKNOWN_ASSOC_TYPE` | error | defined in [spec/05](../spec/05-types.md) §5.2.2; an associated-type binding or projection naming no associated type of the trait |
| `UNKNOWN_FIELD` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.1; a field name that the record, variant or tuple type does not have |
| `UNKNOWN_FORMAT_SPEC` | error | defined in [spec/16](../spec/16-core-library.md) §16.14.3; a format specifier name that is not one of `debug`, `hex`, `upper_hex`, `bin`, `oct`, `exp`, `prec`, `pad`, `left` and `zpad`; data: `name` |
| `UNKNOWN_MODULE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; no module or interface has the given path |
| `UNKNOWN_PACKAGE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; the package of a `PKG_PATH` is neither the current package, `sai:core`, `sai:alloc`, nor a declared dependency |
| `UNKNOWN_TRAIT_ITEM` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; a trait impl item that the trait does not declare |
| `UNKNOWN_VARIANT` | error | defined in [spec/05](../spec/05-types.md) §5.13.6; `E.V` naming no variant of `E`, or a UIDENT member of a non-enum type |
| `UNREACHABLE_CODE` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.26.2; code after a diverging expression |
| `UNREACHABLE_PATTERN` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.1; a match arm or top-level alternative that can never match |
| `UNRESOLVED_IMPORT` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.4; an item of a `use` list is not in the target's member namespace |
| `UNRESOLVED_NAME` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.1; a name or path segment does not resolve |
| `UNSATISFIED_BOUND` | error | defined in [spec/05](../spec/05-types.md) §5.11.2; a trait obligation that no candidate satisfies |
| `UNUSED_ALIAS_PARAM` | error | defined in [spec/05](../spec/05-types.md) §5.6.5; a type alias parameter that does not occur in the aliased type |
| `UNUSED_ATTRIBUTE` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.18.6; a valid attribute without effect, by `attr_lints` (§3.6.10) |
| `UNUSED_IMPORT` | warning (lint) | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.6; a non-`pub` `use` binding that is never referenced, counting trait-in-scope uses (§3.9.2) |
| `UNUSED_LABEL` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.18.4; a loop label that no labeled `break` or `continue` resolves to (§3.4.5) |
| `UNUSED_MUST_USE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.25.2; a discarded value of a `@must_use` type |
| `USE_BINDING_IS_KEYWORD` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.3; the derived binding name of a `use` is a keyword or reserved word; an alias is required |
| `USE_OF_CURRENT_MODULE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; a `use` names the module that contains it |
| `WHERE_WITHOUT_PARAM` | error | defined in [spec/05](../spec/05-types.md) §5.7.3; a `where` predicate whose subject mentions no generic parameter of the item |
| `WRONG_ARGUMENT_COUNT` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.5.1; a call with the wrong number of arguments |
| `WRONG_NAME_KIND` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.1; a name resolves to an entity of the wrong kind for its position |
| `WRONG_NUMBER_OF_GENERIC_ARGS` | error | defined in [spec/05](../spec/05-types.md) §5.2.2; a generic argument list with the wrong number of arguments |
