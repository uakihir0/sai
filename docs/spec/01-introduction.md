# 01. Introduction

**Status:** Normative
**Stage:** all (this chapter defines the stage mechanism itself)
**Related:** [README.md](../README.md), [decisions.md](../design/decisions.md) §0, [glossary.md](../glossary.md), [roadmap.md](../roadmap.md), [impl/15-conformance-and-testing.md](../impl/15-conformance-and-testing.md)

This chapter states what SAI is for, the principles that decide every design question, and what it means for a compiler or a tool to conform to this specification. It also fixes the notation, the judgment forms, and the document conventions that every other chapter uses. It contains a short informative tour of the language. Detailed rules live in the chapters listed in the [document map](../README.md); this chapter only links to them.

---

## 1.1 Scope and goals

### 1.1.1 What SAI is

1. SAI is a statically typed, general-purpose systems programming language that treats WebAssembly as its first-class execution format and its first-class ABI (draft §1).
2. Its guiding statement is: **WebAssembly is the universal software boundary.** SAI uses core Wasm, WASI, the Component Model, and WIT as the common boundary between languages, platforms, and execution environments, not merely as a browser compilation target (draft §1, §89, §90).
3. SAI is designed to be generated, changed, and analyzed by AI agents as well as by people, but it is not an AI-only language. The affinity with AI comes from strong types, a safe memory model, explicit effects, and structured compiler information, not from special syntax (draft §1, §86).

### 1.1.2 Goals

The primary goals of SAI, in the order of draft §1, are:

| Goal | Realized mainly by |
|---|---|
| correctness | ownership and borrowing ([07](07-ownership-and-borrowing.md)), refinements and contracts ([09](09-contracts-and-refinements.md)), no null, no exceptions ([10](10-errors-and-panics.md)) |
| portability | Wasm-first targets ([targets](../targets/targets-and-profiles.md)), WIT boundaries ([13](13-components-and-wit.md)) |
| predictable performance | monomorphization, no mandatory runtime, no GC, explicit allocation ([05](05-types.md), [08](08-effects-and-capabilities.md)) |
| minimal runtime footprint | `sai:core` needs no allocator, OS, WASI, libc, or GC ([16](16-core-library.md)); the tiny profile |
| strong interoperability | the Component Model and WIT ([13](13-components-and-wit.md)) |
| analyzability | explicit effects and capabilities ([08](08-effects-and-capabilities.md)), structured diagnostics, symbol identity ([impl/12](../impl/12-semantic-tooling.md)) |
| maintainability | one canonical syntax and format ([impl/14](../impl/14-formatter.md)), semantic diffs, stable diagnostic codes |

### 1.1.3 Preconditions

The principles of draft §2 are preconditions of the design rather than tie-breakers ([decisions.md](../design/decisions.md) §0.1):

1. **Wasm-first, not Wasm-only.** The primary targets are `wasm32-core`, `wasm64-core`, `wasm32-wasi`, and `wasm64-wasi` (D-TGT-01). A native backend is specified for Stage 7.
2. **Wasm as the universal ABI.** Across languages, SAI speaks Wasm components and WIT; inside SAI, the compiler optimizes freely (draft §89).
3. **No mandatory runtime.** A program that uses neither allocation nor I/O compiles to a core Wasm module with no allocator, no memory section if none is needed (D-WASM-02), and no imports.
4. **Pay only for what you use.** Every runtime facility (allocator, panic reporting, async scheduler) is linked only when reachable (D-WASM-05, D-ASYNC-01).
5. **Explicit over implicit**, which is also principle P1.
6. **AI-native, but human-first readable.**

### 1.1.4 What this specification covers

1. The language specification (`spec/`) defines the syntax, the static semantics (which programs are accepted), and the dynamic semantics (what accepted programs do) of SAI, together with the library items the language depends on (`sai:core` and `sai:alloc`, [16](16-core-library.md)).
2. The package specification (`package/`) defines `sai.package`, version resolution, `sai.lock`, and distribution.
3. The target document ([targets-and-profiles.md](../targets/targets-and-profiles.md)) defines targets, target capabilities, profiles, and entry points.
4. The implementation specification (`impl/`) is normative only for the reference implementation, where it says so (README §2).
5. Libraries other than `sai:core` and `sai:alloc` (such as `sai:collections`, `sai:io`, `sai:async`) are ordinary packages (D-PKG-04). Their APIs are outside the language specification, except for the signatures that [16](16-core-library.md) §16.16 fixes because the specification refers to them.

### 1.1.5 Non-goals

SAI does not aim to be (draft §83): a host that runs every language's runtime (npm, pip, Cargo) unchanged; a language of maximum syntactic expressiveness; a language with a mandatory garbage collector; a JavaScript transpiler; or a language with arbitrary token-rewriting macros. Domain facilities such as HTTP, JSON, dates, money, decimal arithmetic, Unicode normalization, and cryptography belong in libraries, not in the language or in `sai:core` (draft §52, §82).

---

## 1.2 Design principles

### 1.2.1 The nine principles

Every design decision in these documents is justified by the principles of draft §87. They are listed in **priority order**: when two principles pull in different directions, the earlier one wins ([decisions.md](../design/decisions.md) §0.1).

| Tag | Principle | Meaning in this specification |
|---|---|---|
| P1 | Explicit > Implicit | Borrows, conversions, effects, and allocation are visible in source. Implicit behavior is limited to closed, enumerated lists (for example the eight coercions of D-TYPE-04). |
| P2 | Static knowledge > Runtime guessing | What can be checked at compile time is checked at compile time: types, ownership, effects, capabilities, refinements the SFC can prove. |
| P3 | Semantic operations > Text manipulation | Tools operate on syntax trees and symbol identities, not on text: structured fixes, semantic edits, semantic diff. |
| P4 | Portable boundary > Language-specific ABI | Boundaries between components are WIT and the Canonical ABI, not a SAI-specific ABI. |
| P5 | Capability > Ambient authority | Code reaches the outside world only through tracked effects that a capability policy grants. There is no global mutable state. |
| P6 | Source compatibility > Repository location | A package is identified by its name and version, never by the URL it was fetched from. |
| P7 | Small core > Large runtime | `sai:core` and the language stay small; facilities move to libraries. |
| P8 | Predictability > Magic | The same program means the same thing in every profile, on every machine, with every thread count. |
| P9 | Boring correctness > Clever syntax | One obvious way to write each construct; no syntax that needs cleverness to read. |

### 1.2.2 Applying the principles

1. A chapter that decides a question that [decisions.md](../design/decisions.md) leaves open MUST record the decision in its final section, citing the principles that justify it (§1.9.4).
2. A rationale that cites a principle is informative. The rule it justifies is normative.

---

## 1.3 A tour of SAI (informative)

This section shows the main features of the language in small, valid examples. Each feature links to the chapter that defines it.

### 1.3.1 A freestanding module

A source file is a module, and the root module of a package is `lib.sai` (D-SYN-08). Statements end at line ends; there are no semicolons (D-LEX-01). An `export fn` becomes a core Wasm export (D-MOD-02).

```sai
//! A freestanding core Wasm module for the wasm32-core target.

export fn add(a: I32, b: I32) -> I32 {
    a + b
}
```

Integer `+` panics on overflow in every profile (D-TYPE-06). `a +% b` wraps instead.

### 1.3.2 Types

Records are constructed with parentheses and labels; there are no tuple structs (D-SYN-01). Enums have unit, tuple, and record variants (D-SYN-02). `distinct` declares a nominal newtype (D-TYPE-03). See [05](05-types.md).

```sai
distinct UserId = U64

type User {
    id: UserId
    pub name: String
}

enum Shape {
    Circle(F64)
    Rect(width: F64, height: F64)
    Empty
}

fn area(shape: borrow Shape) -> F64 {
    match *shape {
        Shape.Circle(r) => 3.14159 * r * r
        Shape.Rect(width, height) => width * height
        Shape.Empty => 0.0
    }
}

fn make_user(id: U64, name: String) -> User {
    User(id: UserId(id), name)
}
```

A scrutinee of type `borrow Shape` is dereferenced explicitly with `*` (D-EXPR-04). `User(id: UserId(id), name)` uses field punning for `name`.

### 1.3.3 Ownership and borrowing

Non-Copy values move. Borrows are written at the call site; only method receivers are borrowed automatically (D-OWN-02). There are no lifetime annotations (D-OWN-03). See [07](07-ownership-and-borrowing.md).

```sai
fn total_len(words: borrow Vec<String>) -> USize {
    var total: USize = 0
    for w in words.iter() {
        total += w.len()
    }
    total
}

fn report(words: Vec<String>) -> USize {
    let n = total_len(borrow words)
    n
}
```

### 1.3.4 Effects

An effects clause declares what a function may do (D-EFF-01). Allocation is the effect `alloc`; `Str + Str` allocates a new `String` (D-EXPR-13). See [08](08-effects-and-capabilities.md).

```sai
pub fn greeting(name: Str) -> String
    effects { alloc }
{
    "Hello, " + name
}
```

### 1.3.5 Errors

Recoverable errors are `Result` values; `?` propagates an error of the identical type, and `map_err` converts between error types (D-EXPR-05). There are no exceptions and no null (D-ERR-01). See [10](10-errors-and-panics.md).

```sai
enum ConfigError {
    Missing
    BadNumber
}

fn parse_port(text: Option<Str>) -> Result<U16, ConfigError> {
    let raw = text.ok_or(ConfigError.Missing)?
    U16.parse(raw).map_err(fn(_) { ConfigError.BadNumber })
}
```

### 1.3.6 Contracts and refinements

A refined type restricts its base type with a predicate. Converting into it needs either a proof by the Standard Fact Checker or an explicit `try_from` (D-TYPE-03, D-CONTRACT-01). Contracts the SFC cannot prove become runtime checks. See [09](09-contracts-and-refinements.md).

```sai
type Percentage = I32 where self >= 0 and self <= 100

fn to_percentage(v: I32) -> Option<Percentage> {
    Percentage.try_from(v)
}

@derive(Copy, Clone, PartialEq, PartialOrd, Sub)
distinct Money = I64

fn withdraw(balance: Money, amount: Money) -> Money
    requires amount <= balance
    ensures result >= Money(0)
{
    balance - amount
}
```

Because `Sub` and `PartialOrd` are derived, the SFC sees through `Money` and proves the `ensures` clause from the `requires` clause (D-CONTRACT-02, R-05).

### 1.3.7 Traits and generics

Receivers are explicit (D-TYPE-08). Generics are monomorphized (D-TYPE-07). See [05](05-types.md).

```sai
trait Describe {
    fn describe(self: borrow Self) -> Str
}

impl Describe for Shape {
    fn describe(self: borrow Self) -> Str {
        match *self {
            Shape.Circle(_) => "circle"
            Shape.Rect(..) => "rectangle"
            Shape.Empty => "empty"
        }
    }
}

fn sum_all(values: Slice<I64>) -> I64 {
    var total: I64 = 0
    for v in values.iter() {
        total += *v
    }
    total
}
```

### 1.3.8 Components

An interface maps one-to-one to a WIT interface, `impl <interface>` implements it, and a world names imported and exported interfaces (D-WIT-01, D-WIT-03). See [13](13-components-and-wit.md).

```sai
interface greeter {
    fn greet(name: String) -> String
        effects { alloc }
}

impl greeter {
    fn greet(name: String) -> String
        effects { alloc }
    {
        "Hello, " + name.as_str()
    }
}

world hello {
    export greeter
}
```

### 1.3.9 Tests

Tests and property tests are items (Stage 3). See [14](14-testing.md).

```sai
test "greeting mentions the name" {
    let g = greeting("Ada")
    assert g.as_str() == "Hello, Ada"
}

property "wrapping addition commutes" (a: I32, b: I32) {
    a +% b == b +% a
}
```

---

## 1.4 Conformance

### 1.4.1 Conforming programs

1. A **conforming program** is a set of packages (a root package and its resolved dependencies) whose every source file satisfies the lexical rules ([02](02-lexical-structure.md)), the grammar ([03](03-grammar.md)), and every static rule of the language specification, and whose package contracts satisfy [package/01](../package/01-package-contract.md).
2. A program that violates a static rule is **ill-formed**. Static rules are stated as "is an error" with a diagnostic code, or with the keywords MUST and MUST NOT applied to programs.
3. Whether a program is conforming depends only on the procedures defined by this specification (§1.6). It never depends on the target or profile, except where a rule explicitly depends on them: target capabilities and profile policies (D-CAP-01, D-TGT-02), `@target` items, and `only { .. }` dependencies (D-PKG-01).

### 1.4.2 Conforming compilers

A **conforming compiler** for SAI v0.1 MUST satisfy all of the following.

1. **Coverage.** It implements every feature of Stages 0 through 4 (README §4) and structured diagnostics (§1.4.4). Features of Stages 5 through 7 are OPTIONAL. A compiler MAY also claim conformance for a smaller stage range while it is under development; such a claim names the range (§1.4.3).
2. **Acceptance.** It accepts every conforming program that uses only implemented features and stays within its implementation limits (§1.5.4), and produces output for each requested target and profile it supports.
3. **Rejection.** It rejects every ill-formed program with at least one diagnostic of severity `error`. When the rule that is violated names a diagnostic code, the compiler SHOULD report that code (README §3). The conformance suite checks codes ([impl/15](../impl/15-conformance-and-testing.md) §15.3).
4. **Behavior.** The output of an accepted program behaves as the dynamic semantics prescribe, on every supported target and in every profile. The only permitted differences between profiles are those of D-TGT-02 (panic reporting, debug information, allocator, code size and speed). A profile never changes the result of a computation, the checks performed, or which panics occur (C-03).
5. **Targets.** It supports at least `wasm32-core` and `wasm32-wasi`, each in the `dev`, `release`, and `tiny` profiles. `wasm64-core` and `wasm64-wasi` are OPTIONAL. If a compiler offers a target, it MUST conform on that target, as the [targets](../targets/targets-and-profiles.md) document defines it (N-J-1). That definition can itself make a part optional: on the experimental `wasm64-wasi`, component output is OPTIONAL (targets §2.7).
6. **Staged features.** It reports `FEATURE_NOT_IMPLEMENTED` for every use of a staged feature it does not implement (§1.4.5).
7. **Library.** It provides `sai:core` and `sai:alloc` exactly as [16](16-core-library.md) specifies, for the stages it implements.
8. **Robustness.** It never crashes on any input. An internal failure is reported as `INTERNAL_COMPILER_ERROR` (§1.10, which owns the code) and is a defect of the compiler, never a property of the program. How it is reported is described in [impl/11](../impl/11-diagnostics.md) §11.4.6 (the record and exit code 101) and [impl/01](../impl/01-architecture.md) §1.10 (session poisoning).

### 1.4.3 Conformance claims

1. A conformance claim has the form `SAI <language version>, stages <range>, targets <list>`, for example `SAI 0.1, stages 0-4, targets wasm32-core wasm32-wasi`. A claim that includes structured diagnostics and Stages 0 through 4 is a **v0.1 conformance** claim.
2. A claim is substantiated by passing every test of the conformance suite that applies to the claimed stages and targets ([impl/15](../impl/15-conformance-and-testing.md) §15.9). Passing the suite is necessary but not sufficient: the specification, not the suite, defines conformance.

### 1.4.4 Conforming tools

The following tools are part of conformance because other tools and AI agents depend on their exact output (P3, P8).

1. **Structured diagnostics** (required for v0.1). Every diagnostic has a stable UPPER_SNAKE code, a severity (`error`, `warning`, or `info`), a primary span, and a message (D-TOOL-01). The compiler MUST be able to emit every diagnostic as JSON (`--format json`) in the schema of [impl/11](../impl/11-diagnostics.md). Spans are half-open ranges of UTF-8 byte offsets (02 §2.1). Structured fixes refer to semantic node IDs (D-TOOL-02). The set of diagnostics, their codes, spans, severities, and fixes MUST be deterministic (§1.6); the human-readable message text is implementation-defined (§1.5.2).
2. **Formatter** (required for v0.1, Stage 0). `sai fmt` MUST produce the canonical form of [impl/14](../impl/14-formatter.md) (D-TOOL-04). The output MUST be byte-identical across conforming formatters for the same input. Formatting MUST be idempotent (`fmt(fmt(x)) = fmt(x)`), MUST preserve every comment, and MUST preserve meaning. It changes only trivia and makes only the canonicalizations listed in [impl/14](../impl/14-formatter.md) §14.1.3 (among them moving and sorting `use` declarations and the items of their `UseItems` lists, and normalizing the spelling of some literals); the formatted file parses to the same syntax tree as the input once trivia and these canonicalizations are normalized away ([impl/14](../impl/14-formatter.md) §14.11.1).
3. **`sai explain`** MUST describe every diagnostic code the compiler can emit (D-TOOL-03).
4. **Semantic protocol** (Stage 6, OPTIONAL for v0.1): `sai lsp` and the structured edit operations of [impl/12](../impl/12-semantic-tooling.md).

### 1.4.5 Staged features

1. A **staged feature** is a construct, library item, target, or tool operation that a chapter tags with a stage. A chapter header gives the default stage of its contents; a section or item tag overrides it.
2. A compiler that does not implement a staged feature MUST reject every program that uses it with the error `FEATURE_NOT_IMPLEMENTED` (README §4). The diagnostic:
   - is reported at the span of the construct that uses the feature (the keyword, the item reference, or the declaration);
   - carries the data `stage` (an integer 0–7) and `feature` (a feature name from the table below or, for a library item, its path);
   - is reported by `sai check` as well as by `sai build`, before any code generation.
3. A compiler MUST NOT accept a staged feature partially. If it accepts a construct, it MUST implement all of that construct's semantics.
4. Reporting `FEATURE_NOT_IMPLEMENTED` never makes a program conforming or non-conforming; it only states that the compiler cannot process it.
5. Feature names for the stages that are not part of v0.1 conformance:

| Stage | Feature name | Construct |
|---|---|---|
| 5 | `async-fn` | `async fn` declarations |
| 5 | `async-block` | `async { }` and `async move { }` blocks |
| 5 | `await` | `.await` |
| 5 | `for-await` | `for await` loops |
| 5 | `wasi-async` | lowering to the Component Model async ABI |
| 6 | `semantic-protocol` | `sai lsp` extensions, structured edits, impact analysis, semantic diff |
| 7 | `native-target` | any non-Wasm target |
| 7 | `extern-c` | `unsafe extern "C"` blocks and calls to their functions |

For a library item of a stage that the compiler does not implement, the feature name is the item's canonical path ([04](04-modules-and-names.md) §4.9), for example `sai:core/ffi.CStr` at Stage 7 ([16](16-core-library.md) §16.1.6, §16.12.4).

One optional feature belongs to a v0.1 stage: `wasm64-component` (Stage 4), the component output of the experimental target `wasm64-wasi` ([targets](../targets/targets-and-profiles.md) §2.7 item 3). A compiler that does not produce it reports `FEATURE_NOT_IMPLEMENTED` with that name and still conforms (item 4).

A compiler that lacks a stage of v0.1, and whose claim therefore covers fewer stages (§1.4.3), uses the same diagnostic with the stage of the missing feature. The document that stages the feature fixes its `feature` datum (N-J-5):

- an attribute: `attribute @name` ([15](15-attributes-and-lints.md) §15.2.4 step 2);
- a library item, also one that a construct uses as a lang item: its canonical path, as above ([16](16-core-library.md) §16.1.6);
- a `sai` command: its command path ([impl/13](../impl/13-cli.md) §13.1.2);
- a field of `sai.package`: its name ([package/01](../package/01-package-contract.md) §1.21);
- any other language construct: the name in [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.14.4.

---

## 1.5 Kinds of behavior

### 1.5.1 Defined behavior, panics, and undefined behavior

1. Every operation of a safe program (one that uses no `unsafe` block, `unsafe fn` call, or `unsafe impl`) has **defined behavior**: it produces a value, diverges, or panics, as the dynamic semantics prescribe.
2. A **panic** is defined behavior. It lowers to a trap on every target, after an optional report (D-ERR-01, [10](10-errors-and-panics.md)). Exhausting the shadow stack or failing to allocate also traps.
3. **Undefined behavior** arises only when unsafe code violates a safety contract stated in [12](12-unsafe-and-ffi.md) (or in [16](16-core-library.md) for an `unsafe fn` of the core library). Safe code can never cause undefined behavior, regardless of how it uses the core library.

### 1.5.2 Implementation-defined behavior

**Implementation-defined** behavior is behavior that a conforming compiler chooses and MUST document for each compiler version. The following list is **closed**: nothing else in SAI is implementation-defined (N-J-2).

1. The memory layout of every type: size, alignment, field offsets, enum tag encoding, and niche use (D-WASM-03). It is observable only through `core.mem.size_of`, `core.mem.align_of`, and unsafe code.
2. The internal calling convention and symbol names inside a module (D-WASM-04), except for exports, imports, and component boundaries, which are fixed by [13](13-components-and-wit.md) and [12](12-unsafe-and-ffi.md).
3. The default size of the shadow stack and the initial and maximum linear memory, within the bounds set by [targets-and-profiles.md](../targets/targets-and-profiles.md), and therefore the call depth or allocation volume at which a program traps.
4. The allocator's choice of addresses and its use of `memory.grow`, within D-WASM-05.
5. The human-readable text of diagnostic messages, notes, and help lines (codes, severities, spans, data, and fixes are not implementation-defined).
6. The framing of panic reports in the dev and release profiles. The report MUST contain the panic message and the source location (package, file, line, column), when available; the resource panics `StackOverflow` and `OutOfMemory` have no location ([10](10-errors-and-panics.md) §10.4.3, §10.5.2). The surrounding text is implementation-defined.
7. The contents of debug information and of the Wasm name section (D-TGT-02).
8. The generated code itself: instruction selection, inlining, code size, and speed, subject to §1.4.2 item 4.
9. The implementation limits, subject to the minimums of §1.5.4.

### 1.5.3 Unspecified behavior

**Unspecified** behavior is behavior for which this specification allows a set of outcomes and does not require the compiler to document its choice. The list is **closed** (N-J-2).

1. The bit pattern (sign and payload) of a NaN produced by a floating-point operation (D-TYPE-06). It is observable through `to_bits`, `total_cmp`, and `is_sign_negative` of a NaN.
2. The numeric value of an address obtained with `Ptr.addr` or `PtrMut.addr`, and the relative order of addresses of distinct allocations.
3. The contents of padding bytes and of uninitialized memory observed by unsafe code.
4. The interleaving of child tasks of a task scope, beyond the ordering guarantees of [11](11-async-and-concurrency.md) (Stage 5).
5. Everything an import returns: the behavior of the host, of WASI, and of other components is outside the program.

### 1.5.4 Implementation limits

1. A conforming compiler MUST support at least the following limits. Beyond them, it MAY reject a program with the error `IMPLEMENTATION_LIMIT_EXCEEDED`, naming the limit and the value reached.

| Limit | Minimum |
|---|---|
| size of a source file | 16 MiB |
| length of an identifier | 1,024 bytes |
| length of a string or byte-string literal | 16 MiB |
| nesting depth of expressions, blocks, patterns, and types (combined) | 256 |
| parameters of one function | 255 |
| generic parameters of one item | 64 |
| fields of one struct, or variants of one enum | 4,096 |
| elements of one tuple type | 64 |
| statements in one block | 65,535 |
| modules in one package | 65,535 |
| depth of nested monomorphization (an instance requiring another instance) | 128 |
| packages in one resolved graph | 4,096 |

2. Limits MUST be applied deterministically: whether a limit is exceeded depends only on the program, never on available memory, thread count, or timing (§1.6).
3. The limits of the Standard Fact Checker (256 atoms, 16 variables, 16 DNF branches) are not implementation limits. They are part of the language and are fixed for every compiler (D-CONTRACT-02).

---

## 1.6 The determinism principle

1. **Acceptance.** Whether a program is accepted or rejected MUST depend only on deterministic procedures defined by this specification. It MUST NOT depend on the strength of an optimizer or on implementation-specific heuristics ([decisions.md](../design/decisions.md) §0.1). Optimizations change the performance of an accepted program, never whether it is accepted.
2. **Consequences.** Acceptance and the set of diagnostics MUST NOT depend on:
   - the profile, except through the deterministic capability and policy rules of D-TGT-02;
   - the number of threads, scheduling, timing, or available memory;
   - the order in which a file system enumerates directories;
   - environment variables other than those a chapter explicitly names, the locale, the time zone, or the current time;
   - hash seeds or memory addresses inside the compiler;
   - the contents of incremental caches ([impl/01](../impl/01-architecture.md) §1.6).
3. **Fixed procedures.** Every procedure that decides acceptance is fully specified: name resolution ([04](04-modules-and-names.md)), type inference and trait selection ([05](05-types.md)), borrow checking ([07](07-ownership-and-borrowing.md)), effect inference ([08](08-effects-and-capabilities.md)), the SFC with its fixed elimination order and limits ([09](09-contracts-and-refinements.md)), and version resolution with no backtracking (D-PKG-02).
4. **Order of diagnostics.** Diagnostics are reported in a deterministic order: by package identity, then by file path (bytewise), then by primary span start, then by code.
5. **Reproducible outputs.** Given the same sources, `sai.package`, `sai.lock`, compiler version, target, and profile, a compiler MUST produce bit-identical artifacts (draft §72). This includes the WIT and the component metadata. Paths of the build machine MUST NOT appear in artifacts; source locations in panic reports and debug information are relative to the package root.

---

## 1.7 Notation

### 1.7.1 Grammar and code

1. Grammar rules use the EBNF of [03](03-grammar.md) §3.1. Terminals are tokens of [02](02-lexical-structure.md).
2. SAI code is shown in fenced blocks tagged `sai`. Examples that are rejected say so in a comment that names the diagnostic code (README §5). Other material (judgments, layouts, command lines) uses `text` fences.
3. A path written `core.m` or `alloc.m` in prose denotes the module `m` of the package `sai:core` or `sai:alloc`. In source code that module is imported as `use sai:core/m` (D-MOD-01). Nested module paths follow D-SYN-08.

### 1.7.2 Metavariables

| Symbol | Ranges over |
|---|---|
| `x`, `y`, `f` | local names and function names |
| `e` | expressions |
| `s` | statements |
| `p` | places (place expressions) |
| `P` | patterns |
| `v` | values (in dynamic semantics) |
| `T`, `U`, `V` | types |
| `X`, `Y` | type parameters |
| `N` | const generic arguments |
| `Tr` | traits (with their generic arguments) |
| `'a`, `'b`, `'c` | regions |
| `E`, `F` | effect sets |
| `σ` | substitutions |
| `Γ` | local environments |
| `Δ` | generic environments |
| `Σ` | the global environment |

Subscripts and primes form further metavariables of the same kind (`e1`, `T'`).

### 1.7.3 Environments

1. The **global environment** `Σ` maps every item path to its declaration: signatures of functions, definitions of types and traits, impls, constants, effects, interfaces, and worlds of the current package and its dependencies.
2. The **generic environment** `Δ` is a list of the type, const, and region parameters in scope, together with their assumptions: trait bounds `X: Tr`, associated type equalities `X.Item = T`, effect caps on bounds, and region outlives assumptions `'a: 'b`.
3. The **local environment** `Γ` is an ordered list of bindings `x: T` (immutable) or `var x: T` (mutable). `Γ, x: T` extends `Γ`; a later binding shadows an earlier one with the same name. `Γ(x)` is the type of the last binding of `x`. `dom(Γ)` is the set of bound names.
4. When `Σ` and `Δ` are clear from context, they are omitted: `Γ ⊢ ...` abbreviates `Σ; Δ; Γ ⊢ ...`.

### 1.7.4 Judgment forms

The following forms are shared by all chapters. A chapter that needs another form defines it before use.

| Judgment | Reading | Defined in |
|---|---|---|
| `Γ ⊢ e : T ! E` | in `Γ`, expression `e` has type `T` and effects `E` | [06](06-expressions-and-statements.md), effects in [08](08-effects-and-capabilities.md) |
| `Γ ⊢ e ⇐ T ! E` | `e` is checked against the expected type `T` (bidirectional checking) | [05](05-types.md), [06](06-expressions-and-statements.md) |
| `Γ ⊢ s ⊣ Γ' ! E` | statement `s` turns `Γ` into `Γ'` with effects `E` | [06](06-expressions-and-statements.md) |
| `Γ ⊢ P : T ⊣ Γ'` | pattern `P` matches values of type `T` and binds `Γ'` | [06](06-expressions-and-statements.md) |
| `Δ ⊢ T wf` | type `T` is well formed | [05](05-types.md) |
| `Δ ⊢ T: Tr` | type `T` implements trait `Tr` | [05](05-types.md) |
| `Δ ⊢ T ⇝ U` | a value of type `T` coerces to type `U` (D-TYPE-04) | [05](05-types.md) |
| `Δ ⊢ 'a: 'b` | region `'a` outlives region `'b` | [07](07-ownership-and-borrowing.md) |
| `E ⊆ F` | effect set `E` is included in `F`, after expanding `covers` sets | [08](08-effects-and-capabilities.md) |
| `Γ ⊢ φ` | the SFC proves predicate `φ` from the facts available in `Γ` | [09](09-contracts-and-refinements.md) |

An inference rule is written with its premises above a line, its conclusion below, and its name on the right:

```text
Γ ⊢ e1 : T ! E1        Γ ⊢ e2 : T ! E2        Δ ⊢ T: Add<T, Output = T>
------------------------------------------------------------------------ [Example-Add]
Γ ⊢ e1 + e2 : T ! E1 ∪ E2 ∪ latent(Add for T)
```

### 1.7.5 Regions

1. Regions are written `'a`, `'b` in rules, explanations, and diagnostics. They never appear in SAI source (D-OWN-03).
2. In rules, a region argument is written after the type constructor it belongs to: `borrow 'a T`, `borrow mut 'a T`, `Str<'a>`, `Slice<'a, T>`, `SliceMut<'a, T>`, and `User<'a>` for a user type with an implicit region parameter.
3. `'static` denotes the region of static data, which outlives every other region. String literals have type `Str<'static>`.
4. A region is a set of control-flow points (NLL, D-OWN-04). "Borrow region" and "allocation region" (a `region` block) are different concepts and are always named distinctly (D-OWN-07).

### 1.7.6 Effect sets

1. An effect set is a finite set of effect names: built-in effects (D-EFF-03), interface effects (`ns:pkg/iface`), and user-defined effects (`db.write`). `∅` is the empty set; a function with effect set `∅` is **pure**.
2. `E ∪ F` and `E ⊆ F` are set union and inclusion after expanding `covers` sets ([08](08-effects-and-capabilities.md)).
3. `latent(X)` is the set of effects reached through the type parameter `X` (D-EFF-02).

### 1.7.7 Substitution

1. A substitution `σ = [U1/X1, .., Un/Xn]` maps type (or const) parameters to types (or const values). `T σ`, also written `T[U1/X1, .., Un/Xn]`, replaces every free occurrence of each `Xi` in `T` by `Ui` simultaneously.
2. Substitution applies in the same way to bounds, where clauses, associated type projections (`X.Item` becomes `U.Item`, which is then normalized), signatures, and effect sets (`latent(X)` becomes `latentᴮ(U)`, D-EFF-02).
3. Region substitution `['b/'a]` replaces regions. Value substitution `e[v/x]` replaces a variable by a value in the dynamic semantics.
4. Substitution never captures: bound names are renamed apart first.

---

## 1.8 Terms and definitions

1. The [glossary](../glossary.md) defines every technical term used across the documents and links to the defining chapter. The glossary is informative; the linked chapter is normative.
2. The following terms are used throughout with these meanings:
   - **program**: a root package together with its resolved dependency graph, compiled for one target and one profile.
   - **compiler**: a tool that checks programs and translates them into artifacts. The reference compiler is `sai` ([impl/01](../impl/01-architecture.md)).
   - **diagnostic**: a message from the compiler with a code, a severity, and a span. An **error** makes the program rejected; a **warning** or an **info** does not, unless a lint level or policy raises it.
   - **lint**: a diagnostic whose level can be changed with `@allow`, `@warn`, or `@deny` ([15](15-attributes-and-lints.md)). Lint names and diagnostic codes share one namespace (D-TOOL-01).
   - **lang item**: a library item that the compiler knows by identity, because a language construct is defined in terms of it ([16](16-core-library.md) §16.3).
   - **staged feature**: §1.4.5.
   - **trap**: the termination of a Wasm instance by a Wasm trap.

---

## 1.9 Document conventions

### 1.9.1 Chapter structure

Every numbered chapter starts with a header (`Status`, `Stage`, `Related`) and a scope paragraph, followed by numbered sections. A normative chapter ends with a **Diagnostics** section listing every code it defines or relies on, and a section **Decisions introduced by this chapter** when it decides questions that [decisions.md](../design/decisions.md) leaves open. The appendix [appendix-a-diagnostic-codes.md](appendix-a-diagnostic-codes.md) collects all codes.

### 1.9.2 Normative text

1. Requirement keywords follow README §3. "X is an error (`CODE`)" means a conforming compiler MUST reject the program and SHOULD report `CODE`.
2. Examples, notes, and rationale are informative (README §2).
3. When documents conflict, the authority order of README §2 applies.

### 1.9.3 Signature listings

Library chapters present APIs as **signature listings**: `sai` blocks that contain item declarations whose function bodies are omitted. A signature listing is valid SAI under exactly one exception: a function declaration outside a trait, interface, or extern block may appear without its body (which [03](03-grammar.md) §3.5 otherwise forbids with `MISSING_FN_BODY`). Every other rule applies, including the clause order of D-SYN-06, explicit receivers, and explicit effects clauses. A body that is shown in a listing is normative (N-J-3).

### 1.9.4 Decisions introduced by chapters

A rule that a chapter adds because [decisions.md](../design/decisions.md) is silent gets a provisional ID of the form `N-<group>-<n>`, recorded in the chapter's final section with a precise rule and a rationale that cites the principles. The body of the chapter cites the provisional ID where the rule applies. A provisional decision has the same force as the rest of the chapter until it is merged into the decision record under a `D-` ID.

### 1.9.5 Other conventions

1. Decision IDs are cited as `D-AREA-NN`, draft corrections as `R-NN`, changes as `C-NN`, and open issues as `O-NN` ([draft-reconciliation.md](../design/draft-reconciliation.md)).
2. Sections of the draft are cited as "draft §N".
3. Section numbers inside a chapter are written `§N.M`. A reference to another chapter names the chapter: "[07](07-ownership-and-borrowing.md) §7.3".
4. Byte quantities use binary prefixes (`KiB`, `MiB`).

---

## 1.10 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `FEATURE_NOT_IMPLEMENTED` | error | a staged feature that this compiler does not implement is used; data: `stage`, `feature` (README §4; formally defined in §1.4.5) |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | the program exceeds an implementation limit of this compiler; data: `limit`, `value` (§1.5.4) |
| `INTERNAL_COMPILER_ERROR` | error | internal failure of the compiler, never caused by the program (§1.4.2 item 8). Owned by this chapter; [impl/11](../impl/11-diagnostics.md) §11.4.6 and [impl/01](../impl/01-architecture.md) §1.10 describe only how it is reported |

---

## 1.11 Decisions introduced by this chapter

### N-J-1 Targets required for v0.1 conformance

- **Rule:** A v0.1 conforming compiler MUST support `wasm32-core` and `wasm32-wasi`, each in the profiles `dev`, `release`, and `tiny`. `wasm64-core` and `wasm64-wasi` are optional; a compiler that offers them MUST conform on them. A conformance claim lists its targets (§1.4.2, §1.4.3).
- **Rationale:** draft §85 scopes v0.1 to wasm32-core plus WIT and components, which need `wasm32-wasi` (Stage 4). wasm64-wasi is experimental (D-TGT-01), so requiring it would make conformance depend on an unstable platform (P8). Every offered target must conform, so a claim never hides partial support (P1).

### N-J-2 Closed lists of implementation-defined and unspecified behavior

- **Rule:** The behaviors in §1.5.2 are the only implementation-defined behaviors, and those in §1.5.3 are the only unspecified behaviors. Undefined behavior exists only for unsafe code that violates a stated safety contract (§1.5.1). Any other behavior is fully defined by the specification.
- **Rationale:** P8 (predictability) and P2: a program's meaning must be knowable from the specification. A closed list prevents chapters from leaving behavior open by accident; the determinism principle (§1.6) relies on it.

### N-J-3 Signature listings

- **Rule:** Library chapters may omit function bodies in `sai` blocks that present APIs, under the conditions of §1.9.3. Everything else in a listing must be valid SAI.
- **Rationale:** P9: one notation for APIs and code avoids a second pseudo-syntax. Writing the real declaration heads (receivers, where clauses, effects clauses) keeps the listings checkable against [03](03-grammar.md) (P2).

### N-J-4 Implementation limits

- **Rule:** The minimum limits of §1.5.4 apply to every conforming compiler. Exceeding a limit of the compiler is the error `IMPLEMENTATION_LIMIT_EXCEEDED`, and limits are applied deterministically.
- **Rationale:** P8: without stated minimums, portability of large programs would depend on the compiler. Deterministic application keeps acceptance within the determinism principle (§1.6).

### N-J-5 Form of `FEATURE_NOT_IMPLEMENTED`

- **Rule:** `FEATURE_NOT_IMPLEMENTED` is reported at the span of the construct, with the data `stage` and `feature`, by `sai check` as well as by `sai build`. A compiler MUST NOT implement a staged construct partially. The feature names of Stages 5–7 are those of the table in §1.4.5. The names of the features of the earlier stages are fixed by the documents that §1.4.5 lists.
- **Rationale:** P1 and P3: stable machine-readable data lets agents and tools tell "not yet implemented" apart from "ill-formed". Reporting it in `sai check` keeps the edit–check loop of draft §78 meaningful.
