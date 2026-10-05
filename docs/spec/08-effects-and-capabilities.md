# 08. Effects and Capabilities

**Status:** Normative
**Stage:** 3 (interface effects and world-related rules: Stage 4; async attribution: Stage 5)
**Related:** [03-grammar.md](03-grammar.md), [05-types.md](05-types.md), [07-ownership-and-borrowing.md](07-ownership-and-borrowing.md), [09-contracts-and-refinements.md](09-contracts-and-refinements.md), [11-async-and-concurrency.md](11-async-and-concurrency.md), [12-unsafe-and-ffi.md](12-unsafe-and-ffi.md), [13-components-and-wit.md](13-components-and-wit.md), [../impl/08-effect-and-capability-analysis.md](../impl/08-effect-and-capability-analysis.md), [../package/01-package-contract.md](../package/01-package-contract.md), [../package/04-distribution-and-security.md](../package/04-distribution-and-security.md), [../targets/targets-and-profiles.md](../targets/targets-and-profiles.md), [decisions.md](../design/decisions.md) D-EFF-01..03, D-CAP-01..02

This chapter defines **effects** (what code does to the world outside its own memory) and **capabilities** (what a package is permitted to do). It specifies the effect atoms and the algebra of effect sets, the built-in effect vocabulary, user-defined and interface effects, effect clauses on functions, trait methods, bounds and function pointer types, the exact inference of a function's effect set, the rules for when an explicit clause is required, the checking of declared against actual effects, and the capability policy: its meaning, the mapping from effects to capabilities, per-package and transitive checking, target capabilities, and the explanation chain reported with every violation. The surface syntax of effect clauses is defined in [03-grammar.md](03-grammar.md); the syntax of `allow`, `deny`, `policy`, `profile` and `export world` is defined in [package/01](../package/01-package-contract.md). The algorithms that implement this chapter are in [impl/08](../impl/08-effect-and-capability-analysis.md).

---

## 8.1 Overview and terminology

### 8.1.1 Effects and capabilities

- An **effect** is a statically known, named description of an interaction with the world outside the program's own linear memory: allocation from the global heap, a call into a host-provided import, and so on. Every function has an **effect set**. A function whose effect set is empty is **pure**.
- A **capability** is a name in a package's policy that grants the right to perform a class of effects. Capabilities are checked per build, against the code that the build can reach (§8.12).
- Effects describe *code*; capabilities describe *packages*. An effect clause on a function states an upper bound on what the function may do. A package policy states an upper bound on what the code of the package, and everything it pulls in, may do (draft §21–22).
- Effects are a purely static property. They have no runtime representation, they never change the meaning of a well-typed program, and they are never inferred from runtime behaviour (P2). Whether a program is accepted depends only on the deterministic procedures of this chapter and of [impl/08](../impl/08-effect-and-capability-analysis.md), never on optimization.
- **Panics are not effects** (D-EFF-03). A panic, a trap, an `assert`, an arithmetic overflow check, and a bounds check contribute nothing to an effect set ([10-errors-and-panics.md](10-errors-and-panics.md)).
- **Unsafety is not an effect.** `unsafe` operations ([12-unsafe-and-ffi.md](12-unsafe-and-ffi.md)) contribute nothing to an effect set unless they call a function that has effects.
- **Deallocation is not an effect** (D-OWN-05).

### 8.1.2 Why effects are sound

The language has no `static` and no `static mut`, and only import functions (WIT imports and `extern "wasm"` functions) can affect the outside world (D-CAP-02). Every observable interaction therefore passes through a call to an import, or through the allocator. This chapter attributes an effect to each of these primitive operations (§8.3, §8.4) and propagates effects through every call, operator, closure, generic instantiation and await (§8.7). The effect set of a function is therefore a sound upper bound of the primitive operations that executing the function can perform.

### 8.1.3 Terminology

| Term | Meaning |
|---|---|
| effect atom | A single effect: a built-in effect, an interface effect, or a user effect (§8.2.1). |
| effect set | A finite set of effect atoms (§8.2.3). |
| declared set `D(f)` | The set written in the `effects` clause of `f`, if present. |
| own set `Own(f)` | The effects that the body of `f` performs, excluding latent effects (§8.7). |
| signature set `Sig(f)` | `D(f)` if `f` has a clause, otherwise the inferred set `Inf(f)` (§8.7.1). Callers see `Sig(f)`. |
| latent effect | An effect reached through a type parameter; attributed to the instantiating caller (D-EFF-02, §8.8). |
| permits | The relation by which a declared set allows an actual atom (§8.2.4). |
| defining package | The package containing the `effect` declaration of a user effect. |
| capability | A name in `allow`/`deny`: `alloc`, a WIT package or interface, or a target capability (§8.12.1). |
| required capabilities `Req` | The capabilities that the reachable code of a build needs (§8.12.4). |

---

## 8.2 Effect atoms and effect sets

### 8.2.1 Effect atoms

There are exactly three kinds of effect atom:

1. A **built-in effect**: one of the fifteen names of §8.3 (D-EFF-03). Built-in effects are global; their identity is their name.
2. An **interface effect**: the effect of calling a function of an imported WIT interface (D-EFF-03, §8.5). Its identity is the versionless interface name `ns:pkg/iface` (for example `wasi:http/outgoing-handler`). The version of the interface is fixed by the resolved dependency graph and recorded in `sai.lock` ([package/03](../package/03-lock-file.md)); it is not part of the atom (N-D-1).
3. A **user effect**: an effect declared with `effect` (§8.4). Its identity is its declaring symbol (`symbol://ns:pkg/module/name`, D-TOOL-02), not the path used to name it.

Two atoms are equal exactly when they are of the same kind and have the same identity. Paths, aliases and re-exports never create new atoms.

### 8.2.2 Naming effects in source

An effect is written as an `EffectRef` ([03 §3.5](03-grammar.md)): one or more lower identifiers separated by `.`. An `EffectRef` `s1.s2.….sn` is resolved as follows.

1. The **built-in roots** are `alloc`, `fs`, `net`, `clock`, `random`, `stdio`, `env`, `process`, `thread`, `host` and `ffi`. If `s1` is a built-in root, the `EffectRef` denotes a built-in effect: the dotted text MUST be exactly the name of a built-in effect of §8.3, otherwise the reference is the error `UNKNOWN_EFFECT`. This rule takes precedence over every binding in scope, including module aliases and import bindings with the same name (D-EFF-03). For example, in a module that has `use acme:files/fs`, the reference `fs.read` still means the built-in effect, and `fs.sync` is `UNKNOWN_EFFECT` even if module `fs` declares an effect `sync`.
2. Otherwise, `s1 … sn` is resolved as a path in the value/type namespace of the enclosing module (D-SYN-03, [04-modules-and-names.md](04-modules-and-names.md)). The path MUST resolve to an `effect` declaration (a user effect), to an import binding of a WIT interface (an interface effect, §8.5), or to a world that declares `import FnHead` items (a world-import effect, §8.5.1). A path that resolves to anything else, or to nothing, is `UNKNOWN_EFFECT`.
3. A path that resolves to a user effect that is not visible at the point of reference (§8.4.2) is the error `EFFECT_NOT_VISIBLE`.

When an import binding or module alias has the same name as a built-in root (for example `use wasi:random/random`, which binds `random`), the binding cannot be named in effect position. The diagnostic `UNKNOWN_EFFECT` in that case carries the help "rename the binding with `as`", for example `use wasi:random/random as rng` followed by `effects { rng }`.

### 8.2.3 Effect sets

- An effect set is a finite mathematical set of atoms. Order and repetition in source are not significant: `effects { alloc, fs.read }` and `effects { fs.read, alloc }` denote the same set.
- Naming the same atom twice in one `EffectSet` (directly, or through two paths that resolve to the same atom) is the warning `DUPLICATE_EFFECT`, with a structured fix that removes the later occurrence.
- `effects {}` denotes the empty set. A function with `effects {}` is declared pure.
- The operations used in this chapter are union (`∪`), set difference (`∖`) and the permits relation (§8.2.4). There is no intersection in source and there are no effect variables (D-EFF-02).
- **Canonical order.** Wherever the toolchain prints an effect set (diagnostics, structured fixes, `sai inspect effects`, semantic diff, generated documentation), it MUST print the atoms in this canonical order (N-D-2):
  1. built-in effects, in the order of the table in §8.3;
  2. interface effects, ordered by their identity string in byte-wise lexicographic order;
  3. user effects, ordered by the source spelling that the printing context would use, byte-wise lexicographic.

  Atoms are separated by `, ` on one line. The printed form of the empty set is `{}`.

### 8.2.4 The permits relation

Let `P` be the package in which a check takes place (the package that contains the declaration being checked). A declared set `D` **permits** an atom `a` in `P`, written `D ⊨ₚ a`, when at least one of the following holds:

1. `a ∈ D`.
2. `a` is an interface effect of an interface in the WIT package `cap(e)` for some built-in effect `e ∈ D`, where `cap` is the capability column of §8.3 (D-EFF-03), or `a` is an interface effect of an interface of `wasi:io` and `cap(e)` is a WIT package of the `wasi` namespace for some built-in effect `e ∈ D` (N-D-100). For example, `{ fs.read }` permits `wasi:filesystem/types` and `wasi:filesystem/preopens`, because `cap(fs.read) = wasi:filesystem`, and it permits `wasi:io/streams`, which reads the streams that `wasi:filesystem` opens. Built-in effects whose capability is not a WIT package (`alloc`, `process.spawn`, `thread.spawn`, `host`, `ffi`) permit no interface effects through this rule.
3. `a` is a member of `covers*(u)` for some user effect `u ∈ D` whose defining package is `P` (§8.4.3).
4. `D` permits, by rule 3, a built-in effect `e` in `covers*(u)`, and `a` is permitted by `{ e }` through rule 2.

A set `A` is **permitted by** `D` in `P`, written `A ⊑ₚ D`, when `D ⊨ₚ a` for every `a ∈ A`. The empty set is permitted by every set.

Properties (normative, relied on by [impl/08](../impl/08-effect-and-capability-analysis.md)):

- `⊑ₚ` is reflexive and transitive, and `A ⊑ₚ D` and `B ⊑ₚ D` imply `A ∪ B ⊑ₚ D`.
- `⊑ₚ` is monotone in `D`: `A ⊑ₚ D` implies `A ⊑ₚ D ∪ D'`.
- Outside the defining package of `u`, `{ u } ⊨ₚ a` holds only for `a = u`: user effects are **opaque** (§8.4.3).
- The relation is decidable in time linear in the size of `A` times the size of the expanded `D` (impl/08 §8.4).

Note that `fs.read` and `fs.write` both map to `wasi:filesystem`, so either of them permits every `wasi:filesystem/*` interface effect. The read/write distinction is maintained at the effect level, by the declared clauses of the library functions that wrap the filesystem (R-13), not by the WIT interfaces. Likewise `stdio.read`, `stdio.write`, `env.read` and `process.exit` each permit every `wasi:cli/*` interface effect. Every built-in effect of the `wasi` namespace also permits every `wasi:io/*` interface effect, because every other WASI 0.2 package hands out `wasi:io` resources and `wasi:io` grants no authority of its own; this is the effect-level counterpart of the `wasi:io` implication of the [targets doc](../targets/targets-and-profiles.md) §3.3 items 1 and 5 (§8.12.5).

---

## 8.3 Built-in effects

### 8.3.1 The built-in effect table

The built-in vocabulary of v0.1 is fixed (D-EFF-03). No other built-in effect exists, and a package cannot add one.

| Effect | Meaning | Produced by | Capability `cap(e)` |
|---|---|---|---|
| `alloc` | allocation from the global heap or from an arena | the allocation primitives of the core library ([16-core-library.md](16-core-library.md)), and every operation that reaches them: `Box`, `Vec`, `String` growth, f-strings and `Str + Str` (D-EXPR-12/13), arena creation and arena allocation (D-OWN-07), `cabi_realloc` reached from a component export (§8.14.3) | `alloc` |
| `fs.read` | reading files and directories, reading metadata | library functions that declare it and wrap `wasi:filesystem` imports | `wasi:filesystem` |
| `fs.write` | creating, modifying and deleting files and directories | as above | `wasi:filesystem` |
| `net.connect` | sockets, name resolution (DNS) | library functions that wrap `wasi:sockets` imports | `wasi:sockets` |
| `net.http` | outgoing HTTP requests | library functions that wrap `wasi:http` imports | `wasi:http` |
| `clock.read` | reading wall-clock and monotonic time, waiting for time to pass | library functions that wrap `wasi:clocks` imports | `wasi:clocks` |
| `random.read` | reading randomness | library functions that wrap `wasi:random` imports | `wasi:random` |
| `stdio.read` | reading standard input | library functions that wrap `wasi:cli` imports | `wasi:cli` |
| `stdio.write` | writing standard output or standard error | as above | `wasi:cli` |
| `env.read` | reading command-line arguments and environment variables | as above | `wasi:cli` |
| `process.exit` | terminating the process with a status | as above | `wasi:cli` |
| `process.spawn` | spawning processes (native targets only) | native library functions | `process` |
| `thread.spawn` | spawning threads | thread primitives (not implemented in v0.1, D-THREAD-01) | `threads` |
| `host` | calling a raw `extern "wasm"` import | every call of a function declared in an `extern "wasm"` block (D-UNSAFE-01) | `host` |
| `ffi` | calling native foreign functions | every call of a function in an `unsafe extern "C"` block (Stage 7) | `native_ffi` |

The order of this table is the canonical order of built-in effects (§8.2.3).

### 8.3.2 How built-in effects arise

A built-in effect enters an effect set in exactly one of three ways:

1. **Primitive operations.** A call of an `extern "wasm"` function has the effect `host`; a call of an `unsafe extern "C"` function has the effect `ffi` (Stage 7). The allocation primitives of the core library are declared `effects { alloc }`; their bodies are the only place where the language reaches the global allocator ([impl/10](../impl/10-runtime.md)).
2. **Declared clauses.** A function that declares a built-in effect `e` presents `e` to its callers (`Sig(f) = D(f)`, §8.7.1), regardless of which of the permitted atoms its body actually performs. This is how the standard library turns raw interface effects into the built-in vocabulary: a function that calls `wasi:filesystem/types` functions and declares `effects { fs.read }` is seen by every caller as having `fs.read`. The same clause covers the `wasi:io/streams` and `wasi:io/poll` calls that such a function makes on the streams and pollables it obtains (§8.2.4 rule 2).
3. **Propagation.** Every other occurrence of a built-in effect is propagated from callees (§8.7).

Example (accepted). The capability-passing style of D-CAP-02; the `FileSystem` type and `read_to_string` belong to the standard library ([16-core-library.md](16-core-library.md)):

```sai
use sai:fs.{FileSystem, IoError}

pub fn load_config(fs: borrow FileSystem, path: Str) -> Result<String, IoError>
    effects { fs.read, alloc }
{
    let text = fs.read_to_string(path)?
    return Ok(text)
}
```

### 8.3.3 Effects of literals and moves

The following never have effects: evaluating literals (including string literals, which are static data), moves and copies, borrows, field and tuple access, pattern matching, arithmetic and comparison on primitive types, conversions between primitive types, constructing structs, tuples, enums and resources by literal syntax, creating closures (§8.7.6), creating references to functions, `return`, `break`, `continue`, and constructing (but not awaiting) futures (§8.10).

---

## 8.4 User effects

### 8.4.1 Declaration

A user effect is declared with `EffectDecl` ([03 §3.2](03-grammar.md)):

```sai
pub effect write
    covers { net.connect, alloc }

effect audit
```

- The name is a lower identifier (D-LEX-02). It lives in the single namespace of its module (D-SYN-03); a clash with another item of the module is `DUPLICATE_BINDING`.
- The name MUST NOT be a built-in root (§8.2.2); declaring `effect fs` or `effect alloc` is the error `EFFECT_NAME_RESERVED`.
- The `covers` set may name built-in effects, interface effects and other user effects visible at the declaration. An omitted `covers` clause means `covers {}`: the effect is a pure marker that permits nothing beyond itself.
- The covers relation MUST be acyclic. If `u` reaches itself through `covers` (directly or through other user effects), every declaration on the cycle is the error `EFFECT_COVERS_CYCLE`.
- `covers*(u)` is the reflexive-transitive closure: `u` itself, every atom in `covers(u)`, and `covers*(v)` for every user effect `v ∈ covers(u)`.

### 8.4.2 Visibility

- A user effect has the visibility of its declaration ([04-modules-and-names.md](04-modules-and-names.md)): a private effect can be named only inside its module; a `pub` effect can be named wherever its module path is visible.
- An effect is referred to by module path, as in `db.write` for the effect `write` in module `db` (D-EFF-03).
- A `pub` function MUST NOT declare a non-`pub` user effect in its `effects` clause, because callers outside the module could not name it in their own clauses. Doing so is the error `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` (N-D-3). The same applies to the `effects` clause of a `pub` trait's methods, of bounds in `pub` signatures, and of function pointer types in `pub` signatures.

### 8.4.3 Opacity

- **Inside the defining package**, a declared set containing `u` permits every atom in `covers*(u)` (§8.2.4 rule 3). A function in package `acme:db` that declares `effects { db.write }` may call functions with `net.connect` and `alloc`.
- **Outside the defining package**, `u` is opaque: it permits only itself. A caller in another package that calls a function with `db.write` must itself declare `db.write` (or a user effect of its own package whose `covers` contains `db.write`), even if it already declares `net.connect` and `alloc`.
- Opacity concerns permission only. Capability checking always expands `covers*` regardless of package boundaries (§8.12.2), so a user effect never hides authority from a policy (P5).

Example. Package `acme:db`, module `db`:

```sai
use sai:net.{NetError, Socket}

pub effect write
    covers { net.connect, alloc }

pub fn insert(sock: borrow mut Socket, row: Bytes) -> Result<(), NetError>
    effects { write }
{
    sock.send(row)?
    return Ok(())
}
```

Package `acme:app`:

```sai
use acme:db
use sai:net.{NetError, Socket}

pub fn save(sock: borrow mut Socket, row: Bytes) -> Result<(), NetError>
    effects { db.write }
{
    return db.insert(sock, row)
}

// Outside acme:db the effect db.write is opaque, so { net.connect, alloc } does not permit it.
pub fn save_wrong(sock: borrow mut Socket, row: Bytes) -> Result<(), NetError>
    effects { net.connect, alloc }
{
    return db.insert(sock, row) // rejected: EFFECT_NOT_DECLARED
}
```

---

## 8.5 Interface effects (Stage 4)

### 8.5.1 Definition

- Calling a function of an imported WIT interface `ns:pkg/iface` has the interface effect `ns:pkg/iface` (D-EFF-03). This includes the interface's free functions and the constructors, methods and static functions of the resources it defines ([13-components-and-wit.md](13-components-and-wit.md)).
- Dropping an owned handle of an imported resource has no effect (N-D-4, §8.7.5). Passing, returning and moving handles have no effect.
- Calling a function imported directly by a world (`import FnHead`, [03 §3.9](03-grammar.md)) has a **world-import effect** whose identity is `ns:pkg/world`, where `ns:pkg` is the package that declares the world and `world` its kebab-case name (N-D-5). It is an interface effect for every purpose of this chapter. In source it is named by the path of the world declaration (for example `effects { app_world }`).
- An interface declared in the current package and exported by a world is *implemented*, not imported. Calling the implementing functions from inside the package is an ordinary call (§8.7.2) and does not produce an interface effect.

### 8.5.2 Naming interface effects

An interface effect is named through the import binding of the interface (D-EFF-03, D-MOD-01): `use wasi:http/outgoing-handler` binds `outgoing_handler`, and the effect is written `effects { outgoing_handler }`. An alias introduced with `as` names the same atom. A `pub use` re-export of the binding is a further path to the same atom.

```sai
use wasi:http/outgoing-handler
use wasi:http/types.{ErrorCode, FutureIncomingResponse, OutgoingRequest}

// Warning AMBIENT_IMPORT (D-CAP-02): application code imports WASI directly.
fn send_raw(req: owned OutgoingRequest) -> Result<FutureIncomingResponse, ErrorCode>
    effects { outgoing_handler }
{
    return outgoing_handler.handle(req, None)
}
```

`effects { net.http }` would also be accepted here, because `cap(net.http) = wasi:http` (§8.2.4 rule 2). Constructing an `OutgoingRequest` would additionally require `types` or `net.http`.

### 8.5.3 Ambient imports

Importing an interface of a `wasi:` package with `use` in a package outside the `sai` namespace is the lint `AMBIENT_IMPORT` (warning, D-CAP-02). The lint fires once per `use` declaration. `policy { deny ambient_imports }` in `sai.package` makes it an error ([package/01](../package/01-package-contract.md)). Packages in the `sai` namespace that ship with the toolchain ([16-core-library.md](16-core-library.md)) are exempt, because they are the capability-passing layer.

---

## 8.6 Effect clauses

### 8.6.1 Positions

An `effects` clause may appear in exactly these positions ([03-grammar.md](03-grammar.md)):

| Position | Syntax | Meaning |
|---|---|---|
| free function, inherent method, trait impl method, interface impl function | `EffectsClause` in `FnClauses` | upper bound of the function's own effects (§8.6.2) |
| trait method declaration | `EffectsClause` in the `FnHead` of a trait item | upper bound for every impl of the method (§8.6.3) |
| generic bound | `BoundEffects` after a `BoundNoFx` | cap on the latent effects reached through that bound (§8.8.3) |
| function pointer type | `FnPtrTypeFx` | the effects of calling a value of that type (§8.6.4) |
| user effect declaration | `covers` `EffectSet` | the covered set (§8.4) |

In addition:

- The functions of an `interface` declaration MUST NOT carry an `effects` clause (`EFFECTS_ON_INTERFACE_FN`, N-D-6). WIT has no notion of effects (P4); the effect of calling an imported interface function is its interface effect (§8.5), and the effects of an implementation are declared on the functions of the `impl` of the interface.
- Functions in an `extern "wasm"` block have the effect set `{ host }`, and functions in an `unsafe extern "C"` block the effect set `{ ffi }`, by definition (D-UNSAFE-01). An `effects` clause on such a function is optional ([12 §12.7.4, §12.9.2](12-unsafe-and-ffi.md)); if written, it MUST be exactly `{ host }`, respectively `{ ffi }`; any other set is `EXTERN_EFFECTS_MISMATCH`.
- Closures have no `effects` clause (`ClosureExpr`, [03 §3.12](03-grammar.md)); their effects are always inferred (§8.7.6).
- The clause order is `where`, `borrows`, `effects`, `requires`, `ensures` (D-SYN-06, `CLAUSE_ORDER`).

### 8.6.2 Function clauses

- `effects { … }` on a function `f` is an **upper bound**: `Own(f) ⊑ₚ D(f)` MUST hold (§8.9), where `P` is the package of `f`. The function may perform fewer effects than declared.
- An omitted clause **always** means *infer* (D-EFF-01). The meaning of an omitted clause never depends on visibility; visibility only decides whether omitting it is an error or a warning (§8.9.3).
- Callers see the declared set, not the actual one: `Sig(f) = D(f)`. Declaring a set therefore fixes the function's effect contract, and changing the body within that bound does not change any caller (P8).

### 8.6.3 Trait method clauses

- An `effects` clause on a trait method declaration is an upper bound for every implementation of the method, including the trait's default body (D-EFF-01). For every impl, `Sig(impl method) ⊑ₚ D(trait method)` MUST hold, where `P` is the package of the impl, otherwise `TRAIT_IMPL_EFFECTS_EXCEED_BOUND`. If the impl method has its own clause, the check uses that clause, not its inferred set (declaring more than the trait allows is an error even if the body performs less).
- A trait method declaration without a clause has **no bound**. Its effects are latent: each impl may have any effects, and a call through a type parameter reports the effects of the impl actually chosen by the instantiating caller (§8.8).
- `Drop.drop` is declared `effects {}` (D-OWN-05). Every `Drop` impl is therefore pure. An impl whose `drop` has a non-empty declared or inferred set is the error `DROP_HAS_EFFECTS`, which is reported instead of `TRAIT_IMPL_EFFECTS_EXCEED_BOUND`.
- A `Drop` impl MUST NOT have latent effects either, because drop glue is not a call site at which latent effects could be attributed (§8.7.5). The body of `drop` MUST NOT contain a call through a type parameter of the impl or through `Self` (the first two rows of §8.7.3, and `.await` of a type-parameter future, §8.10), and no operation in it may have a residual bound (§8.8.2) whose trait has at least one method; every `Fn`-family bound and every `Future` bound is such a bound. A violation is `DROP_HAS_EFFECTS`, reported at the call (N-D-22). Consequently dropping a value never performs an effect, whatever the type arguments of its type.
- Trait impl methods cannot be `pub` (`VISIBILITY_ON_TRAIT_IMPL_ITEM`) and are never subject to `MISSING_EFFECTS_DECLARATION` or `PUBLIC_EFFECTS_INFERRED` (N-D-7). Their effects reach callers either through the trait's bound or as latent effects, both of which are visible in the signatures involved.

```sai
pub trait Render {
    fn render(self: borrow Self) -> String
        effects { alloc }
}

type Banner {
    text: String
}

impl Render for Banner {
    fn render(self: borrow Self) -> String {
        return f"** {self.text} **" // alloc: within the bound
    }
}

type Clock {
    seed: U64
}

// clock.read is not permitted by the cap { alloc } of the trait method.
impl Render for Clock {
    fn render(self: borrow Self) -> String
        effects { alloc, clock.read } // rejected: TRAIT_IMPL_EFFECTS_EXCEED_BOUND
    {
        return f"{self.seed}"
    }
}
```

### 8.6.4 Function pointer types

- A function pointer type carries its effect set in the type (D-EFF-02): `fn(I32) -> I32 effects { alloc }`. A function pointer type without a clause has the empty set; `fn(I32) -> I32` is the type of *pure* function pointers.
- Calling a value of type `fn(..) -> R effects E` has exactly the effects `E`.
- Two function pointer types are the same type only if their effect sets are equal as sets (order-insensitive, after resolution of every `EffectRef` to its atom). `fn() effects { alloc, host }` and `fn() effects { host, alloc }` are the same type; `fn() effects { fs.read }` and `fn() effects { outgoing_handler }` are different types even where one permits the other.
- A coercion of a function item `f<C̄>` (the item with its type arguments) to a function pointer type with effects `E` (D-TYPE-04, coercion 5) is permitted only if `Sig(f) ∪ Lat(f; C̄) ⊑ₚ E`, where `P` is the package in which the coercion occurs, and every residual bound of `Lat(f; C̄)` (§8.8.2) carries a cap `E'` with `E' ⊑ₚ E`; otherwise the coercion fails with `FN_POINTER_EFFECTS_MISMATCH` (N-D-24). The latent effects must be covered because a pointer can be called where no instantiating caller sees them. The same condition, with `Inf(K)` and the residual bounds of the body of `K` in place of `Sig(f) ∪ Lat(f; C̄)`, applies to every other coercion into a function pointer type that [05-types.md](05-types.md) permits (for example of a closure type `K`); whether such further coercions exist is decided there.
- In a function's `ReturnType` and in where-clause bounds a function pointer type with effects must be parenthesized ([03 §3.5](03-grammar.md)).

```sai
fn twice(x: I32) -> I32 {
    return x * 2
}

fn label(x: I32) -> String
    effects { alloc }
{
    return f"#{x}"
}

fn apply(f: fn(I32) -> I32, x: I32) -> I32 {
    return f(x) // pure: the type says so
}

fn demo() -> I32 {
    let p: fn(I32) -> I32 = twice // ok: {} ⊑ {}
    return apply(p, 21)
}

// Sig(label) = { alloc } is not permitted by the empty set of the pointer type.
fn demo_wrong() -> fn(I32) -> String {
    return label // rejected: FN_POINTER_EFFECTS_MISMATCH
}
```

---

## 8.7 Inference: the effect set of a function

### 8.7.1 Signature set, own set and inferred set

For every function-like body `f` (free function, method, trait default method, impl method, closure, async function body):

- `Own(f)` is the union of the **contributions** (§8.7.2–§8.7.8) of every operation in the MIR of `f` after drop elaboration (D-EFF-01, D-MIR-01). Every operation present in that MIR contributes, whether or not some run-time path reaches it; no optimization runs before effect analysis, so `Own(f)` never depends on optimizer strength. Which basic blocks exist is fixed by MIR construction ([impl/04](../impl/04-mir.md)).
- `Inf(f)` is defined only for a function without a clause: it is the component of the least solution of the system of equations `Inf(f) = Own(f)` over all functions without a clause (§8.7.9).
- `Sig(f) = D(f)` if `f` has a clause, otherwise `Sig(f) = Inf(f)`.
- Latent effects are never part of `Own(f)`, `Inf(f)` or `Sig(f)`. They are attributed at the call site that instantiates the type parameter (§8.8).

### 8.7.2 Calls of functions

Let a call site call the function item `g` with type arguments `C1 … Cn` (written or inferred; for methods, including the type arguments of the impl). Its contribution is

```
contrib(g<C1..Cn>(args)) = Sig(g) ∪ Lat(g; C1..Cn)
```

where `Lat` is defined in §8.8.2. This covers:

- calls of free functions and associated functions, by path or by method-call syntax;
- calls of inherent methods;
- calls of trait methods on a receiver of a concrete type, which resolve statically to the method of the selected impl ([05-types.md](05-types.md)); `g` is that impl method, and `Sig(g)` is the impl method's signature set, not the trait's bound;
- calls of trait methods whose impl uses the trait's default body; `g` is the default body instantiated for the impl's `Self` type, so the latent effects through `Self` are those of that impl (§8.8.2);
- calls of methods of interface resources implemented in the current package.

The contribution of evaluating the arguments is the union of their own contributions; argument order does not matter for effect sets.

### 8.7.3 Calls through type parameters, closures and function pointers

| Callee | Contribution |
|---|---|
| a trait method `B.m` called on a value whose type is a type parameter `T` of the enclosing generic (or `Self` inside a trait default body), with method-level type arguments `D̄` | `Latₘ(m; D̄)` — the effects of the impl are latent in `T` (§8.8); `Latₘ` is defined below |
| a value whose type is a type parameter `F` bounded by `Fn`/`FnMut`/`FnOnce` | `∅` — latent in `F` |
| a value of a concrete closure type `K` | `Inf(K)`, the inferred set of the closure body (§8.7.6) |
| a value of the unique ZST type of a function item `g` (D-TYPE-09) | `Sig(g) ∪ Lat(g; C̄)` as for a direct call |
| a value of function pointer type `fn(..) effects E` | `E` |
| a function of an imported WIT interface `i`, including resource constructors, methods and statics | `{ i }`, plus `alloc` if the import *allocates at the boundary* (§8.14.3, N-D-12) |
| a function imported directly by world `w` | `{ ns:pkg/w }` (§8.5.1), plus `alloc` under the same condition |
| a function in an `extern "wasm"` block | `{ host }` |
| a function in an `unsafe extern "C"` block (Stage 7) | `{ ffi }` |

A trait method may have type parameters of its own (`fn m<U: Fn()>(…)`). Their arguments are chosen at the call site, not by the impl, so their latent effects are attributed there even when the call goes through a type parameter: `Latₘ(m; D̄) = ⋃ { latᴮ(W[D̄/Ȳ]) | W: B a bound on m's own type parameters Ȳ }`, with `latᴮ` as in §8.8.2. It is `∅` for a method without type parameters.

### 8.7.4 Operators and other trait-based syntax

Every operator and syntactic form that [05-types.md](05-types.md) and [06-expressions-and-statements.md](06-expressions-and-statements.md) define in terms of a trait method contributes exactly what the corresponding method call would contribute (§8.7.2–§8.7.3). This includes arithmetic, comparison and indexing operators on non-primitive types, `for` loops (calls of the iterator protocol), `?` (the conversion of the error value), f-strings (formatting and `String` allocation) and string concatenation. In particular `Str + Str` and f-strings contribute `alloc` (D-EXPR-12/13). Operators on primitive types are built in and contribute `∅`.

### 8.7.5 Drops

A `Drop{place}` terminator contributes `∅` (N-D-4):

- `Drop.drop` is pure by D-OWN-05, so the drop glue of every type is pure;
- deallocation is not an effect (D-OWN-05);
- dropping an owned handle of an imported resource calls the canonical `resource.drop` built-in of the component model, which is not an interface function; it contributes `∅` even though the exporting component may run a destructor.

Drop edges are still edges of the call graph for reachability, and therefore for capability computation (§8.12.3): the code of a reachable `Drop` impl is reachable.

### 8.7.6 Closures

- Creating a closure (`Rvalue::Closure`, D-MIR-01) contributes `∅`, whatever it captures.
- A closure body is a body of its own. It has no clause; `Inf(K)` is inferred like a private function. The closure body is in the same generic context as its enclosing function, so calls through the enclosing function's type parameters inside the closure are latent in those parameters.
- Calling the closure contributes `Inf(K)` (§8.7.3). Passing the closure to a generic function makes its effects latent (§8.8): they are attributed to the function that instantiates the generic with the closure type, which is normally the function that created the closure.

```sai
fn total_len(words: borrow Vec<String>) -> USize {
    let measure = fn(w: borrow String) -> USize { return w.len() }
    var n: USize = 0
    for w in words.iter() {
        n = n + measure(w)
    }
    return n
} // Inf(total_len) = {}
```

### 8.7.7 Allocation

`alloc` is contributed only through calls (§8.7.2) and trait-based syntax (§8.7.4) that reach the allocation primitives of the core library, which declare `effects { alloc }`. Arena creation and allocation in an arena have `alloc` (D-OWN-07). There is no implicit allocation: a construct whose semantics in [06-expressions-and-statements.md](06-expressions-and-statements.md) does not call into the allocator contributes no `alloc`.

### 8.7.8 Panics, contracts, constants

- `panic`, traps, `assert`, the implicit checks of arithmetic and indexing, and `unreachable` paths contribute `∅` (D-EFF-03).
- `requires`, `ensures`, `invariant` and refinement predicates are not part of `Own(f)`; they must be pure on their own (§8.11).
- The initializer of a `const` item and every other const-evaluated expression has the empty effect set (N-D-8). Const expressions are restricted to literals, paths to consts and unit variants, built-in operators on primitives and constructions ([06 §6.27](06-expressions-and-statements.md), D-EXPR-07); calls, method calls and operator trait calls are `NOT_CONST_EXPRESSION`. None of the allowed forms contributes an effect (§8.3.3), so no effect check of constants exists. Compile-time evaluation never allocates on the run-time heap and never calls imports.

### 8.7.9 Recursion and mutual recursion

- Functions with a clause are fixed points by definition (`Sig(f) = D(f)`); a call to them never needs inference.
- The equations `Inf(f) = Own(f)` for functions without a clause are solved over the strongly connected components of the call graph restricted to those functions, in reverse topological order (callees before callers). Within one component the solution is the least fixpoint obtained by starting from `∅` and iterating; it exists and is unique because effect sets over the finite set of atoms mentioned in the program form a finite lattice and every contribution is monotone ([impl/08 §8.2](../impl/08-effect-and-capability-analysis.md)).
- Consequence: all functions without a clause in one strongly connected component have the same inferred set. Non-terminating recursion never produces effects by itself.

```sai
fn is_even(n: U32) -> Bool {
    if n == 0 {
        return true
    }
    return is_odd(n - 1)
}

fn is_odd(n: U32) -> Bool {
    if n == 0 {
        return false
    }
    return is_even(n - 1)
} // Inf(is_even) = Inf(is_odd) = {}

fn countdown(n: U32) -> String {
    if n == 0 {
        return "done".to_string()
    }
    return countdown(n - 1)
} // Inf(countdown) = { alloc }
```

---

## 8.8 Latent effects and generics

### 8.8.1 Principle

There are no effect variables, yet generic functions such as `map`, `filter` and `for_each` are effect-polymorphic (D-EFF-02). This works because a generic body never "owns" the effects it reaches through its type parameters: those effects are **latent** and are attributed to the caller that instantiates the parameters. Effects are computed on generic bodies and instantiated at call sites; they never depend on monomorphization.

### 8.8.2 Definition

Let `g` have type parameters `X1 … Xn`. The **bounds of `g`'s signature** are the trait predicates (trait bounds and `Fn`-family bounds) among the elaborated predicates of `g` ([05 §5.7.4](05-types.md)): the bounds and `where` predicates written on `g` and on its enclosing impl or trait, their supertrait bounds, and the bounds that the traits declare on their associated types (`type A: Bs` in `B` gives `X.A: Bs` for a bound `X: B`), recursively. A cap written on a bound (§8.8.3) also applies to every bound elaborated from it (N-D-25).

For a call `g<C1..Cn>`:

```
Lat(g; C1..Cn) = ⋃ { latᴮ(W[C̄/X̄]) | W: B is a bound of g's signature }
```

where `latᴮ(C)` for a type `C` and a bound `B` is:

1. If `C` is a type parameter of the *calling* function (or `Self` inside a trait default body), or an associated type projection on one, or more generally if the obligation `C: B` is answered by a predicate of the calling function (an environment candidate, [05 §5.11.2](05-types.md) step 4): `∅` — the effect stays latent and is attributed one level further out, to whoever instantiates the caller. Type checking guarantees that the caller's own elaborated predicates contain `C: B`, so the effect is part of the caller's own `Lat` (N-D-25).
2. If `B` is a trait: the union, over every method `m` of `B` (including methods with default bodies that the impl does not override), of `Sig(m_impl) ∪ Lat(m_impl; Ā)`, where `m_impl` is the method of the impl of `B` for `C` selected by trait resolution, `Ā` are the type arguments with which that impl matches `C`, and `Lat(m_impl; Ā)` ranges over the bounds of the impl and of `B`'s `Self` only; the bounds on `m`'s own type parameters are attributed at each call of `m` (`Latₘ`, §8.7.3). Recursion into `Ā` is how nested types are handled: for `Map<Filter<I, F1>, F2>` the effects of `Filter.next` and of `F1` and `F2` are included.
3. If `B` is `Fn`, `FnMut` or `FnOnce`: the effects of calling a value of type `C` (§8.7.3): `Inf(K)` for a closure type `K`, `Sig(f) ∪ Lat(f; …)` for a function item type, `E` for a function pointer type.

Each pair `(B, C)` is expanded once. A pair that is reached again while it is being expanded (for example through a default body that calls methods of `Self`) contributes the value being computed, and the least fixpoint is taken (§8.7.9, [impl/08 §8.2](../impl/08-effect-and-capability-analysis.md)). `Sig` of impl methods is computed once per generic impl, not per instantiation. Expansion normally descends into smaller type arguments; it can produce larger types only through predicates on constructed types (such as `where Vec<T>: B`). To keep it finite, an operation whose expansion reaches a pair `(B, C)` in which the type nesting depth of `C` ([05 §5.7.6](05-types.md)) exceeds 64 is the error `INSTANTIATION_DEPTH_EXCEEDED`, and an operation whose expansion needs a trait goal that fails with `TRAIT_SOLVER_OVERFLOW` ([05 §5.11.2](05-types.md)) reports that error; in both cases the operation contributes `∅` and the body's effect checks are skipped (N-D-26).

`Lat` depends only on `g`'s signature (its bounds) and on the impls selected for the arguments, never on `g`'s body (P8): every method of every bound counts, whether or not the body calls it.

**Residual bounds.** A bound `V: B'` of the calling function `h` is a **residual bound** of an operation of `h` if the effects reached through it stay latent in `h`:

- for a call or other use of `g<C̄>`: every bound at which the expansion of `Lat(g; C̄)` reaches rule 1, directly or inside the expansion of rule 2 or rule 3;
- for a call through a type parameter (the first two rows of §8.7.3) or `.await` of a type-parameter future (§8.10): the bound used, together with the residual bounds of `Latₘ`;
- for rule 3 or a call with a closure type `K` (or the future type of an `async` block): the residual bounds of the operations of the body of `K`, which is in the generic context of `h` (§8.7.6).

Residual bounds are the places where effects leave the view of `h`. Caps (§8.8.3), coercions to function pointers (§8.6.4), predicates (§8.11) and `Drop` impls (§8.6.3) restrict them.

### 8.8.3 Bound effect caps

A bound may carry an effect cap (`BoundEffects`):

```sai
fn map_all<F>(xs: borrow Vec<I32>, f: F) -> Vec<I32>
    where F: (Fn(I32) -> I32 effects {})
    effects { alloc }
{
    var out = Vec.with_capacity(xs.len())
    for i in 0..xs.len() {
        out.push(f(xs[i]))
    }
    return out
}
```

- At every instantiation `g<C̄>`, for every bound `W: B effects E` in `g`'s signature, `latᴮ(W[C̄/X̄]) ⊑ₚ E` MUST hold, where `P` is the package of the call site; otherwise `BOUND_EFFECTS_EXCEEDED`. For every residual bound of that expansion (§8.8.2; for example when the argument is itself a type parameter of the caller), the caller's bound MUST carry a cap `E'` with `E' ⊑ₚ E`; otherwise `BOUND_EFFECTS_EXCEEDED` is reported at the call site, with a help to add the cap to the caller's bound (N-D-20).
- A capped bound does not change attribution: the instantiating caller still receives `latᴮ(C)`, which is at most `E`.
- **Caps of trait impl methods.** A trait impl method MUST NOT rely on a cap that the trait method does not guarantee. For every cap `E` on a bound of the impl method's own type parameters (after the positional renaming of [05 §5.9.1](05-types.md) rule 6), the corresponding elaborated bound of the trait method MUST carry a cap `E'` with `E' ⊑ₚ E`, where `P` is the package of the impl; otherwise `TRAIT_ITEM_SIGNATURE_MISMATCH` is reported at the impl's cap. A caller that reaches the method through a type parameter checks only the trait's caps (`Latₘ`, §8.8.2), so a stricter cap in the impl would be relied upon without being checked. An impl bound with a weaker cap, or with none, is allowed (N-D-27).
- Inside `g`'s body, a cap is not needed for `Own(g)` (calls through type parameters contribute `∅`); its purpose is to let callers rely on a bound, for example to call `g` from a predicate (§8.11) or from a function with a narrow declared set.

```sai
fn sum_doubled(xs: borrow Vec<I32>) -> I32
    effects { alloc }
{
    let doubled = map_all(xs, fn(x: I32) -> I32 { return x * 2 })
    var s = 0
    for i in 0..doubled.len() {
        s = s + doubled[i]
    }
    return s
}

// The closure has { alloc }, but the cap of F is {}.
fn lengths(xs: borrow Vec<I32>) -> Vec<I32>
    effects { alloc }
{
    return map_all(xs, fn(x: I32) -> I32 { // rejected: BOUND_EFFECTS_EXCEEDED
        let _ = f"{x}"
        return x
    })
}
```

### 8.8.4 Trait default bodies

A default method body inside `trait B` is generic in `Self: B`. Calls of `B`'s methods on `self` are latent in `Self`. When an impl uses the default body, `latᴮ(C)` (§8.8.2 rule 2) includes the default body's own set and the effects of the impl's methods that the default body could reach through `Self`; the least fixpoint of §8.7.9 resolves the self-reference.

### 8.8.5 Attribution summary

For a call site `s` in function `h` that calls `g<C̄>`:

- `Sig(g)` is attributed to `h`.
- `Lat(g; C̄)` is attributed to `h`, except for the parts that rule 1 of §8.8.2 leaves latent in `h`'s own type parameters, which are attributed to `h`'s instantiating callers.
- This attribution is also the attribution used for per-package capability checks (§8.12.4): a closure written in package `A` and passed to a generic of package `B` charges its effects to `A`.

---

## 8.9 Checking

### 8.9.1 Declared covers actual

For every function `f` with a clause, `Own(f) ⊑ₚ D(f)` MUST hold, where `P` is the package of `f`. Each atom of `Own(f)` that `D(f)` does not permit is reported as `EFFECT_NOT_DECLARED` at the first operation that contributes it (in MIR order, impl/08 §8.5). The diagnostic carries:

- the missing atoms, in canonical order;
- for each missing atom, the shortest "why" chain from the contributing call to the primitive operation or declaration that introduced the atom (§8.13);
- a structured fix that replaces the clause with `D(f) ∪ missing`, in canonical order.

For every function without a clause, no check is needed: `Inf(f)` is exact by construction.

### 8.9.2 Further checks

- Trait impl methods against trait bounds: §8.6.3 (`TRAIT_IMPL_EFFECTS_EXCEED_BOUND`, `DROP_HAS_EFFECTS`).
- Bound caps at instantiation: §8.8.3 (`BOUND_EFFECTS_EXCEEDED`).
- Function pointer coercions: §8.6.4 (`FN_POINTER_EFFECTS_MISMATCH`).
- Predicates and contracts: §8.11 (`EFFECTFUL_PREDICATE`).
- Constants are pure by construction: §8.7.8.
- Visibility of user effects: §8.4.2 (`EFFECT_NOT_VISIBLE`, `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE`).
- A declared atom of a function that is neither `pub` nor `export` nor exposed through a world, and that is not needed to permit any atom of `Own(f)`, is the warning `UNUSED_EFFECT_DECLARATION`, with a fix that removes it (N-D-9). Public functions may over-declare deliberately, to keep room for future changes without breaking callers.

### 8.9.3 When an explicit clause is required

Following D-EFF-01 and C-04:

| Function | Clause omitted, `Inf(f) = ∅` | Clause omitted, `Inf(f) ≠ ∅` |
|---|---|---|
| private (not `pub`) | allowed | allowed (inference) |
| `pub`, package version `< 1.0.0` | allowed | warning `PUBLIC_EFFECTS_INFERRED` |
| `pub`, package version `≥ 1.0.0` | allowed | error `MISSING_EFFECTS_DECLARATION` |
| `export fn` (any version) | allowed | error `MISSING_EFFECTS_DECLARATION` |
| function exposed through a world or an interface (any version) | allowed | error `MISSING_EFFECTS_DECLARATION` |

- "`pub` function" means a function item, inherent method or associated function declared `pub`, independently of whether its module is reachable from outside the package. Trait impl methods are excluded (N-D-7). The entry point `pub fn main` is an ordinary `pub` function for this rule (N-D-10).
- "Exposed through a world or an interface" means: every function of an `impl <interface>` whose interface is exported by a world of the package, and every root-module function that implements a world's `export FnHead` ([13-components-and-wit.md](13-components-and-wit.md)).
- The package version is the version in the `package` declaration of `sai.package` ([package/01](../package/01-package-contract.md)). The comparison uses the major version only: a package whose major version is `1` or greater is "`≥ 1.0.0`", including pre-releases such as `1.0.0-rc.1` (N-D-11).
- Both diagnostics carry the inferred set and a structured fix that inserts `effects { … }` (canonical order) at the clause position required by D-SYN-06. Both diagnostics are reported once per function.
- The rule is about the clause, not the set: a function with a non-empty inferred set that already has a clause is never reported by this section.

---

## 8.10 Async attribution (Stage 5)

The rules of D-ASYNC-01 are restated here in terms of contributions; the async model itself is defined in [11-async-and-concurrency.md](11-async-and-concurrency.md).

- Calling an `async fn g` only constructs its anonymous future. The call contributes `∅`.
- The effects of `g`'s body (`Sig(g)`: its clause, or its inferred set) are carried by the anonymous future type of `g`. Awaiting that future with `.await` contributes `Sig(g) ∪ Lat(g; C̄)`, where `C̄` are the type arguments of the call that constructed it.
- `.await` on a value of a concrete future type `Q` contributes the effects of `Q`'s `Future.poll` impl, as a call would (§8.7.2). `for await` over a concrete stream type contributes the effects of its `Stream.poll_next` impl.
- `.await` or `for await` on a value whose type is a type parameter `F: Future` (or `S: Stream`) contributes `∅`; the effects are latent in `F` (§8.8). Executors and combinators of `sai:async` are generic over `F: Future`, so the effects of the futures they run are attributed to the function that instantiates them.
- `Future.poll` is `unsafe` (D-ASYNC-01); its unsafety is not an effect.
- The `effects` clause of an `async fn` bounds the effects of its body, not of the call.

---

## 8.11 Effects in contracts and predicates

- Every predicate in a refinement (`where`), a `requires` or `ensures` clause, or an `invariant` MUST have the empty effect set, without `alloc` (D-CONTRACT-03). The effect set of a predicate is computed by the rules of §8.7 applied to the predicate expression, including latent effects at every call in it: a predicate calling `g<C̄>` is effectful if `Sig(g) ∪ Lat(g; C̄)` is non-empty.
- A predicate with a non-empty effect set is the error `EFFECTFUL_PREDICATE`, reported at the first contributing operation, with the why chain of §8.13. No fix is offered.
- A predicate whose operations have residual bounds (§8.8.2), including calls through a type parameter of the enclosing declaration and calls of generic functions instantiated with one, is accepted only if each of these bounds carries the cap `effects {}`; otherwise `EFFECTFUL_PREDICATE`.
- Runtime checks inserted for predicates ([09-contracts-and-refinements.md](09-contracts-and-refinements.md)) are therefore pure, and they contribute nothing to the effect set of the function that contains them. The diagnostic message built when a check fails is produced by the panic machinery, which is not an effect (§8.1.1).

```sai
fn is_small(n: U32) -> Bool {
    return n < 1000
}

type Small = U32
    where is_small(self) // accepted: Sig(is_small) = {}

// The f-string allocates.
fn tag(n: U32) -> U32
    ensures f"{result}".len() > 0 // rejected: EFFECTFUL_PREDICATE
{
    return n
}
```

---

## 8.12 Capabilities

### 8.12.1 Capability names

There are exactly three kinds of capability name (D-CAP-01, C-07). Their surface syntax in `allow` and `deny` is defined by [package/01](../package/01-package-contract.md).

| Kind | Names | Meaning |
|---|---|---|
| allocation | `alloc` | use of the global allocator (and therefore linking it, D-WASM-05) |
| WIT package | `ns:pkg`, e.g. `wasi:http` | every interface of that WIT package |
| WIT interface | `ns:pkg/iface`, e.g. `wasi:filesystem/types` | that interface only |
| target capability | `threads`, `host`, `native_ffi`, `process`, `memory64` | features provided by the target ([targets doc](../targets/targets-and-profiles.md)) |

- A WIT package capability **contains** each of its interface capabilities. A capability `c` **covers** `r` when `c = r`, or `c` is a WIT package and `r` is an interface of it.
- Two capabilities **overlap** when one covers the other.
- A WIT package or interface name MUST denote a package (or an interface of a package) that is known to the build: a WASI package of the WASI version selected by the target, or a WIT package in the resolved dependency graph. Otherwise the name is the error `UNKNOWN_CAPABILITY`, raised at resolution time (§8.12.8). For example `wasi:filesystem/read` is unknown (R-13).
- `memory64` is required by every build for a `wasm64-*` target; it is not produced by any effect.

### 8.12.2 From effects to capabilities

The function `caps` maps an atom to a set of capabilities:

```
caps(e)  = { cap(e) }                      for a built-in effect e (§8.3)
caps(i)  = { i }                           for an interface effect ns:pkg/iface
caps(w)  = { ns:pkg/w }                    for a world-import effect (§8.5.1)
caps(u)  = ⋃ { caps(a) | a ∈ covers(u) }   for a user effect u
caps(A)  = ⋃ { caps(a) | a ∈ A }           for a set A
```

A user effect without `covers` maps to the empty set. Expansion of `covers` ignores opacity (§8.4.3) and terminates because `covers` is acyclic (§8.4.1).

### 8.12.3 Reachability

The capability check uses **reachability**, not optimizer dead-code elimination (N-D-13). The reachable set `Reach` of a build is the least set of bodies that contains the **roots** and is closed under the **edges** below. It is a set of bodies computed on generic MIR: an instantiation adds the bodies that it selects (the edges below), never a copy of a body. It is identical for every optimization level and profile.

Roots:

- `main`, when the artifact has an entry point, together with the core-library function that constructs its `process.Env` argument ([16-core-library.md](16-core-library.md), D-CAP-02, [impl/07 §7.8.4](../impl/07-component-lowering.md));
- every `export fn` of the root package, when the artifact is a core module (in a component build `export fn` items are not exported, [13 §13.6.4](13-components-and-wit.md));
- every function exposed through the `export world` of the root package (§8.9.3), and every boundary glue of §8.14.3;
- the `@panic_hook` function, if one is declared and the artifact is a core module. The hook is the reporting channel of core modules only; a component has no `host` and reports panics only through `wasi:cli/stderr` ([10 §10.5.3](10-errors-and-panics.md)), so its hook is never called and is not a root;
- under `sai test`, every test of the selected packages ([14-testing.md](14-testing.md)).

Edges, from a reachable body `f`:

- every function item named in `f`, whether called or used as a value (including coercion to a function pointer);
- every closure body created in `f`;
- for every call or other use of `g<C̄>` in `f`, and every bound `W: B` of `g`, every method of the impl of `B` for `W[C̄/X̄]` (the same impls as §8.8.2, including those reached recursively), and for `Fn`-family bounds the callee body of `C`;
- every `Drop` impl in the drop glue of every type dropped by a `Drop` terminator in `f` (its own `Drop` impl and, recursively, those of its fields and elements), stopping at type parameters of `f` and projections on them;
- for every use of `g<C̄>` in `f` (and for every call of a trait method with method-level type arguments `D̄`), the drop glue, in the same sense, of every type that the use substitutes for a type parameter: the types `C̄` (or `D̄`), the impl arguments `Ā` of the impls selected by the expansion of §8.8.2, and the associated types that those impls define. Together with the previous edge this reaches the `Drop` impl of every type that a reachable generic body can drop;
- for `.await` on a concrete future type, its `Future.poll` impl (§8.10);
- the allocator, if `alloc ∈ Own(f)` (D-WASM-05);
- every import function called in `f`. Imports are leaves, except a function of an interface that a hybrid dependency provides within the build ([13](13-components-and-wit.md) §13.5.2, N-D-104), which reaches the provider's glue and, through it, the provider's function (§8.14.3).

Function pointers make reachability an over-approximation: a function whose item is turned into a value is reachable even if the pointer is never called. This is intended (P8).

### 8.12.4 Required capabilities and attribution

For a body `f`, its **owner** is the package that contains its source (for a closure, the package of the enclosing function; for a trait default body used by an impl, the package of the trait). The required capabilities of a package `Q` in a build are

```
Req(Q) = ⋃ { caps(Own(f)) | f ∈ Reach, owner(f) = Q }
       ∪ { all imports of Q }           if Q is a binary Wasm component dependency
       ∪ { memory64 }                   if Q is the root and the target is wasm64-*
       ∪ TypeImp                         if Q is the root and the build produces a component
```

where `TypeImp` is the set of interfaces in `Imp` by rules 2 and 3 of [13 §13.5.2](13-components-and-wit.md): interfaces whose types occur in the boundary types of the component's exports or of other imported interfaces (N-D-14). They are charged to the root, whose world and exports cause them.

and the required capabilities of the build are `Req = ⋃ Req(Q)` over all packages `Q` of the build.

- Because `Own(f)` already includes the latent effects that `f` instantiates (§8.8.5), effects are charged to the package whose code chose them.
- For a dependency that is a binary Wasm component, all of its imports count as reachable (D-CAP-01), because the compiler cannot see inside it.
- `Own(f)` contains the signature sets of callees, not their actual sets. Capabilities therefore follow the *declared* contracts: a callee that declares `net.http` requires `wasi:http` from its caller's build even if its current body performs less (N-D-13). A library that wants to require less must declare less.

### 8.12.5 Policy meaning

Each package `Q` has a policy `(allow(Q), deny(Q))` given by its `sai.package` ([package/01](../package/01-package-contract.md)):

- `allow { … }` is the **maximum** grant (D-CAP-01, draft §22). It does not cause anything to be imported or linked; the compiler computes what is needed (§8.12.4) and checks it against the grant.
- An omitted `allow` means the empty set. A package without `allow` may contain only code whose reachable effects map to no capability. In particular, a package that allocates must allow `alloc`.
- `deny { … }` overrides `allow`. A deny entry may remove part of an allowed grant: `allow { wasi:http }` with `deny { wasi:http/outgoing-handler }` grants every interface of `wasi:http` except `outgoing-handler`.
- The same name in both `allow` and `deny` is the error `CAPABILITY_ALLOW_DENY_CONFLICT` (D-CAP-01). Overlapping but different names (as in the example above) are not an error.
- An omitted `deny` means the empty set.
- For the root package, the **effective policy** of a build also takes the selected profile into account (for example the `tiny` profile's default denials, D-TGT-02). How a profile modifies `allow` and `deny` is defined by the [targets doc](../targets/targets-and-profiles.md); this chapter checks against the resulting effective policy. Dependencies are always checked against their own `sai.package` policy, independent of the profile.

A required capability `r` is **granted** by a policy `(A, D)` when some `a ∈ A` covers `r`, and no `d ∈ D` overlaps `r`. When `r` is `wasi:io` or an interface of `wasi:io`, it is also granted when some `a ∈ A` is a capability of the `wasi` namespace other than `wasi:io` and its interfaces, and no `d ∈ D` overlaps `r`: this is the **`wasi:io` implication**, defined by the [targets doc](../targets/targets-and-profiles.md) §3.3 item 1, which every check of §8.12.6 uses, and on which [11 §11.10.2](11-async-and-concurrency.md) item 5 relies (targets §3.3 item 5). The implication changes no required set (targets §3.3 item 4). The overlap rule makes deny conservative: if code requires the package capability `wasi:http` (for example through `net.http`) and the policy denies `wasi:http/outgoing-handler`, the requirement is not granted, because the package capability includes the denied interface.

### 8.12.6 The checks

For every build (`sai check`, `sai build`, `sai test`, `sai run`), after effect inference has succeeded:

1. **Per package.** For every package `Q` of the build, every `r ∈ Req(Q)` MUST be granted by `Q`'s own policy.
2. **Transitive (root).** Every `r ∈ Req` (the union over all packages) MUST be granted by the root's effective policy.
3. **Target.** Every `r ∈ Req` MUST be provided by the target (§8.12.7).

Violations:

| Violation | Code |
|---|---|
| no `allow` entry of the checked policy covers `r` or implies it by the `wasi:io` implication (§8.12.5) | `CAPABILITY_NOT_ALLOWED` |
| `r` overlaps a `deny` entry of the checked policy | `CAPABILITY_DENIED` |
| `r` is not provided by the target | `TARGET_CAPABILITY_UNAVAILABLE` |

- Each violation is reported once per (checked policy, capability), with the policy's package, the capability, the why chain of §8.13, and a help. For `CAPABILITY_NOT_ALLOWED` the help offers a structured fix that adds `r` to the `allow` of that package's `sai.package`; for `CAPABILITY_DENIED` no fix is offered (removing a deny entry is a policy decision, P5).
- When `r` violates both deny and allow, only `CAPABILITY_DENIED` is reported.
- Check 3 is independent of the policy: a capability that the target cannot provide is an error even if it is allowed (draft §59).
- The checks do not depend on the order in which packages are compiled.

### 8.12.7 Target capabilities

Each target provides a fixed set of capabilities; the table in the [targets doc](../targets/targets-and-profiles.md) is authoritative. The following consequences are normative here:

- `wasm32-core` and `wasm64-core` provide no WASI package. Any reachable WASI interface effect, or any built-in effect whose capability is a WASI package, is `TARGET_CAPABILITY_UNAVAILABLE` on these targets (draft §39). They do provide `alloc` and `host`; `wasm64-core` also provides `memory64`.
- `wasm32-wasi` and `wasm64-wasi` provide the WASI 0.2 packages listed in the targets doc and `alloc`; `wasm64-wasi` also provides `memory64`.
- `host` is provided only when the artifact is a core module. A component, including one produced by `sai component build` for a core target, imports only WIT interfaces, so a reachable `extern "wasm"` call in a component build is `TARGET_CAPABILITY_UNAVAILABLE` ([13 §13.6.4](13-components-and-wit.md)).
- `threads` is provided by no v0.1 target (D-THREAD-01); `native_ffi` and `process` are provided only by native targets (not in v0.1).
- A WIT capability of a non-WASI package (an interface imported from another component) is provided by every component-producing target, because it is satisfied by composition, not by the host. On a core target it is unavailable, because a core module cannot import a WIT interface (§8.14.2, [13-components-and-wit.md](13-components-and-wit.md)). An interface that a hybrid dependency provides within the build is provided by every target, core targets included, because the build imports nothing for it ([13](13-components-and-wit.md) §13.5.2, §13.6.2 item 4).

### 8.12.8 Build time and resolution time

Capability information is used at two different times.

**Resolution time** (`sai add`, `sai update`, writing `sai.lock`; [package/02](../package/02-resolver.md), [package/03](../package/03-lock-file.md)):

- every capability name in every `allow` and `deny` of the graph is validated (`UNKNOWN_CAPABILITY`, `CAPABILITY_ALLOW_DENY_CONFLICT`);
- for each package, its **capability footprint** is computed and recorded in the lock as its resolved capabilities (D-PKG-03): for a source package, `caps` of the union of `Own(f)` over *all* of its bodies; for a binary component, all of its imports. The footprint is an upper bound of what the package can ever contribute to `Req`;
- the footprint is shown for review when a dependency is added or updated ([package/04](../package/04-distribution-and-security.md), draft §75). A footprint that exceeds the root's policy is reported there as a review item, never as an error, because unreachable code does not count.

**Build time** (`sai check`, `sai build`, `sai test`): the reachability-based checks of §8.12.6. Only build time can reject a program for capability reasons.

The footprint of a package changes only when the signature sets of its functions or the sets of imports change, which is what semantic diff reports ([impl/12](../impl/12-semantic-tooling.md)).

---

## 8.13 The "why" chain

Every diagnostic about effects or capabilities explains *why* the offending atom or capability is present (draft §49, §62). The chain is part of the diagnostic's structured data (D-TOOL-01) and of the text rendering.

### 8.13.1 Structure

A chain is a sequence of **links**, from the place where the check fails to the origin of the atom:

1. **Call link** `f → g` at a call site (or operator, `.await`, drop, coercion) in `f` whose contribution contains the atom.
2. **Instantiation link** `f → g<…C…> via bound B` when the atom is latent and comes from `latᴮ(C)`.
3. **Declaration link**: the atom is in `D(g)` of a function with a clause (the chain stops here; the body of `g` is not shown, because callers depend only on its signature).
4. **Primitive link**: a call of an imported interface function, an `extern "wasm"` function, or an allocation primitive.
5. **Expansion link** (capability chains only): `e ⇒ cap(e)`, or `u covers a`.

For capability diagnostics the chain starts at a root of §8.12.3 and ends at the atom whose `caps` contain the capability.

### 8.13.2 Choice of chain

- The reported chain is a **shortest** chain (fewest links). Among shortest chains, the chain is chosen deterministically by comparing links in order: first by the source position of the call site (file path, then byte offset), then by the symbol ID of the callee (D-TOOL-02). The algorithm is in [impl/08 §8.6](../impl/08-effect-and-capability-analysis.md).
- Only one chain is shown per reported atom or capability. `sai inspect capabilities` (D-TOOL-03, [impl/13](../impl/13-cli.md)) lists, for every required capability, every root that reaches it, each with its shortest chain.

### 8.13.3 Rendering

In diagnostics the chain is a related entry of kind `effect_chain` or `capability_chain` ([impl/11 §11.8](../impl/11-diagnostics.md)), whose steps are the links of §8.13.1 in order; [impl/08 §8.6.4](../impl/08-effect-and-capability-analysis.md) defines the mapping of links to steps. Expansion links are not steps: a capability chain names the capability and the effect that requires it, and its heading states the expansion. The human rendering is that of impl/11:

```
error[CAPABILITY_NOT_ALLOWED]: capability `wasi:http` is required but not allowed by package `acme:app`
  --> sai.package:4:1
   = note: `wasi:http` is reached through effect `net.http`:
             `main` calls `sync_users` at src/main.sai:12:5
             `sync_users` calls `users.fetch_all` at src/main.sai:14:9
             `users.fetch_all` declares `net.http` at src/users.sai:20:5
   = help: add `wasi:http` to `allow` in sai.package
```

`sai inspect effects --why` and `sai inspect capabilities` ([impl/13](../impl/13-cli.md)) print the same chain in the compact form below: the first line names step 0 at its declaration, every further line names the next step at the location of the link that reaches it (the call site, instantiation or `.await`), a declaration link adds `declares effects { … }`, and the expansion link comes last:

```
  = why: main                      src/main.sai:3:1
         -> sync_users             src/main.sai:12:5
         -> users.fetch_all        src/main.sai:14:9   declares effects { net.http }
         => net.http requires wasi:http
```

The text layouts are defined by impl/11 and impl/13; the content (links, order, the stop at declaration links) is defined here.

---

## 8.14 Export functions and world functions

### 8.14.1 `export fn`

- An `export fn` is a root (§8.12.3). If its inferred set is non-empty, it MUST declare its effects (§8.9.3).
- A core `export fn` may have scalar parameters and results only (D-WASM-04); it has no boundary glue and introduces no effect.

### 8.14.2 World imports and exports

- The functions that implement a world's exports are roots, and they MUST have an `effects` clause when their inferred set is non-empty (§8.9.3).
- A world `import` of an interface does not by itself require a capability: the component imports only the interfaces that its reachable code needs, and each of them is checked as a required capability (§8.12.4). The world's import list is an additional upper bound defined in [13-components-and-wit.md](13-components-and-wit.md): an interface effect reachable in code but absent from the `export world`'s imports is an error there (`WORLD_IMPORT_UNDECLARED`).
- Types of an imported interface that appear in exported signatures make the interface part of the component type even if no function of it is called; such an interface is required as a capability as well (N-D-14), because the host must supply it at instantiation.

### 8.14.3 Boundary glue

The Canonical ABI glue that the compiler generates for component imports and exports ([impl/07](../impl/07-component-lowering.md)) may allocate in the component's linear memory. Whether it does is defined on the WIT types of the function alone, never on the compiler-private layout of SAI types (D-WASM-03), so that effects do not change between compiler versions. A WIT type is **flat-primitive** if it is `bool`, an integer type, `f32`, `f64` or `char`.

An imported function (of an interface or of a world) **allocates at the boundary** if

- its WIT result type contains a `string` or a `list` anywhere, because the host lowers the result into memory obtained through `cabi_realloc`; or
- a parameter type contains, anywhere, a `list<u>` whose element type `u` is not flat-primitive, because the glue may have to build a converted copy of the elements ([impl/07 §7.4](../impl/07-component-lowering.md)).

Top-level and nested `string` parameters and lists of flat-primitive elements are passed without copying and do not allocate.

The glue of a component export is a synthetic body owned by the package of the export. Its own set is `{ alloc }` (N-D-12) if any of the following holds, and empty otherwise:

- a parameter type contains a `string` or a `list` anywhere, because the host allocates guest memory through `cabi_realloc` before the call;
- the parameters flatten to more than 16 core values (the Canonical ABI's `MAX_FLAT_PARAMS`), because the host then allocates the parameter area through `cabi_realloc`;
- the result type contains, anywhere, a `list<u>` whose element type `u` is not flat-primitive (converted copy, as above);
- the result type contains an owned handle of a resource implemented by the component (including every constructor), because the value is moved into a heap-allocated box, the resource's representation, before the handle is created ([impl/07 §7.6](../impl/07-component-lowering.md)). The glue is a root (§8.12.3). The SAI function that implements the export does not include this `alloc` in its own set: it receives and returns plain SAI values.

A call of a function of an interface that a hybrid dependency provides within the build ([13](13-components-and-wit.md) §13.5.2) crosses the same boundary inside the module ([impl/07 §7.11.10](../impl/07-component-lowering.md)). The call has the effects of a call of an imported function, including `alloc` when that function allocates at the boundary. The provider's side of the call is glue of an export: a synthetic body owned by the providing package, whose own set follows the conditions for exports above. It is not a root and is reachable only through the calls of the interface. Each call therefore has the same effects, with the same owners, whichever form of the hybrid package the build uses.

---

## 8.15 Formal rules

### 8.15.1 Judgments

- `Γ ⊢ e : T ! E` — under context `Γ`, expression `e` has type `T` and effect set `E` (README notation). `E` is the union of the contributions of the operations of `e` (§8.7); latent effects are not in `E`.
- `Γ ⊢ₚ A ⊑ D` — `A` is permitted by `D` in package `P` (§8.2.4).
- `Γ ⊢ f ok` — function `f` is well-formed with respect to effects.
- `Γ` records, for each in-scope type parameter `X`, its bounds and caps, and for each function item `g` its signature `∀X̄. (T̄) → R ! Sig(g)` and its bounds.

The typing rules of [05-types.md](05-types.md) and [06-expressions-and-statements.md](06-expressions-and-statements.md) determine types; the rules below determine only the effect component. Rules for forms not listed (literals, moves, borrows, field access, constructors, control flow, `let`, blocks, `match`) take the union of the effects of their immediate subexpressions.

### 8.15.2 Rules

```
(E-Call)
  g : ∀X̄. (T̄) → R ! Eg  bounds(g) = { Wk: Bk effects Ck? }
  Γ ⊢ ai : Ti[C̄/X̄] ! Ei      L = ⋃k latᴮᵏ(Wk[C̄/X̄])
  ∀k with cap Ck:  Γ ⊢ₚ latᴮᵏ(Wk[C̄/X̄]) ⊑ Ck, and every residual bound
                   of it has a cap C' with Γ ⊢ₚ C' ⊑ Ck   (else BOUND_EFFECTS_EXCEEDED)
  ──────────────────────────────────────────────────────────
  Γ ⊢ g<C̄>(ā) : R[C̄/X̄] ! Eg ∪ L ∪ ⋃ Ei

(E-CallParam)          X ∈ typarams(Γ),  X: B ∈ Γ,  m ∈ methods(B) or B ∈ {Fn, FnMut, FnOnce}
  Γ ⊢ ai : Ti ! Ei      Lm = Latₘ(m; D̄)  (∅ for Fn-family calls)
  ──────────────────────────────────────────────────────────
  Γ ⊢ x.m<D̄>(ā) : R ! Lm ∪ ⋃ Ei     (the impl's effects are latent in X)

(E-CallClosure)        Γ ⊢ k : K ! E0    K closure type, body effects Inf(K)
  Γ ⊢ ai : Ti ! Ei
  ──────────────────────────────────────────────────────────
  Γ ⊢ k(ā) : R ! E0 ∪ Inf(K) ∪ ⋃ Ei

(E-CallPtr)            Γ ⊢ p : fn(T̄) -> R effects Ep ! E0     Γ ⊢ ai : Ti ! Ei
  ──────────────────────────────────────────────────────────
  Γ ⊢ p(ā) : R ! E0 ∪ Ep ∪ ⋃ Ei

(E-Import)             g imported from WIT interface i     Γ ⊢ ai : Ti ! Ei
                       Ea = { alloc } if g allocates at the boundary (§8.14.3), else ∅
  ──────────────────────────────────────────────────────────
  Γ ⊢ g(ā) : R ! { i } ∪ Ea ∪ ⋃ Ei

(E-Host)               g declared in extern "wasm"     Γ ⊢ ai : Ti ! Ei
  ──────────────────────────────────────────────────────────
  Γ ⊢ g(ā) : R ! { host } ∪ ⋃ Ei

(E-Closure)            K = closure type of fn(..) { body }
  ──────────────────────────────────────────────────────────
  Γ ⊢ fn(..) { body } : K ! ∅

(E-Drop)
  ──────────────────────────────────────────────────────────
  Γ ⊢ drop(place) : () ! ∅

(E-AsyncCall)          g async,  Γ ⊢ ai : Ti ! Ei
  ──────────────────────────────────────────────────────────
  Γ ⊢ g<C̄>(ā) : Fut_g<C̄> ! ⋃ Ei

(E-Await)              Γ ⊢ e : Fut_g<C̄> ! E0
  ──────────────────────────────────────────────────────────
  Γ ⊢ e.await : Output ! E0 ∪ Sig(g) ∪ Lat(g; C̄)

(E-AwaitParam)         Γ ⊢ e : F ! E0     F ∈ typarams(Γ),  F: Future
  ──────────────────────────────────────────────────────────
  Γ ⊢ e.await : F.Output ! E0

(E-Coerce)             Γ ⊢ g<C̄> : FnItem(g, C̄) ! ∅      Γ ⊢ₚ Sig(g) ∪ Lat(g; C̄) ⊑ Ep
                       every residual bound of Lat(g; C̄) has a cap E' with Γ ⊢ₚ E' ⊑ Ep
  ──────────────────────────────────────────────────────────
  Γ ⊢ g<C̄> as-coerced : fn(T̄) -> R effects Ep ! ∅      (else FN_POINTER_EFFECTS_MISMATCH)

(E-Pred)               Γ ⊢ φ : Bool ! E     E = ∅
  ──────────────────────────────────────────────────────────
  Γ ⊢ φ pred                                         (else EFFECTFUL_PREDICATE)
```

Operators and other trait-based syntax are first rewritten to the method calls they denote ([06-expressions-and-statements.md](06-expressions-and-statements.md)) and then use `E-Call` or `E-CallParam`. `E-Await` on a concrete non-anonymous future type uses `E-Call` on its `Future.poll` impl.

### 8.15.3 Function rules

```
(F-Declared)   Γ, params ⊢ body : R ! A       Γ ⊢ₚ A ⊑ D(f)
               ∀ trait bound m of f:  Γ ⊢ₚ D(f) ⊑ D(m)
  ──────────────────────────────────────────────────────────
  Γ ⊢ f ok      Sig(f) = D(f)

(F-Inferred)   Γ, params ⊢ body : R ! A       Inf(f) = lfp(A)       (§8.7.9)
               ∀ trait bound m of f:  Γ ⊢ₚ Inf(f) ⊑ D(m)
               Inf(f) = ∅  ∨  clause-requirement(f) ≠ error
  ──────────────────────────────────────────────────────────
  Γ ⊢ f ok      Sig(f) = Inf(f)
```

where "trait bound `m` of `f`" is the declaration of the trait method that `f` implements, if `f` is an impl method of a trait whose method has a clause, and `clause-requirement(f)` is the entry of the table in §8.9.3 for `f` (`error` gives `MISSING_EFFECTS_DECLARATION`; `warning` is accepted and gives `PUBLIC_EFFECTS_INFERRED`).

### 8.15.4 Permits

```
(P-Mem)     a ∈ D                                       ⟹  D ⊨ₚ a
(P-Cap)     e ∈ D built-in,  a = ns:pkg/i,  cap(e) = ns:pkg   ⟹  D ⊨ₚ a
(P-Io)      e ∈ D built-in,  a = wasi:io/i,  cap(e) = wasi:pkg   ⟹  D ⊨ₚ a
(P-Cover)   u ∈ D,  pkg(u) = P,  { e } ⊨ₚ a  for some e ∈ covers*(u)  ⟹  D ⊨ₚ a
(P-Set)     ∀ a ∈ A. D ⊨ₚ a                              ⟹  A ⊑ₚ D
```

`P-Cap` and `P-Io` are the two cases of rule 2 of §8.2.4 (`P-Io` is N-D-100; `wasi:pkg` ranges over the WIT packages of the `wasi` namespace). `P-Cover` with `e = a` gives rule 3 of §8.2.4; with `e` a built-in it gives rule 4.

### 8.15.5 Soundness statement

Let `f` be a function of a well-formed program, instantiated by some caller with type arguments `C̄`. Let `L` be the latent effects of that instantiation (`Lat` of the call, §8.8.2). Every primitive operation performed while executing `f<C̄>` (a call of an imported interface function, of an `extern "wasm"` function, or of an allocation primitive) has an atom `a` that is permitted by `Sig(f) ∪ L`, where permission is taken transitively along the call chain from `f` to the operation (each link being checked in the package of its caller by §8.9 and §8.8.3), and `caps(a) ⊆ caps(Sig(f) ∪ L)`. Consequently, for every build, every import that the artifact can call at run time, and the allocator if it can be called, is covered by `Req` (§8.12.4).

---

## 8.16 Decisions introduced by this chapter

Each entry is provisional until it is merged into [decisions.md](../design/decisions.md).

**N-D-1 Versionless interface-effect identity.** The identity of an interface effect is `ns:pkg/iface` without version. The version is fixed by the resolved graph and recorded in `sai.lock` (§8.2.1). *Rationale:* source written against an interface must not change when the lock moves to a compatible version (P6, P8); `PKG_PATH` has no version syntax ([03 §3.3](03-grammar.md)).

**N-D-2 Canonical effect-set order and duplicates.** Printed effect sets use the order built-ins (table order of §8.3), interface effects (byte-wise by identity), user effects (byte-wise by spelling). A repeated atom in one `EffectSet` is the warning `DUPLICATE_EFFECT` (§8.2.3). *Rationale:* structured fixes, diagnostics and semantic diff must be byte-for-byte reproducible (P8, determinism principle).

**N-D-3 No private effects in public signatures.** A `pub` function, a `pub` trait's method, a bound or a function pointer type in a `pub` signature MUST NOT name a non-`pub` user effect (`PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE`) (§8.4.2). *Rationale:* callers must be able to state the effect in their own clauses (P1).

**N-D-4 Drops contribute no effects.** `Drop` terminators, including the drop of an imported resource handle (the canonical `resource.drop`), contribute `∅`. Drop edges remain reachability edges (§8.5.1, §8.7.5). *Rationale:* D-OWN-05 makes `Drop.drop` pure and deallocation effect-free; making implicit drops effectful would make effect sets depend on drop placement (P8, P9).

**N-D-5 World-import effects.** Calling a function imported directly by a world (`import FnHead`) has the effect `ns:pkg/world`, named in source by the world's path, mapped to the capability `ns:pkg/world` (§8.5.1). *Rationale:* every import must carry an effect (D-CAP-02, P5); a world is the WIT scope of such functions.

**N-D-6 No effects clause on interface functions.** `EFFECTS_ON_INTERFACE_FN` (§8.6.1). *Rationale:* WIT cannot express effects (P4); the effect of an import is its interface identity, and implementations declare their own effects.

**N-D-7 Trait impl methods are exempt from clause requirements.** `MISSING_EFFECTS_DECLARATION` and `PUBLIC_EFFECTS_INFERRED` never apply to trait impl methods (§8.6.3, §8.9.3). *Rationale:* they cannot be `pub`; their effects are bounded by the trait's clause or surface as latent effects in visible signatures (P1 is satisfied by the trait declaration; P9).

**N-D-8 Constants are pure by construction.** Const expressions have the empty effect set because [06 §6.27](06-expressions-and-statements.md) admits only forms without effects; no separate effect check or diagnostic exists for them (§8.7.8). *Rationale:* compile-time evaluation has no run-time heap and no imports (P2), and a check that can never fire would only add a diagnostic code without meaning (P7, P9).

**N-D-9 Unused declared effects.** `UNUSED_EFFECT_DECLARATION` (warning) applies only to functions that are neither `pub`, `export`, nor exposed through a world (§8.9.2). *Rationale:* public functions may over-declare to keep their contract stable (P8); in private functions an unneeded atom is noise.

**N-D-10 `main` is an ordinary `pub` function for clause requirements.** The entry point `pub fn main` is subject to the clause requirements of D-EFF-01 as a `pub` function, and not as an `export` or world-exposed function (§8.9.3). *Rationale:* D-EFF-01 lists `pub`, `export` and world-exposed functions; the `wasi:cli/run` export that wraps `main` is generated glue, not a source-level world export (P9).

**N-D-11 Major-version rule.** "Package version ≥ 1.0.0" in D-EFF-01 means major version ≥ 1, including pre-releases (§8.9.3). *Rationale:* a `1.0.0-rc` is published to stabilize the API, which is when explicit effect contracts matter (P1).

**N-D-12 Canonical-ABI allocation is an effect.** A call of an imported function that allocates at the boundary (§8.14.3: its WIT result contains `string` or `list`, or a parameter contains a list of non-flat-primitive elements) contributes `alloc`; the glue of an export whose parameters contain `string` or `list` or flatten to more than 16 values, or whose result contains a list of non-flat-primitive elements or an owned handle of a resource implemented by the component, is a root with own set `{ alloc }`. The conditions depend only on WIT types (§8.7.3, §8.14.3). *Rationale:* both cause `cabi_realloc` to run in the guest (P1, P5); without this, the allocator could be linked into a package that does not allow `alloc`.

**N-D-13 Capabilities follow specified reachability and signature sets.** Required capabilities are computed over the reachability of §8.12.3 (not optimizer DCE) from `Own(f)`, which contains callee signature sets (§8.12.3, §8.12.4). *Rationale:* acceptance must not depend on optimizer strength (determinism principle, P2); policies check declared contracts, which are stable across implementation changes (P8).

**N-D-14 Interfaces used only for types are required.** An imported interface whose types appear in the exported signatures of a component is a required capability (§8.12.4, §8.14.2). *Rationale:* the component type imports it, so the host must supply it (P5).

**N-D-15 Built-in roots are reserved in effect position.** An `EffectRef` whose first segment is a built-in root always denotes a built-in effect (or is `UNKNOWN_EFFECT`); user effects cannot be named after built-in roots (`EFFECT_NAME_RESERVED`) (§8.2.2, §8.4.1). *Rationale:* D-EFF-03 precedence, extended to single-segment references so that the meaning of an effect name never depends on imports (P8).

**N-D-16 Acyclic covers.** `covers` cycles are `EFFECT_COVERS_CYCLE` (§8.4.1). *Rationale:* a cycle has no meaning beyond the union of its members; rejecting it keeps expansion trivially terminating (P9).

**N-D-17 Deny by overlap.** A required capability is denied when it overlaps a deny entry (either covers the other) (§8.12.1, §8.12.5). *Rationale:* a package-level requirement includes the denied interface; granting it would bypass the denial (P5).

**N-D-18 Scope of `AMBIENT_IMPORT`.** The lint fires on `use` of a `wasi:` interface in packages outside the `sai` namespace (§8.5.3). *Rationale:* the toolchain's `sai:` packages are the capability-passing layer of D-CAP-02, so they must import WASI, while every other package receives authority from them explicitly (P5).

**N-D-19 Function pointer type identity.** Function pointer types are equal only if their effect sets are equal as sets of atoms (§8.6.4). *Rationale:* type identity must be structural and context-free; `⊑ₚ` depends on the package (P8).

**N-D-20 Caps through type parameters.** When the expansion of a capped bound leaves effects latent in the caller (a residual bound, §8.8.2, for example when the bound is instantiated with a type parameter of the caller), the caller's bound must carry a cap that is permitted by it (§8.8.3). *Rationale:* the check must be decidable at the generic call site without monomorphization (D-EFF-02, P2).

**N-D-21 Resolution-time footprint.** The lock records, per package, its capability footprint (the `caps` of all its bodies, or all imports of a binary component); resolution never rejects a graph because of reachability (§8.12.8). *Rationale:* reachability needs a build; resolution must be fast and target-independent (P8), while review needs an upper bound (P5).

**N-D-22 Drop impls have no latent effects.** The body of a `Drop.drop` impl may not call through the impl's type parameters or `Self`, nor instantiate a generic so that effects stay latent in them (`DROP_HAS_EFFECTS`) (§8.6.3). *Rationale:* drop glue runs without an instantiating call site, so latent effects of a `Drop` impl could not be attributed to anyone and would escape both effect checking and capability checking; forbidding them keeps D-OWN-05 sound (P2, P5, P9).

**N-D-23 Method-level type arguments are attributed at the call.** A call of a trait method through a type parameter contributes the latent effects of the method's own type arguments (`Latₘ`); the latent expansion of an impl (§8.8.2 rule 2) covers only impl-level bounds (§8.8.2). *Rationale:* the arguments of a method's own type parameters, typically closures, are chosen by the calling body and are unknown to the instantiator of the type parameter, so attributing them anywhere else would lose them (P2, P5).

**N-D-24 Coercions to function pointers cover latent effects.** Coercing `g<C̄>` to `fn(..) effects E` requires `Sig(g) ∪ Lat(g; C̄) ⊑ₚ E` and caps permitted by `E` on its residual bounds; other coercions into function pointer types (for example of closures) require the same of `Inf(K)` and the residual bounds of `K` (`FN_POINTER_EFFECTS_MISMATCH`) (§8.6.4). *Rationale:* a function pointer is called where no instantiating caller is known, so checking `Sig(g)` alone would let a pointer typed as pure perform the effects of its type arguments (P2, P5).

**N-D-25 Latent expansion follows the elaborated predicates.** The bounds of a signature are its elaborated trait predicates (supertraits and associated-type bounds included); a cap applies to the bounds elaborated from it; an obligation answered by the caller's own predicates stays latent (rule 1 of §8.8.2) (§8.8.2). *Rationale:* the expansion must follow the obligations that the trait solver proves ([05 §5.11.2](05-types.md), where environment candidates win); otherwise effects reached through an associated type, such as `X.Item: Display` implied by `X: Iterator`, would be attributed to no one (P2, P8).

**N-D-26 Latent expansion is bounded.** An operation whose latent expansion reaches a type nested deeper than 64 is `INSTANTIATION_DEPTH_EXCEEDED`; a trait goal of the expansion that overflows is `TRAIT_SOLVER_OVERFLOW` (§8.8.2). *Rationale:* predicates on constructed types can make the expansion grow without bound; reusing the limits that [05-types.md](05-types.md) already applies to monomorphization and to trait solving keeps acceptance deterministic and independent of the implementation (P2, P8, P9).

**N-D-27 Impl methods cannot strengthen bound caps.** A cap on a bound of a trait impl method's own type parameters must be implied by a cap of the trait method's corresponding bound (`TRAIT_ITEM_SIGNATURE_MISMATCH`); weaker or missing caps are allowed (§8.8.3). *Rationale:* calls through a type parameter check the trait's caps only, so a stricter impl cap would be an unchecked assumption (P2, P5); allowing weaker caps follows the usual rule that an implementation may require less than its contract (P9).

**N-D-100 WASI built-in effects permit `wasi:io`.** A declared built-in effect whose capability is a WIT package of the `wasi` namespace permits every interface effect of `wasi:io`, in addition to the interfaces of its own package (§8.2.4 rule 2). *Rationale:* every other WASI 0.2 package returns `wasi:io` streams and pollables, so a wrapper that declares `fs.read` or `stdio.write` must be able to use them; `wasi:io` operates only on resources that another WASI interface created and grants no authority of its own, which is why the policy level grants it by the same implication ([targets](../targets/targets-and-profiles.md) §3.3, N-F-69) (P5, P9).

---

## 8.17 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_EFFECT` | error | an `EffectRef` names no built-in effect, user effect, interface binding or world (§8.2.2) |
| `EFFECT_NOT_VISIBLE` | error | a user effect is named where it is not visible (§8.2.2, §8.4.2) |
| `DUPLICATE_EFFECT` | warning (lint) | the same atom appears twice in one effect set (§8.2.3) |
| `EFFECT_NAME_RESERVED` | error | a user effect is named after a built-in root (§8.4.1) |
| `EFFECT_COVERS_CYCLE` | error | the `covers` relation of user effects has a cycle (§8.4.1) |
| `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` | error | a public signature names a non-`pub` user effect (§8.4.2) |
| `EFFECTS_ON_INTERFACE_FN` | error | a function of an `interface` declaration has an `effects` clause (§8.6.1) |
| `EXTERN_EFFECTS_MISMATCH` | error | an `extern "wasm"` function is given an effect set other than `{ host }`, or an `extern "C"` function one other than `{ ffi }` (§8.6.1) |
| `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` | error | an impl method's effects are not permitted by the trait method's clause (§8.6.3) |
| `DROP_HAS_EFFECTS` | error | a `Drop.drop` impl has a non-empty declared or inferred set, or makes a call that leaves effects latent in its type parameters (§8.6.3) |
| `FN_POINTER_EFFECTS_MISMATCH` | error | a function's effects, including the latent effects of its type arguments, are not permitted by the target function pointer type (§8.6.4) |
| `BOUND_EFFECTS_EXCEEDED` | error | latent effects of an instantiation exceed a bound's cap (§8.8.3) |
| `EFFECT_NOT_DECLARED` | error | a function performs an effect its clause does not permit (§8.9.1) |
| `UNUSED_EFFECT_DECLARATION` | warning (lint) | a private function declares an atom it does not need (§8.9.2) |
| `MISSING_EFFECTS_DECLARATION` | error | a function that must declare its effects omits a non-empty clause (§8.9.3); named by [decisions.md](../design/decisions.md) D-EFF-01 |
| `PUBLIC_EFFECTS_INFERRED` | warning (lint) | a `pub` function of a pre-1.0 package has a non-empty inferred set (§8.9.3); named by [decisions.md](../design/decisions.md) D-EFF-01 |
| `EFFECTFUL_PREDICATE` | error | a refinement, contract or invariant predicate has effects (§8.11) |
| `UNKNOWN_CAPABILITY` | error | a capability name denotes no known WIT package, interface or target capability (§8.12.1) |
| `CAPABILITY_ALLOW_DENY_CONFLICT` | error | the same capability name is in both `allow` and `deny` (§8.12.5) |
| `CAPABILITY_NOT_ALLOWED` | error | a required capability is neither covered nor implied by the checked policy's `allow` (§8.12.5, §8.12.6) |
| `CAPABILITY_DENIED` | error | a required capability overlaps the checked policy's `deny` (§8.12.6) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | a required capability is not provided by the target (§8.12.6, §8.12.7) |
| `WORLD_IMPORT_UNDECLARED` | error | defined in [13](13-components-and-wit.md) §13.5.2; a reachable interface effect is not imported by the `export world` (§8.14.2) |
| `AMBIENT_IMPORT` | warning (lint) | a `use` of a `wasi:` interface outside the `sai` namespace; error under `policy { deny ambient_imports }` (§8.5.3); named by [decisions.md](../design/decisions.md) D-CAP-02 |
| `DUPLICATE_BINDING` | error | defined in [04-modules-and-names.md](04-modules-and-names.md) §4.4.3; an effect name clashes with another item (§8.4.1) |
| `CLAUSE_ORDER` | error | defined in [03](03-grammar.md) §3.5; see §8.6.1 |
| `INSTANTIATION_DEPTH_EXCEEDED` | error | defined in [05](05-types.md) §5.7.6; a latent expansion reaches a type nested deeper than 64 (§8.8.2) |
| `TRAIT_SOLVER_OVERFLOW` | error | defined in [05](05-types.md) §5.11.2; a trait goal of a latent expansion exceeds the solver depth (§8.8.2) |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | defined in [03](03-grammar.md) §3.7; see §8.6.3 |
| `TRAIT_ITEM_SIGNATURE_MISMATCH` | error | defined in [05](05-types.md) §5.9.1; also reported when a trait impl method's bound carries a cap that the trait method's bound does not imply (§8.8.3) |
