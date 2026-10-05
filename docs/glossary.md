# Glossary

**Status:** Informative
**Stage:** all
**Related:** [README.md](README.md) §1, [spec/01-introduction.md](spec/01-introduction.md) §1.8, [design/decisions.md](design/decisions.md)

This glossary defines the technical terms that are used across the documents, in alphabetical order. Each entry gives a short definition and links to the section that defines the term normatively. The glossary is informative: when an entry and the linked section differ, the linked section is right ([spec/01](spec/01-introduction.md) §1.8 item 1). Terms that are used only within one chapter are defined in that chapter and are not repeated here. Section references follow the numbering of each document; [spec/09](spec/09-contracts-and-refinements.md) numbers its sections `09.n`.

---

## A

- **adapter.** A program that translates the commands of the conformance suite into the command line of an implementation other than `sai`, so that the suite can drive it as a black box. [impl/15](impl/15-conformance-and-testing.md) §15.1.3.
- **affine.** A property of a type whose values are used by value at most once and need not be consumed explicitly: if a value is never moved, it is dropped. Resources are affine; SAI has no linear types in v0.1. [spec/07](spec/07-ownership-and-borrowing.md) §7.14, D-OWN-06.
- **`alloc`.** The built-in effect of allocating from the global heap, and the capability that grants it. Names of `sai:alloc` always resolve; whether code may use them is decided by this effect. [spec/08](spec/08-effects-and-capabilities.md) §8.3, §8.12.1, D-CAP-01.
- **allocation region.** The arena introduced by a `region r { .. }` block, from which values are allocated explicitly and freed together when the block ends. It is never called just "region", which means a borrow region. [spec/07](spec/07-ownership-and-borrowing.md) §7.1.1, §7.15, D-OWN-07.
- **ambient authority.** Authority that code has without being granted it. SAI has none: there are no `static` items, and only import functions can affect the world outside the program's memory. [spec/08](spec/08-effects-and-capabilities.md) §8.1.2, D-CAP-02.
- **applicable test.** A test of the conformance suite whose stage is within a claim and that has a configuration of a claimed target and profile; it must pass for the claim to be substantiated. [impl/15](impl/15-conformance-and-testing.md) §15.9.3.
- **artifact package.** A package that ships prebuilt Wasm components instead of, or as well as, source (Stage 4). [package/04](package/04-distribution-and-security.md) §4.5, [package/01](package/01-package-contract.md) §1.7.
- **attribute.** An annotation of the form `@name` or `@name(..)` from a closed set, such as `@derive(..)` or `@must_use`. There are no user-defined attributes. [spec/15](spec/15-attributes-and-lints.md) §15.2, D-SYN-09.

## B

- **base type.** The type from which a distinct type or a refined type is formed, as `U64` in `distinct Id = U64`. [spec/05](spec/05-types.md) §5.6.3, §5.6.4.
- **black-box testing.** The conformance suite observes an implementation only through its commands, its files and its outputs, never through its internals. [impl/15](impl/15-conformance-and-testing.md) §15.1.3, N-J-38.
- **bootstrap compiler.** The first of the two builds of the reference toolchain, which embeds the library source without metadata and produces the metadata that the release compiler embeds. It is unrelated to the stages of the [roadmap](roadmap.md). [impl/01](impl/01-architecture.md) §1.12.2.
- **borrow.** A non-owning reference to a place, created explicitly with `borrow e` or `borrow mut e`, also at call sites. A borrow must stay valid for its region. [spec/07](spec/07-ownership-and-borrowing.md) §7.5, D-OWN-02.
- **borrow checker.** The analysis that rejects every use of a place that conflicts with a live loan, and every use of a moved or uninitialized value. [spec/07](spec/07-ownership-and-borrowing.md) §7.9, [impl/05](impl/05-borrow-checker.md), D-OWN-04.
- **borrow region.** See *region*.
- **`borrows` clause.** A signature clause that states from which inputs a returned borrow comes, in place of lifetime syntax. [spec/07](spec/07-ownership-and-borrowing.md) §7.7.3, D-OWN-03.
- **build ID.** A 16-digit hexadecimal identifier of one build of the toolchain, computed from its sources only. Caches and metadata are valid only for the build ID that wrote them. [impl/01](impl/01-architecture.md) §1.13.2.

## C

- **Canonical ABI.** The calling convention of the Component Model, which defines how values are passed between components. It is the only public ABI of SAI; the internal layout and calling convention are private. [spec/13](spec/13-components-and-wit.md) §13.1.1, [impl/07](impl/07-component-lowering.md) §7.2 to §7.4, P4.
- **canonical form.** The unique formatting of a source file that `sai fmt` produces. It is byte-identical across conforming formatters, idempotent, and preserves comments and meaning. [impl/14](impl/14-formatter.md) §14.1, [spec/01](spec/01-introduction.md) §1.4.4 item 2, D-TOOL-04.
- **canonical path.** The fully qualified, package-prefixed name of an item, such as `sai:core/str.Str.split`. It names library items in diagnostics, in the staged item table and in contract IDs. [spec/04](spec/04-modules-and-names.md) §4.9.
- **capability.** A name in the policy of a package that grants the right to perform a class of effects: `alloc`, a WIT package or interface name, or a target capability. Effects describe code; capabilities describe packages. [spec/08](spec/08-effects-and-capabilities.md) §8.1.1, §8.12, D-CAP-01.
- **capability policy.** The `allow` and `deny` declarations of a `sai.package`. `allow` is the maximum grant, and an omitted `allow` means the empty set. [package/01](package/01-package-contract.md) §1.11, [spec/08](spec/08-effects-and-capabilities.md) §8.12.5, D-CAP-01.
- **claim.** See *conformance claim*.
- **compatibility line.** The set of versions of a package that are expected to replace each other compatibly: the major version from 1.0.0, the minor version for `0.y`, and the exact patch for `0.0.z`. Different lines of one package are different nodes and may coexist. [package/02](package/02-resolver.md) §2.2.3, D-PKG-02.
- **component.** A Wasm component of the Component Model, whose interface is described in WIT. SAI produces components for the `wasi` targets and with `sai component build`. [spec/13](spec/13-components-and-wit.md) §13.1, D-TGT-01.
- **component build.** A build that produces a component: every build for `wasm32-wasi` or `wasm64-wasi`, and `sai component build` for any target. [spec/13](spec/13-components-and-wit.md) §13.1.2.
- **configuration.** A target, a profile and the other settings under which a test of the conformance suite runs once. [impl/15](impl/15-conformance-and-testing.md) §15.1.1, §15.2.6.
- **conformance claim.** A statement of the form `SAI <version>, stages <range>, targets <list>` about a compiler. It is substantiated by the conformance suite; a claim of Stages 0 to 4 with structured diagnostics is a v0.1 conformance claim. [spec/01](spec/01-introduction.md) §1.4.3, [impl/15](impl/15-conformance-and-testing.md) §15.9.4.
- **conformance suite.** The tests that substantiate conformance claims, including the tests generated from the specification's examples and listings. [impl/15](impl/15-conformance-and-testing.md) §15.1.1, §15.2.
- **conforming compiler.** A compiler that implements Stages 0 to 4 and structured diagnostics, supports the required targets and profiles, and accepts and rejects programs as the specification prescribes. [spec/01](spec/01-introduction.md) §1.4.2.
- **conforming program.** A set of packages that satisfies every static rule of the language specification and whose package contracts are valid. A program that violates a static rule is ill-formed. [spec/01](spec/01-introduction.md) §1.4.1.
- **contract.** A `requires` or `ensures` clause of a function. An unproven contract is checked at run time in every profile and panics with kind `Contract`. [spec/09](spec/09-contracts-and-refinements.md) §9.3, D-CONTRACT-01.
- **contract ID.** The stable identifier of a contract clause, refinement clause or invariant element, such as `app.bank.withdraw#requires[1]`. [spec/09](spec/09-contracts-and-refinements.md) §9.1.3.
- **core target.** `wasm32-core` or `wasm64-core`: a target that produces a freestanding core Wasm module without WASI. [targets](targets/targets-and-profiles.md), D-TGT-01.
- **CST.** The lossless concrete syntax tree of a source file, which keeps every token and every piece of trivia. The formatter and the semantic operations work on it. [impl/02](impl/02-frontend-ast-hir.md) §2.6.

## D

- **designated item.** An item of a standard package other than `sai:core` and `sai:alloc` that the compiler knows by canonical path: `sai:process.Env`, `sai:process.ExitCode`, the package-private `Env.for_entry`, and `sai:async.scope`. No desugaring uses it, and the compiler looks it up only when its package is a dependency of the build. It is not a lang item. [spec/16](spec/16-core-library.md) §16.16.1, N-J-26.
- **deviation.** A difference between the diagnostic codes that an implementation reports and those that a test expects, in a test whose acceptance or rejection is correct. Deviations are reported with a claim and are not failures. [impl/15](impl/15-conformance-and-testing.md) §15.1.1, §15.3.4.
- **diagnostic.** A message of the compiler with a stable UPPER_SNAKE code, a severity, a primary span and a message. An error rejects the program; a warning or an info does not. [spec/01](spec/01-introduction.md) §1.8, [impl/11](impl/11-diagnostics.md) §11.4, D-TOOL-01.
- **distinct type.** A nominal type `distinct D = B` with the representation of its base type but none of its traits or operators, which `@derive(..)` can forward. Literals never have a distinct type directly. [spec/05](spec/05-types.md) §5.6.3, D-TYPE-03.
- **drop.** The destruction of an owned value: running its `Drop` impl, if any, and then dropping its components. Values are dropped at the end of their scope unless they were moved. [spec/07](spec/07-ownership-and-borrowing.md) §7.13, D-OWN-05.
- **drop glue.** The compiler-generated code that drops the components of a value in their fixed order. [spec/07](spec/07-ownership-and-borrowing.md) §7.13.3.

## E

- **effect.** A statically known, named description of an interaction with the world outside the program's own memory, such as heap allocation or a call to a host import. Every function has an effect set; panics, unsafety and deallocation are not effects. [spec/08](spec/08-effects-and-capabilities.md) §8.1.1, D-EFF-01 to D-EFF-03.
- **effect atom.** A single effect: a built-in effect, an interface effect or a user effect. [spec/08](spec/08-effects-and-capabilities.md) §8.2.1.
- **effect set.** A finite set of effect atoms. A function whose effect set is empty is pure. [spec/08](spec/08-effects-and-capabilities.md) §8.2.3.
- **elaborated world.** A world with every `include` replaced by the included items and with an `import` added for every interface that its interfaces use, transitively. Duplicate items are checked on it, and the component, the generated bindings and the import set are computed from it. [spec/13](spec/13-components-and-wit.md) §13.4.2, [impl/16](impl/16-interfaces-and-wit.md) §16.8.3.
- **elision.** The fixed rules that give the regions of a function signature without lifetime syntax. Provenance is never inferred from a function body. [spec/07](spec/07-ownership-and-borrowing.md) §7.7, D-OWN-03.
- **error gate.** One of the two points at which errors stop the reference compiler's pipeline: the MIR of an owner is built only if its type checking reported no error, and code is generated only if the whole analysis reported no error. Otherwise analysis continues past errors. [impl/01](impl/01-architecture.md) §1.2.3, N-J-27.
- **explain record.** The text that `sai explain <CODE>` prints for a diagnostic code, with a summary, an explanation, an example that produces the code, and a fix. The conformance suite compiles its example and fix. [impl/11](impl/11-diagnostics.md) §11.13, [impl/15](impl/15-conformance-and-testing.md) §15.8.4, D-TOOL-03.
- **`export`.** The highest of the three visibility levels. An `export fn` is public and is exported from the Wasm module or component. [spec/04](spec/04-modules-and-names.md) §4.6, [spec/12](spec/12-unsafe-and-ffi.md) §12.8, D-MOD-02.
- **export world.** The world named by `export world` in the root `sai.package`, which defines the contract of the component that a build produces. [spec/13](spec/13-components-and-wit.md) §13.5, [package/01](package/01-package-contract.md) §1.9.

## F

- **fact.** A formula that the Standard Fact Checker may assume at a program point, such as a `requires` clause of the current function, an integer `let` definition or a dominating branch condition. [spec/09](spec/09-contracts-and-refinements.md) §9.1.1, §9.6.3.
- **`FEATURE_NOT_IMPLEMENTED`.** The error with which a compiler rejects every use of a staged feature that it does not implement, naming the stage and the feature. See *staged feature*. [spec/01](spec/01-introduction.md) §1.4.5, N-J-5.
- **fixpoint (bootstrap).** The check that the release compiler, rebuilding the metadata of `sai:core` and `sai:alloc`, writes bytes identical to the metadata it embeds. [impl/01](impl/01-architecture.md) §1.12.2 step 6.
- **flags type.** An `enum` inside an interface marked `@flags`, whose values are sets of its 1 to 32 members, combined with `|`, `&` and `^`. It maps to a WIT `flags` type and is stored as an unsigned integer of 1, 2 or 4 bytes with one bit per member (Stage 4). [spec/13](spec/13-components-and-wit.md) §13.9.4, [spec/15](spec/15-attributes-and-lints.md) §15.10, [impl/16](impl/16-interfaces-and-wit.md) §16.9.
- **f-string.** A format string `f"..."` that builds text from embedded expressions and format specs, without macros (Stage 3). [spec/16](spec/16-core-library.md) §16.14, D-EXPR-12.

## G

- **glue.** Compiler-generated code that converts between the Canonical ABI and SAI values at a component boundary. [spec/13](spec/13-components-and-wit.md) §13.1.2, [impl/07](impl/07-component-lowering.md) §7.7, §7.8.
- **golden file.** A file of a test that holds the exact expected bytes of an output of the reference implementation, such as MIR text, WIT, formatted source or diagnostics JSON. Golden files are regenerated only deliberately and reviewed. [impl/15](impl/15-conformance-and-testing.md) §15.1.1, §15.4.1, N-J-42.

## H

- **harness.** The program that runs the conformance suite (`cargo xtask test`), with its deterministic execution engine and its oracles. [impl/15](impl/15-conformance-and-testing.md) §15.1.1, §15.5.
- **HIR.** The high-level intermediate representation of the reference compiler: the desugared source of a package, partitioned by owner and built before name resolution. It is the input of name resolution, type checking and MIR building. [impl/02](impl/02-frontend-ast-hir.md) §2.8, §2.9.
- **hybrid package.** A package that ships both source and prebuilt components; which form is used is chosen by deterministic rules, which the root package can override (Stage 4). [package/04](package/04-distribution-and-security.md) §4.5, D-PKG-05.

## I

- **ICE.** See *internal compiler error*.
- **ill-formed program.** A program that violates a static rule. A conforming compiler rejects it with at least one error. [spec/01](spec/01-introduction.md) §1.4.1 item 2.
- **impact.** What a planned semantic operation changes beyond the edited text, computed before anything is written as the semantic diff between the current and the edited version. [impl/12](impl/12-semantic-tooling.md) §12.7.
- **implementation-defined behavior.** Behavior that an implementation chooses and documents, from a closed list. [spec/01](spec/01-introduction.md) §1.5.2, N-J-2.
- **implementation limit.** A bound, such as a maximum nesting depth or file size, that every conforming compiler supports at least; a program beyond an implementation's limit is reported with `IMPLEMENTATION_LIMIT_EXCEEDED`. [spec/01](spec/01-introduction.md) §1.5.4, N-J-4.
- **interface.** A declaration of types and functions that maps one-to-one to a WIT interface, and that a package can import, implement and export. [spec/13](spec/13-components-and-wit.md) §13.2, §13.3, D-WIT-01.
- **interface effect.** The effect of calling a function of an imported WIT interface, identified by the versionless interface name, such as `wasi:http/outgoing-handler`. [spec/08](spec/08-effects-and-capabilities.md) §8.2.1, §8.5.
- **internal compiler error.** A failure of the compiler itself, never a property of the program. It is reported as `INTERNAL_COMPILER_ERROR` with a record for the bug report. [spec/01](spec/01-introduction.md) §1.10, [impl/01](impl/01-architecture.md) §1.10, N-J-33.
- **invariant.** (1) A predicate of an `invariant` block that every value of a type satisfies at its check points; an unproven invariant is checked at run time. [spec/09](spec/09-contracts-and-refinements.md) §9.4, D-CONTRACT-04. (2) In the variance of regions, a position in which a region can be neither shortened nor lengthened. [spec/07](spec/07-ownership-and-borrowing.md) §7.6.3.
- **IUT.** The implementation under test: the toolchain whose claim the conformance suite checks. The reference implementation is one IUT among others. [impl/15](impl/15-conformance-and-testing.md) §15.1.1.

## L

- **lang item.** A library item of `sai:core` or `sai:alloc` that the compiler knows by identity, because a language construct is defined in terms of it, such as `Option`, `Iterator` or `core.panic.panic`. Items of other standard packages that the compiler knows, such as `sai:async.scope`, are designated items, not lang items. [spec/16](spec/16-core-library.md) §16.3, [spec/01](spec/01-introduction.md) §1.8.
- **latent effect.** An effect reached through a type parameter, such as the effects of a closure passed to a generic function. It is attributed to the caller that instantiates the parameter. [spec/08](spec/08-effects-and-capabilities.md) §8.8, D-EFF-02.
- **lint.** A diagnostic whose level can be changed with `@allow`, `@warn` or `@deny`, the `lints { }` block or a flag. Lint names and diagnostic codes share one namespace. [spec/15](spec/15-attributes-and-lints.md) §15.15, §15.16, D-TOOL-01.
- **loan.** The record of one borrow: the borrowed place, its kind, its region and the point that created it. [spec/07](spec/07-ownership-and-borrowing.md) §7.1.1, §7.9.3.
- **lock file.** `sai.lock`, which records the resolved dependency graph with the source, commit and `sai1:` checksums of every node, so that builds are reproducible. [package/03](package/03-lock-file.md), D-PKG-03.

## M

- **MIR.** The mid-level intermediate representation of the reference compiler: a control-flow graph of basic blocks over places, on which borrow checking, effect analysis, contract checks and code generation work. [impl/04](impl/04-mir.md), D-MIR-01.
- **module.** A source file. Modules form a tree inside a package; there are no inline modules. [spec/04](spec/04-modules-and-names.md) §4.2, D-SYN-08.
- **MVS.** Minimal Version Selection: for every compatibility line of a package, the resolver selects the maximum of the minimum versions that are requested, and then checks every requirement's range, without backtracking. [package/02](package/02-resolver.md) §2.4, D-PKG-02.

## N

- **node (resolver).** A pair of a package identity and a compatibility line, such as `acme:json#1`. The resolver selects one version per node. [package/02](package/02-resolver.md) §2.1.
- **node ID.** An identifier of the form `<kind>:<hex>` for a syntax node, valid within one snapshot. Structured fixes and semantic operations refer to nodes through it. [impl/12](impl/12-semantic-tooling.md) §12.3, D-TOOL-02.

## O

- **obligation.** A predicate that must hold at a program point, created by a refinement, a contract or an invariant. The Standard Fact Checker either proves it or reports it as not proven. [spec/09](spec/09-contracts-and-refinements.md) §9.1.1.
- **owner.** (1) The variable or value that owns a value and is responsible for dropping it. [spec/07](spec/07-ownership-and-borrowing.md) §7.3.1. (2) In the reference compiler, an item, associated item or other definition that has its own HIR; the per-body analyses are keyed by owners. [impl/01](impl/01-architecture.md) §1.1.1.

## P

- **package.** The unit of distribution, versioning and policy, described by a `sai.package` file and identified as `ns:name`. [spec/04](spec/04-modules-and-names.md) §4.2, [package/01](package/01-package-contract.md) §1.4.
- **panic.** The defined termination of an execution for a checked failure, such as an overflow, a failed bounds check or an unproven contract. A panic ends in a trap on every target, after a report in the `dev` and `release` profiles when a reporting channel exists. [spec/10](spec/10-errors-and-panics.md) §10.1, §10.4, D-ERR-01.
- **panic kind.** The classification of a panic, such as `Overflow(Add)`, `BoundsCheck` or `Contract(id)`, each with a fixed message. [spec/10](spec/10-errors-and-panics.md) §10.1.2, §10.4.4.
- **permits.** The relation by which a declared effect set allows an actual effect atom. [spec/08](spec/08-effects-and-capabilities.md) §8.2.4.
- **place.** An expression that denotes a memory location, such as a variable, a field of a place or an index into a place. Moves and borrows act on places. [spec/07](spec/07-ownership-and-borrowing.md) §7.2.
- **portable.** Applying to every implementation under test, as opposed to the reference implementation only. [impl/15](impl/15-conformance-and-testing.md) §15.1.1.
- **prelude.** The fixed set of bindings, such as `Option` and `String`, that is visible in every module after all other scopes. A module may shadow a prelude name, with a warning. [spec/04](spec/04-modules-and-names.md) §4.2, §4.4.4, [spec/16](spec/16-core-library.md) §16.4.
- **profile.** One of `dev`, `release` and `tiny`, which differ only in panic reporting, debug information, the allocator, code size and speed, never in results or checks. [targets](targets/targets-and-profiles.md), D-TGT-02, C-03.
- **property.** A test declaration that checks a predicate on generated values for a fixed number of cases from a reproducible seed. [spec/14](spec/14-testing.md) §14.2.2, §14.5.
- **pure.** Having an empty effect set. Predicates of contracts and refinements must be pure. [spec/08](spec/08-effects-and-capabilities.md) §8.1.1, [spec/09](spec/09-contracts-and-refinements.md) §9.1.2.

## Q

- **query.** In the reference compiler, a memoized, pure function from a key to a value, executed by the query system, which recomputes only what an edit invalidates. An input is a query whose value the driver sets. [impl/01](impl/01-architecture.md) §1.1.1, §1.4, N-J-29.

## R

- **reference implementation.** The `sai` toolchain that the implementation specification describes. Other implementations only have to match the observable behavior that the language specification defines. [README](README.md) §2, [spec/01](spec/01-introduction.md) §1.8, [impl/01](impl/01-architecture.md).
- **refined type.** A nominal type `type R = B where p` or `distinct D = B where p`, whose values are the values of the base type that satisfy the predicate `p`. A conversion from the base type is either proven by the Standard Fact Checker or written explicitly with `try_from`. [spec/09](spec/09-contracts-and-refinements.md) §9.2, [spec/05](spec/05-types.md) §5.6.4, D-TYPE-03.
- **region.** A borrow region: the set of program points during which a borrow must stay valid. Regions are written `'a` in the specification and in diagnostics but have no source syntax. [spec/07](spec/07-ownership-and-borrowing.md) §7.1.1, §7.6, D-OWN-03. See also *allocation region*.
- **release compiler.** The second of the two builds of the reference toolchain, which embeds the library metadata that the bootstrap compiler produced; it is the shipped binary. [impl/01](impl/01-architecture.md) §1.12.2.
- **reporting channel.** The means by which a panic report reaches the host: WASI standard error or the `@panic_hook` import. [spec/10](spec/10-errors-and-panics.md) §10.1.2, §10.5.3.
- **resource.** A nominal, affine type with identity, which is never `Copy` and is dropped by its `Drop` impl or, for an imported handle, by the resource-drop of the handle. At a component boundary it maps to a WIT resource (Stage 4). [spec/05](spec/05-types.md) §5.6.6, [spec/07](spec/07-ownership-and-borrowing.md) §7.14, [spec/13](spec/13-components-and-wit.md) §13.10, D-OWN-06.
- **root package.** The package whose `sai.package` a command operates on. Root-only declarations, such as `overrides` and `profile`, are read only from it. [package/02](package/02-resolver.md) §2.1, [impl/13](impl/13-cli.md) §13.2.1.

## S

- **`sai1:` checksum.** A checksum written `sai1:` followed by 64 lowercase hexadecimal digits of a SHA-256 digest. For a directory of a Git commit (the tree hash), it is the digest of the lines `sha256(content)  path` of the selected files in path order. [package/03](package/03-lock-file.md) §3.4, D-PKG-03.
- **SAI view.** The SAI declarations through which a program uses a visible WIT interface of another package: types, resources and functions with SAI names, computed deterministically from the WIT. Not to be confused with a *view type*. [spec/13](spec/13-components-and-wit.md) §13.7.3, [impl/16](impl/16-interfaces-and-wit.md) §16.4.
- **`scope`.** The structured-concurrency function of `sai:async`. `scope(body)` passes a `borrow Scope` to `body`; child tasks are started only with `Scope.spawn` (`s.spawn(..)`), and the scope completes only when the body and all children have completed. It is a designated item with a special region rule. [spec/11](spec/11-async-and-concurrency.md) §11.7, D-ASYNC-01.
- **sealed trait.** A library trait that only `sai:core` implements, for a closed set of types, such as `Integer` or `Float`. An impl in any other package is `SEALED_TRAIT_IMPL`. [spec/16](spec/16-core-library.md) §16.3.5, N-J-11.
- **semantic diff.** The changes between two versions of a workspace, stated per entity with their public API classification, as opposed to a text diff (Stage 6). [impl/12](impl/12-semantic-tooling.md) §12.8.
- **semantic graph.** A directed graph of semantic objects, such as symbols and modules, and typed relations between them, such as calls and effects (Stage 6). [impl/12](impl/12-semantic-tooling.md) §12.4.
- **semantic operation.** A named, parameterized change of a program stated in terms of symbols and nodes rather than text, such as `rename_symbol`; the toolchain computes its edits and its impact. [impl/12](impl/12-semantic-tooling.md) §12.6.
- **semantic protocol.** The SAI extensions of the language server protocol, through which tools and agents query semantic graphs and plan and apply semantic operations (Stage 6). [impl/12](impl/12-semantic-tooling.md) §12.5, [spec/01](spec/01-introduction.md) §1.4.4 item 4.
- **SFC.** See *Standard Fact Checker*.
- **shadow stack.** The area of linear memory, below the static data, in which functions keep the locals whose address is taken, such as borrowed locals, and some call temporaries. It grows downward from a fixed 1 MiB area after an 8-byte null guard, and every function with a frame checks for overflow in its prologue, in every profile. [impl/06](impl/06-wasm-lowering-and-layout.md) §6.3.2, §6.6.6, §6.7, D-WASM-02.
- **signature listing.** A `sai` block in a library chapter that declares items with their bodies omitted. It is valid SAI apart from that one exception. [spec/01](spec/01-introduction.md) §1.9.3, N-J-3.
- **snapshot.** A consistent view of the inputs of a compilation: in the CLI, the bytes of every file as read once at the start of a session; in the language server, one version of every document. Node IDs are valid within one snapshot. [impl/01](impl/01-architecture.md) §1.7.2 item 2, [impl/12](impl/12-semantic-tooling.md) §12.1.2.
- **stage.** One of the eight implementation stages 0 to 7 of draft §84 with which every feature is tagged. v0.1 conformance covers Stages 0 to 4. [README](README.md) §4, [spec/01](spec/01-introduction.md) §1.4.5, [roadmap](roadmap.md) §1.
- **staged feature.** A construct, library item, target or tool operation tagged with a stage. A compiler that does not implement it rejects every use with `FEATURE_NOT_IMPLEMENTED` and never accepts it partially. [spec/01](spec/01-introduction.md) §1.4.5.
- **staged item table.** The list, embedded in the reference toolchain, of the library items of stages that a release does not implement, with their canonical paths and stages; name resolution consults it before reporting an unresolved name. [impl/01](impl/01-architecture.md) §1.12.1 item 4, §1.12.5.
- **staged mode.** The way the conformance suite runs a test of a stage above a claim: the test passes as `staged` if the implementation reports `FEATURE_NOT_IMPLEMENTED` for a higher stage, and is otherwise judged by its own expectations. [impl/15](impl/15-conformance-and-testing.md) §15.3.5.
- **standard package.** A package of the `sai` namespace that ships with the toolchain. `sai:core` and `sai:alloc` are implicit in every package; the others, such as `sai:process` or `sai:http`, are declared in `requires` like any dependency. [spec/16](spec/16-core-library.md) §16.1.1, §16.16, D-PKG-04.
- **Standard Fact Checker.** The deterministic decision procedure, with fixed limits, that defines which contract and refinement obligations count as proven. Every conforming compiler proves exactly the same obligations. [spec/09](spec/09-contracts-and-refinements.md) §9.6, D-CONTRACT-02.
- **structured diagnostics.** Diagnostics with stable codes, severities, spans in UTF-8 byte offsets and structured fixes, emitted as JSON in a fixed schema; required for v0.1. [spec/01](spec/01-introduction.md) §1.4.4 item 1, [impl/11](impl/11-diagnostics.md) §11.9.
- **structured fix.** A machine-applicable repair attached to a diagnostic, stated as edits that refer to semantic node IDs. [impl/11](impl/11-diagnostics.md) §11.7, D-TOOL-01.
- **symbol.** An entity with a canonical path that is not a package or a module, such as an item, a member or a test. [impl/12](impl/12-semantic-tooling.md) §12.2.1.
- **symbol ID.** An identifier of a symbol of the form `symbol://<ns>:<pkg>/<module path>/<item>[/<member>]#<6 hex digits>`. The path part only describes the symbol; its identity is the package node and the hexadecimal part, which the symbol index keeps across renames and moves. [impl/12](impl/12-semantic-tooling.md) §12.2, D-TOOL-02.

## T

- **target.** The platform a build produces code for: `wasm32-core`, `wasm32-wasi`, `wasm64-core`, `wasm64-wasi`, and the native targets of Stage 7. v0.1 requires `wasm32-core` and `wasm32-wasi`. [targets](targets/targets-and-profiles.md), D-TGT-01, [spec/01](spec/01-introduction.md) §1.4.2 item 5, N-J-1.
- **target capability.** A capability that only some targets provide: `threads`, `host`, `native_ffi`, `process` and `memory64`. Reaching one that the target does not provide is a compile error. [spec/08](spec/08-effects-and-capabilities.md) §8.12.7, D-CAP-01.
- **`tiny`.** The profile that optimizes for size: panics trap without a report, there is no debug information, a small allocator is used, and some capabilities are denied by default. Its checks and results are those of every other profile. D-TGT-02, C-03, [targets](targets/targets-and-profiles.md).
- **trap.** The termination of a Wasm instance by a Wasm trap. Every panic ends in a trap; a plain trap, such as an unreachable instruction, has no panic kind or report. [spec/01](spec/01-introduction.md) §1.8, [spec/10](spec/10-errors-and-panics.md) §10.7.
- **two-phase borrow.** An auto-referenced `borrow mut` receiver of a method call or of `IndexMut` indexing, which is reserved when the receiver is evaluated but activated only at the call, so that the arguments may still read the borrowed place, as in `v.push(v.len())`. [spec/07](spec/07-ownership-and-borrowing.md) §7.10.

## U

- **undefined behavior.** Behavior on which the specification places no requirement. It arises only when unsafe code violates a safety contract of [spec/12](spec/12-unsafe-and-ffi.md) or of an `unsafe fn` of the core library; safe code can never cause it. [spec/01](spec/01-introduction.md) §1.5.1, [spec/12](spec/12-unsafe-and-ffi.md) §12.6.
- **unsafe.** The marker of code whose safety the programmer, not the compiler, guarantees: an `unsafe` block, an `unsafe fn` or an `unsafe impl`. Only an unsafe context may perform the unsafe operations, such as dereferencing a `Ptr` or `PtrMut` or calling an `unsafe fn`. [spec/12](spec/12-unsafe-and-ffi.md) §12.2, §12.3.
- **unspecified behavior.** Behavior for which the specification allows a set of outcomes and does not require the compiler to document its choice, such as the bits of a NaN that an operation produces. The list is closed. [spec/01](spec/01-introduction.md) §1.5.3, N-J-2.
- **user effect.** An effect declared with `effect name covers { .. }` and named by its module path, such as `db.write`. Inside its package it stands for its covers set; outside it is opaque, and capability checking expands it. [spec/08](spec/08-effects-and-capabilities.md) §8.4, D-EFF-03.

## V

- **v0.1 conformance.** A conformance claim that covers Stages 0 to 4 and structured diagnostics, on at least `wasm32-core` and `wasm32-wasi` in the `dev`, `release` and `tiny` profiles. [spec/01](spec/01-introduction.md) §1.4.2, §1.4.3, N-J-1, [roadmap](roadmap.md) §2.
- **view type.** A type that borrows data it does not own and carries exactly one borrow region: `Str`, `Slice<T>`, `SliceMut<T>`, `borrow T` and `borrow mut T`. The shared views are `Copy`. [spec/07](spec/07-ownership-and-borrowing.md) §7.1.1, §7.8.1, D-TYPE-02.

## W

- **WASI.** The WebAssembly System Interface. SAI targets WASI 0.2 or later through the Component Model; WASI preview1 is out of scope. D-TGT-01, [targets](targets/targets-and-profiles.md).
- **wasi target.** `wasm32-wasi` or `wasm64-wasi`: a target whose output is a component for WASI, as opposed to a core target. `wasm64-wasi` is experimental. D-TGT-01, [spec/01](spec/01-introduction.md) §1.4.2 item 5.
- **WAVE.** The WebAssembly Value Encoding, the text form in which conformance tests write the arguments and results of calls to an exported function of a reactor component. [impl/15](impl/15-conformance-and-testing.md) §15.5.4.
- **WIT.** The WebAssembly Interface Type language of the Component Model. SAI interfaces and worlds map one-to-one to WIT interfaces and worlds, and the compiler reads the `wit/` directory of a package directly (Stage 4). [spec/13](spec/13-components-and-wit.md), D-WIT-01, D-WIT-02.
- **WIT unit.** One WIT package as the reference compiler loads it: a WASI package embedded in the toolchain, the `wit` directory of a package, or the WIT generated from a source package. [impl/16](impl/16-interfaces-and-wit.md) §16.2.1.
- **workspace.** The root package and its workspace members: the root package itself and every package of the resolved graph that is required `from path` and whose directory lies inside the root package's directory. Workspace members are formatted, linted and analyzed like the root package. [impl/13](impl/13-cli.md) §13.2.2.
- **world.** The declaration of the contract of a component: the interfaces it imports and exports, each named individually. A world maps to a WIT world (Stage 4). [spec/13](spec/13-components-and-wit.md) §13.4, D-WIT-01, D-WIT-03.
