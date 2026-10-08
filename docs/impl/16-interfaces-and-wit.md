# 16. Interfaces, Worlds and WIT

**Status:** Normative for the reference implementation
**Stage:** 4 (`async` interface items and `FutureReader`/`StreamReader`: Stage 5)
**Related:** [../spec/13-components-and-wit.md](../spec/13-components-and-wit.md), [../spec/04-modules-and-names.md](../spec/04-modules-and-names.md), [../spec/03-grammar.md](../spec/03-grammar.md), [../spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md), [../package/01-package-contract.md](../package/01-package-contract.md), [../package/04-distribution-and-security.md](../package/04-distribution-and-security.md), [../targets/targets-and-profiles.md](../targets/targets-and-profiles.md), [01-architecture.md](01-architecture.md), [02-frontend-ast-hir.md](02-frontend-ast-hir.md), [03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md), [04-mir.md](04-mir.md), [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md), [07-component-lowering.md](07-component-lowering.md), [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md), [13-cli.md](13-cli.md), [15-conformance-and-testing.md](15-conformance-and-testing.md), [decisions.md](../design/decisions.md) D-WIT-01, D-WIT-02, D-WIT-03, D-OWN-06, D-TOOL-05

This document specifies how the reference implementation handles interfaces, worlds and WIT. It covers:

- locating, parsing and validating WIT packages;
- the SAI view of WIT and how it enters name resolution ([03](03-name-resolution-and-type-checking.md));
- the HIR and the checks of interface declarations, interface implementations and worlds;
- the elaboration of worlds and of the export world;
- flags types, from HIR to MIR and memory layout;
- the generation of WIT from SAI source for `sai bindgen wit`, `sai inspect wit`, the dependents of a package and the hybrid consistency check.

The language rules belong to [spec/13](../spec/13-components-and-wit.md). Paths belong to [spec/04](../spec/04-modules-and-names.md), the `wit` directory and `export world` to [package/01](../package/01-package-contract.md), and hybrid packages to [package/04](../package/04-distribution-and-security.md). This document fixes the queries, data structures, orders and diagnostic locations. It never changes which programs are accepted.

The component itself (Canonical ABI glue, composition and validation) is built by [07](07-component-lowering.md) from the results of this chapter. The effect rules of interface functions are checked by [08](08-effect-and-capability-analysis.md).

---

## 16.1 Overview

### 16.1.1 Position in the pipeline

The phases are those of [01](01-architecture.md) §1.2.1.

| Phase | Work of this chapter | Queries |
|---|---|---|
| 3 | The driver sets the `wit_files` input of every package that declares `wit` (§16.2.2). | `wit_files` |
| 4 | The parser reports `INTERFACE_FN_BODY` (§16.5.2). Validation reports `INTERFACE_FN_CLAUSE` and `WORLD_IMPORT_NEEDS_INTERFACE` ([02](02-frontend-ast-hir.md) §2.5.12). | the parser and validation of [02](02-frontend-ast-hir.md) |
| 5 | The `DefPath`s of interface items, world items and the provided functions of flags types (§16.5.3). | `def_collector` |
| 6 | Several pieces of resolution happen in this phase: WIT loading and SAI views (§16.2–§16.4), the member scopes of local interfaces, `use` targets in WIT packages and world references (§16.8.1), interface implementations, and the redirect (§16.7.4). | `wit_package`, `wit_closure`, `wit_view`, `package_wit_names`, `interface_uses`, `interface_impls`, `world_includes`, `world_import_fns`, `export_world` |
| 7 | The signatures of view items, then flags, view derives, the interface, impl and world checks, and elaboration (§16.6–§16.9). | `flags_def`, `view_derives`, `interface_check`, `interface_impl_check`, `elaborated_world`, `world_check`, `hybrid_wit_check` |
| 10 | The component world, the generated WIT, and the two `info` notes of spec/13 §13.6.2 and §13.6.4. | `component_world`, `generated_wit`, `generated_wit_text`, `export_fn_note`, `export_world_note` |
| 13 | Stage 1 of [07](07-component-lowering.md) §7.1.1 builds its `wit_parser::Resolve` with `resolve_for` (§16.10.2). | none |

### 16.1.2 Crates

The crate graph is the one of [01](01-architecture.md) §1.3.1, with these responsibilities:

- **`sai_wit`** (new, placed directly after `sai_hir`) owns:
  - the WIT units;
  - WIT loading with `wit-parser`;
  - the WIT model;
  - the canonical WIT printer;
  - the `DefId`s and member tables of view packages.

  It depends on `sai_hir` and `sai_package` and never on `sai_resolve` or `sai_ty`. It reads only the package graph types and the contract queries of `sai_package`, like `sai_resolve` does (impl/01 §1.3.2 rule 1).
- **`sai_resolve`** owns:
  - interface scopes;
  - `use` targets in WIT packages;
  - world references and world members;
  - interface implementations and the redirect;
  - interface and world cycles;
  - the export world.

  It gains the dependency `sai_wit`.
- **`sai_ty`** owns, in the module `sai_ty::iface`:
  - the signatures and ADTs of view items;
  - flags;
  - view derives;
  - the interface, impl and world checks;
  - elaboration;
  - WIT generation.

  It gains the dependency `sai_wit`.
- **`sai_component`** reads the model, the closures and the generated text through `sai_wit` and does not load or generate WIT itself.

`sai_wit` needs the generated WIT of a source package (§16.4.7), which `sai_ty` computes. Following impl/01 §1.3.2 rule 6, `sai_wit` declares a hook trait. The value type `GeneratedText` is defined in `sai_wit`, and `RootDatabase` implements the trait by calling `sai_ty::iface::generated_wit_text`:

```rust
pub trait WitHooks {
    fn generated_wit_text(&self, package: PackageId) -> Arc<GeneratedText>;
}
pub struct GeneratedText {
    pub text: Arc<str>,                         // the WIT text of spec/13 §13.11.2
    pub complete: bool,                         // no item was omitted (§16.10.1)
    pub origins: Arc<[(SmolStr, DefId)]>,       // WIT interface name → SAI interface, in WIT order
}
```

### 16.1.3 Queries

| Query | Crate | Key | Value | Section |
|---|---|---|---|---|
| `wit_files` | `sai_wit` | `PackageId` | input: `Option<Arc<[FileId]>>` | §16.2.2 |
| `wit_contract` | `sai_wit` | `PackageId` | `Arc<WitContract>`: the contract fields this chapter reads (firewall) | §16.1.4 |
| `wit_unit_of` | `sai_wit` | `PackageId` | `Option<WitUnit>` | §16.2.1 |
| `wit_package` | `sai_wit` | `WitUnit` | `Arc<WitPackageResult>`: the model or a failure, with diagnostics | §16.2.4 |
| `wit_closure` | `sai_wit` | `WitUnit` | `Arc<[WitUnit]>` in post-order | §16.2.4 |
| `wit_view` | `sai_wit` | `WitUnit` | `Arc<WitView>` | §16.4.1 |
| `view_origin` | `sai_wit` | `PackageId` | `Option<WitUnit>` | §16.4.1 |
| `package_wit_names` | `sai_resolve` | `PackageId` | `Arc<PackageWitNames>` with diagnostics | §16.6.4 |
| `interface_uses` | `sai_resolve` | `PackageId` | `Arc<InterfaceUses>` with diagnostics | §16.6.5 |
| `interface_impls` | `sai_resolve` | `PackageId` | `Arc<InterfaceImpls>` with diagnostics | §16.7.1 |
| `world_includes` | `sai_resolve` | `PackageId` | `Arc<WorldIncludes>` with diagnostics | §16.8.3 |
| `world_import_fns` | `sai_resolve` | `DefId` (a world) | `Arc<BTreeMap<Symbol, DefId>>` | §16.8.2 |
| `export_world` | `sai_resolve` | `PackageId` | `Arc<ExportWorld>` with diagnostics | §16.8.5 |
| `flags_def` | `sai_ty` | `DefId` | `Option<Arc<FlagsDef>>` | §16.9.2 |
| `view_derives` | `sai_ty` | `DefId` | `DeriveSet` | §16.4.6 |
| `interface_check` | `sai_ty` | `LocalDefId` (an interface) | diagnostics | §16.6.1 |
| `interface_impl_check` | `sai_ty` | `LocalDefId` (an interface impl) | diagnostics | §16.7.2 |
| `elaborated_world` | `sai_ty` | `DefId` (a world) | `Arc<ElaboratedWorld>` | §16.8.3 |
| `world_check` | `sai_ty` | `LocalDefId` (a world) | diagnostics | §16.8.4 |
| `component_world` | `sai_ty` | `(PackageId, bool)` | `Arc<ComponentWorld>` with diagnostics | §16.8.6 |
| `wit_fn_of` | `sai_ty` | `DefId` | `Option<WitFnRef>` | §16.7.4 |
| `generated_wit` | `sai_ty` | `PackageId` | `Arc<GeneratedWit>` | §16.10.1 |
| `generated_wit_text` | `sai_ty` | `PackageId` | `Arc<GeneratedText>` | §16.10.1 |
| `hybrid_wit_check` | `sai_ty` | `PackageId` | diagnostics | §16.10.3 |
| `export_fn_note` | `sai_ty` | `PackageId` (the root) | diagnostics | §16.8.6 |
| `export_world_note` | `sai_ty` | `PackageId` (the root) | diagnostics | §16.8.6 |

Values carry their diagnostics, and the driver collects them as impl/01 §1.9 states. The **check queries** of this chapter, which the driver asks for so that every diagnostic is reported exactly once, are:

1. `wit_package(Dir(P))` for every artifact or hybrid package `P` of the graph, and for the root when it declares `wit`.
2. For every source package `P`: `package_wit_names(P)`, `interface_uses(P)`, `interface_impls(P)` and `world_includes(P)`. Then, for every interface `I` of `P`, `interface_check(I)`; for every interface impl `X`, `interface_impl_check(X)`; and for every world `W`, `world_check(W)`.
3. For the root: `export_world(root)`, and `hybrid_wit_check(P)` for the root and for every hybrid node built in source form (§16.10.3).
4. Session-level queries, asked by the driver as [03](03-name-resolution-and-type-checking.md) §3.6.9 does for `core_export_check`:
   - `component_world((root, command))` in a component build;
   - `export_fn_note(root)` in a component build and in a native executable build;
   - `export_world_note(root)` in a core-module build and in a native executable build.

   [03](03-name-resolution-and-type-checking.md) §3.6.9 defines the entry-point queries `entry_point(root)`, asked in every build, and `entry_point_note(root)`, asked in a core-module build outside test builds.

`WIT_ITEM_SHADOWED` is carried by `package_scopes` (§16.4.5). The diagnostics of `elaborated_world` are returned only through `world_check` and `export_world`, so that they are not reported twice.

### 16.1.4 Determinism and incrementality

1. **Generation order.** Several rules of this chapter speak of "the later item" or list items in order. Unless stated otherwise, they use **generation order**:
   - modules ordered with the root module first, then by the bytes of their module path (spec/13 §13.11.1);
   - within a module, source order;
   - the items of an interface or world in source order.

   It is never `ModuleId` order, which depends on the session.
2. **WIT order** is the order of the WIT model (§16.3.2).
3. No result depends on the iteration order of a hash map. Tables are `BTreeMap`s or `IndexVec`s, and lists are sorted as each section states.
4. **Durability.** The embedded WASI WIT has high durability ([01](01-architecture.md) §1.4.4). `wit_files` and the contents of WIT files have the durability of source files.
5. **Firewall.** `wit_contract(P)` extracts what this chapter reads from the contract of [package/01](../package/01-package-contract.md) §1.6.3 and §1.9:
   - the package kind (source, artifact or hybrid);
   - the `wit` directory;
   - the `export world` text and its span;
   - the identity and the version.

   An edit to another field of `sai.package` leaves every query of this chapter green.
6. **Early cutoff.** `generated_wit_text` is compared by value. Editing a function body of a source dependency recomputes its `generated_wit_text`, but leaves the text equal, so the views and checks of its dependents stay green.

### 16.1.5 Stages

Everything in this chapter is Stage 4. Before Stage 4, interfaces, worlds, `wit` and `export world` are reported as `FEATURE_NOT_IMPLEMENTED` where [03](03-name-resolution-and-type-checking.md) §3.1.5 states, and no query of this chapter runs.

The following are Stage 5:

- `async` interface functions (reported by [02](02-frontend-ast-hir.md) §2.9.15);
- imported `async` functions;
- `future` and `stream` (§16.3.3);
- `FutureReader` and `StreamReader`.

---

## 16.2 Loading WIT

### 16.2.1 WIT units

A **WIT unit** is one WIT package as the compiler sees it:

```rust
pub enum WitUnit {
    Wasi { package: Symbol },   // an embedded WASI package (`cli`, `io`, …) at the version of §16.2.3
    Dir(PackageId),             // the `wit` directory of a package
    Generated(PackageId),       // the WIT generated from a source package (§16.10)
}
```

`wit_unit_of(P)` gives the unit through which **dependents** of `P` see it:

1. `sai:core` and `sai:alloc` have none.
2. An artifact package or a hybrid package has `Dir(P)`. For a hybrid this holds in both forms, because dependents see a hybrid package only through its WIT (package/04 §4.5.3 item 1).
3. Every other source package has `Generated(P)`. It realizes source 4 of spec/13 §13.7.1.

The root never sees itself through a unit, because a `use` of its own identity resolves to its SAI modules (spec/04 §4.3.2 step 1.1). The root's own `wit` directory is still loaded as `Dir(root)`:

- for validation;
- for the hybrid check (§16.10.3);
- for `export world` of an artifact (§16.8.5).

The embedded WASI units are the packages that spec/13 §13.7.1 item 1 lists, with the WIT text of the WASI release that the target fixes ([targets](../targets/targets-and-profiles.md) §2.5). The toolchain embeds them as static strings, file by file, sorted by file name.

### 16.2.2 Files and parsing

The input `wit_files(P)` is set by the driver in phase 3 for each package whose contract declares `wit` and whose directory exists. `WIT_DIR_NOT_FOUND` belongs to package/01 §1.6.3. The input lists every file with the extension `.wit` directly inside the directory, sorted by the bytes of its relative path. Subdirectories, including `deps/`, are not read (§16.2.3).

The VFS currently stores this listing as sorted package-relative paths under a validated stable `PackageKey`; the driver must intern each path to a session-local `FileId` when adapting it to the `wit_files(P)` query value above. `NotApplicable` denotes the absence of a usable declared directory, while a present empty directory is represented by an empty path list. The driver reports `WIT_DIR_NOT_FOUND` separately.

`wit_package(Dir(P))` parses these files as follows:

1. **Reading.** Each file is read from `file_contents` ([01](01-architecture.md) §1.7.2) and never from the file system. A file that is not valid UTF-8 is `WIT_PARSE_ERROR`, at the first invalid byte.
2. **Parsing.** The files are added, in the order of `wit_files`, to one `wit_parser::SourceMap` under their relative paths, and the map is parsed. No API that reads the file system is called. A syntax error is `WIT_PARSE_ERROR` at the file and byte range that `wit-parser` reports, mapped back to the `FileId`, with the message of `wit-parser`. `wit-parser` stops at the first error, so a unit has at most one parse error.
3. **One package.** The files MUST declare exactly one WIT package (package/01 §1.6.3 item 2). A second package declaration, including a nested `package … { }` block, is `WIT_PARSE_ERROR` at that declaration.
4. **Name and version.** The package name `namespace:name` MUST equal the identity of `P`, and its version MUST equal the version of `P`. A difference, including a missing version, is `WIT_PACKAGE_MISMATCH` (package/01 §1.6.3 item 3), at the package declaration. The unit still loads, under the name and version of `P`.
5. **Resolution.** Foreign references are resolved by §16.2.3. The package is then pushed into a `wit_parser::Resolve` that already contains the units of its closure (§16.2.4). A validation error of `wit-parser`, such as an unknown type, a duplicate name or an invalid `use`, is `WIT_PARSE_ERROR` at the reported location.
6. **Features.** The `Resolve` is created with no features enabled, so `wit-parser` removes every item gated by `@unstable` (spec/13 §13.7.3, N-D-34). `@since` is ignored, and `@deprecated` is recorded in the model.

`wit_package(Wasi{..})` and `wit_package(Generated(P))` run steps 2, 5 and 6 on the embedded text or on `generated_wit_text(P).text`. A failure there is an internal compiler error ([11](11-diagnostics.md) §11.4.6), not a diagnostic of the program.

### 16.2.3 Foreign references and versions

WIT `use` statements and world items name foreign packages as `ns:pkg/…` with an optional `@version`. The `deps/` directory of a `wit` directory is **ignored** (N-K-3): the package graph decides which packages are visible (spec/13 §13.7.1). In unit `Dir(P)`, a foreign package name `ns:pkg` resolves as follows:

1. `wasi:p` resolves to `Wasi { package: p }` when `p` is an embedded WASI package. Otherwise it is `WIT_PARSE_ERROR`.
2. The name of the unit itself resolves to the unit.
3. An active edge of `P` whose target has the identity `ns:pkg` resolves to `wit_unit_of(target)`. A target without a unit (`sai:core`, `sai:alloc`) is `WIT_PARSE_ERROR`.
4. Anything else is `WIT_PARSE_ERROR`, at the reference, with the help "add a dependency on `ns:pkg` to `sai.package`".

A generated unit refers only to units that rule 1 or 3 finds, and an embedded unit refers only to WASI units, so neither can fail these rules.

**Versions.** A written version `@v` MUST be in the compatibility line of the version `w` that the build selects for the referenced unit, and MUST NOT be newer than `w`. A violation is `WIT_PARSE_ERROR` with the message "requires `ns:pkg@v`, the build provides `ns:pkg@w`". The selected versions are:

- for a WASI unit, the WASI version that the target fixes;
- for a package unit, the version of its node.

A reference without a version is accepted. Before the package is pushed into the `Resolve`, every foreign package name in the unresolved package is rewritten to the selected version. A `Resolve` therefore contains exactly one version of each unit, and two compatibility lines of one package are two units with two versions (spec/13 §13.7.1).

**One WASI version for checking** (N-K-4). Every target uses the WASI version of [targets](../targets/targets-and-profiles.md) §2.5 (0.2.3, N-F-63) to load the WASI units, including core-module and native targets. On those targets the imports and the `export world` are checked but not applied (spec/13 §13.6.2), so a program gets the same diagnostics on every target.

### 16.2.4 Validation and the closure

```rust
pub struct WitPackageResult {
    pub package: Option<Arc<WitPackage>>,   // None: the unit failed (this unit's or a dependency's error)
    pub diagnostics: Vec<Diagnostic>,       // only the errors located in this unit
}
```

`wit_package(U)`:

1. parses the own sources of `U` (§16.2.2 steps 1 to 4);
2. collects the direct foreign units in the order of their first reference;
3. asks `wit_package` for each of them.

If a direct foreign unit failed, `U` fails **silently**: it reports no diagnostic, because the failing unit reports its own error through its check query (§16.1.3 item 1). Otherwise `U` is resolved (§16.2.2 steps 5 and 6) and converted to the model (§16.3).

`wit_closure(U)` is the post-order of a depth-first walk over direct foreign units, in first-reference order, without duplicates, ending with `U`. The units form no cycle:

- a `Dir` or `Generated` unit refers only to itself, to WASI units, and to the units of its package's edges;
- WASI units refer only to each other, acyclically;
- the package graph is acyclic.

A query cycle here is therefore an internal compiler error.

---

## 16.3 The WIT model

### 16.3.1 Data structures

The model is a `wit-parser`-independent copy of one resolved package. It is a query value (`Eq`, stable-hashable), so `wit_parser::Resolve` never appears in query values.

```rust
pub struct WitPackage {
    pub unit: WitUnit,
    pub name: WitPackageName,
    pub interfaces: IndexVec<WitIfaceIdx, WitInterface>,
    pub worlds: IndexVec<WitWorldIdx, WitWorld>,
}
pub struct WitPackageName { pub namespace: SmolStr, pub name: SmolStr, pub version: Option<Version> }
pub struct WitInterface {
    pub name: SmolStr,                       // the WIT name (kebab case)
    pub docs: Option<Arc<str>>,
    pub deprecated: Option<Version>,
    pub items: IndexVec<WitItemIdx, WitItem>,
    pub span: Option<WitSpan>,               // None in embedded and generated units
}
pub struct WitItem {
    pub name: SmolStr,
    pub docs: Option<Arc<str>>,
    pub deprecated: Option<Version>,
    pub kind: WitItemKind,
    pub unsupported: Option<Unsupported>,    // §16.3.3
    pub span: Option<WitSpan>,
}
pub enum WitItemKind {
    Record(Vec<WitField>),                   // WitField { name, docs, ty }
    Variant(Vec<WitCase>),                   // WitCase { name, docs, payload: Option<WitType> }
    Enum(Vec<SmolStr>),
    Flags(Vec<SmolStr>),
    Resource(Vec<WitMember>),
    Alias(WitType),                          // `type t = …`
    Used(WitTypeRef),                        // `use other.{t}` or `use other.{t as u}`
    Func(WitFunc),
}
pub struct WitMember { pub name: SmolStr, pub kind: WitMemberKind, pub func: WitFunc, pub docs: Option<Arc<str>>, pub deprecated: Option<Version> }
pub enum WitMemberKind { Constructor, Method, Static }
pub struct WitFunc { pub params: Vec<(SmolStr, WitType)>, pub result: Option<WitType>, pub is_async: bool }
pub enum WitType {
    Bool, S8, S16, S32, S64, U8, U16, U32, U64, F32, F64, Char, String,
    List(Box<WitType>),
    Option(Box<WitType>),
    Result { ok: Option<Box<WitType>>, err: Option<Box<WitType>> },
    Tuple(Vec<WitType>),
    Named(WitTypeRef),                        // a record, variant, enum, flags or alias
    Own(WitTypeRef), Borrow(WitTypeRef),      // resource handles
    Future(Option<Box<WitType>>), Stream(Option<Box<WitType>>),
    Unsupported(Unsupported),                 // error-context, fixed-length list
}
pub struct WitTypeRef { pub interface: WitIfaceRef, pub item: WitItemIdx }
pub struct WitIfaceRef { pub unit: WitUnit, pub interface: WitIfaceIdx }
pub struct WitWorld {
    pub name: SmolStr,
    pub docs: Option<Arc<str>>,
    pub imports: Vec<WitWorldItem>,           // as elaborated by `wit-parser`, in its order
    pub exports: Vec<WitWorldItem>,
    pub unsupported: Option<Unsupported>,
    pub span: Option<WitSpan>,
}
pub enum WitWorldItem { Interface(WitIfaceRef), Func { name: SmolStr, func: WitFunc } }
pub enum Unsupported { ErrorContext, FixedList, NamedResults, Async, Future, Stream, WorldType, InlineInterface }
```

A function with several named results gets `result: None` and `unsupported: Some(NamedResults)`. A type of another unit is referenced by `WitTypeRef`. The model of a unit therefore never copies a definition of another unit, and nominal identity follows the referenced unit (§16.4.1).

### 16.3.2 Order

The **WIT order** of a unit is the order that `wit-parser` assigns when it is fed the files of §16.2.2 in path order:

- interfaces and worlds in the order of their declarations;
- the items of an interface in the order of `Interface::types`, followed by `Interface::functions`;
- the members of a resource in declaration order;
- the imports and exports of a world in the order of the elaborated `World::imports` and `World::exports` of `wit-parser`.

Generated units have the generation order of §16.10.1, because the printer writes them in that order.

### 16.3.3 Unsupported constructs

Unsupported constructs are recorded in the model and reported where the program **uses** them, never when the unit loads (spec/13 §13.9.2, N-K-5):

| Construct | Recorded on | Diagnostic at a use |
|---|---|---|
| `error-context`, a fixed-length list, multiple named results | the type or function whose definition or signature contains it, and transitively every type and function that contains such a type | `WIT_UNSUPPORTED_FEATURE` |
| an `async` function, `future<T>`, `stream<T>` | the same | before Stage 5, `FEATURE_NOT_IMPLEMENTED`; from Stage 5 on, the rule of spec/13 §13.9.2 |
| a type or `use` declared in a world, or an inline `interface { … }` in a world | the world (`WorldType`, `InlineInterface`) | `WIT_UNSUPPORTED_FEATURE` |

The last row is a decision of this chapter. A SAI world has no types, and an inline interface has no `ns:pkg/name` by which SAI could name it. Such a world therefore has no SAI form.

A **use** is one of:

1. A path of a SAI owner that resolves to the item. It is reported by `resolve_owner` at the path, once per path, and the path gets `Res::Err`. A `use` item list that names the item binds it without a diagnostic, and the paths that resolve through the binding are the uses.
2. An interface impl of an interface that contains an unsupported function. It is reported by `interface_impls` at the target path, once per such function, in WIT order (§16.7.1).
3. A world reference to an unsupported world in `include` or in `export world`. It is reported at the reference (§16.8.1, §16.8.5).

An interface that appears in a world, an elaborated world or a component world is not a use of its items.

### 16.3.4 The canonical printer

`sai_wit::print_wit(&WitPackage) -> String` writes the text of a package by the rules of spec/13 §13.11.2, §13.11.3 and §13.11.4. It writes interfaces, worlds and items in model order. It is used only for generated units (§16.10.1), whose model is already in generation order. The text of a `Dir` unit is always the file as written. The printer has no options, and its output is a pure function of the model.

---

## 16.4 SAI views

### 16.4.1 View packages and DefIds

Each unit that loads successfully has a **view package**. A view package is a `PackageId` with no module tree, no HIR and no bodies. Its key string ([01](01-architecture.md) §1.6.2) is one of:

- `wit:wasi:<name>@<version>` for `Wasi`;
- `wit:dir:<package key>` for `Dir(P)`;
- `wit:gen:<package key>` for `Generated(P)`.

`view_origin(id)` maps a view package back to its unit, and is `None` for an ordinary package. The `DefId`s of a view package are defined by `wit_view(U)`. Their `DefPath`s use the `DefPathData` of [02](02-frontend-ast-hir.md) §2.8.2 with the SAI names of §16.4.2:

| Entity | `DefPath` | `DefKind` |
|---|---|---|
| interface `i` | `[Interface(i)]` | `Interface` |
| world `w` | `[World(w)]` | `World` |
| record, variant, enum, flags, resource, alias or used type `T` | `[Interface(i), TypeNs(T)]` | `Struct`, `Enum`, `Enum`, `Enum`, `Resource`, `Alias`, `Alias` |
| field `f` of a record | `[…, TypeNs(T), Field(f)]` | (a field) |
| case `C` of a variant or enum | `[…, TypeNs(T), Variant(C)]` | `Variant` |
| resource member `m` | `[…, TypeNs(R), AssocFn(m)]` | `AssocFn` |
| provided function of a flags type (§16.9.4) | `[…, TypeNs(F), AssocFn(empty \| all \| contains \| is_empty)]` | `AssocFn` |
| function `f` | `[Interface(i), ValueNs(f)]` | `Fn` |
| function `f` of a world | `[World(w), WorldItem { import, name: f }]` | `WorldFn` |

`WorldFn` is a new `DefKind` ([03](03-name-resolution-and-type-checking.md) §3.4.1). It is also used for the function items of local worlds (§16.8.2). `DefPathHash` is computed from the key string of the view package and the `DefPath`, as for any package. `DefIndex`es are assigned in `DefPathHash` order (impl/02 §2.8.2). The `DefId`s of a view are therefore stable across sessions and do not depend on query order.

```rust
pub struct WitView {
    pub unit: WitUnit,
    pub package: PackageId,                               // the view package
    pub interfaces: BTreeMap<SmolStr, ViewInterface>,     // key: the WIT name
    pub worlds: BTreeMap<SmolStr, ViewWorld>,
    pub defs: IndexVec<DefIndex, ViewDef>,                // every DefId, with the model entity it stands for
}
pub struct ViewInterface {
    pub def: DefId,
    pub wit: WitIfaceIdx,
    pub sai_name: Symbol,
    pub members: BTreeMap<Symbol, ViewMember>,            // SAI name → type or function
    pub shadowed: Vec<Shadowed>,                          // §16.4.5
    pub origin: Option<DefId>,                            // Generated units only: the SAI interface (§16.4.7)
}
pub struct ViewMember { pub def: DefId, pub kind: DefKind, pub item: WitItemIdx, pub unsupported: Option<Unsupported> }
pub struct ViewWorld { pub def: DefId, pub wit: WitWorldIdx, pub sai_name: Symbol, pub import_fns: BTreeMap<Symbol, DefId> }
pub struct Shadowed { pub wit_name: SmolStr, pub resource: Option<SmolStr>, pub winner: DefId }
```

`sai_ty::iface` answers the item queries of [03](03-name-resolution-and-type-checking.md) for view `DefId`s: `adt_def`, `type_of`, `fn_sig`, `generics_of`, `predicates_of` (always empty), `inherent_impls`, `flags_def` and `view_derives`. No query of impl/03 reads HIR for a view `DefId`.

### 16.4.2 Names

SAI names come from WIT names by spec/13 §13.8.3:

- interfaces, worlds, functions, members, fields and parameters get snake case;
- types and the cases of variants, enums and flags get upper camel case;
- an all-uppercase word is lowercased first;
- a SAI keyword gets the `r#` prefix.

When two items of one scope of spec/13 §13.8.5 get the same SAI name, one of them **wins**:

1. A constructor wins over a static function named `new` (spec/13 §13.7.4).
2. Otherwise the first item in WIT order wins.

The losing item gets no `DefId` and is recorded in `shadowed` (§16.4.5). The names in WIT are unique up to case, so in practice only rule 1 applies, but the general rule keeps the view total.

### 16.4.3 Types and signatures

Types map by spec/13 §13.9.2. The column "top-level parameter" applies to the parameters of functions and members, and the column "elsewhere" applies everywhere else:

1. **Records.** `adt_def` is `AdtKind::Record` with one variant. Its fields are in WIT order, `vis: Pub`, and have the "elsewhere" types.
2. **Variants.** `AdtKind::Enum`. A case without a payload has `VariantShape::Unit`. A case with a payload has a tuple shape with one field.
3. **Enums.** `AdtKind::Enum` with unit variants. Discriminants are the case indices.
4. **Flags.** `AdtKind::Flags` (§16.9.2).
5. **Resources.** `AdtKind::Resource` with no fields and the flag `IS_HANDLE` (§16.7.5).
6. **Aliases.** `type_of` of an `Alias` is the mapped target type. For a used type it is the view type that the `use` names. Aliases are transparent, as in SAI.
7. **Functions and members.**
   - `fn_sig` maps the parameters with the parameter column, the result with the "elsewhere" column, and no result to `()`.
   - A method has the first parameter `self: borrow Self`.
   - A constructor is the associated function `new` returning `R`, or `Result<R, E>` for a WIT constructor with a `result` type (spec/13 §13.7.4).
   - A static function has no `self`.
8. **Lifetimes.** View ADTs have no region parameters, because the "elsewhere" column contains only owned types. The borrowed parameter types `Str`, `Slice<T>` and `borrow R` are region positions of the signature (impl/03 §3.5.2).
9. **Unsupported positions.** The type of an item marked `unsupported` has the error type (impl/03 §3.5.6) at each unsupported position, without a diagnostic. Its uses are reported by §16.3.3.

**Nominal identity.** Each view entity has one `DefId` per session, because each unit is loaded once. A WIT type reached through a `use` of another unit is an `Alias` of the `DefId` in that unit. For example, `wasi:io/streams#input-stream` is the same SAI type in every view that uses it. A `Dir(P)` unit and a `Generated(P)` unit are different units. The view types of a hybrid package and the types of its provider are therefore distinct nominal types (spec/13 §13.5.1).

### 16.4.4 Resources

The members of a view resource `R` are inherent items of `R`. The `InherentItems` that `inherent_impls` returns for the simplified type of `R` (impl/03 §3.9.3) list them by SAI name, with the `DefId` of `R` itself in the impl position, because a view has no impl `DefId`. The calls of spec/13 §13.7.4 then need no special case:

- `R.new(…)` and `iface.R.new(…)` are type-relative paths (impl/03 §3.9.5);
- `value.m(…)` is found by the method probe.

A value of an `IS_HANDLE` type needs drop: `needs_drop` is `Yes`, as for every resource (impl/03 §3.8.7). Its drop glue calls the `resource.drop` import of the resource, which [07](07-component-lowering.md) §7.6.2 generates.

### 16.4.5 Shadowed items

A shadowed item (§16.4.2) is not accessible from SAI. `WIT_ITEM_SHADOWED` (warning) is reported by `package_scopes(P)` of each package `P` that **binds** the interface:

- through a module alias `use ns:pkg/iface`;
- or through an item list of such a `use`.

It is reported once per view interface with a non-empty `shadowed` list, at the first such binding in generation order. The message lists the shadowed WIT names (for example, "static function `new` of resource `fields`") and names the winning item.

An interface that `P` reaches only through world elaboration does not warn. When `P` implements an interface whose resource has a shadowed static `new`, the static function cannot be provided in SAI. The binding `type R = C` then gets `INTERFACE_IMPL_MISSING_ITEM` for it, with a note that the constructor shadows the static function `new` (§16.7.2).

### 16.4.6 Derived traits

The traits that a view type has (spec/13 §13.7.3, N-D-35) are provided as compiler impls. Two rows are added to the table of [03](03-name-resolution-and-type-checking.md) §3.8.5:

| Self type | Traits and nested obligations |
|---|---|
| a view enum (cases without payloads) | `Copy`, `Clone`, `PartialEq`, `Eq`, `Hash`; nothing nested |
| a view record or variant | `Clone` and `PartialEq` with `F: Tr` for every field and payload type `F`; `Copy` and `Eq` with `F: Tr` for every `F` |

A view resource has none of these traits. A view flags type has the impls of every flags type (§16.9.4). WIT types are not recursive, so the nested obligations terminate.

`view_derives(D)` is the set of traits among `Copy`, `Clone`, `PartialEq`, `Eq` and `Hash` for which the goal `D: Tr` holds in the empty `ParamEnv`. It is used only for printing (§16.4.9) and for the testing hook of §16.11.

When the solver selects one of these candidates, it is a `Builtin` source. `resolve_instance` (impl/03 §3.8.7) gives `Shim(ViewDerive { adt, method })` for it, where `method` is the trait method. The body of the shim is built by [04](04-mir.md) §4.3.3 with the semantics of a derived impl of spec/05 §5.12:

- `clone` clones the fields in order;
- `eq` compares the discriminants and then the fields in order, and stops at the first difference;
- `hash` hashes the discriminant.

### 16.4.7 Views of SAI source packages

A source package `Q` other than the root is seen by its dependents through the view of `Generated(Q)`. That view is built from `generated_wit_text(Q)` (§16.10.1) through the `WitHooks` of §16.1.2, and is parsed like every other unit. Its `ViewInterface::origin` is filled from `GeneratedText::origins`.

Wherever the resolution of a path in a package `P` yields an interface `I` of another source package `Q`, the resolver replaces the result by the view interface of `Generated(Q)` whose `origin` is `I`. This covers a `use` target, an item list, a module member, a re-export and a qualified chain. The SAI interface of `Q` is found first by spec/04 §4.3.2 step 2 and spec/04 §4.5.2, with the visibility rules of SAI. The replacement then gives `P` the WIT-normalized form of spec/13 §13.11.6, which is what a component sees.

If no view interface has the origin `I`, then `I` was omitted from the generated WIT because it has errors (§16.10.1). The path then gets `Res::Err` without a diagnostic, because the errors of `Q` are reported by the check queries of `Q`. An omitted item of a generated interface is likewise absent from `members`, and a path to it gets `Res::Err` without a diagnostic.

### 16.4.8 Entry into name resolution

`resolve_pkg_path` (impl/03 §3.3.3 item 1) implements spec/04 §4.3.2 with these rules for WIT:

1. **Step 1.** A `PKG_PATH` `wasi:p/…` whose `p` is an embedded WASI package resolves to the unit `Wasi { package: p }`. An active edge whose target has `wit_unit_of(target) = Dir(target)` resolves to that unit. In both cases resolution continues with step 3 on the view of the unit. A source dependency continues with step 2 on its SAI modules, followed by the replacement of §16.4.7.
2. **Step 3.** The segments after `ns:pkg/` are `s1 … sn`.
   - When `n = 1` and `s1` equals, byte for byte, the WIT name of an interface of the unit with its all-uppercase words lowercased (spec/13 §13.7.2), the target is `Res::Module(view interface)`. WIT names are unique up to case, so at most one interface matches.
   - When `s1` matches the WIT name of a world in the same way, the `use` is `INVALID_USE_TARGET`. A world is never bound by `use`, and `include` and `export world` name worlds directly (§16.8.1).
   - When `n = 0`, the `use` is `INVALID_USE_TARGET`.
   - When `s1` matches nothing, the `use` is `UNKNOWN_WIT_ITEM` (spec/13 §13.7.2), with the candidates of the edit-distance rule of [11](11-diagnostics.md).
   - When `n ≥ 2`, the `use` is `UNKNOWN_MODULE`.
3. **Binding name.** The module alias binds the SAI name of the interface (spec/13 §13.8.3). For example, `use wasi:http/outgoing-handler` binds `outgoing_handler`.
4. **Item lists.** `use wasi:io/streams.{InputStream}` looks up each name in `ViewInterface::members` by SAI name. A missing name is `UNRESOLVED_IMPORT` (impl/03 §3.3.3 item 4). A shadowed item is missing, and its warning is §16.4.5. A view member whose SAI name is a reserved type name (spec/04 §4.4.4, for example a WIT type `int`) can be reached only qualified, as in `iface.Int`. Binding it with an item list is `RESERVED_TYPE_NAME` (impl/03 §3.3.4 item 2).
5. **Qualified chains.** A qualified chain whose head is `Res::Module(view interface)` looks up the next segment in `members` (impl/03 §3.4.2). A further segment on a type follows impl/03 §3.9.5: a case, a flag constant (§16.9.3) or a member.

**Local interfaces.** `PackageScopes` (impl/03 §3.3.2) gains the field

```rust
pub interfaces: BTreeMap<LocalDefId, InterfaceScope>,
pub struct InterfaceScope { pub members: BTreeMap<Symbol, Binding> }
```

This field is filled in Phase 1 when the walk reaches an interface. Its members are the names of its type declarations, enum declarations, resources and function heads (spec/04 §4.8.1). A second item with the same name is `DUPLICATE_BINDING` on the later name.

The interface is bound in its module as `Res::Def(DefKind::Interface, def)`. A `use` or a path that names it as a module alias gets `Res::Module(def)`. In the owners of the interface's items and resource members, an **interface rib** with `members` is pushed between the item's own ribs and the module scope. Names therefore resolve in the order members, module scope, prelude (spec/04 §4.8.1). Interface members are not members of the module, so `use self.T` naming an interface member is `UNRESOLVED_IMPORT`.

### 16.4.9 Printing a view

`sai bindgen wit --show` ([13](13-cli.md) §13.6.19 item 2) prints a view as canonical SAI source. `sai_ty::iface::print_view(unit, interfaces)` builds a syntax tree of the view and formats it with the formatter of [14](14-formatter.md). It visits interfaces in WIT order and items and members in WIT order. Each interface is preceded by the line comment `// <ns>:<name>/<iface>@<version>`.

- A record prints as an interface record with all fields.
- A variant or enum prints as an `enum`, with payloads.
- A flags type prints as `@flags enum`.
- A resource prints as a `resource` with its members.
- An alias prints as `type T = U`, and a used type as `type T = other.T`.
- A function prints as a function head.
- Documentation prints as `///` comments, and `@deprecated` as the attribute.
- The derived traits of a record or variant print as `@derive(…)` in the order `Copy`, `Clone`, `PartialEq`, `Eq`, `Hash`. Enums and flags print without `@derive`, because their impls are implied.
- A shadowed item prints as a line comment `// shadowed: <wit name>`.
- An unsupported item prints as a line comment `// unsupported: <wit name>`.

---

## 16.5 HIR of interfaces and worlds

### 16.5.1 Data structures

The owner nodes `InterfaceItem` and `WorldItem` of [02](02-frontend-ast-hir.md) §2.8.5 are:

```rust
pub struct InterfaceItem<'hir> {
    pub owner_id: OwnerId,
    pub interface: LocalDefId,
    pub name: Name,
    pub kind: InterfaceItemKind<'hir>,
    pub span: Span,
}
pub enum InterfaceItemKind<'hir> {
    Struct { generics: Generics<'hir>, fields: &'hir [FieldDef<'hir>] },     // fields lowered with `vis: Pub`
    Alias { generics: Generics<'hir>, ty: &'hir Ty<'hir> },
    Refined { base: &'hir Ty<'hir>, clause: Span },                          // rejected by §16.6.1
    Enum { generics: Generics<'hir>, variants: &'hir [Variant<'hir>] },
    Resource { members: &'hir [OwnerId] },
    Fn { sig: FnSig<'hir>, generics: Generics<'hir> },
    ResourceMember { resource: LocalDefId, role: MemberRole, sig: FnSig<'hir>, generics: Generics<'hir> },
}
pub enum MemberRole { Constructor, Method, Static }

pub struct WorldItem<'hir> {
    pub owner_id: OwnerId,
    pub world: LocalDefId,
    pub kind: WorldItemKind<'hir>,
    pub span: Span,
}
pub enum WorldItemKind<'hir> {
    Import(WorldRef<'hir>),
    Export(WorldRef<'hir>),
    ImportFn { name: Name, sig: FnSig<'hir> },
    ExportFn { name: Name, sig: FnSig<'hir> },
    Include(WorldRef<'hir>),
}
pub enum WorldRef<'hir> {
    Pkg { hir_id: HirId, path: &'hir str, span: Span }, // a PKG_PATH, resolved by §16.8.1
    Mod(Path<'hir>),                                    // a ModPath
}
```

The reference implementation stores the package path in the query-session HIR arena; this uses `&'hir str` in place of `SmolStr` so world-item nodes remain copyable.

`MemberRole` follows the classification in spec/13 §13.2.4:

- a member whose first parameter has `is_self` is a `Method`;
- a member named `new` without `self` is the `Constructor` only when its return type is exactly `Self` or the resource's own name;
- every other member is `Static`.

In particular, `new` returning `Result<Self, E>` is `Static` for a declared interface resource.

The generics of an interface item are lowered so that §16.6.1 can report them, and they are otherwise ignored.

### 16.5.2 Bodies

Interface items and world items have no bodies. The parser reports a `{` after a function head inside an `interface`, an interface `resource` or a `world` as `INTERFACE_FN_BODY`, at the block. It parses the block and discards it into an `ERROR` node, as for `UNEXPECTED_FN_BODY` in an `extern` block ([02](02-frontend-ast-hir.md) §2.5.4). The owner has no `BodyId`, and `typeck` is never asked for it. `UNEXPECTED_FN_BODY` keeps its meaning for `extern` blocks only (N-K-23).

### 16.5.3 Lowering and DefPaths

- An interface item gets `[…, Interface(i), TypeNs(T)]` or `[…, Interface(i), ValueNs(f)]`. Its fields and variants get `Field` and `Variant` components. A resource member gets `[…, Interface(i), TypeNs(R), AssocFn(m)]`.
- A world function item gets `WorldItem { import, name }` with its function name. A world `import` or `export` gets `WorldItem { import, name }` with the text of its `WorldRef`: the `PKG_PATH` text, or the `ModPath` segments joined with `.`.
- A world `include` gets the new component `WorldInclude(Symbol)` with the text of its `WorldRef` (N-K-27). It cannot collide with an `import` of the same text.
- Two equal components under one parent get the disambiguator of impl/02 §2.8.2.
- **Flags functions.** For every `enum` with a written `@flags` attribute, whether it is valid or not, the def collector creates the four `DefPath`s `[…, TypeNs(F), AssocFn(empty)]`, `AssocFn(all)`, `AssocFn(contains)` and `AssocFn(is_empty)`, with `DefKind::AssocFn` and no HIR owner. Their signatures come from §16.9.4. Creating them also for an invalid flags type avoids cascading errors. A program with `INVALID_FLAGS_ENUM` never reaches MIR.

---

## 16.6 Interface checks

### 16.6.1 Item restrictions

`interface_check(I)` checks the items of a local interface `I` in source order. For each item, it applies the rows of this table in order. The table follows the restrictions of spec/13 §13.2.2:

| Construct | Code | Reporter and location |
|---|---|---|
| generic parameters or a `where` clause on an item | `GENERIC_INTERFACE_ITEM` | `interface_check`, at the generic parameter list or the `where` clause |
| a refinement clause on an alias (`Refined`) | `WIT_INCOMPATIBLE_TYPE` | `interface_check`, at the clause |
| `borrows`, `requires`, `ensures`, `unsafe` | `INTERFACE_FN_CLAUSE` | validation ([02](02-frontend-ast-hir.md) §2.5.12) |
| `effects` | `EFFECTS_ON_INTERFACE_FN` | [08](08-effect-and-capability-analysis.md) |
| `async` | `FEATURE_NOT_IMPLEMENTED` before Stage 5 | [02](02-frontend-ast-hir.md) §2.9.15 |
| a body | `INTERFACE_FN_BODY` | the parser (§16.5.2) |
| `@flags` on an enum with payloads or discriminants, or with more than 32 variants | `INVALID_FLAGS_ENUM` | `adt_def` (§16.9.1) |
| an explicit discriminant `= INT` | `WIT_EXPLICIT_DISCRIMINANT` | `interface_check`, at the variant; not for an enum that got `INVALID_FLAGS_ENUM` |
| a record without fields, an enum without variants, a flags type without flags | `WIT_EMPTY_TYPE` | `interface_check`, at the item name |
| a case with a tuple payload of two or more types, or a record payload | `WIT_INCOMPATIBLE_TYPE` | `interface_check`, at the payload |
| `@repr` on an interface type | `WIT_INCOMPATIBLE_TYPE` | `interface_check`, at the attribute |
| a resource member with an invalid receiver or constructor (§16.6.2) | `WIT_INCOMPATIBLE_TYPE` | `interface_check` |
| a type without a WIT mapping (§16.6.3) | `WIT_INCOMPATIBLE_TYPE` | `interface_check` |
| a name without a WIT name (§16.6.3) | `WIT_NAME_NOT_REPRESENTABLE` | `interface_check`, at the name |
| two names with the same WIT name (§16.6.4) | `WIT_NAME_COLLISION` | `interface_check`, at the later name |

`@derive` is permitted on interface records and enums (spec/13 §13.2.2). `@derive` together with `@flags` is `CONFLICTING_ATTRIBUTES` (spec/15 §15.10 item 3), reported by attribute validation.

### 16.6.2 Resource members

For each member of an interface `resource R` (spec/13 §13.2.4):

- A `Method` must have the receiver `self: borrow Self` or `self: borrow mut Self`, where `Self` and `R` are the same after alias expansion. Any other receiver type, including `self: Self`, is `WIT_INCOMPATIBLE_TYPE` at the receiver.
- A member is a `Constructor` only when it is named `new`, has no `self` parameter, and returns exactly `Self` or `R`. A `new` with any other or missing return type is `Static`, following spec/13 §13.2.4.
- A `Static` member has no extra rule.

The boundary types of members are then checked by §16.6.3. The receiver is not a boundary type, because WIT does not print it (spec/13 §13.11.2).

### 16.6.3 WIT compatibility and names

**Boundary type conversion.** One function converts a HIR type in a boundary position to a WIT type, or reports why it cannot:

```rust
pub fn boundary_type(db: &dyn TyDb, owner: OwnerId, ty: &hir::Ty, pos: Position) -> Result<WitType, Incompatible>;
pub enum Position { Param, Result, Nested }
pub struct Incompatible { pub span: Span, pub reason: IncompatibleReason }
```

Both the WIT-compatibility check (R-08, spec/13 §13.9.3) and generation (§16.10.1) use this function, so the two cannot disagree.

1. It applies spec/13 §13.9.1 recursively. Components of a type are in position `Nested`.
2. Paths use the **original** resolution of each path: `ResolvedOwner::redirected` (§16.7.4) where it has an entry, otherwise `paths`. A bound resource `R` is therefore still `R` and not its implementation `C`, which is a nominal type outside an interface.
3. Aliases outside interfaces are expanded. An alias of an interface stays a named WIT type.
4. The type of a local interface refers to the generated unit of the package. A view type refers to its unit.
5. The function stops at the first incompatible component in pre-order. It reports the span of the smallest HIR type that contains that component, with the reason from spec/13 §13.9.3 (for example "`USize` has no WIT type; use `U32` or `U64`").

`interface_check` calls `boundary_type` for the boundary positions of spec/13 §13.9.3:

- the parameters (`Param`) and result (`Result`) of every function and member;
- every field;
- every payload;
- every alias target (`Nested`).

Each `Err` is `WIT_INCOMPATIBLE_TYPE`, one per top-level position. `world_check` does the same for world function items (§16.8.4). A return type `()` has no result. A written `Option<()>` or `()` in a parameter is incompatible.

**Names.** Every SAI name that crosses the boundary is converted by spec/13 §13.8.4:

- interfaces and worlds;
- items, fields, cases and flags;
- members and parameters other than `self`.

A name that does not match the patterns there is `WIT_NAME_NOT_REPRESENTABLE` at the name, with the suggested name of spec/13 §13.8.4 when one exists. A WIT keyword is not an error; it is printed with `%`. A parameter without a name (a pattern parameter or `_`) is `WIT_NAME_NOT_REPRESENTABLE` at the parameter.

### 16.6.4 WIT names and collisions

`WIT_NAME_COLLISION` is reported when two distinct entities of one scope have the same WIT name. It is reported at the later entity in generation order, with a note at the earlier one. Two entities with equal SAI names in one SAI scope are already `DUPLICATE_BINDING` (spec/04 §4.4.3) or `WORLD_DUPLICATE_ITEM` (§16.8.3) and are not reported again. The scopes are those of spec/13 §13.8.5, plus one more:

1. **The package scope** (N-K-24). The interfaces and worlds of one package share one WIT namespace. Interfaces `a.net` and `b.net` in two modules therefore collide.

   This scope is checked by `package_wit_names(P)`:

   ```rust
   pub struct PackageWitNames {
       pub names: BTreeMap<LocalDefId, SmolStr>,     // every interface and world → its WIT name
       pub diagnostics: Vec<Diagnostic>,
   }
   ```

   `package_wit_names(P)` also reports `WIT_NAME_NOT_REPRESENTABLE` for the names of interfaces and worlds. An interface or world whose name failed has no entry. Its owner is poisoned for generation (§16.10.1).
2. **Inside an interface**, the types and functions of the interface, the members of each resource, the fields of each record, the cases of each enum, and the parameters of each function. These are checked by `interface_check`.
3. **Inside a world**, the imports and the exports. These are checked by `world_check` on the elaborated world (§16.8.3).

For imported WIT, collisions are resolved by §16.4.2 and give at most `WIT_ITEM_SHADOWED`.

### 16.6.5 Uses between interfaces

`interface_uses(P)` computes the edges between local interfaces:

```rust
pub struct InterfaceUses {
    pub uses: BTreeMap<LocalDefId, Vec<(LocalDefId, Span)>>,   // interface → the local interfaces it uses, first reference each
    pub cyclic: BTreeSet<LocalDefId>,
    pub diagnostics: Vec<Diagnostic>,
}
```

An interface `I` uses `J ≠ I` when a path in an item of `I` resolves, by its original resolution (§16.6.3 rule 2), to an item of `J`. The edges are listed in the order of their first reference. A use of the interface's own items is not an edge.

The query runs Tarjan's algorithm over the interfaces in generation order. Each strongly connected component with more than one interface is one `INTERFACE_USE_CYCLE` (spec/13 §13.2.3). It is reported at the first reference, in source order, from the first interface of the component in generation order to another member of the component. A note is added for one reference of each further edge of the cycle. Every interface of the component goes into `cyclic`, and its items are poisoned for generation (§16.10.1).

The uses of view interfaces are not edges here. Generation finds them itself (spec/13 §13.11.3).

---

## 16.7 Interface implementations

### 16.7.1 Collecting impls

```rust
pub struct InterfaceImpls {
    pub impls: Vec<InterfaceImpl>,                          // generation order
    pub by_interface: BTreeMap<DefId, LocalDefId>,          // interface → its (first) impl
    pub redirect: BTreeMap<DefId, DefId>,                   // §16.7.4
    pub bindings: BTreeMap<DefId, DefId>,                   // bound resource R → its implementation C
    pub diagnostics: Vec<Diagnostic>,
}
pub struct InterfaceImpl {
    pub impl_def: LocalDefId,
    pub interface: Option<DefId>,                           // None: the target is not an interface
    pub fns: BTreeMap<DefId, LocalDefId>,                   // interface function → implementing function
    pub resources: BTreeMap<DefId, (DefId, Span)>,          // R → (C, the span of `type R = C`)
    pub unsupported: Vec<DefId>,                            // functions skipped by step 3
}
```

`interface_impls(P)` reads only:

- the `resolve_owner` results of the impl items themselves;
- the `resolve_owner` results of their `type R = C` items;
- HIR;
- the views.

The redirect is never applied to these owners (§16.7.4), so the query cannot be part of a cycle with the redirect. It processes the impls with `ImplKind::Interface` in generation order:

1. **Target.** The target path is resolved by `resolve_owner` of the impl item. If it resolves to something other than an interface, the impl gets `IMPL_TARGET_NOT_INTERFACE` (spec/13 §13.3.1) at the path. This code takes the place of the `WRONG_NAME_KIND` of spec/04 §4.8.1. A path that does not resolve has only its resolution error. In both cases the impl's items are not checked.
2. **Duplicate impls.** If the interface already has an impl in `by_interface`, the later impl gets `DUPLICATE_INTERFACE_IMPL` at its target path, with a note at the earlier one. Its items are not checked.
3. **Unsupported functions.** For each function of the interface marked `unsupported` (§16.3.3), in WIT order, the impl gets `WIT_UNSUPPORTED_FEATURE` (or `FEATURE_NOT_IMPLEMENTED` for an `async` item before Stage 5) at the target path. Such a function is ignored by the next steps.
4. **Matching.** Each item of the impl is matched by name: a function `f` to the interface function `f`, and `type R = C` to the resource `R`.
   - An impl item whose name the interface does not declare with that kind is `INTERFACE_IMPL_EXTRA_ITEM` at the item name.
   - A second impl item with the name of an earlier one is also `INTERFACE_IMPL_EXTRA_ITEM`, at the later name, with a note at the earlier one (N-K-26).
   - An interface function or resource without an impl item is `INTERFACE_IMPL_MISSING_ITEM` at the target path, one diagnostic per item in interface order.
5. **Resource bindings.** `type R = C` is resolved by `resolve_owner` of its own owner, and checked by §16.7.3.

Generic parameters of an implementing function, the conformance of signatures and the members of bound resources are checked later by `interface_impl_check` (§16.7.2). Those checks need `fn_sig` and `inherent_impls`, which depend on the redirect.

### 16.7.2 Items and conformance

`interface_impl_check(X)` checks one interface impl `X` that §16.7.1 matched. For each pair of an interface function `F` and an implementing function `G`:

1. Generic parameters or a `where` clause on `G` are `GENERIC_INTERFACE_ITEM` (spec/13 §13.3.1), at the generics of `G`. The pair is not compared further.
2. The **expected signature** is `fn_sig(F)` with every bound resource `R` of the package replaced by its `C` (`InterfaceImpls::bindings`).
   - For a local interface, `fn_sig(F)` already contains `C`, because the redirect also applies in the owners of interface items.
   - For a view interface, the replacement is done here.
3. The two signatures **conform** under spec/13 §13.3.2 when all of these hold:
   - they have the same number of parameters;
   - every parameter type is equal after alias expansion, with regions erased;
   - the return types are equal;
   - they agree on `async`.

   Parameter names and `var` are not compared. `G` may add `effects`, `requires` and `ensures`. `unsafe` on `G` is `INTERFACE_FN_CLAUSE` from validation and is not checked again.
4. A difference is `INTERFACE_IMPL_SIGNATURE_MISMATCH`. It is reported at the first differing parameter type, then the return type, and otherwise at the name of `G` (for a different count or `async`). A note gives the expected signature in SAI syntax.

For each bound resource `R ↦ C`:

1. For every member `m` of `R` in member order, `C` must have an inherent function named `m` (`inherent_impls` of `C`). Otherwise the binding gets `INTERFACE_IMPL_MISSING_ITEM` at `type R = C`, one diagnostic per member. A shadowed static `new` (§16.4.5) is always missing and gets the note of §16.4.5.
2. Each present member is compared like a function, with `Self` meaning `C` on both sides. The receiver types must also be equal: `borrow Self` and `borrow mut Self` differ. A difference is `INTERFACE_IMPL_SIGNATURE_MISMATCH` at the binding `type R = C`, with a related location at the member of `C`. The members of `C` are ordinary inherent functions that may be declared anywhere in the package. The binding is the one place that names both sides.

Functions exposed through an exported interface need an `effects` clause, which [08](08-effect-and-capability-analysis.md) checks (`MISSING_EFFECTS_DECLARATION`).

### 16.7.3 Resource bindings

For `type R = C` in an impl of an interface that declares the resource `R`, these are `INVALID_RESOURCE_BINDING` (spec/13 §13.3.3) at `C`, checked in this order:

1. `C` is not a resource declared at module level in the current package, or it is generic. A resource declared in an interface is abstract and cannot be an implementation.
2. `C` is already bound by an earlier binding of the package in generation order. A note is added at the earlier binding.
3. **External restriction** (N-K-10). `R` is a view resource, and `R` occurs, at any depth, in a record field, a variant payload or an alias target of a WIT interface visible to the package. Aliases include the aliases created by `use`. The visible interfaces are those of the WASI units and of the closures of the units of the package's direct edges. A note names the first occurrence in WIT order.

   Such a type denotes the imported resource in every view. It would therefore carry a handle of the imported `R` where the implementation expects a `C`, which the redirect cannot repair. spec/13 §13.3.3 states this rule.

A failed binding is not entered in `bindings`, and the `R` stays unbound.

### 16.7.4 The redirect

Spec/13 §13.3.4 makes a reference to an implemented interface item mean the implementation. The resolver realizes this as a redirect of resolutions. `ResolvedOwner` (impl/03 §3.4.1) gains

```rust
pub redirected: HashMap<HirId, DefId>,     // path → its original resolution, where the redirect changed it
```

In every owner of a package `P`, except the interface impl items and their `type R = C` owners, `resolve_owner` applies `interface_impls(P).redirect` after a path resolves:

1. An interface function `F` that `P` implements becomes `Res::Def(DefKind::Fn, G)`.
2. A resource `R` (abstract or view) that `P` binds becomes `Res::Def(DefKind::Resource, C)`. `Self` in the members of a local resource `R` denotes `C`.
3. In each case the original `DefId` is stored in `redirected` under the path's `HirId`.

An interface function that `P` does not implement is not redirected.

The redirect also applies in the owners of interface items. The signatures of a local interface therefore mention `C`. Generation (§16.10.1) and `boundary_type` (§16.6.3) read `redirected` to recover `R`.

**Imported calls.** A call whose callee, after the redirect, is still one of the following is a call of an import:

- an interface function;
- a view function;
- a world function (§16.8.2);
- a member of an `IS_HANDLE` resource.

[08](08-effect-and-capability-analysis.md) adds it to the imported interface functions, and [07](07-component-lowering.md) generates its import glue. `wit_fn_of(def)` gives both chapters the WIT name of such a callee, and `None` for any other `DefId`:

```rust
pub struct WitFnRef {
    pub owner: WitFnOwner,                    // Interface(WitIfaceRef) | World(DefId)
    pub name: SmolStr,                        // the WIT function name
    pub member: Option<(SmolStr, WitMemberKind)>,   // the resource and the member kind
}
```

The `WitIfaceRef` of a local interface of the root refers to `Generated(root)`, which is the package that the component's WIT contains ([07](07-component-lowering.md) §7.1.1).

### 16.7.5 Opaque resources

`AdtFlags` (impl/03 §3.6.4) gains `IS_HANDLE`. It is set for view resources and for resources declared in local interfaces. A record construction `R(f: e, …)` (a `Call` whose callee resolves to a record type, impl/03 §3.7.12) whose callee resolves, after the redirect, to an `IS_HANDLE` type is `OPAQUE_RESOURCE_CONSTRUCTION` (spec/13 §13.7.4), at the callee path. It is reported by `typeck` in place of the field checks of impl/03 §3.7.12, and the arguments are still checked by synthesis. Constructors are called as functions (§16.4.4).

---

## 16.8 Worlds

### 16.8.1 World references

`resolve_owner` of a world item resolves its `WorldRef` and records the result in `paths` at the `WorldRef`'s `HirId`:

1. **`Pkg`.** A `PKG_PATH` resolves like a `use` path (§16.4.8), except that spec/04 §4.3.2 step 2.2 and step 3.1 may also find a world. This is the only way to name a world of another package.
2. **`Mod`.** A `ModPath` resolves in the module scope of the world's module (spec/04 §4.5.2). It may name a local interface, a local world, or a module alias of an external interface bound by `use` (spec/03 §3.9). Worlds of other packages are never bound by `use` (§16.4.8 step 2), so a `ModPath` names only local worlds.
3. **Kinds.**
   - `import` and `export` require an interface, and `include` requires a world. A world in `import` or `export`, or an interface in `include`, is `INVALID_WORLD_ITEM` (spec/13 §13.4.1) at the `WorldRef`.
   - A whole package (`wasi:http`) in `include` is also `INVALID_WORLD_ITEM`. In `import` or `export` it is `WORLD_IMPORT_NEEDS_INTERFACE`, which validation reports ([02](02-frontend-ast-hir.md) §2.5.12).
   - Any other entity (a module, a type, a function) is `WRONG_NAME_KIND`.
4. **Nothing found.** In a WIT package, a name that matches nothing is `UNKNOWN_WIT_ITEM`. In a SAI package, it is the error of spec/04 §4.3.2 step 2 (`UNKNOWN_MODULE`, `PRIVATE_ITEM`).
5. **Unsupported worlds.** An `include` of a world marked `unsupported` (§16.3.3) is `WIT_UNSUPPORTED_FEATURE` at the `WorldRef`.

### 16.8.2 World functions

The function items of a world are function heads with `DefKind::WorldFn`. `fn_sig` converts them like interface functions, and `world_check` checks their boundary types and names (§16.8.4).

**World members.** In `w.f(…)`, where `w` resolves to a world of the current package and exactly one further segment follows, the segment is looked up in `world_import_fns(w)` (spec/13 §13.4.1, spec/04 §4.5.2):

```rust
// world_import_fns(w): SAI name → WorldFn DefId of an `import fn`
```

The map holds the `import fn` items of `w` and of the worlds that `w` includes, in elaboration order, and the first item of each name wins. The includes are followed depth-first with a visited set, so a cycle (§16.8.3) does not loop. An external included world contributes the `WorldFn`s of its view. The query reads only `resolve_owner` of the include items, so it cannot form a cycle with the owner that asks for it.

- A name that is not in the map is `UNRESOLVED_NAME` at the segment. If the world has an `export fn` of that name, a note says that an exported world function is called through its implementation.
- A further segment, or a world of another package, is `WRONG_NAME_KIND`, which is spec/04 §4.5.2 step 5 for every other world.

### 16.8.3 Elaboration

```rust
pub struct ElaboratedWorld {
    pub world: DefId,
    pub imports: Vec<ElabItem>,
    pub exports: Vec<ElabItem>,
    pub diagnostics: Vec<Diagnostic>,          // local worlds only; returned by world_check
}
pub struct ElabItem { pub key: SmolStr, pub kind: ElabKind, pub origin: ElabOrigin }
pub enum ElabKind { Interface(DefId), Func(DefId) }
pub enum ElabOrigin {
    Written(HirId),                            // an item of this world
    Included { include: HirId },               // reached through this include item
    Implicit { cause: HirId, used_by: DefId }, // added by spec/13 §13.4.2 steps 2 and 3
    External,                                  // an item of a view world
}
```

The **key** of an item is its WIT name in the world:

- `ns:pkg/iface`, without version, for an interface of another package;
- the WIT name for a local interface or a function.

The **identity** of an item is its `DefId`: the view `DefId` for external interfaces and the local `DefId` for local ones.

`elaborated_world(w)` follows spec/13 §13.4.2:

1. **External worlds.** For a world of a view, the result is the `wit-parser` elaboration in WIT order, with origin `External`.
2. **Local worlds, step 1.** The items are taken in source order. Each `include` is replaced, in place, by the elaborated items of the included world, with origin `Included`. Includes into a world of a cyclic component (below) contribute nothing.
3. **Step 2.** For every exported interface, in order, the interfaces it uses, transitively and in first-reference order, are appended to the imports with origin `Implicit`, unless they are exported. The uses of a local interface are its `interface_uses` edges plus the view interfaces its items refer to. The uses of a view interface are the targets of its `Used` items.
4. **Step 3.** For every imported interface, its uses are appended the same way.
5. **Duplicates.** Items with the same direction and key are compared (N-K-14):
   - With the same identity, where at least one occurrence comes from an include or is implicit, they are merged and the first position is kept.
   - With the same identity and both written directly in this world, the later item is `WORLD_DUPLICATE_ITEM`.
   - With different identities, the item is always `WORLD_DUPLICATE_ITEM`.
   - An interface that is both imported and exported is `WORLD_DUPLICATE_ITEM`. This includes an interface imported by step 3 because an imported interface uses an exported one; the note then names the imported interface that uses it.

   The diagnostic is at the **reporting location** of the later occurrence, with a note at the earlier one. The reporting location is the item for `Written`, the include item for `Included`, and `cause` for `Implicit`. An item that got the diagnostic is dropped.

Diagnostics are produced only for local worlds and only for occurrences whose reporting location is in `w`. Duplicates inside an included local world are reported by that world's own check, and its elaborated items are already free of them.

**Include cycles.** `world_includes(P)` builds the graph of local worlds with an edge for each `include` of a local world. A strongly connected component with more than one world, or a world that includes itself, is one `WORLD_INCLUDE_CYCLE` (spec/13 §13.4.2). It is reported at the first include item, in source order, of the first world of the component in generation order, with notes at the other includes of the cycle. `WorldIncludes::cyclic` lists the worlds of these components.

### 16.8.4 Exports of a world

`world_check(w)` returns the diagnostics of `elaborated_world(w)` and adds these checks:

1. The boundary types and names of the world's function items, and the name collisions of its elaborated imports and exports (§16.6.3, §16.6.4).
2. **Implementations** (spec/13 §13.4.1, N-K-16). Only the items written directly in `w` are checked:
   - An exported interface `I` needs `interface_impls(P).by_interface[I]`.
   - An `export fn f` needs a function `f` among the members `X(root)` of the root module of the world's package (spec/04 §4.8.2). Its signature must conform to the world function by §16.7.2 step 3, and a difference is `INTERFACE_IMPL_SIGNATURE_MISMATCH` at the implementing function.
   - A missing implementation is `WORLD_EXPORT_UNIMPLEMENTED` at the export item.
3. **Included external worlds.** The exports of an included external world are checked the same way, at the include item. The message names the export.
4. **Included local worlds** are checked by their own `world_check` and are not checked again.

The exports of an external `export world` are checked by `export_world` (§16.8.5).

### 16.8.5 The export world

`export_world(P)` resolves the `export world` of `wit_contract(P)` by package/01 §1.9 item 3:

```rust
pub struct ExportWorld { pub world: Option<DefId>, pub diagnostics: Vec<Diagnostic> }
```

1. **A local world** has the form of `WORD` segments joined by `/`.
   - In a source or hybrid package, the segments before the last are a module path from the root module, and the last segment is a world of that module.
   - In an artifact package, the single segment is converted by spec/13 §13.8.4 and looked up among the worlds of `Dir(P)`.
2. **An external world** `ns:pkg/w` is looked up in this order:
   1. the WASI unit `ns:pkg`, when `ns` is `wasi`;
   2. the unit of the direct active edge with the identity `ns:pkg`;
   3. the unit of the only node of the graph with that identity.

   If several nodes have the identity and none is a direct edge, the reference is ambiguous. The world must then be named through a direct dependency (N-K-15).
3. **Errors.**
   - No world, or an ambiguous reference, is `UNKNOWN_EXPORT_WORLD` (spec/13 §13.5.1) at the value of `export world` in `sai.package`. For an ambiguous reference, a note lists the versions.
   - A world marked `unsupported` is `WIT_UNSUPPORTED_FEATURE` at the same place.
4. **External exports.** For an external world, the exports of its elaboration are checked by §16.8.4 item 2. Diagnostics are at the `export world` value.

### 16.8.6 The component world and the notes

`component_world((root, command))` computes `W'` of spec/13 §13.5.3. `command` holds exactly when the target is a WASI target and `entry_point(root)` is `Some` ([03](03-name-resolution-and-type-checking.md) §3.6.9):

```rust
pub struct ComponentWorld { pub imports: Vec<ElabItem>, pub exports: Vec<ElabItem>, pub diagnostics: Vec<Diagnostic> }
```

- It starts from the elaboration of `export_world(root)`, or from an empty world when there is none.
- For a command, it adds `include wasi:cli/command` at the WASI version of the target, with the duplicate rule of §16.8.3.
- When the export world already exports `wasi:cli/run` and the root has `main`, the build is `WORLD_DUPLICATE_ITEM` at the value of `export world`.

[08](08-effect-and-capability-analysis.md) §8.7.2 reads `W'` for its roots, and [07](07-component-lowering.md) §7.1.1 reads it as its input `W'`. The import set `Imp` is defined by spec/13 §13.5, not here.

The `info` diagnostics of spec/13 §13.6.2 and §13.6.4 are session-level queries (N-K-28). The driver asks for them only in the builds that §16.1.3 item 4 names, so each is reported at most once per build:

- `export_fn_note(root)` is `EXPORT_FN_NOT_APPLIED` when the root has at least one `export fn`. It is reported at the first export function in the order of `package_scopes(root).exports`.
- `export_world_note(root)` is `EXPORT_WORLD_NOT_APPLIED` when the root declares `export world`. It is reported at that declaration.

---

## 16.9 Flags

### 16.9.1 Validity

`adt_def` of an `enum` with `@flags` applies spec/13 §13.9.4 and spec/15 §15.10. It reports at most one `INVALID_FLAGS_ENUM` per enum, for the first rule that applies, in this order:

1. The enum is not declared inside an interface. Reported at the attribute.
2. A variant has a payload. Reported at the first such payload.
3. A variant has an explicit discriminant. Reported at the first such variant. `WIT_EXPLICIT_DISCRIMINANT` is then not reported for the enum (§16.6.1).
4. The enum has more than 32 variants. Reported at the name of the 33rd variant.

After `INVALID_FLAGS_ENUM`, `adt_def` treats the enum as an ordinary enum (`AdtKind::Enum`), so the later checks of the program see a well-formed type.

An enum with `@flags` and no variants is also treated as an ordinary empty enum. `interface_check` reports it as `WIT_EMPTY_TYPE` (spec/13 §13.2.2). A flags type therefore always has between 1 and 32 flags (N-K-19).

### 16.9.2 Representation

A valid flags type has `AdtKind::Flags`. Its `AdtDef` has one variant with one hidden field `bits` of the **representation type**:

- `U8` for up to 8 flags;
- `U16` for up to 16 flags;
- `U32` for up to 32 flags.

The field's name is a symbol that is not an identifier, so it cannot be written. A source access `f.bits` is `UNKNOWN_FIELD` (spec/06 §6.6.1). The flag names are not `VariantDef`s.

```rust
pub struct FlagsDef {
    pub adt: DefId,
    pub flags: Arc<[(Symbol, Span)]>,     // declaration order = WIT order; flag i is bit i
    pub repr: IntTy,                      // U8 | U16 | U32
}
```

`flags_def(D)` is `Some` exactly for a type whose `adt_def` is `AdtKind::Flags`. This includes the flags types of views.

### 16.9.3 Constants

`F.X`, with `F` a flags type and `X` one of its flags, is a constant of type `F` (spec/13 §13.9.4). Type-relative path resolution (impl/03 §3.9.5) records it as `TypeDependentDef::FlagsConst { flags: F, bit: i }` (impl/03 §3.15.1). Its value is `ConstValue::Aggregate { variant: None, fields: [Int(1 << i, repr)] }` (impl/03 §3.7.15).

- A name that is not a flag of `F` and not a provided function is `UNKNOWN_VARIANT` (spec/05 §5.13.6).
- An implicit member `.X` whose expected type is a flags type is `IMPLICIT_MEMBER_NOT_ENUM` (spec/06 §6.4.3). A flags type is not an enum for this rule, and flags are always written with their type (N-K-20).
- `F.X` is allowed in a constant expression. The operators and provided functions of §16.9.4 are calls, and in a constant initializer they are `NOT_CONST_EXPRESSION` (spec/06 §6.27).

### 16.9.4 Operators and provided functions

Two rows are added to the compiler-provided impls of [03](03-name-resolution-and-type-checking.md) §3.8.5:

| Self type | Traits and nested obligations |
|---|---|
| a flags type `F` | `Copy`, `Clone`, `PartialEq`, `Eq`, `Hash`, and `BitOr`, `BitAnd`, `BitXor` with `Rhs = F` and `Output = F`; nothing nested |

The provided functions of spec/13 §13.9.4 are the four synthetic inherent items of §16.5.3:

| Function | Signature |
|---|---|
| `empty` | `fn empty() -> Self` |
| `all` | `fn all() -> Self` |
| `contains` | `fn contains(self: Self, other: Self) -> Bool` |
| `is_empty` | `fn is_empty(self: Self) -> Bool` |

`inherent_impls` lists these functions for every flags type, with the flags type in the impl position. A user inherent item with one of these names is `DUPLICATE_INHERENT_ITEM` (spec/05 §5.9.2) at the user item.

`resolve_instance` gives `Shim(Flags { flags, op })` for the methods of these impls and for the provided functions:

```rust
pub enum FlagsOp { Or, And, Xor, Clone, Eq, Hash, Empty, All, Contains, IsEmpty }
```

### 16.9.5 Patterns

A pattern that names a flag of a flags type is `FLAGS_PATTERN` (spec/13 §13.9.4) at the pattern, reported by `typeck` when it checks the pattern against the scrutinee type. This covers `F.X`, `F.X(…)` and `.X`.

Wildcard and binding patterns are allowed. `check_match` (impl/03 §3.12) treats a flags type as a type without constructors: only a wildcard or a binding covers it.

### 16.9.6 MIR and layout

The bodies of `Shim(Flags { flags, op })` are built by [04](04-mir.md) §4.3.3 from integer operations on `bits`. Here `n` is the number of flags and `M` is the mask `2^n − 1` of the representation type:

| Operation | Body |
|---|---|
| `Or`, `And`, `Xor` | the value with `bits = (a.bits ⊕ b.bits) & M`, for the integer operator `⊕` |
| `Clone` | a copy |
| `Eq` | `(a.bits & M) == (b.bits & M)` |
| `Hash` | `Hash.hash` of `a.bits & M` as a value of the representation type |
| `Empty` | the value with `bits = 0` |
| `All` | the value with `bits = M` |
| `Contains` | `(b.bits & !a.bits & M) == 0` |
| `IsEmpty` | `(a.bits & M) == 0` |

Bits `n` and above have no meaning. A value that comes across the component boundary can have them set, because [07](07-component-lowering.md) §7.4.7 lifts by truncating to the size and lists of flags can share the caller's buffer. Every operation therefore ignores them, and every operation that produces a value clears them (N-K-18). When `n` is 8, 16 or 32, `M` has every bit set and the `& M` is omitted.

The memory layout of a flags type is its representation type: an unsigned integer of 1, 2 or 4 bytes in which bit `i` is flag `i`. The rule is in [06](06-wasm-lowering-and-layout.md) §6.4.18 (N-F-90). It is also the Canonical ABI layout, so [07](07-component-lowering.md) §7.2.4 and §7.4.7 lift and lower a flags value without conversion.

---

## 16.10 Generated WIT

### 16.10.1 Building the model

`generated_wit(P)` builds the WIT model of the unit `Generated(P)` by spec/13 §13.11:

```rust
pub struct GeneratedWit {
    pub package: Arc<WitPackage>,
    pub origins: Arc<[(SmolStr, DefId)]>,
    pub complete: bool,
}
```

1. **Package.** The package name is the identity of `P`, and its version is the version of `P` (spec/13 §13.11.4).
2. **Interfaces.**
   - Every local interface is included in generation order, under its name from `package_wit_names(P)`.
   - Its items are included in source order.
   - Types come from `boundary_type` (§16.6.3), which uses the original resolutions, so a bound resource appears as `R`.
   - Resource members keep their roles.
   - Documentation comments and `@deprecated` are carried over.
   - The `use` statements are derived from the references, one per referenced interface (spec/13 §13.11.3).
3. **Worlds.** All local worlds are included after all interfaces, in generation order. Their items are taken as written, not elaborated (spec/13 §13.11.1). An `include` prints as `include`.
4. **References.** References to WASI and to dependencies are written with the selected versions (spec/13 §13.11.4).
5. **Poisoning.** The following items are omitted:
   - an item at or inside which an error was reported by `resolve_owner`, `adt_def`, `interface_check`, `world_check`, `package_wit_names(P)` or `interface_uses(P)`;
   - every item that refers to an omitted item, transitively, including through `use`;
   - every interface in `InterfaceUses::cyclic`;
   - every world item that names an omitted interface.
6. **Completeness.** `complete` is true when nothing was omitted (N-K-25).

`generated_wit_text(P)` prints the model with `print_wit` (§16.3.4) and returns a `GeneratedText` with the text, `complete` and `origins`. The model and the text depend only on the declarations of `P` and on the views it refers to. A body edit therefore changes neither (§16.1.4 item 6).

### 16.10.2 Clients

1. **Bindgen.** `sai bindgen wit` ([13](13-cli.md) §13.6.19 item 1) checks the root, and writes `generated_wit_text(root).text` only when the root has no errors and the text is `complete`.
2. **Inspect.** `sai inspect wit` ([13](13-cli.md) §13.6.15) prints the same text, so the two commands are byte-identical.
3. **Dependents.** Dependents read the text through the unit `Generated(P)` (§16.4.7). An incomplete text is still usable, and the errors of `P` fail the build anyway.
4. **Component lowering.** `sai_wit::resolve_for(db, root_unit) -> wit_parser::Resolve` is a plain function, not a query, because a `Resolve` is not a query value. It builds a `Resolve` from `wit_closure(root_unit)` in post-order, with the version rewriting of §16.2.3. [07](07-component-lowering.md) §7.1.1 stage 1 calls it with `Generated(root)` in phase 13.

### 16.10.3 Hybrid consistency

`hybrid_wit_check(P)` implements package/04 §4.5.3 item 2. The driver asks for it for a hybrid root and for every hybrid node built in source form. It reports nothing when either side is unavailable: when `wit_package(Dir(P))` failed, when `generated_wit(P)` is not `complete`, or when `export_world(P)` failed. Those errors are reported by their own queries.

Let `w` be the WIT name of the export world. The check compares the elaborated world `w` of `Generated(P)` with the world `w` of `Dir(P)`. If `Dir(P)` has no world `w`, there is one diagnostic at the value of `export world`. Otherwise:

1. The imports and exports are compared as sets of keys (§16.8.3). A key present on one side only is a difference.
2. For each interface of `P`'s own WIT package that the world contains, the items are compared by name. Each item must have the same kind and structurally equal types. Named types are compared by package name, interface name and item name. For functions, the parameter names, parameter types and results are compared, and for resources, the members and their kinds.
3. Interfaces of other packages are compared only by key.
4. Documentation, `@since`, `@deprecated`, the order of items and the form of `use` statements are ignored. A type brought in by `use` is compared as its target.

Each difference is one `HYBRID_WIT_MISMATCH` (package/04 §4.5.3). It is reported at the SAI declaration of the differing item when the generated side has it, and otherwise at the value of `export world`. The diagnostics are sorted by world key and then by item name in byte order. Each one has the help "regenerate the WIT with `sai bindgen wit`".

---

## 16.11 Testing hooks

The suite `tests/impl/wit/` of [15](15-conformance-and-testing.md) §15.12.2 contains at least:

1. **Loader tests.** One `wit` directory for each loading case:
   - an invalid UTF-8 file, a syntax error, two packages, a wrong name or version;
   - a foreign reference without a dependency, a reference to a version that is too new;
   - a `deps/` directory, which must be ignored.

   Each case expects the diagnostic of §16.2 at the stated location.
2. **View goldens.** `sai bindgen wit --show` for every embedded WASI package of [targets](../targets/targets-and-profiles.md) §2.5, compared with golden files. These goldens pin the names, types, derives and shadowed items of §16.4.
3. **Round trip.** For each test package:
   - generate its WIT, load it as `Generated(P)` and print the view;
   - compare the result with the source after the normalizations of spec/13 §13.11.6;
   - generate WIT again from the printed view, and require byte-identical text (impl/15 §15.12.2 item 2).
4. **Determinism.** Generation must produce identical bytes:
   - with the files of a `wit` directory presented in reversed order;
   - with the modules of a package added in a different order;
   - with 1 and with 8 worker threads.
5. **Derive agreement.** For every view ADT of the WASI views, `view_derives` agrees with spec/13 §13.7.3 computed directly from the model.
6. **Flags.** All operations for 1, 8, 9, 16, 17 and 32 flags; layout sizes 1, 2 and 4; the Canonical ABI round trip of [07](07-component-lowering.md) §7.4.7; values from the host with bits `n` and above set, which every operation ignores (§16.9.6). Also `FLAGS_PATTERN`, `INVALID_FLAGS_ENUM` precedence, `WIT_EMPTY_TYPE` for zero flags, `UNKNOWN_FIELD` for `bits`, and `NOT_CONST_EXPRESSION` for `F.A | F.B` in a const.
7. **Diagnostics.** One test for each code of §16.13 at the location that this chapter gives, written with the annotations of [15](15-conformance-and-testing.md) §15.3.1.
8. **Hybrid.** For each kind of difference of §16.10.3, a `wit` directory edited by hand that yields exactly one `HYBRID_WIT_MISMATCH`, plus one test with several differences that checks their order.
9. **Incrementality.** Editing a function body in a source dependency leaves `generated_wit_text` of that dependency equal, and leaves the `wit_view` and `interface_impl_check` of its dependents green.

---

## 16.12 Decisions introduced by this chapter

| ID | Rule | Rationale |
|---|---|---|
| N-K-1 | WIT loading, the model, the printer and view `DefId`s live in a new crate `sai_wit` below `sai_resolve`. It obtains generated WIT through the hook trait `WitHooks`. | Resolution needs views before type checking exists. A hook keeps the crate graph acyclic (P7). |
| N-K-2 | Dependents see a package through exactly one WIT unit: `Dir` for artifact and hybrid packages, `Generated` for other source packages. The root's own `wit` directory is loaded only for validation, the hybrid check and an artifact's `export world`. | One view per package gives one nominal identity per type (P8). |
| N-K-3 | A `deps/` directory is ignored. Foreign WIT packages resolve through the package graph only, and their names are rewritten to the selected versions before resolution. | The graph and the lock file are the single source of truth for dependencies (P1, P9). |
| N-K-4 | Every target loads WASI at the WASI version of targets §2.5, including core-module and native targets. | The same program gets the same diagnostics on every target (P8). |
| N-K-5 | Unsupported WIT constructs, including world-level types and inline interfaces in worlds, are recorded at load time and reported at each use: a path, an impl of the interface, or a world reference. | spec/13 §13.9.2 requires reporting at use; unaffected items stay usable (P2). |
| N-K-6 | View packages have the keys `wit:wasi:…`, `wit:dir:…` and `wit:gen:…`, and `DefPath`s built from SAI names, so view `DefId`s are stable and independent of query order. | Stable keys are required for incrementality and caching (impl/01 §1.6.2). |
| N-K-7 | The interfaces of a source dependency are seen through its generated view. A path to an interface or item omitted for errors gets `Res::Err` without a diagnostic. | Dependents see what a component would see (spec/13 §13.7.1 item 4), and errors are reported once (P9). |
| N-K-8 | Shadowed WIT items get no `DefId`. `WIT_ITEM_SHADOWED` is reported once per view interface per binding package, at the first binding. Implementing a shadowed static function is `INTERFACE_IMPL_MISSING_ITEM`. | One warning where the user can act; no warnings for unused interfaces (P2). |
| N-K-9 | Implemented interface functions and bound resources are realized as a redirect of resolutions. The original target is kept in `ResolvedOwner::redirected`, and the redirect is never applied to the impl items and their bindings. | One mechanism serves calls, types and `Self`, and it cannot create a query cycle (P7). |
| N-K-10 | A view resource that occurs in a record field, variant payload or alias target of any visible WIT interface cannot be bound (`INVALID_RESOURCE_BINDING`). | Such types would carry imported handles where the implementation expects its own; rejecting them is boring and correct (P9). |
| N-K-11 | View resources and resources of local interfaces carry `AdtFlags::IS_HANDLE`. A record construction of such a type is `OPAQUE_RESOURCE_CONSTRUCTION`. | One flag identifies every handle type for typing, drops and imports. |
| N-K-12 | A world item of the wrong kind is `INVALID_WORLD_ITEM`, including a whole package in `include`. A `use` of a world is `INVALID_USE_TARGET`. Worlds of other packages are named only by `PKG_PATH`. | Specific codes beat the generic `WRONG_NAME_KIND` (P1). |
| N-K-13 | `w.f` for a local world `w` resolves among the `import fn` items of `w` and its includes, first in elaboration order. Anything else after a world is `WRONG_NAME_KIND`. | It realizes spec/13 §13.4.1 with a cycle-safe, deterministic lookup (P8). |
| N-K-14 | Elaboration keeps source order with includes expanded in place and implicit imports appended. Equal items reached through an include or implicitly are merged. Every other repetition of a key is `WORLD_DUPLICATE_ITEM` at the reporting location of the later occurrence. | It matches the WIT elaboration and gives one diagnostic at the place the user wrote (P2). |
| N-K-15 | An external `export world` is looked up in WASI, then the direct edges, then the only node with the identity. An ambiguous reference is `UNKNOWN_EXPORT_WORLD`. | No guessing between two versions (P1). |
| N-K-16 | `WORLD_EXPORT_UNIMPLEMENTED` is checked per world on its directly written items, on the exports of included external worlds at the include, and on an external export world at the `export world` value. | Each missing implementation is reported once, where it was introduced (P2). |
| N-K-17 | The hybrid check compares elaborated worlds structurally. It ignores documentation, gates, order and the form of `use`, and reports one sorted diagnostic per differing item. | Only differences that change the component type matter, and the output is deterministic (P8). |
| N-K-18 | A flags type is an ADT with one hidden unsigned field of 1, 2 or 4 bytes, where bit `i` is flag `i`. Bits `n` and above are ignored by every operation and cleared by every operation that produces a value. | It is identical to the Canonical ABI layout, so no conversion is needed. Values from the boundary need no check (P9). |
| N-K-19 | `INVALID_FLAGS_ENUM` is reported once, with the precedence outside-interface, payload, discriminant, count, and suppresses `WIT_EXPLICIT_DISCRIMINANT`. An invalid or empty flags enum falls back to an ordinary enum, and zero flags is `WIT_EMPTY_TYPE`. | It resolves the contradiction between spec/15 §15.10 and spec/13 §13.2.2 without cascades (P9). |
| N-K-20 | Flag constants are written `F.X`. `.X` is `IMPLICIT_MEMBER_NOT_ENUM`. Constants are allowed in consts, and the operators and provided functions are not. | Flags are values, not variants. Constant evaluation stays small (P7). |
| N-K-21 | The flags operators and provided functions are compiler-provided impls and synthetic inherent items, with the shims `ShimKind::Flags`. | They need no library code, and their semantics are fixed by spec/13 §13.9.4. |
| N-K-22 | View derives are compiler-provided impls with nested field obligations and `ShimKind::ViewDerive` bodies. Hash is given only to enums and flags, as N-D-35 says. | It reuses the solver for the structural rule; no impl `DefId`s are needed in views. |
| N-K-23 | A body on an interface function, a resource member or a world function is `INTERFACE_FN_BODY`. `UNEXPECTED_FN_BODY` is only for `extern` blocks. | One code per construct, as spec/13 §13.2.2 defines. |
| N-K-24 | The interfaces and worlds of a package form one WIT name scope. A collision across modules is `WIT_NAME_COLLISION`. | They share one WIT package namespace. |
| N-K-25 | Items with errors are omitted from generated WIT, transitively. `sai bindgen wit` and `sai inspect wit` require a complete result, and dependents use whatever was generated. | Dependents keep working on partial results, and published WIT is never partial (P9). |
| N-K-26 | A second impl item with the name of an earlier one is `INTERFACE_IMPL_EXTRA_ITEM`. | The interface declares the item once; the second one is extra (P1). |
| N-K-27 | A world `include` has the `DefPath` component `WorldInclude(Symbol)`. | Includes and imports with the same text must have distinct stable paths. |
| N-K-28 | `EXPORT_FN_NOT_APPLIED` and `EXPORT_WORLD_NOT_APPLIED` are session-level queries that the driver asks for only in the builds where they apply. | Each info is reported exactly once per build (P8). |

---

## 16.13 Diagnostics

The codes below are defined in [spec/13](../spec/13-components-and-wit.md) or in the documents named in each row. This chapter fixes their reporters and locations.

| Code | Severity | Description |
|---|---|---|
| `DUPLICATE_INTERFACE_IMPL` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.3.1; `interface_impls`, at the target path of the later impl (§16.7.1 step 2) |
| `EXPORT_FN_NOT_APPLIED` | info | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.4; `export_fn_note`, at the first export function (§16.8.6) |
| `FLAGS_PATTERN` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.9.4; `typeck`, at the pattern (§16.9.5) |
| `GENERIC_INTERFACE_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; `interface_check` and `interface_impl_check`, at the generics (§16.6.1, §16.7.2) |
| `IMPL_TARGET_NOT_INTERFACE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.3.1; `interface_impls`, at the target path, in place of `WRONG_NAME_KIND` (§16.7.1 step 1) |
| `INTERFACE_FN_BODY` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; the parser, for a body on an interface function, a resource member or a world function (§16.5.2) |
| `INTERFACE_IMPL_EXTRA_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.3.1; `interface_impls`, at the item name, also for a repeated name (§16.7.1 step 4) |
| `INTERFACE_IMPL_MISSING_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.3.1; `interface_impls` at the target path, and `interface_impl_check` at the resource binding (§16.7.1, §16.7.2) |
| `INTERFACE_IMPL_SIGNATURE_MISMATCH` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.3.2; `interface_impl_check` and `world_check`, at the first difference (§16.7.2, §16.8.4) |
| `INTERFACE_USE_CYCLE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.3; `interface_uses`, once per cycle (§16.6.5) |
| `INVALID_FLAGS_ENUM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.9.4; `adt_def`, once per enum by precedence (§16.9.1) |
| `INVALID_RESOURCE_BINDING` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.3.3; `interface_impls`, at `C`, including the external restriction (§16.7.3) |
| `INVALID_WORLD_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.4.1; `resolve_owner` of the world item, at the `WorldRef` (§16.8.1) |
| `OPAQUE_RESOURCE_CONSTRUCTION` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.7.4; `typeck`, at the path of the construction (§16.7.5) |
| `UNKNOWN_EXPORT_WORLD` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.1; `export_world`, at the value of `export world` (§16.8.5) |
| `WIT_EMPTY_TYPE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; `interface_check`, at the item name, also for zero flags (§16.6.1, §16.9.1) |
| `WIT_EXPLICIT_DISCRIMINANT` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; `interface_check`, at the variant, unless `INVALID_FLAGS_ENUM` was reported (§16.6.1) |
| `WIT_INCOMPATIBLE_TYPE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; `interface_check` and `world_check` through `boundary_type` (§16.6.1–§16.6.3) |
| `WIT_ITEM_SHADOWED` | warning | defined in [spec/13](../spec/13-components-and-wit.md) §13.8.5; `package_scopes`, once per view interface at the first binding (§16.4.5) |
| `WIT_NAME_COLLISION` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.8.5; `interface_check`, `world_check` and `package_wit_names`, at the later entity (§16.6.4) |
| `WIT_NAME_NOT_REPRESENTABLE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.8.4; `interface_check`, `world_check` and `package_wit_names`, at the name (§16.6.3) |
| `WIT_PARSE_ERROR` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.7.1; `wit_package` of a `Dir` unit, at the WIT file and position (§16.2.2, §16.2.3) |
| `WIT_UNSUPPORTED_FEATURE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.9.2; at each use: `resolve_owner`, `interface_impls`, world references (§16.3.3) |
| `WORLD_DUPLICATE_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.4.2; `elaborated_world` through `world_check`, and `component_world` (§16.8.3, §16.8.6) |
| `WORLD_EXPORT_UNIMPLEMENTED` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.4.1; `world_check` and `export_world`, at the export item, include or `export world` value (§16.8.4) |
| `WORLD_INCLUDE_CYCLE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.4.2; `world_includes`, once per cycle (§16.8.3) |
| `EXPORT_WORLD_NOT_APPLIED` | info | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2; `export_world_note`, at the `export world` declaration (§16.8.6) |
| `UNKNOWN_WIT_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.7.2; `resolve_pkg_path` and world references (§16.4.8, §16.8.1) |
| `INTERFACE_FN_CLAUSE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; validation of [02](02-frontend-ast-hir.md) §2.5.12, not checked again (§16.6.1, §16.7.2) |
| `HYBRID_WIT_MISMATCH` | error | defined in [package/04](../package/04-distribution-and-security.md) §4.5.3; `hybrid_wit_check`, one per differing item (§16.10.3) |
| `WIT_PACKAGE_MISMATCH` | error | defined in [package/01](../package/01-package-contract.md) §1.6.3; `wit_package`, at the package declaration (§16.2.2 step 4) |
| `INVALID_USE_TARGET` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; a `use` of a world or of a whole WIT package (§16.4.8) |
| `UNKNOWN_MODULE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; a WIT `use` path with two or more segments after the package (§16.4.8) |
| `WRONG_NAME_KIND` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.2; world references of another kind and world member paths (§16.8.1, §16.8.2) |
| `UNRESOLVED_NAME` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.2; `w.f` naming no imported world function (§16.8.2) |
| `UNRESOLVED_IMPORT` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.5.2; an item list naming no member of a view interface (§16.4.8) |
| `RESERVED_TYPE_NAME` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.4.4; binding a view type whose SAI name is reserved (§16.4.8) |
| `DUPLICATE_BINDING` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.4.3; two members of one local interface with one name (§16.4.8) |
| `WORLD_IMPORT_NEEDS_INTERFACE` | error | defined in [spec/03](../spec/03-grammar.md) §3.9; validation, for a whole package in `import` or `export` (§16.8.1) |
| `UNEXPECTED_FN_BODY` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; the parser, for `extern` blocks only (§16.5.2) |
| `CONFLICTING_ATTRIBUTES` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.2.4; `@flags` with `@derive` (§16.6.1) |
| `UNKNOWN_FIELD` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.1; access to the hidden field of a flags type (§16.9.2) |
| `IMPLICIT_MEMBER_NOT_ENUM` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.4.3; `.X` with a flags type expected (§16.9.3) |
| `NOT_CONST_EXPRESSION` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.27; flags operators and provided functions in a const (§16.9.3) |
| `UNKNOWN_VARIANT` | error | defined in [spec/05](../spec/05-types.md) §5.13.6; `F.X` naming no flag (§16.9.3) |
| `DUPLICATE_INHERENT_ITEM` | error | defined in [spec/05](../spec/05-types.md) §5.9.2; a user item named like a provided flags function (§16.9.4) |
| `EFFECTS_ON_INTERFACE_FN` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.1; reported by [08](08-effect-and-capability-analysis.md) (§16.6.1) |
| `MISSING_EFFECTS_DECLARATION` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; reported by [08](08-effect-and-capability-analysis.md) for exposed functions (§16.7.2) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; `async` items and their uses before Stage 5 (§16.1.5, §16.3.3) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; a failure to load an embedded or generated unit (§16.2.2) |
