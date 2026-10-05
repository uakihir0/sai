# 14. Testing

**Status:** Normative
**Stage:** 3 (tests, properties, test-only dependencies, `@cases`, `@seed`); 5 (async code in tests, §14.6)
**Related:** [decisions.md](../design/decisions.md) D-SYN-08, D-MOD-01, D-TYPE-08, D-EXPR-01, D-EXPR-05, D-EXPR-06, D-EFF-01, D-EFF-02, D-CAP-01, D-CAP-02, D-ERR-01, D-TGT-02, D-PKG-01, D-PKG-03, D-TOOL-01; [draft-reconciliation.md](../design/draft-reconciliation.md) R-07; [01](01-introduction.md), [02](02-lexical-structure.md), [03](03-grammar.md), [04](04-modules-and-names.md), [05](05-types.md), [06](06-expressions-and-statements.md), [07](07-ownership-and-borrowing.md), [08](08-effects-and-capabilities.md), [09](09-contracts-and-refinements.md), [10](10-errors-and-panics.md), [11](11-async-and-concurrency.md), [15](15-attributes-and-lints.md), [16](16-core-library.md), [impl/13](../impl/13-cli.md), [package/01](../package/01-package-contract.md), [package/02](../package/02-resolver.md), [targets](../targets/targets-and-profiles.md)

This chapter defines the meaning of `test` and `property` declarations: how their bodies are typed, how they are named, in which builds they are analyzed, how they use test-only dependencies, which authority they have, when they fail, and how a property generates its cases from a seed. It also defines what a test build contains and how its tests are isolated from each other. The syntax of both declarations belongs to [03](03-grammar.md) §3.11, their canonical paths to [04](04-modules-and-names.md) §4.9, the attributes `@cases` and `@seed` (form and targets) to [15](15-attributes-and-lints.md) §15.12, the `Arbitrary` trait, the `Gen` generator and the library generators to [16](16-core-library.md) §16.17, derived `Arbitrary` impls to [05](05-types.md) §5.12, panics to [10](10-errors-and-panics.md), and the `sai test` command (options, selection, order, output, exit codes) to [impl/13](../impl/13-cli.md) §13.6.6.

---

## 14.1 Overview

### 14.1.1 Model

1. Tests are items. A `test` or `property` declaration is written in an ordinary module, next to the code it tests (03 §3.11). There are no test modules, test directories or conditionally included files: a file is a module (D-SYN-08), every file below `<source>` is a module in every build, and files outside it are never modules ([04](04-modules-and-names.md) §4.2.2, §4.2.3).
2. Tests are analyzed and compiled only by **test analysis** (§14.2.4), which `sai test` and `sai check --tests` perform. Every other build parses them, checks their names and attributes, and otherwise ignores them. They never contribute code to a build artifact.
3. A **test** runs its body once. A **property** runs its body once per generated **case**, with parameter values produced by `Arbitrary` impls from a deterministic generator (§14.5).
4. Tests are **hermetic** (§14.3). The only effect they may perform is allocation. Their outcome therefore depends only on the program and, for a property, on its seed. Code that needs host authority is tested through capability values and traits with fake implementations (D-CAP-02).
5. `sai test` builds a **test harness** for the root package and runs each selected test and property in a fresh instance (§14.7). It reports each outcome and, for a failed property, the counterexample and the seed that reproduces it.

### 14.1.2 Terminology

- **Test declaration**, **property declaration**: the items of 03 §3.11. Where this chapter says "a test" without qualification, it means either kind; "a `test`" means a test declaration only.
- **Name**: the string value of the declaration's `STRING` (§14.2.3).
- **Test path**: the canonical path of the declaration ([04](04-modules-and-names.md) §4.9.1), for example `acme:image/codec.property["roundtrip"]`.
- **Test-only use**: a `use` declaration that names a test dependency (§14.2.5).
- **Test analysis**: the analysis of tests and test-only uses of a package (§14.2.4).
- **Test build**: a build with test analysis whose artifact is a test harness (§14.7.1).
- **Case**: one execution of a property body with one set of generated parameter values (§14.5.5).
- **Base seed**, **case seed**, **size**: the inputs of generation (§14.5.2, §14.5.3).
- **Counterexample**: the parameter values of the first failing case of a property (§14.5.6).

---

## 14.2 Declarations

### 14.2.1 `test`

1. A `test` declaration is an item (03 §3.11). It has no visibility, introduces no binding ([04](04-modules-and-names.md) §4.4.1), and cannot be referred to by any expression, type or path. Written inside a block it is `ITEM_IN_BLOCK` ([06](06-expressions-and-statements.md) §6.25.1).
2. Its body is checked like the body of a non-generic, package-private function with no parameters, the return type `()`, and no clauses ([06](06-expressions-and-statements.md) §6.11.1, [08](08-effects-and-capabilities.md) §8.6.2). In particular:
   - a tail value is discarded under the rules of [06](06-expressions-and-statements.md) §6.25.2;
   - `return` without an operand ends the test ([06](06-expressions-and-statements.md) §6.16.5), and a `return` with a value other than `()` is `TYPE_MISMATCH` ([05](05-types.md));
   - `?` in the body is `TRY_INVALID_RETURN_TYPE` ([10](10-errors-and-panics.md) §10.3.2), because the return type is `()`. A test unwraps with `expect` or `match`;
   - the body is synchronous, so `.await` in it is `AWAIT_OUTSIDE_ASYNC` ([11](11-async-and-concurrency.md) §11.13, N-I-20).
3. A test cannot declare effects, contracts or provenance: the grammar has no clauses for it. Its effect set is inferred ([08](08-effects-and-capabilities.md) §8.7) and restricted by §14.3.
4. A test has no other privilege than its position. It sees the items that are visible in its module, including package-private ones ([04](04-modules-and-names.md) §4.6). The rules of visibility, ownership, effects, unsafe code and contracts apply to its body as to any function body (P9).
5. A test **passes** if its body completes or returns, and **fails** if it panics or traps (§14.4). There is no third way to fail: a test does not return a `Result` (N-I-37).

```sai
pub fn clamp_percent(value: I32) -> I32 {
    value.clamp(0, 100)
}

test "clamp_percent keeps values in range" {
    assert clamp_percent(150) == 100
    assert clamp_percent(-5) == 0
    assert clamp_percent(42) == 42, "values in range are unchanged"
}

test "parse accepts digits" {
    let n = I32.parse("12").expect("12 is a number")
    assert n == 12
}

test "parse with the try operator" {
    let n = I32.parse("12")?                // rejected: TRY_INVALID_RETURN_TYPE
    assert n == 12
}
```

### 14.2.2 `property`

1. A `property` declaration is an item with the same position, visibility and binding rules as a `test` (§14.2.1 item 1). Its parameters are `NamedParam`s (03 §3.5): a name or `_`, an optional `var`, and a type. Parameter names follow the rules of function parameters ([04](04-modules-and-names.md), [06](06-expressions-and-statements.md)). A property has no generic parameters, so every parameter type is a concrete type resolved in the module scope.
2. Every parameter type MUST implement `Arbitrary` ([16](16-core-library.md) §16.17), otherwise `UNSATISFIED_BOUND` ([05](05-types.md) §5.11.2) at the type. When the type is a record, enum, distinct type or refined type of the current package, the help suggests `@derive(Arbitrary)` ([05](05-types.md) §5.12). Views, borrows, raw pointers and function types have no `Arbitrary` impl. A property that needs a `Str` or a `Slice<T>` generates an owned value and borrows it in the body.
3. The parameters are owned values bound in the body's scope, like the parameters of a function ([07](07-ownership-and-borrowing.md)). A `var` parameter is mutable. Each value is dropped when the case ends, in the order of [07](07-ownership-and-borrowing.md).
4. **Result type.** The result type of a property is inferred from the tail of its body and the operands of its `return` expressions, as the return type of a closure without an annotation is ([06](06-expressions-and-statements.md) §6.17.2). It MUST be `Bool` or `()`. Any other type is `TYPE_MISMATCH` at the tail or the `return`, with the expectation "`Bool` or `()`". A body without a tail has type `()` ([06](06-expressions-and-statements.md) §6.11.1). A body whose type is `Never`, and that has no `return` with an operand, has the result type `()` (N-I-38).
5. **Meaning of the result.** With the result type `Bool`, a case whose body produces `false` fails (§14.5.5). With the result type `()`, a case fails only by a panic or a trap. Both styles are equivalent except for the failure message (§14.4.1).
6. `?` in a property body is `TRY_INVALID_RETURN_TYPE`, and `.await` is `AWAIT_OUTSIDE_ASYNC`, as in a `test`.
7. The effects of a property are those of its body together with those of the `Arbitrary.arbitrary` impls of its parameter types and of the `Format.format` impls that print a counterexample (§14.5.6), including the latent effects they leave to the instantiation ([08](08-effects-and-capabilities.md) §8.8). §14.3.1 defines this set, and §14.3 restricts it.
8. A property without parameters is valid. Each of its cases runs the same body with no input, so a `test` says the same thing more directly.

```sai
fn reversed(items: Vec<I32>) -> Vec<I32> {
    items.into_iter().rev().collect()
}

property "addition commutes" (a: I32, b: I32) {
    a +% b == b +% a
}

property "reverse twice" (xs: Vec<I32>) {
    reversed(reversed(xs.clone())) == xs
}

property "clamp stays in range" (x: I32) {
    let y = x.clamp(-10, 10)
    assert y >= -10 and y <= 10
}

property "strings by borrow" (s: Str) {         // rejected: UNSATISFIED_BOUND
    s.len() >= 0
}

property "counts elements" (xs: Vec<I32>) {
    xs.len()                                     // rejected: TYPE_MISMATCH
}
```

The second property is the corrected form of draft §54 (R-07): `xs` is cloned before it is moved into `reversed`, so it can still be compared afterwards.

### 14.2.3 Names and test paths

1. The **name** of a test is the value of its `STRING` ([02](02-lexical-structure.md) §2.6.4): the code points after escapes, line continuations and the indentation rule of multiline strings are applied. Simple, multiline and raw strings are all accepted.
2. A name MUST be non-empty and MUST NOT contain a character of the Unicode general category `Cc` (control characters, including line breaks and tabs). Otherwise the error is `INVALID_TEST_NAME`, reported at the string (N-I-49). Names are printed on one line in reports and matched by filters ([impl/13](../impl/13-cli.md) §13.6.6), which control characters would break.
3. Within one module, the names of all tests and properties that remain after `@target` filtering ([15](15-attributes-and-lints.md) §15.11) MUST be distinct. Names are compared by code points without normalization, as string values are ([02](02-lexical-structure.md) §2.6.4 item 6). Each declaration after the first in source order with an equal name is `DUPLICATE_TEST_NAME`, reported at its string with a note at the first. A `test` and a `property` with the same name are duplicates, although their test paths differ, because reports and filters would otherwise show two entries that read alike (P9).
4. The **test path** of a declaration is its canonical path `m.test["name"]` or `m.property["name"]` ([04](04-modules-and-names.md) §4.9.1). Because module paths are unique in a package and names are unique in a module, test paths are unique in a package. The test path identifies the test in reports and filters ([impl/13](../impl/13-cli.md) §13.6.6) and determines the default seed of a property (§14.5.2).
5. The checks of this section are made in every build, with or without test analysis, so a package is accepted or rejected for its test names independently of the command (P8).

```sai
use sai:core/num.{USIZE_BITS}

test "" {                                         // rejected: INVALID_TEST_NAME
    assert 1 + 1 == 2
}

test "sums small numbers" {
    assert 1 + 1 == 2
}

property "sums small numbers" (n: U8) {           // rejected: DUPLICATE_TEST_NAME
    U16.from(n) + 1 > U16.from(n)
}

@target(memory64)
test "pointer width" {
    assert USIZE_BITS == 64
}

@target(none(memory64))
test "pointer width" {
    assert USIZE_BITS == 32
}
```

The two `"pointer width"` tests are not duplicates, because at most one of them remains after `@target` filtering for any target.

### 14.2.4 Test analysis

1. **Test analysis** is the analysis of the tests, properties and test-only uses (§14.2.5) of a package. It is performed only by `sai test` and by `sai check --tests` ([impl/13](../impl/13-cli.md) §13.6.2, §13.6.6), and only for the packages that the command selects: the root package, or the workspace members named by `--package` or `--workspace`. These commands also activate the test edges of the root package ([package/02](../package/02-resolver.md) §2.5.3). Semantic tooling ([impl/12](../impl/12-semantic-tooling.md) §12.1.2, N-H-79) analyzes the tests view of a workspace for navigation and queries, as `sai check --tests` does. This does not change the diagnostics that `sai build` and `sai check` report.
2. **With test analysis**, every test and property is analyzed as a function body is: names ([04](04-modules-and-names.md)), types ([05](05-types.md)), expressions ([06](06-expressions-and-statements.md)), ownership ([07](07-ownership-and-borrowing.md)), effects ([08](08-effects-and-capabilities.md) and §14.3), contracts ([09](09-contracts-and-refinements.md)) and lints ([15](15-attributes-and-lints.md)). All their diagnostics are reported. Tests and properties are then instantiation roots ([05](05-types.md) §5.7.6) and reachability roots ([08](08-effects-and-capabilities.md) §8.12.3) of the build.
3. **Without test analysis** (in `sai build`, `sai run`, `sai check` without `--tests`, for packages that are not selected, and for every dependency), each test and property is parsed, its attributes are validated ([15](15-attributes-and-lints.md) §15.2.4), and its name is checked (§14.2.3). Nothing else is done with it: its parameter types and its body are not resolved or checked, no diagnostic is reported for them, and it is neither an instantiation root nor a reachability root. Test-only uses are not resolved (§14.2.5 item 4). The inventory of `sai inspect unsafe` still lists the unsafe constructs of a test or property, found from its syntax alone and without operations ([12](12-unsafe-and-ffi.md) §12.11.1 item 2).
4. **References from tests.** `UNUSED_IMPORT` ([04](04-modules-and-names.md) §4.3.6) and `UNUSED_ITEM` ([15](15-attributes-and-lints.md) §15.18.3) count references from tests and properties. With test analysis these are the references that analysis finds. Without test analysis, a `use` binding counts as referenced if its name occurs as an identifier token (after NFC normalization and without the `r#` prefix, [02](02-lexical-structure.md) §2.4) in a test or property of the same module, and an item counts as referenced if its name occurs so in a test or property of the same package. This over-approximation never reports a binding or item that test analysis finds referenced, so a helper that only tests use is not reported by `sai build` (N-I-39).
5. Lints inside tests and properties, such as `UNUSED_VARIABLE` for a property parameter ([15](15-attributes-and-lints.md) §15.18.1), are reported only with test analysis.
6. A compiler that does not implement Stage 3 parses tests and properties (03 §3.11) and treats every build as a build without test analysis. `sai test` and `sai check --tests` are then `FEATURE_NOT_IMPLEMENTED` ([01](01-introduction.md) §1.4.5).
### 14.2.5 Test-only dependencies

1. A **test dependency** of a package is a package identity listed in its `requires test` block ([package/01](../package/01-package-contract.md) §1.10.6). A `use` declaration whose `PKG_PATH` names a test dependency (its `ns:pkg` part, [04](04-modules-and-names.md) §4.3.2 step 1) is a **test-only use**, and the bindings it introduces ([04](04-modules-and-names.md) §4.3.3, §4.3.4) are **test-only bindings**. The identities are read from `sai.package` in every build, also for a dependency, whose `requires test` block is otherwise ignored (package/01 §1.10.6 item 2). A `use` of the current package, of `sai:core`, of `sai:alloc` or of an ordinary dependency is never test-only: `DUPLICATE_REQUIREMENT` excludes an identity that is both an ordinary and a test dependency (package/01 §1.10.4).
2. A test-only binding MAY be referred to only in the body of a test or property, including the closures and blocks in it, and in the parameter types of a property. Every other reference is `TEST_DEPENDENCY_OUTSIDE_TEST`, reported at the reference with a note at the test-only use. This includes references from the signature, body or contracts of a function, from a type, trait, impl, `const` or `invariant`, and from another `use`. This is the error that [04](04-modules-and-names.md) §4.3.2 step 1.3 and [package/01](../package/01-package-contract.md) §1.10.6 item 3 leave to this chapter (N-I-40). A trait bound by a test-only binding is among the traits in scope ([05](05-types.md) §5.13.3 rule 1) only for the bodies and parameter types of tests and properties. In other code it is not in scope, so a method call or associated item path there never selects it, and its meaning is the same with and without test analysis (N-I-52).
3. A test-only use MUST NOT be `pub`. A `pub use` of a test dependency is `TEST_DEPENDENCY_OUTSIDE_TEST` at the `PKG_PATH`, because a re-export is reachable from code outside the tests.
4. Items 2 and 3 are checked in every build, with or without test analysis. They need only the binding names, which follow from the syntax of the `use` ([04](04-modules-and-names.md) §4.3.3 and §4.3.4, N-A-5), and the scope of each reference. **Without test analysis**, a test-only use is not resolved: its package is not looked up, its module path and its items are not checked, and no `UNKNOWN_PACKAGE`, `UNKNOWN_MODULE` or `UNRESOLVED_IMPORT` is reported for it. Its bindings stay in the module scope, so that they still conflict with other bindings of the same name ([04](04-modules-and-names.md) §4.4.1). **With test analysis**, a test-only use is resolved as any `use` ([04](04-modules-and-names.md) §4.3.2), against the test edges that the command activates ([package/02](../package/02-resolver.md) §2.5.3).
5. Test edges exist only for the root package (package/01 §1.10.6 item 2). When test analysis is performed for a selected workspace member that is not the root ([impl/13](../impl/13-cli.md) §13.2.2, §13.6.6), each of its test-only uses is `TEST_DEPENDENCY_UNAVAILABLE`, reported at the `PKG_PATH`. The help says that the test dependencies of a package are available only when it is the root, and suggests running `sai test` in that package's directory. The other tests of the member are still analyzed. A test that refers to a binding of an unavailable use gets no further diagnostic for that reference.
6. Because only test bodies can refer to a test dependency, no code of a test dependency is ever reachable from a root of a build without test analysis ([08](08-effects-and-capabilities.md) §8.12.3), and the dependency is not even active in such a build ([package/02](../package/02-resolver.md) §2.5.3). A helper function that needs a value from a test dependency takes it as a parameter, and the test passes it.

```sai
use acme:fixtures.{small_png}

pub fn has_header(bytes: Slice<Byte>) -> Bool {
    bytes.len() >= 8
}

test "fixture image has a header" {
    let png = small_png()
    assert has_header(png.as_slice())
}

pub fn default_image() -> Vec<Byte> {
    small_png()                                   // rejected: TEST_DEPENDENCY_OUTSIDE_TEST
}

pub use acme:fixtures.{small_png}                 // rejected: TEST_DEPENDENCY_OUTSIDE_TEST
```

In this example `acme:fixtures` is listed in `requires test` of the package (package/01 §1.10.6). `sai build` rejects the package for the last two declarations even though it never resolves `acme:fixtures`. The `pub use` would also conflict with the first `use` as a duplicate binding ([04](04-modules-and-names.md) §4.4.1); it is shown here only for its first error.

---

## 14.3 Hermetic tests

### 14.3.1 The permitted effects

1. The **entry set** `E(t)` of a test or property `t` is the effect set that [08](08-effects-and-capabilities.md) §8.7 infers for a function without a clause whose body is the **entry** of `t`:
   - for a `test`, its body;
   - for a property with parameters `p1: T1, …, pn: Tn`, a body that evaluates `Ti.arbitrary(borrow mut gen)` for `i = 1 … n` in order, with `gen` a local `Gen` (§14.5.3), then the property body, and finally, for every `Ti` that implements `Format`, the call `Ti.format(borrow pi, borrow mut buf, FormatSpec.Debug)`, with `buf` a local `FixedBuf<4096>` ([16](16-core-library.md) §16.14.5), that prints a counterexample (§14.5.6).
   Every type in these calls is concrete, so the latent effects of the `Arbitrary` and `Format` impls, and of every generic function that the body instantiates, are attributed to `t` ([08](08-effects-and-capabilities.md) §8.8.5). The entry is also the reachability root that `t` contributes ([08](08-effects-and-capabilities.md) §8.12.3).
2. **Rule.** `caps(E(t))` ([08](08-effects-and-capabilities.md) §8.12.2) MUST be a subset of `{ alloc }` (N-I-41). For every other capability `r` in it, the error is `TEST_EFFECT_NOT_PERMITTED`, reported once per test and capability at the test's name string. It names `r` and carries the capability chain of [08](08-effects-and-capabilities.md) §8.13 from `t` to the atom whose `caps` contain `r`.
3. Consequently a test MAY allocate, and MAY perform user effects whose `covers` expand to nothing or to `alloc` only ([08](08-effects-and-capabilities.md) §8.4). Every other built-in effect (`fs`, `net`, `clock`, `random`, `stdio`, `env`, `process`, `thread`, `host` and `ffi`, [08](08-effects-and-capabilities.md) §8.3.1), every interface effect and every world-import effect has a capability other than `alloc` and is not permitted. In particular, a test cannot call a function of an `extern "wasm"` block (`host`, [12](12-unsafe-and-ffi.md) §12.7.2) or of an `unsafe extern "C"` block (`ffi`, [12](12-unsafe-and-ffi.md) §12.9).
4. Panics, traps, contract checks and deallocation are not effects ([08](08-effects-and-capabilities.md) §8.7.8, D-OWN-05). A test may panic, and that is how it fails.
5. A test receives no capability value. No `process.Env` is constructed for it ([16](16-core-library.md), D-CAP-02), and there is no static state through which it could obtain one. Unsafe code in a test follows [12](12-unsafe-and-ffi.md) and the package's `policy` ([package/01](../package/01-package-contract.md) §1.12) as anywhere else; an unsafe block gives no authority beyond the rule of item 2, because every way out of linear memory is an import.

The rule makes the outcome of a test a function of the program and, for a property, of its base seed (§14.1.1 item 4). A test that passed once passes on every host, target, profile, job count and run, unless its code changes (P8, P9). Authority that a test does not have cannot be misused by it, even by a compromised test dependency (P5).

### 14.3.2 Relation to the capability policy

1. Tests are ordinary reachability roots for the checks of [08](08-effects-and-capabilities.md) §8.12.6. Their entries are owned by the package that contains them ([08](08-effects-and-capabilities.md) §8.12.4). A test that allocates therefore requires `alloc` to be granted by its own package's policy and by the root's effective policy, and to be provided by the target. Otherwise the build reports `CAPABILITY_NOT_ALLOWED`, `CAPABILITY_DENIED` or `TARGET_CAPABILITY_UNAVAILABLE` as for any root.
2. A test build grants nothing beyond the policies. There is no test-specific `allow`, and `sai test` has no option that adds authority ([impl/13](../impl/13-cli.md) §13.6.6). A `grant` on an entry of `requires test` narrows what that test dependency may require, as for any requirement ([package/01](../package/01-package-contract.md) §1.10.8 item 5).
3. A capability that causes `TEST_EFFECT_NOT_PERMITTED` for a test is not reported again for the same reason, neither by the checks of [08](08-effects-and-capabilities.md) §8.12.6, the target check among them, nor by the grant check of [package/01](../package/01-package-contract.md) §1.10.8 (`CAPABILITY_NOT_GRANTED`): a violation of any of these checks for a capability `r` is suppressed when every root that reaches `r` is a test or property for which `TEST_EFFECT_NOT_PERMITTED` with `r` has been reported ([impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.6, N-D-88).

### 14.3.3 Testing code that needs authority

Code that needs host authority is written against a trait or a capability value, and the test supplies a fake (D-CAP-02). The trait method below has no effect clause, so its effects are latent, and each instantiation has the effects of the impl it chooses ([08](08-effects-and-capabilities.md) §8.6.3, §8.8).

```sai
pub trait Clock {
    fn now_ms(self: borrow Self) -> I64
}

pub fn is_expired<C: Clock>(clock: borrow C, deadline: I64) -> Bool {
    clock.now_ms() >= deadline
}

type FixedClock {
    ms: I64
}

impl Clock for FixedClock {
    fn now_ms(self: borrow Self) -> I64 {
        self.ms
    }
}

test "deadline in the past has expired" {
    let clock = FixedClock(ms: 2_000)
    assert is_expired(borrow clock, 1_000)
}

extern "wasm" module "env" {
    fn host_now_ms() -> I64
}

test "host clock is positive" {                   // rejected: TEST_EFFECT_NOT_PERMITTED
    assert host_now_ms() > 0
}
```

The entry set of the first test is empty: the call `is_expired(borrow clock, 1_000)` instantiates `C` with `FixedClock`, whose `now_ms` is pure. A production impl of `Clock` reads the clock through `wasi:clocks` and has `clock.read`; only the program's `main` or an export instantiates `is_expired` with it. The second test calls an import, whose effect `host` maps to the capability `host`. The diagnostic's chain is `test "host clock is positive" → host_now_ms`.

---

## 14.4 Outcomes

### 14.4.1 Outcomes and failure kinds

1. Every selected test that runs has exactly one **outcome**: `passed` or `failed`. A test of a selected package that the filters exclude is **filtered out** and does not run ([impl/13](../impl/13-cli.md) §13.6.6). There is no other outcome: no skipped, ignored, expected-failure or timed-out test (N-I-42).
2. A `test` passes when its body completes or returns. A property passes when all of its cases pass (§14.5.5).
3. A failed test has one **failure kind**, with the failure message and location that the report carries ([impl/13](../impl/13-cli.md) §13.6.6):

| Kind | Cause | Message | Location |
|---|---|---|---|
| `panic` | a panic ([10](10-errors-and-panics.md)) while the test runs, in `Report` mode (§14.4.2) | the panic message ([10](10-errors-and-panics.md) §10.4.4) | the panic location ([10](10-errors-and-panics.md) §10.4.5), or none for the resource kinds `StackOverflow` and `OutOfMemory` ([10](10-errors-and-panics.md) §10.4.3) |
| `trap` | a plain trap ([10](10-errors-and-panics.md) §10.7), or any panic in `TrapOnly` mode | in `Report` mode, `trap: ` followed by the reason of item 4; in `TrapOnly` mode, none | none |
| `counterexample` | a case of a property whose result type is `Bool` produced `false` (§14.5.5) | `property returned false` | none |

4. The **reason** of a trap is the trap condition that the embedded runtime ([impl/13](../impl/13-cli.md) §13.6.5) observed, written as one of `unreachable`, `memory out of bounds`, `table out of bounds`, `indirect call type mismatch`, `integer divide by zero`, `integer overflow`, `invalid conversion to integer`, `call stack exhausted`, or `other` for any condition not in this list. Safe SAI code reaches a trap in a test only through `call stack exhausted` ([10](10-errors-and-panics.md) §10.7.1); the others come from undefined behavior in unsafe code ([12](12-unsafe-and-ffi.md) §12.6) or from a panic in `TrapOnly` mode.
5. A failure is reported for the first failing event only. A test stops at its first panic or trap, because the instance ends ([10](10-errors-and-panics.md) §10.4.1, §10.7.2), and a property stops at its first failing case (§14.5.5).
6. For a failed property, the report also carries its base seed, the number of cases that ran, and its counterexample if one could be printed (§14.5.6, §14.5.7), whatever the failure kind.

### 14.4.2 Panic reporting in test builds

1. Every test build has a reporting channel: the **harness channel**, through which the panic machinery ([10](10-errors-and-panics.md) §10.5.2) delivers a report to the runner. The panic mode of a test build is therefore `Report` under the `dev` and `release` profiles and `TrapOnly` under `tiny` (N-I-50; [10](10-errors-and-panics.md) §10.5.3, §10.9 item 3). The report has the form of [10](10-errors-and-panics.md) §10.5.2. The runner reads the message and location from it and does not print the report itself.
2. The harness channel is part of the test harness (§14.7.1). It is not an import of the program, no SAI code can name or call it, and it adds no effect to any function and no capability to any package, in the same way as the panic hook ([10](10-errors-and-panics.md) §10.6.4, N-C-81). It is not a `wasi:cli/stderr` import, and a `@panic_hook` of the root package ([10](10-errors-and-panics.md) §10.6) is not imported by a test build.
3. Under `tiny` a panic is a trap without a message (D-TGT-02). Its failure kind is `trap`. To see the message of a failing test, run it under `dev`.

### 14.4.3 What a test cannot express

These omissions are deliberate (N-I-42):

1. **No expected panics.** A test cannot require its body to panic. A panic is a bug, not an error value (D-ERR-01). Code whose failure a test should check returns a `Result` or an `Option`, and the test matches on it.
2. **No catching.** A test cannot catch a panic or a trap ([10](10-errors-and-panics.md) §10.9 item 4).
3. **No limits.** The runner imposes no time limit, instruction budget or memory limit of its own. A test that does not terminate keeps `sai test` from terminating. A test that exhausts the linear memory or the shadow stack of its instance fails with the panic `OutOfMemory` or `StackOverflow` ([10](10-errors-and-panics.md) §10.4.3). A wall-clock limit would make the outcome depend on the machine and on `--jobs`, contrary to §14.3.1 (P8).
4. **No skipping.** A test is removed for a target with `@target` ([15](15-attributes-and-lints.md) §15.11), or by deleting it. Selection by filter ([impl/13](../impl/13-cli.md) §13.6.6) is the only way to run some tests and not others.

---

## 14.5 Properties

### 14.5.1 Number of cases

1. A property runs `N` cases, where `N` is the argument of `@cases(N)` ([15](15-attributes-and-lints.md) §15.12), or 100 if the property has no `@cases` (N-I-43). [15](15-attributes-and-lints.md) §15.12 validates the range of `N`.
2. `N` is the same for every target and profile. No command option changes it.

### 14.5.2 Base seed

1. The **base seed** `S` of a property is a `U64`, chosen in this order (N-I-43):
   1. the argument of its `@seed(N)` attribute ([15](15-attributes-and-lints.md) §15.12);
   2. otherwise, the value of the `--seed` option of `sai test` ([impl/13](../impl/13-cli.md) §13.6.6), if given;
   3. otherwise, the **default seed**: the FNV-1a 64-bit hash of the bytes of the test path text (item 2).
2. The **test path text** is the canonical path of the property ([04](04-modules-and-names.md) §4.9.1) encoded in UTF-8, with the name written between `"` characters and every `"` and `\` in it preceded by `\`. No other escape is used, because a name contains no control character (§14.2.3). This is the text that reports print ([impl/13](../impl/13-cli.md) §13.6.6), for example `acme:image/codec.property["roundtrip"]`.
3. **FNV-1a 64.** The hash starts with `h = 0xCBF2_9CE4_8422_2325`. For each byte `b` in order, `h` becomes `(h ^ U64.from(b)) *% 0x0000_0100_0000_01B3`, with the wrapping multiplication of [06](06-expressions-and-statements.md) §6.8.4 on `U64`. The result is the final `h`.
4. The default seed depends only on the package identity, the module path and the name. It does not depend on the package version, on the file contents, on the order of declarations or on the command. Every run of `sai test` without `--seed` therefore runs the same cases (P8). Renaming a property or its module changes its cases. `--seed` runs other cases, for exploration.

### 14.5.3 Case seeds and sizes

1. The cases of a property are numbered `i = 0 … N−1`. The **case seed** `s_i` is the output of step `i + 1` of SplitMix64 ([16](16-core-library.md) §16.17) started with the state `S`: `s_0` is the first output, `s_1` the second, and so on (N-I-44).
2. The **size** of case `i` is `z_i = ⌊(i + 1) × 100 / N⌋`, computed exactly. Sizes grow from `⌊100 / N⌋` to 100 in the last case. Small values therefore come first, and the last case of every property uses the full size 100. With `N > 100`, the first cases have size 0.
3. The generator of case `i` is `Gen.from_seed(s_i, z_i)` ([16](16-core-library.md) §16.17). Each case has its own generator, so case `i` can be generated again from `s_i` and `z_i` alone (§14.5.6).
4. These definitions and the algorithms of [16](16-core-library.md) §16.17 are fixed, so that a base seed produces the same cases with every conforming implementation, on every target and in every profile (P8).

### 14.5.4 Generating values

1. The value of each parameter is produced by `T.arbitrary(borrow mut gen)`, where `T` is the parameter type and `gen` the generator of the case. The parameters are generated in declaration order from the same generator. The impl is the one that trait resolution selects ([05](05-types.md) §5.11): a library impl ([16](16-core-library.md) §16.17), a compiler-provided impl for a tuple or an array ([05](05-types.md) §5.11.4), a derived impl ([05](05-types.md) §5.12), or a user impl.
2. A user impl draws from the generator with `next_u64` and `below`, reads the size with `size`, and calls the `Arbitrary` impls of its parts. It MAY change the size with `set_size`. The change lasts until the next `set_size` in the same case, and affects the parameters generated after it. An impl that changes the size SHOULD restore the value that `set_size` returned.
3. **Rejection limit** (N-I-45). A derived impl that filters, for a refined type, a distinct type with a refinement clause, or a type with an `invariant` ([05](05-types.md) §5.12.3), makes at most 100 **attempts** per value. Each attempt generates a base value with the base type's `Arbitrary` impl from the same generator and evaluates the predicate with `self` bound to it ([09](09-contracts-and-refinements.md) §9.2.1). The first value that satisfies the predicate is the result. If none of the 100 does, the impl panics with the kind `ExplicitPanic` ([10](10-errors-and-panics.md) §10.4.4) and the message `Arbitrary: no value of <T> found in 100 attempts`, where `<T>` is the type in the `CanonType` form of [04](04-modules-and-names.md) §4.9.1. When the entry of a property calls the impl, the location of the panic is the parameter's type in the property declaration ([10](10-errors-and-panics.md) §10.4.5, compiler-generated bodies).
4. A derived impl of a non-recursive type uses the size only through the impls of its fields. A derived impl of a recursive type halves the size for its recursive fields and payloads and chooses only non-recursive variants at size 0, so its values stay small; a derived impl of a type with an `invariant` filters candidates with the rejection limit of item 3 ([05](05-types.md) §5.12.3, N-A-66). A type whose values need another distribution, or whose invariant few candidates satisfy, uses a user impl.

```sai
use sai:core/testing.{Gen}

@derive(Arbitrary)
type Even = I32 where self % 2 == 0

enum Expr {
    Lit(I32)
    Add(Box<Expr>, Box<Expr>)
}

impl Arbitrary for Expr {
    fn arbitrary(gen: borrow mut Gen) -> Expr {
        if gen.size() == 0 or gen.below(3) == 0 {
            return Expr.Lit(I32.arbitrary(gen))
        }
        let old = gen.set_size(gen.size() / 2)
        let left = Box.new(Expr.arbitrary(gen))
        let right = Box.new(Expr.arbitrary(gen))
        gen.set_size(old)
        Expr.Add(left, right)
    }
}

property "even values stay even when doubled" (x: Even) {
    let base: I32 = x
    base *% 2 % 2 == 0
}
```

About half of all `I32` values are even, so the derived impl of `Even` practically never reaches the rejection limit. A predicate that few generated values satisfy, such as `self >= 0 and self <= 100` at large sizes, can reach it; such a type needs a user impl. The impl of `Expr` halves the size at each level, so the depth of a value is at most about `log2` of the size plus one. Its effect set is `{ alloc }`, from `Box.new`, which a test may perform (§14.3.1).

### 14.5.5 Evaluating cases

1. The cases of a property run one after another, in the order `i = 0 … N−1`, in one instance (§14.7.3). Case `i` consists of these steps:
   1. create the generator `Gen.from_seed(s_i, z_i)` (§14.5.3);
   2. generate the parameter values in declaration order (§14.5.4);
   3. evaluate the body with the parameters bound to the values;
   4. drop the parameter values that the body has not moved, and the generator, in the order of [07](07-ownership-and-borrowing.md).
2. A case **fails** if a panic or a trap occurs in any step, or if the result type is `Bool` and the body produces `false`. Otherwise it **passes**.
3. The first failing case ends the property, which fails. No further case runs. If all `N` cases pass, the property passes. The number of cases that ran is reported in both cases (§14.4.1 item 6).
4. A case cannot observe the cases before it. SAI has no mutable static state (D-CAP-02), and a test can perform no effect other than allocation (§14.3), so the values and the result of case `i` depend only on `s_i`, `z_i` and the program. Only the exhaustion of linear memory can depend on earlier cases, and it is a panic either way.

### 14.5.6 Counterexamples

1. The **counterexample** of a failed property is the list of parameter values of its failing case, exactly as generated. There is no shrinking: `Arbitrary` has no method for it ([16](16-core-library.md) §16.17 item 1), and the runner does not search for smaller failing values (N-I-46). The growing sizes of §14.5.3 make early failing cases small.
2. The failing instance is never used again ([10](10-errors-and-panics.md) §10.7.2). To print the counterexample, the runner creates a fresh instance of the test harness and, in it, generates the parameter values of the failing case again from `s_i` and `z_i`, and formats them. Generation is deterministic (§14.5.5 item 4), so these are the values of the failing case, unless an impl observes addresses through unsafe code.
3. The **counterexample text** is `(`, then for each parameter in declaration order its name (`_` for a parameter without a name), `: ` and its value, separated by `, `, then `)`. A property without parameters has the text `()`. A value whose type implements `Format` is written in its `Debug` form ([16](16-core-library.md) §16.14.6). A value whose type does not is written `<T>`, where `T` is its type in the `CanonType` form of [04](04-modules-and-names.md) §4.9.1. The whole text is written to one `FixedBuf<4096>` ([16](16-core-library.md) §16.14.5). If the buffer is truncated, the text is its content followed by `…` (U+2026).
4. The counterexample is absent if the failure occurred while the values were generated (step 2 of §14.5.5 item 1), because the case has no complete list of values, or if the new generation or the formatting panics or traps. The report then still carries the base seed and the number of cases.
5. For the library types the values use the `Debug` forms of [16](16-core-library.md) §16.14.6, which read like SAI literals: `(xs: [3, -1], name: "a\n")`. For a type with a user `Format` impl, the value is whatever that impl writes.

### 14.5.7 Reproducing a failure

1. A failure of a property is reproduced by running the property again with the same program, the same `@cases` and the same base seed. The report gives the base seed `S` and the number of cases `k`; the failing case is case `k − 1` (§14.5.3).
2. Without `@seed` and `--seed`, `S` is the default seed, so a plain `sai test` reproduces the failure. A failure found with `--seed S` is reproduced with `--seed S`. To keep a failing input as a regression test, the property is given `@seed(S)`, or a `test` is written with the printed values.
3. A change of `@cases` changes the sizes (§14.5.3), and a change of the parameter list, of a parameter type or of an `Arbitrary` impl changes the values. Such a change may make the failure disappear without fixing it.

```sai
@cases(500)
@seed(42)
property "sorting keeps the length" (xs: Vec<I32>) {
    var ys = xs.clone()
    ys.sort()
    ys.len() == xs.len()
}
```

This property runs 500 cases from the base seed 42 in every run, with and without `--seed`.

---

## 14.6 Async code in tests

1. Test and property bodies are synchronous (N-I-20). `.await` in them is `AWAIT_OUTSIDE_ASYNC` (§14.2.1 item 2). A test of async code drives a future to completion with `block_on` of `sai:async` ([11](11-async-and-concurrency.md) §11.13), which the package lists in `requires` or, for tests only, in `requires test` (§14.2.5).
2. `block_on` has the effect `alloc` only, so it is permitted in a test (§14.3.1). The futures it runs are hermetic for the same reason as any other code of the test: their effects are part of the entry set.
3. Execution under `block_on` is deterministic ([11](11-async-and-concurrency.md) §11.9.1, §11.13): the outcome of an async test, including the order in which its tasks run, depends only on the program. A test whose futures can make no progress panics with the message `block_on: deadlock`, so it fails instead of hanging.
4. Async code in tests is available from Stage 5, with async code itself ([11](11-async-and-concurrency.md)).

```sai
use sai:async.{block_on}

async fn double(x: I32) -> I32 {
    x * 2
}

test "double doubles" {
    let n = block_on(double(21))
    assert n == 42
}
```

---

## 14.7 Running tests

### 14.7.1 Test builds

1. A **test build** is a build with test analysis (§14.2.4) for one target and one profile, whose artifact is a **test harness**. `sai test` produces it ([impl/13](../impl/13-cli.md) §13.6.6). The harness is stored in the build cache and never written to `out/`. It is not an artifact of the package: it is not distributed, it has no WIT world, and nothing in it is part of the package's API (N-I-47).
2. A test build is made only if test analysis reports no error. An error in any selected package, or in a dependency, prevents the test build, and no test runs ([impl/13](../impl/13-cli.md) §13.6.6, exit code 1). A partial run would make it unclear which tests were checked (P9).
3. **Roots.** The reachability roots of a test build ([08](08-effects-and-capabilities.md) §8.12.3) and its instantiation roots ([05](05-types.md) §5.7.6) are the entries (§14.3.1) of the selected tests and properties, and nothing else. The harness contains the code reachable from them and the harness code of item 5. `main`, the `export fn` items, the functions of the `export world` and the `@panic_hook` are not roots of a test build, because the harness has none of the program's entry points or exports. Their capability checks are made by the check that precedes the build, which `sai check --tests` also performs (§14.2.4 item 2).
4. **Target and profile.** The test build uses the target and the profile that `sai test` selects ([impl/13](../impl/13-cli.md) §13.6.6). The capability checks of [08](08-effects-and-capabilities.md) §8.12.6, including the target check, apply to its roots. Code generation, optimization and the panic mode (§14.4.2) follow the profile, so a test observes the same behavior as the program built with that profile and target, except for the absence of every capability other than `alloc`.
5. **Harness code.** The harness adds, for each property, the code that computes its base seed, case seeds and sizes (§14.5.2, §14.5.3) and runs its cases (§14.5.5), the harness channel (§14.4.2), and the interface through which the runner starts a test and receives its result. The form of this interface (the harness's exports and imports and the encoding of results) is private to the toolchain and not specified (N-I-47). It is never seen by SAI code and never distributed, and every observable behavior of a test is fixed by this chapter (P7).

### 14.7.2 Selection and order

1. The selection of tests by package and filter, the test order, parallel execution with `--jobs`, and the order of output are defined by [impl/13](../impl/13-cli.md) §13.6.6.
2. A test removed by `@target` ([15](15-attributes-and-lints.md) §15.11) does not exist in the build. It is neither selected nor counted as filtered out.
3. Because tests are hermetic (§14.3) and isolated (§14.7.3), the outcome of each test, its failure message, and for a property its cases and counterexample, are independent of which other tests are selected, of the order in which they run, and of `--jobs` (P8).

### 14.7.3 Isolation

1. Each selected `test` and each selected property runs in its own, fresh instance of the test harness (N-I-48). A fresh instance has new linear memory initialized from the harness's data, a new shadow stack and a new allocator state ([impl/10](../impl/10-runtime.md)). Nothing is shared between the instances of two tests.
2. All cases of one property run in the same instance (§14.5.5). Printing a counterexample uses one more fresh instance (§14.5.6 item 2).
3. A test that panics, traps or, through undefined behavior in unsafe code, corrupts its linear memory therefore cannot affect another test. After a panic or a trap the runner uses the instance no more ([10](10-errors-and-panics.md) §10.7.2, §10.9 item 2).
4. The runner MAY run several instances at the same time, up to `--jobs` ([impl/13](../impl/13-cli.md) §13.6.6). A test cannot spawn a thread (`thread.spawn` is not permitted, §14.3.1), so each instance runs on one thread.

### 14.7.4 Reports

1. For each test that ran, the runner reports its test path, whether it is a `test` or a property, its outcome, and, for a failed test, the failure kind, message and location of §14.4.1. For a property it also reports its base seed (§14.5.2), the number of cases that ran (§14.5.5) and, if it failed, its counterexample text or the absence of one (§14.5.6). The human and JSON forms and the exit codes are defined by [impl/13](../impl/13-cli.md) §13.6.6.
2. In the JSON record of a property, `seed` is the base seed, `cases` the number of cases that ran, and `counterexample` the counterexample text, or `null` when the property passed or the counterexample is absent. The text is the generated counterexample, not a shrunk one (§14.5.6 item 1).
3. The diagnostics of test analysis are reported like all other diagnostics ([impl/11](../impl/11-diagnostics.md)). They are not test results.

---

## 14.8 Decisions introduced by this chapter

These entries are provisional until they are merged into [decisions.md](../design/decisions.md). IDs are shared with [11](11-async-and-concurrency.md) and [12](12-unsafe-and-ffi.md).

| ID | Rule | Rationale | Section |
|---|---|---|---|
| N-I-37 | A `test` body is checked as the body of a non-generic, package-private function without parameters or clauses that returns `()`. It passes when it completes and fails only by a panic or a trap. It cannot return a `Result`, so `?` in it is `TRY_INVALID_RETURN_TYPE`. | P7: one way to fail, the one that `assert` and `expect` already use. P1: an error that a test does not expect is unwrapped visibly. | §14.2.1 |
| N-I-38 | A property's parameter types must implement `Arbitrary`, and its parameters are owned. Its result type is inferred like a closure's and must be `Bool` or `()`; `false` fails the case. | P7: both styles reuse expressions and `assert`. P2: a property that returns something else is a static error, not a silent pass. | §14.2.2 |
| N-I-39 | Tests and properties are analyzed only by `sai test` and `sai check --tests`, for the selected packages. Semantic tooling also analyzes them for navigation and queries (impl/12 N-H-79), which does not change the diagnostics of other builds. Other builds parse them and check their names and attributes. For `UNUSED_IMPORT` and `UNUSED_ITEM`, such builds count an identifier token with the binding's name in a test of the same module (imports) or package (items) as a reference. | P8: building a program never depends on code that it does not contain. P9: helpers used only by tests are not reported by builds that cannot see the uses. | §14.2.4 |
| N-I-40 | A `use` of a `requires test` dependency is test-only. Its bindings may be referred to only in test and property bodies and in property parameter types, and it cannot be `pub` (`TEST_DEPENDENCY_OUTSIDE_TEST`, checked in every build). Without test analysis it is not resolved. In a selected workspace member that is not the root, it is `TEST_DEPENDENCY_UNAVAILABLE`. | P5: a test dependency can never become reachable from a program. P8: the check needs only syntax, so every build gives the same answer. | §14.2.5 |
| N-I-41 | Tests are hermetic: the capabilities of the entry set of a test or property, which includes its generators and counterexample formatting, must be a subset of `{ alloc }`, otherwise `TEST_EFFECT_NOT_PERMITTED`. `alloc` remains subject to the normal policy checks, and no other grant exists for tests. | P5: tests get no authority, so a compromised test dependency gets none. P8: an outcome depends only on the program and the seed. D-CAP-02: authority is tested through fakes. | §14.3 |
| N-I-42 | The outcomes are `passed` and `failed`. The failure kinds are `panic`, `trap` (with a fixed list of reasons) and `counterexample` (message `property returned false`). There are no expected panics, catching, skipping or time and memory limits of the runner. | P7: no test attributes beyond `@cases` and `@seed`. D-ERR-01: a panic is a bug. P8: a wall-clock limit would make outcomes depend on the machine. | §14.4 |
| N-I-43 | A property runs 100 cases unless it has `@cases`. Its base seed is `@seed`, else `--seed`, else the FNV-1a 64-bit hash of its test path text. | P8: every run of `sai test` is reproducible without configuration, and different properties get different cases. | §14.5.1, §14.5.2 |
| N-I-44 | Case `i` of `N` uses the seed of step `i + 1` of SplitMix64 from the base seed and the size `⌊(i + 1) × 100 / N⌋`, with its own `Gen`. | P8: identical cases with every implementation. P9: small inputs come first, and a case can be regenerated alone. | §14.5.3 |
| N-I-45 | A filtering derived impl makes at most 100 attempts per value and then panics with `Arbitrary: no value of <T> found in 100 attempts`, located at the parameter type when the property calls it. | P9: a predicate that generation cannot satisfy fails loudly instead of looping. P8: a fixed limit gives the same result everywhere. | §14.5.4 |
| N-I-46 | There is no shrinking. The counterexample is the failing case's values, regenerated in a fresh instance and printed in their `Debug` form (or `<T>` without `Format`) into a 4096-byte buffer, as `(name: value, …)`. | P7: no shrinking API in `Arbitrary`. P9: printing never runs code in a broken instance. P8: the text is a function of the seed. | §14.5.6 |
| N-I-47 | A test build has as roots only the entries of the selected tests, uses the selected target and profile, and is made only when test analysis reports no error. Its harness is not an artifact, and its interface with the runner is private to the toolchain. | P7: nothing outside the toolchain depends on the harness. P9: either every selected test was checked and ran, or none ran. | §14.7.1 |
| N-I-48 | Each `test` and each property runs in its own fresh instance. All cases of a property share one instance. | P8: outcomes do not depend on selection, order or `--jobs`. P9: a trap or memory corruption cannot spread to another test. | §14.7.3 |
| N-I-49 | A test name must be non-empty and contain no `Cc` character (`INVALID_TEST_NAME`), and names must be unique per module across tests and properties after `@target` filtering (`DUPLICATE_TEST_NAME`), checked in every build. | P3, P9: names identify tests in filters and one-line reports. P8: acceptance does not depend on the command. | §14.2.3 |
| N-I-50 | A test build always has the harness channel as its reporting channel, so its panic mode is `Report` under `dev` and `release` and `TrapOnly` under `tiny`. The channel adds no import, effect or capability, and a `@panic_hook` is not used. | D-ERR-01, D-TGT-02: profiles keep their meaning in tests. P5: reporting never needs a capability. | §14.4.2 |
| N-I-52 | A trait bound through a test-only binding is in scope only in tests and properties. | P8: without test analysis a test-only use is not resolved, so a method call outside tests could otherwise select a different method with and without test analysis. P1: the methods available to ordinary code follow from its ordinary imports alone. | §14.2.5 |

## 14.9 Diagnostics

Codes defined in another document are marked with that document. The lints of [15](15-attributes-and-lints.md) are reported inside tests only with test analysis (§14.2.4 item 5).

| Code | Severity | Description |
|---|---|---|
| `AWAIT_OUTSIDE_ASYNC` | error | defined in [11](11-async-and-concurrency.md) §11.3.1; `.await` in a test or property body, which is synchronous (§14.2.1, §14.6) |
| `CAPABILITY_DENIED` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; `alloc` required by a test is denied by a policy (§14.3.2) |
| `CAPABILITY_NOT_ALLOWED` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; `alloc` required by a test is not allowed by a policy (§14.3.2) |
| `DUPLICATE_TEST_NAME` | error | two tests or properties of one module with the same name after `@target` filtering, reported at the later one with a note at the first (§14.2.3) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5; `sai test` or `sai check --tests` with a compiler that does not implement Stage 3 (§14.2.4) |
| `INVALID_TEST_NAME` | error | an empty test name, or one that contains a character of the general category `Cc` (§14.2.3) |
| `ITEM_IN_BLOCK` | error | defined in [06](06-expressions-and-statements.md) §6.25.1; a `test` or `property` inside a block (§14.2.1) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [08](08-effects-and-capabilities.md) §8.12.6; a capability required by a test that the target does not provide (§14.3.2) |
| `TEST_DEPENDENCY_OUTSIDE_TEST` | error | a reference to a binding of a test-only use outside a test or property body and property parameter types, or a `pub use` of a test dependency; checked in every build (§14.2.5) |
| `TEST_DEPENDENCY_UNAVAILABLE` | error | a test-only use in a selected workspace member that is not the root, whose test dependencies are not resolved (§14.2.5 item 5) |
| `TEST_EFFECT_NOT_PERMITTED` | error | a test or property whose entry set maps to a capability other than `alloc`, reported once per test and capability at the name, with the capability chain (§14.3.1) |
| `TRY_INVALID_RETURN_TYPE` | error | defined in [10](10-errors-and-panics.md) §10.3.2; `?` in a test or property body (§14.2.1, §14.2.2) |
| `TYPE_MISMATCH` | error | defined in [05](05-types.md) §5.14; a property result type other than `Bool` or `()`, or a `return` with a value in a `test` (§14.2.1, §14.2.2) |
| `UNKNOWN_MODULE` | error | defined in [04](04-modules-and-names.md) §4.3.2; reported for a test-only use only with test analysis (§14.2.5 item 4) |
| `UNKNOWN_PACKAGE` | error | defined in [04](04-modules-and-names.md) §4.3.2; reported for a test-only use only with test analysis (§14.2.5 item 4) |
| `UNRESOLVED_IMPORT` | error | defined in [04](04-modules-and-names.md) §4.3.4; reported for a test-only use only with test analysis (§14.2.5 item 4) |
| `UNSATISFIED_BOUND` | error | defined in [05](05-types.md) §5.11.2; a property parameter type without an `Arbitrary` impl, with the fix "add `@derive(Arbitrary)`" for a derivable type of the current package (§14.2.2) |
| `UNUSED_IMPORT` | warning (lint) | defined in [04](04-modules-and-names.md) §4.3.6; counts references from tests, approximated lexically without test analysis (§14.2.4 item 4) |
| `UNUSED_ITEM` | warning (lint) | defined in [15](15-attributes-and-lints.md) §15.18.3; counts references from tests, approximated lexically without test analysis (§14.2.4 item 4) |
| `UNUSED_VARIABLE` | warning (lint) | defined in [15](15-attributes-and-lints.md) §15.18.1; an unused property parameter, reported only with test analysis (§14.2.4 item 5) |
