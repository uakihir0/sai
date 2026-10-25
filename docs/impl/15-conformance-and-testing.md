# 15. Conformance and Testing

**Status:** Normative. §15.1 to §15.6, §15.8 and §15.9 define the conformance suite, which substantiates the conformance claims of every implementation ([spec/01](../spec/01-introduction.md) §1.4.3); the parts of them marked as applying to the reference implementation bind it only. §15.7 and §15.10 to §15.12 define the testing strategy of the reference implementation and are normative for it only (README §2).
**Stage:** 0 (suite format, harness, expected diagnostics, diagnostics and formatter goldens, frontend fuzzing, specification cross-checks); 1 (execution on `wasm32-core`, MIR goldens); 2 (the `tiny` profile, `wasm64-core`, ownership and library tests); 3 (package, lock and Git fixtures, `sai test`); 4 (execution on `wasm32-wasi`, component and WIT goldens); 5 to 7 (the tests of their features, §15.9.1)
**Related:** [decisions.md](../design/decisions.md) D-TGT-02, D-TOOL-01, D-TOOL-03, D-TOOL-04, D-TOOL-05; [draft-reconciliation.md](../design/draft-reconciliation.md) C-03; [spec/01](../spec/01-introduction.md) §1.4, §1.5, §1.6; [spec/03](../spec/03-grammar.md); [spec/09](../spec/09-contracts-and-refinements.md) §9.6.8, §9.6.9; [spec/14](../spec/14-testing.md); [spec/16](../spec/16-core-library.md) §16.1.2, §16.1.6, §16.3; [01-architecture.md](01-architecture.md); [02-frontend-ast-hir.md](02-frontend-ast-hir.md); [04-mir.md](04-mir.md); [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md); [07-component-lowering.md](07-component-lowering.md) §7.13.2; [16-interfaces-and-wit.md](16-interfaces-and-wit.md) §16.11; [09-optimization-and-size.md](09-optimization-and-size.md) §9.11; [10-runtime.md](10-runtime.md) §10.11; [11-diagnostics.md](11-diagnostics.md); [13-cli.md](13-cli.md); [14-formatter.md](14-formatter.md) §14.11; [package/03](../package/03-lock-file.md) §3.4.7; [package/04](../package/04-distribution-and-security.md) §4.2

This chapter defines the conformance suite of SAI: how it is organized, the format of its tests, how expected diagnostics are checked, which outputs are compared with golden files, how Wasm artifacts are executed deterministically, which configurations must give equal results, how the specification itself is turned into tests, and which tests a conformance claim for a range of stages must pass. It also defines the testing strategy of the reference implementation: fuzzing, differential testing against independent oracles, benchmarks, and the compiler's own tests and merge gates. The meaning of `test` and `property` declarations belongs to [spec/14](../spec/14-testing.md), and the `sai test` command to [13](13-cli.md) §13.6.6. This chapter uses them as tools; it does not restate them.

**Implementation status (2026-10-25).** The requirements in this chapter specify the target conformance system; they do not describe checks already implemented unless explicitly stated. The current [GitHub Actions workflow](../../.github/workflows/ci.yml) checks formatting and `cargo test --workspace --locked` on Ubuntu, macOS and Windows, and runs two additional package-cache tests on Windows. The current `sai check` implementation explicitly does not establish Stage 0 conformance ([13](13-cli.md) §13.6). The target claim and merge gate below remain normative requirements.

**Review required:** Revalidate this status when conformance infrastructure is introduced, before presenting the target claim or gate as active.

---

## 15.1 Overview

### 15.1.1 Terms

| Term | Meaning |
|---|---|
| **conformance suite** | the tests under `tests/conformance/` of the compiler repository, together with the tests generated from the specification (§15.8). The suite has a version, which is the language version it tests ([01](01-architecture.md) §1.13.1). |
| **implementation under test** (IUT) | the toolchain whose claim is being checked. The reference implementation is one IUT among others. |
| **harness** | the program that runs the suite: the crate `sai_test_harness` ([01](01-architecture.md) §1.3), invoked as `cargo xtask test`. It contains the execution engine of §15.5 and the oracles of §15.10. |
| **test** | one file or directory of the suite, with its directives (§15.2.3) and expectations (§15.3) |
| **configuration** | a target, a profile, and the other settings under which a test runs once (§15.2.6) |
| **expectation** | a statement of a test about the IUT's output: an annotation (§15.3), a directive, or a sidecar file (§15.2.5) |
| **golden file** | a sidecar file that holds the exact expected bytes of an output of the reference implementation (§15.4) |
| **deviation** | a difference between the IUT's diagnostic codes and the expected ones, in a test whose acceptance or rejection is correct (§15.3.4) |
| **portable** | applies to every IUT. A part of the suite that is not portable applies to the reference implementation only. |

### 15.1.2 Layers

| Layer | Location | Portable | Section |
|---|---|---|---|
| Conformance tests | `tests/conformance/` | yes, except the golden comparisons of §15.4 | §15.2 to §15.6 |
| Specification tests | generated from `docs/` into `tests/spec/` | yes, except the grammar check of §15.8.1 | §15.8 |
| Fuzzing | `fuzz/` | no | §15.7 |
| Differential tests | `tests/differential/` | no | §15.10 |
| Benchmarks | `bench/` | no | §15.11 |
| Compiler tests | the unit tests of each crate and `tests/impl/` | no | §15.12 |

Only the portable layers decide a conformance claim (§15.9). The other layers exist to find defects of the reference implementation before they reach a release.

### 15.1.3 Black-box interface (N-J-38)

1. The harness interacts with an IUT only through its command line, its NDJSON output in the format of [11](11-diagnostics.md) §11.9 with the result records of [13](13-cli.md) §13.8, and the artifacts it writes. It never calls the IUT's internal APIs. Only the parts of this chapter that are marked as applying to the reference implementation (the in-process checks of §15.6.4 and §15.6.6, and the layers of §15.1.2 that are not portable) use the reference implementation's crates directly, through the `sai_driver` API ([01](01-architecture.md) §1.11).
2. The commands used are `sai check`, `sai build`, `sai component build`, `sai test`, `sai fmt`, `sai inspect wit`, `sai explain --list` and `sai explain <CODE>`, with the syntax of [13](13-cli.md) §13.6 and `--format json`, and, for claims that include Stage 6, `sai lsp` ([13](13-cli.md) §13.6.21). An IUT whose command line differs is driven through an **adapter**: a program that accepts these command lines and produces the same records and artifacts. A claim that uses an adapter names it.
3. The harness executes built artifacts itself, in its own execution engine (§15.5). It does not use `sai run`, whose host provides clocks and randomness ([13](13-cli.md) §13.6.5). `sai run` is tested separately as a command of the reference implementation (§15.12.5).
4. `sai test` is the exception to item 3: test outcomes are defined by [spec/14](../spec/14-testing.md) and reported by the IUT's own `test` records (§15.2.4).

### 15.1.4 General requirements (N-J-39)

1. **Hermetic.** Each run of a test starts in a fresh temporary directory that contains a copy of the test's files and nothing else. The environment of the IUT is exactly `NO_COLOR=1`, `SAI_CACHE_DIR=<tmp>/cache` and `HOME=<tmp>/home`, with the variables of [13](13-cli.md) §13.7 otherwise unset, and the current directory is the test's root package. The only network service a test can reach is the Git fixture server of §15.2.7.
2. **Deterministic.** The result of a test is a function of the test and the IUT. The harness does not retry a test. A test whose result differs between two runs on the same IUT is a defect, either of the IUT (which violates [spec/01](../spec/01-introduction.md) §1.6) or of the test (which observes unspecified behavior, §15.5.3), and it is investigated as such.
3. **Bounded.** Execution is bounded by fuel, not by time (§15.5.1). A wall-clock limit of 600 seconds per test run exists only to stop a hung IUT. Exceeding it is the failure `hung`; it never makes a test pass.
4. **Independent.** Tests can run in any order and in parallel. The harness runs them on `--jobs` workers and reports them in the byte order of their paths, so that its report does not depend on scheduling.
5. **Report.** The harness writes one NDJSON record per test, `{"test":<path>,"configurations":[...],"outcome":<outcome>,"deviations":[...],"failures":[...]}`, in path order, followed by a summary record with the counts and the claim checked (§15.9.4). The outcome is `pass`, `pass-with-deviations`, `fail`, `staged` (§15.3.5), `not-run` (§15.2.7), or `not-applicable` (§15.9.3).

---

## 15.2 Suite structure and test format

### 15.2.1 Layout

```text
tests/
  conformance/
    spec-02-lexical-structure/      one directory per chapter, named after its file
    spec-03-grammar/
    ...
    spec-16-core-library/
    package-01-package-contract/
    ...
    package-04-distribution-and-security/
    impl-11-diagnostics/            the JSON wire format and the explain records
    impl-13-cli/                    exit codes, streams and result records
    impl-14-formatter/              the canonical form
    impl-12-semantic-tooling/       the semantic protocol (Stage 6)
  spec/                             generated by `cargo xtask spec-tests` (§15.8)
  fixtures/git/                     Git repository descriptions (§15.2.7)
  differential/                     §15.10
  impl/                             §15.12
fuzz/                               §15.7
bench/                              §15.11
```

1. A test belongs to the chapter that defines the rule it checks. Inside a chapter directory, subdirectories are free; their names carry no meaning.
2. Implementation chapters have a conformance directory only where conformance depends on them: the diagnostics format ([spec/01](../spec/01-introduction.md) §1.4.4 item 1), the formatter (item 2), `sai explain` (item 3), the semantic protocol (item 4), and the command line (for the reference implementation and adapters, §15.1.3).
3. The path of a test is its path relative to `tests/`, with `/` separators. It identifies the test in reports and in coverage (§15.9).

### 15.2.2 Single-file and directory tests

1. A **single-file test** is a file `<name>.sai`. The harness compiles it as the root module `src/lib.sai` of a package that it writes in the form of `sai new` ([13](13-cli.md) §13.6.1): identity `conformance:test`, version `0.1.0`, `compiler ^<L>` with the language version `L` of the claim, `source "src"`, and a `targets` declaration that lists the targets of the test's configurations (§15.2.6).
2. A **directory test** is a directory `<name>.t/` that contains a `sai.package`. That package is the root, and the directory may contain further packages (workspace members and path dependencies), lock files and WIT files. Its directives are the `//@` lines at the start of the root `sai.package` ([package/01](../package/01-package-contract.md) §1.2.2 allows `//` comments). Directory tests are used for everything that involves more than one source file, a hand-written `sai.package`, a lock, or a dependency.
3. **Canonical test files** (N-J-40). Every `.sai` file and every `sai.package` file of a test is in the canonical form of [14](14-formatter.md), except the inputs of `fmt` and `fmt-refuse` tests (§15.2.4) and files that a test requires to be lexically or syntactically ill-formed. The harness checks this before it runs the test with the reference formatter, and a test that is not canonical is a defect of the suite. Because the formatter never moves a trailing comment to another line ([14](14-formatter.md) §14.6.2), the line of every annotation (§15.3.1) is stable.
4. The harness copies a test into its temporary directory unchanged. It never rewrites a test file, so the byte offsets in the IUT's output refer to the file as it is checked in.

### 15.2.3 Directives

A directive is a line comment `//@ <name>: <value>` before the first token of the file (or of the root `sai.package` of a directory test). A `//@` comment after the first token, an unknown name, or a repeated name that is not repeatable is a defect of the test, which the harness reports without running it.

| Directive | Value | Default | Meaning |
|---|---|---|---|
| `kind` | a kind of §15.2.4 | required | what is run and what passes |
| `stage` | an integer from 0 to 7 | required | the highest stage of any feature the test uses (§15.9.3) |
| `spec` | a comma-separated list of references `<document> §<section>`, such as `spec/07 §7.4.2` | required | the sections whose rules the test checks (§15.9.2) |
| `targets` | target names separated by spaces | every target of §15.2.6 | restricts the targets of the test |
| `profiles` | profile names separated by spaces | `dev release tiny` | restricts the profiles of the test |
| `options` | options from the list below | none | passed to every command of the test |
| `component` | empty | absent | build with `sai component build` instead of `sai build` |
| `call` | `<export>(<value>, ...) -> <result>` | none | repeatable; an export call on a core target (§15.5.4) |
| `args` | arguments separated by spaces, each a JSON string | none | the arguments of a command component after its name (§15.5.4) |
| `seed` | a decimal integer | none | passed to `sai test --seed` |
| `diag` | `<severity> <CODE>`, optionally followed by `at <file>:<line>` | none | repeatable; an expectation for a diagnostic that cannot carry an annotation (§15.3.3) |
| `fuel` | a decimal integer | 10,000,000,000 | the fuel of each execution (§15.5.1) |
| `memory-limit` | a number of 64 KiB pages | 4096 | the largest memory an execution may grow to (§15.5.1) |
| `stderr-fails` | `first-write` | absent | the host's standard error stream fails on the first write (§15.5.4) |
| `reference-only` | the reason | absent | the test depends on implementation-defined behavior ([spec/01](../spec/01-introduction.md) §1.5.2), such as a limit above the minimum of §1.5.4, and is not portable |

The `options` directive accepts only `--tests`, `--deny-warnings`, `--allow <CODE>`, `--warn <CODE>`, `--deny <CODE>` ([13](13-cli.md) §13.4.4), `--wasm-feature <name>` (§13.4.7), and `--locked`, `--offline` and `--frozen` (§13.4.3). No other option can influence a result, and options that only affect presentation (`--color`, `--max-errors`, `--jobs`) are set by the harness itself (§15.6).

### 15.2.4 Kinds

| Kind | Commands, per configuration | The test passes if |
|---|---|---|
| `check-pass` | `sai check` | no error is reported, and the diagnostics match the expectations (§15.3) |
| `check-fail` | `sai check` | at least one error is reported, and the diagnostics match the expectations |
| `build-pass` | `sai build` (or `sai component build`) | the exit code is 0, the diagnostics match, and every output passes the validation of §15.5.2 |
| `build-fail` | `sai build` (or `sai component build`) | `sai check` reports no error, and the build reports the expected errors. Used for the diagnostics that only an artifact build produces, such as `NO_COMPONENT_EXPORTS` ([spec/13](../spec/13-components-and-wit.md) §13.5.3). |
| `run-pass` | `sai build`, then execution (§15.5) | the build passes as for `build-pass`, and the execution has the outcome `success` and matches the expectations of §15.5.4 |
| `run-fail` | as `run-pass` | a command component whose execution has the outcome `failure` (`main` returned a non-zero `ExitCode`) and matches the expectations |
| `run-panic` | as `run-pass` | the execution panics at the site of the `//~ panic` annotation (§15.3.6), after the expected output |
| `run-trap` | as `run-pass` | the execution traps without a panic in every profile, after the expected output, as for the boundary traps of [07](07-component-lowering.md) §7.13.2 |
| `test` | `sai test` | the outcome of every `test` and `property` declaration is the expected one (§15.3.7) |
| `fmt` | `sai fmt -` with the file on standard input | the output equals the sidecar `<name>.expected.sai` byte for byte, and formatting that file again yields it unchanged |
| `fmt-refuse` | `sai fmt -` | the formatter refuses the file and reports the expected lexical or syntax errors ([14](14-formatter.md) §14.2.2) |
| `protocol` | `sai lsp`, driven with the transcript `expected/session` of a directory test | every message that the server sends equals the next expected message of the transcript (§15.4.8) |

`sai.package` files are formatted with `--stdin-kind package` when the test is a `fmt` test of a file named `sai.package`. Every kind except `fmt` and `fmt-refuse` also runs `sai check` first when its main command is not `sai check`, and requires that check to report the same diagnostics as the main command, apart from the diagnostics that only the main command produces ([13](13-cli.md) §13.6).

### 15.2.5 Sidecar files

A single-file test `<name>.sai` may have sidecar files next to it, and a directory test has them in its subdirectory `expected/`:

| Single-file sidecar | Directory test file | Content | Portable |
|---|---|---|---|
| `<name>.expected.sai` | — | the formatter's output (`fmt` tests) | yes |
| `<name>.stdin` | `expected/stdin` | the standard input of a command component | yes |
| `<name>.stdout` | `expected/stdout` | the expected standard output of a command component, or the bytes written through the host module `sai-test` on a core target (§15.5.4) | yes |
| `<name>.stderr` | `expected/stderr` | the expected standard error, excluding the panic report | yes |
| `<name>.panic` | `expected/panic` | the delivered panic report (§15.4.6) | no |
| `<name>.json` | `expected/diagnostics.json` | the NDJSON diagnostics stream (§15.4.4) | no |
| `<name>.human` | `expected/diagnostics.human` | the rendered human output (§15.4.5) | no |
| `<name>.color-human` | `expected/diagnostics.color-human` | the rendered human output with colors (§15.4.5) | no |
| `<name>.fixed.sai` | `expected/fixed/` | the source files after `sai check --fix` (§15.4.4) | no |
| `<name>.wit/` | `expected/wit/` | generated WIT and the decoded world (§15.4.7) | yes |
| `<name>.mir/` | `expected/mir/` | MIR dumps (§15.4.2) | no |
| — | `expected/session` | the transcript of a `protocol` test (§15.4.8) | yes |

When an expected output depends on the configuration, a sidecar file can be specialized by inserting the target, the profile, or both before the extension: for `<name>.<ext>` the harness uses the first file that exists among `<name>.<target>.<profile>.<ext>`, `<name>.<profile>.<ext>`, `<name>.<target>.<ext>` and `<name>.<ext>`. A specialized sidecar never changes the outcome of a test, only bytes that are permitted to differ (for example, the absence of a panic report under `tiny`, D-TGT-02).

### 15.2.6 Configurations

1. A **configuration** is a target and a profile. A test runs once in each configuration that is applicable to it: the target and the profile are claimed (§15.9.3), they are permitted by its `targets` and `profiles` directives, and their stages are within the claim:

   | Target or profile | Stage |
   |---|---|
   | `wasm32-core`, `dev`, `release` | 1 |
   | `wasm64-core`, `tiny` | 2 |
   | `wasm32-wasi`, `wasm64-wasi` | 4 |

2. A claim whose highest stage is 0 has no target. Its `check-*` tests run once, without `--target` and `--profile`, and its other tests are not applicable.
3. In every command, the harness passes `--format json`, `--max-errors 0` and `--jobs 1`, and the `--target` and `--profile` of the configuration. The equivalence runs of §15.6 vary `--jobs` and the cache state. The human-renderer goldens (§15.4.5) are produced by one further run with `--format human` and `--color never`.
4. A test passes only if it passes in every applicable configuration. The report lists the failing configurations.

### 15.2.7 Git fixtures and lock files

1. **Repositories.** A directory `tests/fixtures/git/<host>/<path>/` describes one Git repository. It contains numbered directories `0001/`, `0002/`, ..., each the complete tree of one commit on the branch `main`, and a file `tags` whose lines are `<tag> <commit number>`. The harness builds the repository at the start of a run: each commit has the previous one as its parent, the author and committer `SAI Conformance <conformance@sai.test>`, and the time 1,700,000,000 plus the commit number in seconds, UTC. The object identifiers are therefore the same in every run and can appear in checked-in lock files.
2. **Server.** The harness serves every fixture repository at `https://<host>/<path>` with the Git smart HTTP protocol. Every `<host>` ends in `.sai.test`. The server's certificate is issued by the suite's test certificate authority (`tests/fixtures/ca/`). TLS verification cannot be disabled ([package/04](../package/04-distribution-and-security.md) §4.2), so tests that fetch run in the CI sandbox of the suite, where the test authority is in the host trust store and every `*.sai.test` name resolves to the fixture server. A run outside the sandbox reports these tests as `not-run`, and it is not a complete run for a claim (§15.9.4).
3. **Changed sources.** A test of a moved tag or a rewritten history does not change a fixture during the test. It checks in a `sai.lock` that records the old state, while the fixture has the new one.
4. **Locks.** A directory test that has a file `expected/sai.lock` requires that the lock written by the IUT equals it byte for byte. The lock format is canonical ([package/03](../package/03-lock-file.md)), so this comparison is portable. The test vector of [package/03](../package/03-lock-file.md) §3.4.7 is one such test.
5. `ssh` sources are tested only for what needs no connection (URL validation and normalization, [package/04](../package/04-distribution-and-security.md) §4.2.1). The `ssh` transport is covered by the compiler tests of §15.12.4.

---

## 15.3 Expected diagnostics

### 15.3.1 Annotations

An **annotation** is a line comment in a `.sai` or `sai.package` file of a test that states an expected diagnostic.

1. `//~ <severity> <CODE>` expects one diagnostic of that severity (`error`, `warning`, or `info`) and code whose primary location starts on the line of the comment. Several expectations on one line are separated by commas: `//~ error USE_AFTER_MOVE, warning UNUSED_VARIABLE`.
2. `//~^`, `//~^^`, and so on, with `k` carets, refers to the `k`-th line above the comment. An annotation on a line of its own MUST use carets, because the line of the comment has no code.
3. `// rejected: <CODE>` is equivalent to `//~ error <CODE>`, and `// lint: <CODE>` expects the lint `CODE` with the severity of its default level ([11](11-diagnostics.md) §11.3.3; the default level is taken from the IUT's `sai explain` record of the code). These are the forms that the specification's examples use ([spec/07](../spec/07-ownership-and-borrowing.md), [spec/15](../spec/15-attributes-and-lints.md)), so an example can become a test without rewriting (§15.8.3).
4. `//~ panic` (§15.3.6) and `//~ test-failed: <kind>` (§15.3.7) state execution expectations.
5. Line numbers are those of [11](11-diagnostics.md) §11.5.2, counted from 1 in the file as checked in (§15.2.2 item 4). A comment that starts with `//~` but does not have one of these forms is a defect of the test.

### 15.3.2 Compared diagnostics

1. The **actual diagnostics** of a command are the `diagnostic` records of its JSON stream ([11](11-diagnostics.md) §11.9.4), that is, the diagnostics after level resolution, the dependency cap, cascade suppression and deduplication (§11.3.3, §11.11). Lints at level `allow` are not in the stream and are never expected.
2. Each actual diagnostic is reduced to a tuple `(file, line, severity, code)`. `file` is the path of the primary location's file relative to the test's root directory, computed from the record's `path` and `package` ([11](11-diagnostics.md) §11.5.4); `line` is `primary.start.line`. A diagnostic with `primary: null` has the tuple `(null, null, severity, code)`.
3. The **expected diagnostics** are the tuples of all annotations and `diag` directives of the test. Both are multisets: two identical expected tuples require two actual diagnostics.

### 15.3.3 Expectations without annotations

1. `//@ diag: <severity> <CODE>` expects a diagnostic with `primary: null`, such as a resolution error that concerns the package graph as a whole.
2. `//@ diag: <severity> <CODE> at <file>:<line>` expects a diagnostic in a file that cannot carry an annotation: `sai.lock` (whose comments make it non-canonical, [package/03](../package/03-lock-file.md) §3.2), WIT files, and files that a test requires to contain no comment.

### 15.3.4 Matching (N-J-41)

Let `E` be the expected and `A` the actual multiset of a command (§15.3.2).

1. **Robustness.** If `A` contains `INTERNAL_COMPILER_ERROR`, or the IUT exits with 101, the test fails in every case ([spec/01](../spec/01-introduction.md) §1.4.2 item 8).
2. **Outcome.** Whether the program is accepted or rejected MUST be as the kind states: a `*-pass` kind, and the `sai check` of a `build-fail` test, require that `A` has no error; `check-fail` and `build-fail` require at least one error. A wrong outcome fails the test for every IUT ([spec/01](../spec/01-introduction.md) §1.4.2 items 2 and 3).
3. **Reference implementation.** `A` MUST equal `E`. Every missing or additional tuple fails the test.
4. **Other implementations.** The tuples are paired by equal `(file, line, severity, code)`. Every unpaired expected tuple is a deviation `missing`, and every unpaired actual tuple is a deviation `unexpected`. A test with a correct outcome and at least one deviation has the result `pass-with-deviations`, and its report lists the deviations. Deviations do not fail a claim, because reporting the named code is a SHOULD ([spec/01](../spec/01-introduction.md) §1.4.2 item 3), but they are part of the claim's report (§15.9.4).
5. Expectations are the same for every configuration of a test. Diagnostics whose presence depends on the configuration through a profile policy or a target capability ([spec/01](../spec/01-introduction.md) §1.4.1 item 3) are tested in separate tests with `targets` or `profiles` directives.

### 15.3.5 Staged features

A test whose stage is above the highest stage `k` of the claim is run in **staged mode**. The harness runs the commands of the test's kind (§15.2.4) in order, in one configuration, and stops after the first command that reports an error. The configuration is the first applicable configuration whose target and profile have stages at most `k` (§15.2.6), or, if there is none, the first configuration of the test, whose target or profile is then itself a staged feature ([spec/01](../spec/01-introduction.md) §1.4.5 item 1). The result is:

1. `staged` if a command reports an error `FEATURE_NOT_IMPLEMENTED` whose `stage` datum is greater than `k`;
2. otherwise, the result that the test has with its own expectations in that configuration (§15.3.4 and the kind), because an IUT that reports no staged feature has accepted every construct of the test and MUST implement all of their semantics ([spec/01](../spec/01-introduction.md) §1.4.5 item 3).

An `INTERNAL_COMPILER_ERROR` fails the test in both cases (§15.3.4 item 1). A compiler therefore cannot pass a claim by accepting part of a later stage: it either rejects the feature with the staged diagnostic or implements it fully. In particular, a compiler that accepts an ill-formed program of a later stage without reporting a staged feature, such as a use after move under a Stage 1 claim, fails that test. The target v0.1 reference-implementation claim covers Stages 0 to 4 (§15.9.5); once the planned conformance gate is implemented, it will check the tests of Stages 5 to 7 in this mode against that implementation.

### 15.3.6 Panic expectations

`//~ panic` marks the line of the expression whose evaluation panics: the panic site whose location the panic report gives ([spec/10](../spec/10-errors-and-panics.md) §10.4.5). A `run-panic` test has exactly one such annotation.

1. **Portable.** The execution traps. The output written before the trap equals the `stdout` and `stderr` sidecars. In a configuration with panic mode `Report` ([10](10-runtime.md) §10.5.3), exactly one panic report is delivered through the channel. In `TrapOnly` mode, in particular under `tiny`, nothing is delivered (D-TGT-02).
2. **Reference implementation.** In addition, the delivered bytes equal the `.panic` golden (§15.4.6), and the line in the report's location is the line of the annotation.

### 15.3.7 Test and property expectations

In a `test` test, `//~ test-failed: <kind>` on the first line of a `test` or `property` declaration expects that declaration to fail with the failure kind `panic`, `trap`, or `counterexample` of the `test` record ([13](13-cli.md) §13.6.6). Every other declaration MUST pass. Under `tiny`, an expected `panic` is satisfied by `trap` (a panic is a trap in that profile, D-TGT-02). The seed of every property comes from its `@seed` attribute or the `seed` directive, so the outcome is a function of the test ([spec/14](../spec/14-testing.md)). For the reference implementation, the counterexample text and the case counts are compared through the JSON golden of the `test` records (§15.4.4).

### 15.3.8 Spans, data and fixes

The portable matching uses lines, not byte spans, because the language chapters define the primary location of a diagnostic as a construct ("reported at the parameter", "at the call"), and two conforming compilers may delimit the same construct differently by a token. The exact spans, labels, data, related entries and fixes of the reference implementation are compared through the JSON goldens (§15.4.4). Two properties are checked for every IUT and every diagnostic:

1. every diagnostic record validates against the JSON Schema of [11](11-diagnostics.md) §11.9.5, every result record against the schema of its kind in [13](13-cli.md) §13.8, and the stream has the shape of 11 §11.9.1;
2. every code is registered: `sai explain --list` contains it (§15.8.4).

---

## 15.4 Golden tests

### 15.4.1 Golden files and blessing (N-J-42)

1. A golden file holds the exact bytes that the reference implementation produces. It is compared byte for byte, with no normalization other than the one stated for its kind below.
2. Golden files are written only by `cargo xtask bless <test path>...`, which runs the named tests and overwrites their goldens with the actual output. A changed golden file is part of the change that caused it and is reviewed with it. The harness never creates or updates a golden file during a normal run: a missing golden file is a failure.
3. Golden comparisons are not portable (§15.1.2), with two exceptions whose text the specification defines: the formatter outputs (§15.4.3), which every conforming formatter produces byte for byte ([spec/01](../spec/01-introduction.md) §1.4.4 item 2), and the WIT texts (§15.4.7). These are called expected files rather than golden files, but they are blessed and reviewed in the same way.

### 15.4.2 MIR

A test whose sidecar directory `<name>.mir/` (or `expected/mir/`) exists contains one file `<function>.<phase>.mir` for each function and MIR phase to dump. The harness runs `sai inspect mir <path> --phase <phase> --format json` ([13](13-cli.md) §13.6.15) and compares the `text` of each body, in the order of the `bodies` array, separated by one empty line, with the file.

- The MIR text format and the phase names belong to [04](04-mir.md). The format is unstable ([13](13-cli.md) §13.6.15), so a change of it re-blesses every MIR golden in the same change.
- MIR goldens exist for the lowering of every construct of [04](04-mir.md) that has a phase of its own, for drop elaboration, for monomorphization of generic functions, and for the optimizations of [09](09-optimization-and-size.md) whose result is visible in MIR. They run in one configuration, `wasm32-core` and `dev`, unless the dump depends on the target or the profile.

### 15.4.3 Formatter

The formatter tests of `tests/conformance/impl-14-formatter/` implement the conformance suite of [14](14-formatter.md) §14.11.3 item 2. They are portable.

1. **Rules.** For every rule of [14](14-formatter.md) §14.4 to §14.10 and §14.12, at least one `fmt` test whose input violates the rule and whose expected output applies it. The `spec` directive names the rule's section.
2. **Fixed point.** Every expected output, formatted again, is unchanged (part of the `fmt` kind, §15.2.4). The same check runs on every canonical test file of the whole suite (§15.2.2 item 3), so each of them is a fixed-point test too.
3. **Layout independence (F6).** For every `fmt` input, the harness derives 16 variants: it re-lays out the input at random safe positions ([14](14-formatter.md) §14.5.1) and with random runs of spaces, without changing comments or blank lines, using a pseudo-random generator seeded with the BLAKE3 hash of the test path. Each variant MUST format to the expected output. The harness finds the safe positions with the reference lexer; the variants are inputs to the IUT like any other.
4. **Refusal and `--check`.** `fmt-refuse` tests cover every condition of [14](14-formatter.md) §14.2.2. Every `fmt` test also runs `sai fmt --check` on its input and on its expected output, each as the only file of a package: the expected output MUST produce no diagnostic, and an input that differs from it MUST produce exactly one `UNFORMATTED_FILE` whose primary line and `line` datum are the first differing line (§14.11.4).
5. **Specification blocks.** Every `sai` and `sai-package` block of the specification that is not rejected is a fixed point (§15.8.3).
6. A verification failure of the formatter is `INTERNAL_COMPILER_ERROR` ([14](14-formatter.md) §14.11.2), so it fails a test under §15.3.4 item 1. The reference implementation's tests that inject such a failure are compiler tests (§15.12.3).

### 15.4.4 Diagnostics JSON

A test with a JSON golden compares the complete NDJSON stream of its main command (§15.2.4) in the first applicable configuration, unless the golden is specialized (§15.2.5).

1. Two members are normalized before the comparison: the `tool` member of the header record is replaced by `{"name":"sai","version":"<version>"}`, so that a version change does not re-bless every golden, and every `duration_us` value of a `test` record is replaced by 0, because it is the only nondeterministic value of a stream ([13](13-cli.md) §13.6.6). All other bytes are compared, including the order of members and of records ([11](11-diagnostics.md) §11.9.2, §11.11.1).
2. Paths in the stream are relative to their package ([11](11-diagnostics.md) §11.5.4). The temporary directory of the test MUST NOT appear in it; if it does, the test fails ([01](01-architecture.md) §1.7.1).
3. Every hard diagnostic code has at least one JSON golden, so that its primary span, labels, data, related entries and fixes are fixed by a test (§15.9.2). Codes with code-specific data ([11](11-diagnostics.md) §11.4.5) have a golden for every shape of their data.
4. **Fixes.** A test with a `.fixed.sai` sidecar (or `expected/fixed/`) also runs `sai check --fix`. The changed files MUST equal the sidecars byte for byte, MUST be canonical, and a following `sai check` MUST NOT report the fixed diagnostics again ([11](11-diagnostics.md) §11.7.5).

### 15.4.5 Human renderer

A test with a `.human` golden runs its main command once more with `--format human` and `--color never`, and compares standard error byte for byte with the golden ([11](11-diagnostics.md) §11.10.1). Nothing is normalized: the human output contains no version, no time and no absolute path, and the renderer's layout depends on no terminal width ([11](11-diagnostics.md) §11.10.4).

- The tests of the renderer cover every rule of [11](11-diagnostics.md) §11.10: line number widths, TAB expansion, the `<U+XXXX>` replacements, wide and zero-width characters, empty and end-of-line spans, the 140-column window, multi-line spans, children and suggestions, and the summary line.
- A small set of tests runs with `--color always` instead, and compares with a `.color-human` golden, to fix the escape sequences of [11](11-diagnostics.md) §11.10.7.
- The JSON and human goldens of one test are produced by the same command line, so they describe the same diagnostics.

### 15.4.6 Panic reports

For a `run-panic` test with a `.panic` golden, the harness concatenates the bytes delivered through the panic channel ([10](10-runtime.md) §10.5.4): the bytes passed to the panic hook on a core target, or the bytes written by the runtime's standard error channel on a WASI target, which the harness separates from the program's own writes because it records the write calls of each delivery. The concatenation equals the golden in `dev` and in `release`. In `TrapOnly` configurations nothing is delivered (§15.3.6).

These tests implement [10](10-runtime.md) §10.11 item 6: one test per panic kind of [spec/10](../spec/10-errors-and-panics.md) §10.4.4 and per channel, the message lengths 1023, 1024, 1025, 70,000 and 300,000 bytes, the three writes of a 10,000-byte report, and a host stream that fails on the first write, which the execution engine simulates (§15.5.4).

### 15.4.7 WIT and components

For a test with a `.wit/` sidecar, the harness compares two texts with the files of the sidecar ([07](07-component-lowering.md) §7.13.2, "Golden WIT"):

1. `generated.wit`: the `text` of `sai inspect wit --format json` ([13](13-cli.md) §13.6.15), the WIT package generated from the root package in the text format of [spec/13](../spec/13-components-and-wit.md) §13.11.2;
2. `component.wit`: the world that the harness's WIT decoder (the `wit-component` crate, D-TOOL-05) prints from the built component.

Both texts are defined by the specification (the generated text by spec/13 §13.11, the component's types by the type mapping of §13.9), so these comparisons are portable. The round trip of [spec/13](../spec/13-components-and-wit.md) §13.11.6 (the SAI view of the generated WIT equals the interface after normalization) compares internal views and is a compiler test of the reference implementation (§15.12.2), run on every interface of the suite.

### 15.4.8 Protocol transcripts (Stage 6)

A `protocol` test drives `sai lsp` ([13](13-cli.md) §13.6.21) over its standard input and output with the LSP base protocol framing. Its transcript `expected/session` has one JSON object per line:

1. `{"send": <message>}` sends a JSON-RPC message to the server. The harness replaces the string `"<root>"` anywhere in the message by the `file:` URI of the test's temporary root directory.
2. `{"expect": <message>}` reads the next message from the server and requires it to equal `<message>` as a JSON value, after the normalization of §15.4.4 and after `"<root>"` is replaced as in item 1. Object members compare without regard to their order; arrays compare in order.
3. `{"edit": {"file": <path>, "text": <string>}}` replaces a file of the test's temporary directory, so that a request can observe a change made outside the client.

Messages of the methods `window/logMessage`, `window/showMessage` and `$/progress` are skipped, because their content is not specified. The transcript ends with `shutdown` and `exit`, and the test passes if every `expect` line matched and the server exited with code 0. Transcripts are expectations written by hand, not blessed; they are portable for the methods and records of [12](12-semantic-tooling.md) §12.5 and for the LSP messages whose content [12](12-semantic-tooling.md) defines.

---

## 15.5 Deterministic Wasm execution

### 15.5.1 The execution engine (N-J-43)

The harness executes artifacts with `wasmtime` (D-TOOL-05), in a version pinned by the harness's `Cargo.lock` and named in every report. The engine is configured identically for every execution:

| Setting | Value | Reason |
|---|---|---|
| Wasm features | exactly those of the configuration's target ([06](06-wasm-lowering-and-layout.md) §6.2.1) plus the test's `--wasm-feature` options | an artifact that uses any other feature fails validation (§15.5.2) |
| NaN canonicalization | on | NaN bit patterns are unspecified ([spec/01](../spec/01-introduction.md) §1.5.3); canonicalization makes the engine produce one pattern on every host, so that an accidental dependence on NaN bits is at least reproducible |
| Fuel | the `fuel` directive (default 10,000,000,000 units) per execution | running out is the failure `out-of-fuel`. Time never decides an outcome (§15.1.4 item 3). |
| Memory | at most `memory-limit` pages (default 4096, that is 256 MiB) | `memory.grow` beyond the limit returns −1, which the runtime reports as out of memory ([10](10-runtime.md) §10.2.6) |
| Engine stack | 8 MiB | far above what any test needs. A test that reaches it observes a host-dependent depth and is a defect (§15.5.3). |
| Clocks, randomness, interruption | see §15.5.4 | no ambient input |
| Threads, relaxed SIMD | disabled | not used by v0.1 ([06](06-wasm-lowering-and-layout.md) §6.2.1) |

The harness runs on `x86_64` and `aarch64` hosts, and its results are the same on both (§15.12.6).

### 15.5.2 Validation

Before it executes anything, and for every `build-pass` test, the harness validates each output of a build:

1. A core module validates with `wasmparser` under exactly the features of §15.5.1. Its exports are the `export fn` items of the root package and `memory` ([spec/13](../spec/13-components-and-wit.md) §13.6.2, [10](10-runtime.md) §10.5.4).
2. A component validates under the Component Model, and its imports and exports are those of the export world `W'` ([spec/13](../spec/13-components-and-wit.md) §13.5.2, §13.5.3). A command exports `wasi:cli/run`.
3. A failed validation fails the test, whatever its kind expects afterwards.

### 15.5.3 Unspecified and implementation-defined behavior

A portable test MUST NOT observe behavior that [spec/01](../spec/01-introduction.md) §1.5.3 leaves unspecified, and it MUST NOT depend on implementation-defined behavior (§1.5.2) unless it is `reference-only`:

1. **NaN bits.** A test may check that a value is a NaN, but it never prints, compares, or transmutes the bits of a NaN that arithmetic produced. NaN constants written in the program are not exempt, because constant folding by a compiler happens outside the engine.
2. **Addresses and padding.** A test never prints an address, converts a pointer to an integer for output, or reads padding bytes. The allocator model tests of [10](10-runtime.md) §10.11 item 1, which compare addresses, are compiler tests (§15.12.2).
3. **Layout and sizes.** A portable test never depends on the size or alignment of a type other than those that the specification fixes, on the shadow stack size, or on the memory size at instantiation.
4. **Imports.** A portable test never depends on the order of imports in a module.
5. **Messages.** Message texts and the framing of panic reports are compared only by goldens, which are not portable (§15.4.1).
6. **Task interleaving** (Stage 5). A test of async code observes only results that the specification fixes for every interleaving ([spec/11](../spec/11-async-and-concurrency.md)).

The perturbation runs of §15.6.5 detect most violations of items 2 and 3 in the reference implementation's runs.

### 15.5.4 Execution and its expectations

**Core modules.** The harness instantiates the module once per test and configuration.

1. Its imports are satisfied by the harness's host module `sai-test`: `write(ptr, len)` appends `len` bytes at `ptr` of the module's memory to the captured standard output; `write-i64(v)` appends the decimal text of the signed value `v` and a line feed; `panic-hook(ptr, len)` is the panic hook ([spec/10](../spec/10-errors-and-panics.md) §10.6.2), whose bytes are captured as the panic report. The pointer and length parameters have the type `i32` on `wasm32-core` and `i64` on `wasm64-core`. A test declares them in an `extern "wasm" module "sai-test"` block, with `@import_name` for the hyphenated names ([spec/12](../spec/12-unsafe-and-ffi.md) §12.7.1); the address passed to `write` is a `USize`, so tests that use it are of Stage 2 at least. An import that the module of a passing test has and the host module does not provide is a test failure.
2. Each `call` directive is executed in order on the same instance ([10](10-runtime.md) §10.7.3). An argument or result is written `i32:<n>`, `i64:<n>`, `f32:<bits>` or `f64:<bits>`, with `n` a decimal integer and `bits` the IEEE 754 bit pattern in hex (`f64:0x3ff0000000000000`), and a result can be `trap`. A result of several values is a parenthesized list. A `call` after one whose result is `trap` is a defect of the test, because a trapped instance cannot be called again ([10](10-runtime.md) §10.7.4).
3. The outcome is `success` if every call returns its expected result, and `trapped` if the last call traps as expected.

**Command components.** The harness instantiates the component with a deterministic implementation of the WASI 0.2 interfaces of the target ([spec/13](../spec/13-components-and-wit.md) §13.6.1) and calls `wasi:cli/run`.

1. The arguments are the name part of the root package's identity (`test` for a single-file test) followed by the `args` directive, as under `sai run`. The environment is empty. Standard input is the `stdin` sidecar, or empty. Standard output and standard error are captured.
2. The wall clock reads 1,700,000,000 seconds after the epoch at the first read and advances by 1 ms per read; the monotonic clock starts at 0 and advances by 1 µs per read. `wasi:random` produces the ChaCha20 stream seeded with the BLAKE3 hash of the test path and the configuration. Network operations fail with `access-denied`, as under `sai run` without `--allow-network` ([13](13-cli.md) §13.6.5). A directory test's directory `preopen/` is copied and preopened as `/`; otherwise there is no preopened directory.
3. The outcome is `success` if `run` returns `ok`, `failure` if it returns `err`, and `trapped` if the component traps. The captured outputs are compared with the `stdout` and `stderr` sidecars byte for byte (an absent sidecar means empty output), with the bytes of the panic channel separated out (§15.4.6).
4. With the directive `stderr-fails: first-write`, the host's standard error stream returns an error on the first write, to test the failure path of [10](10-runtime.md) §10.5.4.

**Reactor components.** A `call` directive names an exported function as `<interface>#<function>` (for example `acme:image/codec#encode`), and its arguments and result are written in the WebAssembly Value Encoding (WAVE) of the `wasm-wave` crate. Calls run in order on one instance, with the host of the previous paragraph.

---

## 15.6 Equivalence tests (N-J-44)

An equivalence test runs one test in two or more settings that MUST NOT change its observable result, and compares the results with each other rather than with an expectation. This catches differences that no expectation names, such as an output that a test does not check.

### 15.6.1 Profiles

Profiles never change semantics (C-03; [spec/01](../spec/01-introduction.md) §1.4.2 item 4). For every `run-*` and `test` test that is applicable in more than one profile, the harness compares, across `dev`, `release` and `tiny`:

1. the outcome (`success`, `failure` or `trapped`), where a panic in `dev` or `release` corresponds to a trap in `tiny` (D-TGT-02);
2. the result of every `call`, including the call that traps;
3. the program's standard output and standard error, without the panic channel (§15.4.6);
4. the sequence of host calls (WASI and `sai-test` functions) with their arguments, except the addresses of memory;
5. for `test` tests, the outcome of every declaration, with `panic` and `trap` equivalent as in item 1.

What may differ is listed by D-TGT-02: the panic report, debug information and the name section, the allocator, code size, speed, and fuel consumption. For `check-*` tests, the diagnostics are compared too, so that a profile cannot change which checks are performed. The rules that do depend on the profile (profile policies, [spec/01](../spec/01-introduction.md) §1.4.1 item 3) are tested by tests restricted with `profiles` directives (§15.3.4 item 5). These comparisons are portable.

### 15.6.2 Targets

For a test applicable on several targets, the outcome, the call results and the outputs MUST be equal on `wasm32-core` and `wasm64-core`, and on `wasm32-wasi` and `wasm64-wasi`, unless a sidecar file is specialized for a target (§15.2.5). A target specialization is permitted only for values that the specification makes target-dependent: `USize` and `ISize` widths and the values derived from them. Between core and WASI targets, only the tests that run on both are compared: the `check-*` tests and the `run-*` tests whose results do not depend on the host.

### 15.6.3 Parallelism and directories

1. Every test runs with `--jobs 1` and with `--jobs 8`. The normalized JSON streams (§15.4.4), the human output, the written lock files and the artifacts MUST be byte-identical ([spec/01](../spec/01-introduction.md) §1.6; [01](01-architecture.md) §1.8.3). An IUT without parallel compilation passes this check trivially.
2. Every test also runs from a second temporary directory whose path differs in length and contains a space and a non-ASCII character. The outputs MUST again be byte-identical ([13](13-cli.md) §13.5.3; [07](07-component-lowering.md) §7.13.2, "Determinism").
3. Both checks are portable.

### 15.6.4 Caches and incremental compilation (reference implementation)

The equivalence invariant ([01](01-architecture.md) §1.6.1, N-J-31) is tested in three ways:

1. **Cold and warm.** Every test runs once with an empty `.sai/cache/` (cold) and once more with the cache that the first run wrote (warm). The outputs MUST be byte-identical.
2. **Incremental.** The tests of `tests/impl/incremental/` each consist of a directory test and a file `edits` with a sequence of edit steps (a file path, a byte range and replacement text). After each step, the harness runs the command with the cache of the previous step and compares the outputs with those of a cold run of the same state. Each step also states the queries whose execution counts the step may change, by query name and key ([01](01-architecture.md) §1.6.2). The harness counts executions with a subscriber installed through `SessionBuilder::tracing_subscriber` ([01](01-architecture.md) §1.11) and requires that no other query is executed. The expected lists follow from the dependencies of the queries and from backdating ([01](01-architecture.md) §1.4.3): for example, a step that edits a function body in another file than the one under test leaves every query of the file under test unexecuted unless a signature that it uses changes. A change of the query structure updates these lists in the same change, like a golden file (§15.4.1).
3. **Damaged caches.** The harness truncates, corrupts and replaces the files of `.sai/cache/query/` and `.sai/cache/deps/` between two runs. The second run MUST produce the outputs of a cold run, without a diagnostic ([01](01-architecture.md) §1.6.3, §1.6.5 rule 8).

### 15.6.5 Execution perturbation

Every portable `run-*` test without a `fuel` or `memory-limit` directive runs a second time with twice the fuel, twice the memory limit and twice the engine stack. The results MUST be equal. A test whose result changes depends on a limit, which is implementation-defined, or on unspecified behavior (§15.5.3), and is a defect of the test.

### 15.6.6 Metadata and source (reference implementation)

1. **Dependencies.** Every multi-package test is compiled twice in-process: once with every dependency computed from source, and once with every dependency decoded from its metadata ([01](01-architecture.md) §1.6.5). For each package, the fingerprints ([01](01-architecture.md) §1.5.4) of the values of every query that the metadata contains MUST be equal in both sessions, and the outputs MUST be byte-identical.
2. **Library.** The whole suite runs with the release compiler and with the bootstrap compiler of `cargo xtask dist` ([01](01-architecture.md) §1.12.2), which compiles `sai:core` and `sai:alloc` from their embedded source. The outputs MUST be byte-identical ([01](01-architecture.md) §1.12.4 item 3).

---

## 15.7 Fuzzing (reference implementation)

### 15.7.1 Targets

The fuzz targets live in `fuzz/` and are built with `cargo fuzz` (libFuzzer through `libfuzzer-sys`), structure-aware where noted (the `arbitrary` crate). Each target has an oracle; any panic, internal compiler error, abort, native stack overflow or hang of the compiler code under test is a finding.

| Target | Input | Oracle |
|---|---|---|
| `lex` | arbitrary bytes | the lexer is total; the tokens and trivia cover the input exactly and in order; two runs give the same tokens and diagnostics ([02](02-frontend-ast-hir.md) §2.4) |
| `parse` | arbitrary bytes, and token sequences mutated from the corpus | the parser is total; the text of the syntax tree equals the input (lossless tree, [02](02-frontend-ast-hir.md) §2.6); every error has a parser or lexer code ([02](02-frontend-ast-hir.md) §2.5.12) |
| `nesting` | generated nesting of every recursive construct, at depths around the limits | below the limits no `IMPLEMENTATION_LIMIT_EXCEEDED`; above them exactly that error, and never a native stack overflow ([01](01-architecture.md) §1.8.1, §1.10.4) |
| `grammar` | files generated from the grammar of [spec/03](../spec/03-grammar.md) (§15.8.1) | no lexical or syntax error |
| `typeck` | well-typed programs from the type-directed generator of §15.10.2, and their mutations | the generated programs are accepted; mutations are accepted or rejected without an internal compiler error |
| `fmt` | arbitrary bytes and the corpus | a refused input produces no output; otherwise verification never fails ([14](14-formatter.md) §14.11.1), and the output is a fixed point |
| `package` | arbitrary bytes, and `sai.package` files mutated from the corpus | the contract parser is total; a valid contract formats to a fixed point and parses back to the same contract |
| `lock` | arbitrary bytes, and locks mutated from the corpus | the lock parser is total; printing a parsed lock and parsing it again gives the same lock, and the printed form is canonical ([package/03](../package/03-lock-file.md) §3.2.3) |
| `wit` | WIT texts that `wit-parser` accepts | importing them ([spec/13](../spec/13-components-and-wit.md) §13.7) never causes an internal compiler error |
| `cache` | arbitrary bytes as files of `.sai/cache/` | every file is discarded silently or decoded to values equal to the computed ones; the archive reader never reads out of bounds ([01](01-architecture.md) §1.6.3, §1.6.4) |
| `edits` | a program and a sequence of random edits | after each edit, the output of a warm session equals that of a cold session (§15.6.4) |

The `cache` and `lex` targets also run in builds with AddressSanitizer, because they reach the unsafe code of `sai_intern` and of the archive reader ([01](01-architecture.md) §1.1.2).

### 15.7.2 Corpus, budget and findings (N-J-45)

1. The corpus of each target is seeded with every test file of the suite, every block extracted from the specification (§15.8.3), and every minimized finding. It is stored in `fuzz/corpus/<target>/`.
2. Fuzzing is not a merge gate, because its results depend on time and chance. It runs continuously on dedicated machines, with at least one CPU hour per target and day, and before every release, for at least 24 CPU hours per target.
3. The merge gate runs every corpus input of every target once (corpus replay). This part is deterministic.
4. A finding is minimized and added as a regression test: to `tests/impl/fuzz/<target>/` when it concerns the reference implementation only, and to `tests/conformance/` when it shows a rule that the suite did not cover. The fix of a finding is not merged without its regression test.

---

## 15.8 Specification tests (N-J-46)

`cargo xtask spec-tests` reads the documents of `docs/` and generates the tests of `tests/spec/`. The generated tests are checked in, and the merge gate regenerates them and requires that nothing changes, so that the specification and the suite cannot drift apart.

### 15.8.1 Grammar

The grammar description `sai.ungram`, from which the syntax tree views are generated ([02](02-frontend-ast-hir.md) §2.7.1), is checked against [spec/03](../spec/03-grammar.md):

1. **Extraction.** The rules of all `ebnf` blocks of spec/03 form the grammar `G`, in the notation of spec/03 §3.1.1. `G` is checked first: every non-terminal is defined exactly once, every rule is reachable from the file rule (spec/03 §3.2), every quoted terminal is a keyword or punctuator of [spec/02](../spec/02-lexical-structure.md), and every token kind is one of spec/02 §2.2.
2. **Mapping.** A mapping table checked in next to `sai.ungram` assigns every rule of `G` a node kind of `sai.ungram`, or marks it `inline` (a rule that produces no node of its own, such as `Sep` or `nl`). Every rule of `G` is mapped, and every node kind of `sai.ungram` is the image of at least one rule or is marked `error` (the error nodes of recovery, [02](02-frontend-ast-hir.md) §2.5.11). The token kinds of `G` and of `sai.ungram` are the same set.
3. **Positive sentences.** The harness generates files from `G`: for every alternative of every rule at least one sentence that uses it, and further random sentences, up to a depth of 12, with a seeded generator. Sentences that violate one of the restrictions that spec/03 states in prose next to its rules, which are checked while parsing with the diagnostics of spec/03 §3.16, are excluded by a list in the mapping table. Each sentence MUST parse without a lexical or syntax error, and its syntax tree MUST conform to `sai.ungram` and contain the node kind of every mapped rule that the derivation used.
4. **Negative sentences.** Each positive sentence is mutated by deleting, inserting or exchanging a token. A recognizer for `G` in the harness decides whether the mutated sentence is in the language. The parser MUST report a syntax error exactly for the mutations that `G` rejects.
5. **Portability.** Items 3 and 4 have a portable form: the formatter refuses a file if and only if it has a lexical or syntax error ([14](14-formatter.md) §14.2.2), so every IUT's `sai fmt -` MUST accept the positive sentences and refuse the rejected mutations. The comparison with `sai.ungram` (items 2 and 3) applies to the reference implementation only.

### 15.8.2 Library signatures and lang items

The listings of [spec/16](../spec/16-core-library.md) are read in the form of spec/16 §16.1.2: trait and public type declarations from `sai` blocks, inherent functions from the *Signature*, *Effects* and *Panics* columns of the tables, with the schemas of spec/16 §16.1.2 item 7 expanded by a table of placeholders in the generator (a placeholder without an entry is a defect of the generator).

1. **Signature probes** (portable). For each listed function `f` with the declared head `H`, the generator writes a probe module with one function that has the parameter types of `H`, the generic parameters and `where` clause of `H`, and the listed effect set, calls `f` with its parameters (with `borrow` where `H` takes a borrow), and binds the result to a `let` with the result type of `H`. The probe MUST be accepted. For a non-empty effect set, a second probe declares the set without its first atom and MUST be rejected with `EFFECT_NOT_DECLARED` ([spec/08](../spec/08-effects-and-capabilities.md) §8.9.1).
2. **Trait probes** (portable). For each listed trait that is not sealed, a probe implements it for a fresh record type, with the required items declared exactly as listed, and MUST be accepted. For each sealed trait (§16.3.5), the same probe MUST be rejected with `SEALED_TRAIT_IMPL`. Public record types are constructed in a probe with all their listed fields.
3. **Lang items** (portable). For each row of spec/16 §16.3.2 to §16.3.4 whose stage the claim includes, a probe names the item by its canonical path in a position that requires the listed kind (a type in a type position, a trait in a bound, a function in a call). Lang items that are package-private (`Arena.region_new`) are not probed.
4. **Staged items** (portable). For each library item of a stage above the claim, a probe that uses it MUST be rejected with `FEATURE_NOT_IMPLEMENTED` whose `stage` is the item's stage and whose `feature` is its canonical path ([spec/01](../spec/01-introduction.md) §1.4.5, [spec/16](../spec/16-core-library.md) §16.1.6).
5. **Exhaustiveness** (reference implementation). The harness parses the library source of `library/core/` and `library/alloc/` with the reference parser and requires that its `pub` items are exactly the listed items of the stages the toolchain implements, with the listed signatures and effects, and that `library/staged-items.txt` lists exactly the items of the other stages ([01](01-architecture.md) §1.12.1 item 4). This is the check that [spec/16](../spec/16-core-library.md) §16.3.1 item 4 and [01](01-architecture.md) §1.12.4 refer to.

### 15.8.3 Examples in the specification

Every fenced block with the info string `sai`, `sai-package` or `sai-lock` in `docs/spec/`, `docs/package/` and `docs/impl/` becomes a test `tests/spec/<document>/<section>-<k>.sai`, where `<k>` numbers the blocks of the section from 1. The documents of `docs/design/` are excluded: they record decisions, and their examples are restated by the chapters that own them.

1. **Expectations.** A line with `// rejected: CODE` expects that error on that line, and a line with `// lint: CODE` expects that lint (§15.3.1 item 3). A block with a `rejected` line is a `check-fail` test; any other block is a `check-pass` test. Errors are compared exactly in every chapter: no unmarked line produces an error ([spec/07](../spec/07-ownership-and-borrowing.md) §7.1.4). Warnings and infos on unmarked lines fail the test in a chapter whose example convention states that unmarked lines produce no diagnostic ([spec/15](../spec/15-attributes-and-lints.md) §15.1.2), except `UNUSED_ITEM` outside the examples that mark it; in the other chapters they are listed in the report and do not fail the test, because those conventions constrain errors only.
2. **Contexts.** A block that cannot be compiled on its own (it refers to declarations shown elsewhere, is a sequence of statements, or is one file of a larger package) has a context directory `tests/spec-contexts/<document>/<section>-<k>/`, which holds the other files of the package, a `sai.package` if the default one does not fit, dependency packages, and optionally a file `wrap.sai` in which a line `//@ block` marks where the block is inserted, indented like that line. The harness maps the lines of the composed file back to the block, so that annotations and reports refer to the block.
3. **Stage.** The test's stage is the stage that the context's file `directives` states, or, if there is none, the single stage of the chapter's **Stage** line. A Stage line that names several stages assigns them to features (for example [05](05-borrow-checker.md): "2 (borrows held across `.await`: Stage 5)"). A test then belongs to the highest stage among the features that its block uses, and its `directives` file states that stage. A block of a chapter with several stages and without a stated stage is a defect of the suite.
4. **Fixed points.** Every block without a `rejected` line is a fixed point of the formatter (with `--stdin-kind package` for `sai-package` blocks; a wrapped block is a fixed point if the composed file is) ([14](14-formatter.md) §14.11.3 item 2). A block whose info string has the word `noncanonical` after its kind (```` ```sai-package noncanonical ````) is a test of its kind but is exempt from this check: it marks a formatter input ([14](14-formatter.md) §14.7.2, §14.12.2) or an accepted layout that is not canonical ([package/01](../package/01-package-contract.md) §1.2.6). `sai-lock` blocks without a `rejected` line MUST be canonical locks ([package/03](../package/03-lock-file.md) §3.2.3).
5. **Failures.** A block that does not behave as its markers state is a defect of the specification or of the IUT. The harness reports it with the block's document and line, and the merge gate of the reference implementation requires every generated test to pass. Fixing a block is a change of its chapter; the suite never carries a private corrected copy.

### 15.8.4 `sai explain` records

For every code in the IUT's `sai explain --list`, the harness reads `sai explain <CODE> --format json` ([11](11-diagnostics.md) §11.13.2) and checks the record:

1. The Markdown has the sections of §11.13.2 in order, and its heading is the code.
2. **Example.** The `sai` block of `## Example` is compiled as a single-file test with the default `sai.package` (§15.2.2), first with `sai check` and, if that does not report the code, with `sai build`. The code MUST be reported at least once ([11](11-diagnostics.md) §11.13.2 item 4). The line on which it is reported is not compared, because §11.13.2 does not fix where the block's marker comment stands.
3. **Fix.** The `sai` block of `## Fix`, compiled with `sai check`, MUST produce no diagnostic of any severity.
4. Both blocks MUST be formatter fixed points. Blocks with the info string `text` (codes about manifests, locks and the command line, §11.13.2 item 5) are not compiled; for the reference implementation, these codes must be expected by at least one other test (§15.9.2).
5. The record's `stage` decides applicability as the `stage` directive does: the blocks of a code of a stage above the claim are checked in the staged mode of §15.3.5.
6. Every code that the IUT emitted anywhere during the run of the suite MUST be in its list ([spec/01](../spec/01-introduction.md) §1.4.4 item 3).

### 15.8.5 Diagnostic tables and the registry

The harness reads the diagnostics table of every chapter of `docs/spec/`, `docs/package/` and `docs/impl/` (the tables with the columns `Code`, `Severity` and `Description`) and compares them with the registry of the IUT, that is, `sai explain --list` together with each record's `class`, `severity`, `default_level`, `stage` and `owner` ([11](11-diagnostics.md) §11.2.4).

1. Every code in a table of a chapter whose stage the claim includes is in the registry, and every code of the registry is in at least one table.
2. The severity of every table row equals the registry's severity, or for a lint its default level.
3. The registry's `owner` of a code is one of the documents whose tables list it. The chapters mark reused codes in different words ("defined in", "owned by", or the defining document's name at the start of the description), so the harness does not infer ownership from the wording; for the reference implementation, the owner is compared with a mapping file `tests/conformance/owners.txt`, which a reviewer updates together with the chapter that defines a code.
4. Every code of the registry is expected by at least one test of the suite other than the checks of §15.8.4 (§15.9.2).

For the reference implementation, a difference under any item fails the check. For another IUT, a difference under items 1 and 2 is a deviation (§15.3.4), because codes are recommended rather than required ([spec/01](../spec/01-introduction.md) §1.4.2 item 3), and items 3 and 4 do not apply. An inconsistency between two chapters, or between a chapter and the reference registry, is a defect of the specification or of the reference implementation. The harness reports it with both locations and fails, and it is resolved there, not in the harness.

---

## 15.9 Coverage per stage (N-J-47)

The suite is **complete** for a stage when it satisfies §15.9.1 and §15.9.2 for that stage. Completeness is a property of the suite, which the merge gate of the reference implementation checks (§15.12.6). It is what makes passing the suite evidence for a claim (§15.9.4).

### 15.9.1 Required content

For each stage, the suite MUST contain at least the following portable tests, in addition to the generated tests of §15.8 for the chapters of that stage. "Each" means one test or more per item named; a test may cover several items.

| Stage | Required tests |
|---|---|
| 0 | each token class and each lexical diagnostic of [spec/02](../spec/02-lexical-structure.md), including forbidden characters, NFC and line endings; each syntax diagnostic of [spec/03](../spec/03-grammar.md) §3.16; the module tree, `use`, visibility and name resolution within a package ([spec/04](../spec/04-modules-and-names.md)); type checking of non-generic programs ([spec/05](../spec/05-types.md), [spec/06](../spec/06-expressions-and-statements.md)); the attributes and lints of Stage 0 and level resolution from attributes, `lints` entries and the lint flags ([spec/15](../spec/15-attributes-and-lints.md)); each formatter rule (§15.4.3); the diagnostics stream and result records (§15.4.4); the exit codes and usage errors of [13](13-cli.md) §13.3.4 and §13.3.5 |
| 1 | execution on `wasm32-core` in `dev` and `release`: the arithmetic of each primitive numeric type at its boundaries, with each overflow, division and shift panic; records, enums, `match`, loops, `Option`, `Result` and `?` ([spec/06](../spec/06-expressions-and-statements.md), [spec/10](../spec/10-errors-and-panics.md)); each panic kind of Stage 1 on both reporting channels (§15.4.6); traps (§15.5.4); `extern "wasm"`, `@import_name`, `@export_name` and `@panic_hook` ([spec/12](../spec/12-unsafe-and-ffi.md), [spec/15](../spec/15-attributes-and-lints.md)); the library items of Stage 1 |
| 2 | each rule of [spec/07](../spec/07-ownership-and-borrowing.md) with an accepted and a rejected program, including two-phase borrows, closures and captures, and arenas; drop order, observed through `sai-test.write`; generics, traits, impls, coherence, `@derive`, `Copy`, `Clone` and `Drop` ([spec/05](../spec/05-types.md)); unsafe code and raw pointers ([spec/12](../spec/12-unsafe-and-ffi.md)); for each library function whose *Panics* entry is not `—`, a `run-panic` test per listed condition, and for each library function a `run-pass` test of its result ([spec/16](../spec/16-core-library.md)); the `tiny` profile and `wasm64-core`, through the equivalence tests of §15.6 |
| 3 | effects and capabilities, including opaque effects across packages ([spec/08](../spec/08-effects-and-capabilities.md)); contracts, refinements and invariants ([spec/09](../spec/09-contracts-and-refinements.md)), with, for each limit of spec/09 §9.6.8 (256 atomic formulas, 16 integer variables, 16 cubes, 256 constraints), an obligation at the limit that is proven and one beyond it that is `NotProven(Limit)` (spec/09 §9.6.9); `test` and `property` declarations, `@cases`, `@seed` and test-only dependencies ([spec/14](../spec/14-testing.md)); f-strings and the format specs of [spec/16](../spec/16-core-library.md) §16.14; each manifest rule of [package/01](../package/01-package-contract.md); resolution and its conflicts on Git fixtures ([package/02](../package/02-resolver.md)); lock files, their canonical layout and the vector of [package/03](../package/03-lock-file.md) §3.4.7; `--locked`, `--offline`, `--frozen` and the integrity failures of [package/04](../package/04-distribution-and-security.md) §4.6; the formatting of `sai.package` |
| 4 | WIT interfaces and worlds ([spec/04](../spec/04-modules-and-names.md), [spec/13](../spec/13-components-and-wit.md)); each row of the type mapping of [spec/13](../spec/13-components-and-wit.md) §13.9, with `.wit/` sidecars; resources and handles (§13.10), with the ownership rules of resources of [spec/07](../spec/07-ownership-and-borrowing.md) §7.14, because resources are a Stage 4 feature ([spec/05](../spec/05-types.md) §5.6.6); the boundary rules of [spec/08](../spec/08-effects-and-capabilities.md) and the boundary failures of [spec/13](../spec/13-components-and-wit.md) §13.13; command components on `wasm32-wasi` under the deterministic host, and reactors called with WAVE values (§15.5.4); artifact and hybrid packages ([package/04](../package/04-distribution-and-security.md) §4.5) |
| 5 | `async fn`, `async` blocks, `.await`, `for await`, scopes and cancellation ([spec/11](../spec/11-async-and-concurrency.md)); borrows across `.await` ([spec/07](../spec/07-ownership-and-borrowing.md) §7.16); `future`, `stream` and async exports ([spec/13](../spec/13-components-and-wit.md)); async code in tests ([spec/14](../spec/14-testing.md) §14.6). A test does not depend on the interleaving of tasks beyond what [spec/11](../spec/11-async-and-concurrency.md) fixes ([spec/01](../spec/01-introduction.md) §1.5.3) |
| 6 | a `protocol` test (§15.4.8) for each method of [12](12-semantic-tooling.md) §12.5.3 and each error row of §12.5.2 |
| 7 | `unsafe extern "C"`, `core.ffi` and the C interop of [spec/12](../spec/12-unsafe-and-ffi.md) and [spec/16](../spec/16-core-library.md) §16.12.4, and the native targets. A native executable is run by the harness as a child process in the environment of §15.1.4 item 1, with its standard streams compared as for a command component; it is bounded by the wall-clock limit of §15.1.4 item 3, because fuel exists only in the execution engine, and `call` directives do not apply to it |

### 15.9.2 Coverage rules

1. **Sections.** Every section at the lowest level of the heading tree of a covered chapter is named by the `spec` directive of at least one portable test, except the sections listed in `tests/conformance/exempt-sections.txt`. The covered chapters are [spec/02](../spec/02-lexical-structure.md) to [spec/16](../spec/16-core-library.md), [package/01](../package/01-package-contract.md) to [package/04](../package/04-distribution-and-security.md), [11](11-diagnostics.md), [13](13-cli.md) for the commands and records of §15.1.3, [14](14-formatter.md), [12](12-semantic-tooling.md) §12.5, and [spec/01](../spec/01-introduction.md) §1.4.5, §1.5.4 and §1.6. Each line of the exempt list names a section and one reason: `informative` (a section that its chapter marks as informative), `decisions` (a section of decisions), `diagnostics` (a table of codes), or `generated §15.8.n` (a section that a generated test of §15.8 checks in full, such as a listing of [spec/16](../spec/16-core-library.md)).
2. **References.** A `spec` directive that names a section that does not exist is a defect of the test. When a chapter is renumbered, the directives are updated with it.
3. **Codes.** Every code of the registry is expected by at least one portable test, through an annotation or a `diag` directive (§15.3), other than the checks of §15.8.4. The only exceptions are the codes in `tests/conformance/exempt-codes.txt`, each with the reason that no portable test can produce it and the test of §15.12 that covers it: `INTERNAL_COMPILER_ERROR` (§15.12.3), `SOURCE_READ_FAILED` (§15.12.5), and `IMPLEMENTATION_LIMIT_EXCEEDED`, which another IUT may never report for a program that the reference implementation rejects ([spec/01](../spec/01-introduction.md) §1.5.4). In addition, every hard code has a JSON golden (§15.4.4 item 3).
4. **Panic kinds.** Every panic kind of [spec/10](../spec/10-errors-and-panics.md) §10.4.4 is produced by at least one `run-panic` test.
5. **Limits.** For every row of [spec/01](../spec/01-introduction.md) §1.5.4, a portable test at the minimum is accepted and, if its stage is 1 or higher, built. The tests are generated by `cargo xtask spec-tests` from the table; a test file larger than 1 MiB is not checked in, and the harness generates it from the same generator when it runs the test. Tests beyond a minimum carry the `reference-only` directive.
6. **Report.** `cargo xtask coverage` reads the directives of all tests, the headings of the covered chapters, the registry and the two exempt lists, and writes `target/conformance/coverage.json`: for each section the tests that name it, and the uncovered sections, codes and panic kinds. The merge gate requires the uncovered lists to be empty for the stages that the reference implementation implements (§15.12.6), and the exempt lists change only together with the chapter or test they concern.

### 15.9.3 Applicability

A claim names a stage range, a set of targets, and through the stage range the profiles (§15.2.6). The stage range of a claim always starts at 0 (N-J-47), because each stage builds on all earlier ones (README §4); a claim `stages a-b` with `a > 0` cannot be substantiated. For a claim with highest stage `k`, each test is:

1. **applicable** if its stage is at most `k`, it has at least one configuration of a claimed target and profile, and it is portable or the IUT is the reference implementation. It runs in every such configuration (§15.2.6), and its outcome is `pass`, `pass-with-deviations`, `fail` or `not-run`;
2. **staged** if its stage is above `k`. It runs once in staged mode (§15.3.5);
3. **not applicable** otherwise: its `targets` or `profiles` directives exclude every claimed configuration, or it carries `reference-only` and the IUT is another implementation. Its outcome is `not-applicable`.

The optional targets of v0.1 (`wasm64-core` and `wasm64-wasi`, [spec/01](../spec/01-introduction.md) §1.4.2 item 5) make tests not applicable only through their configurations: a test of a rule that holds on every target runs on the claimed ones.

### 15.9.4 Substantiating a claim

The harness checks a claim when it is invoked as `cargo xtask test --iut <path> --claim "<claim>"`, with the claim in the form of [spec/01](../spec/01-introduction.md) §1.4.3 and, for an IUT with another command line, `--adapter <path>` (§15.1.3). The claim is **substantiated** if:

1. every applicable test has the outcome `pass` or `pass-with-deviations`;
2. every staged test has the outcome `staged`, `pass` or `pass-with-deviations`;
3. no test has the outcome `not-run`: a claim that includes Stage 3 is checked where the Git fixture server can run (§15.2.7);
4. the checks of §15.8.4 and §15.8.5 that apply to the IUT pass.

The summary record of the report (§15.1.4 item 5) states the claim, the suite version, the IUT's `sai --version` output, the adapter if any, the counts of each outcome, and the list of deviations, which is part of the claim. A claim that is checked with an older suite version names that version.

### 15.9.5 Target claim of the reference implementation

The target v0.1 reference implementation is intended to claim `SAI 0.1, stages 0-4, targets wasm32-core wasm32-wasi wasm64-core`, which is a v0.1 conformance claim ([spec/01](../spec/01-introduction.md) §1.4.3). The target claim excludes `wasm64-wasi`, because the component build for that target stops with `FEATURE_NOT_IMPLEMENTED` ([07](07-component-lowering.md) §7.11.7, N-D-58); the tests whose only configurations are on `wasm64-wasi` are therefore not applicable to this claim. Once implemented, the planned merge gate checks this claim on every change (§15.12.6), and each release publishes the report of §15.9.4 with it.

---

## 15.10 Differential testing (reference implementation, N-J-48)

Differential tests compare the reference implementation with an **oracle**: an independent computation of the same result. An oracle is code of the harness that shares no code with the compiler crates ([01](01-architecture.md) §1.3), except the parsers of data formats that the specification defines by reference (WIT, WAVE and JSON). A disagreement is a defect of the compiler or of the oracle, and it is investigated before either is changed. The target strategy places the tests in `tests/differential/`; the planned merge gate runs them on a fixed set of seeds (§15.12.6), and a planned nightly job runs fresh seeds. Every disagreement found with a fresh seed must be added as a conformance test with its minimized input.

### 15.10.1 Host oracles for values

For each operation below, the harness generates operands from a seeded generator that includes the boundary values of each type (zero, one, the minimum and maximum, powers of two and their neighbors, and for floats the subnormals, infinities, signed zeros and NaN), compiles one SAI program per type that applies the operation to operands read from an exported buffer, runs it on `wasm32-core` and `wasm64-core` in every profile, and compares each result with the oracle's:

1. integer arithmetic, comparisons, shifts and bit operations, including the panics on overflow, division by zero and out-of-range shifts ([spec/06](../spec/06-expressions-and-statements.md)), against Rust's checked operations on the same widths;
2. the conversions of [spec/16](../spec/16-core-library.md) §16.8.5, the wrapping, saturating and checked methods of §16.8.2, and float methods of §16.8.4 whose results are exact, against a host implementation written from the listings;
3. integer and float parsing (spec/16 §16.8.3, §16.8.4), against the host parsers on the generated texts and on texts mutated by one character;
4. the float forms of §16.14.6, against an independent shortest-round-trip formatter (the `ryu` algorithm) and the exact decimal expansion for `Prec(n)`, and the integer forms against the host formatter;
5. the hashing and comparison impls of §16.5.5 and §16.5.6, where the listing fixes the result, against a host model.

Comparisons of NaN results compare the NaN class, not the bits (§15.5.3).

### 15.10.2 Generated programs

1. **Type-directed generator.** The harness contains a generator of well-typed SAI programs: integer, float, `Bool` and `Char` arithmetic, records, enums with payloads, `match`, loops with `break` values, `Option` and `Result` with `?`, closures, generic functions with trait bounds, `Vec`, `String` and `Str` operations, and controlled panics. Each generated program writes a trace of its values through `sai-test.write`, and its result is a function of its seed. The generator is the source of the `typeck` fuzz target (§15.7.1).
2. **Configuration equivalence.** Each generated program is built for `wasm32-core` and `wasm64-core` in `dev`, `release` and `tiny`, and in `release` with and without each `--wasm-feature` of [13](13-cli.md) §13.4.7. All runs MUST produce the same trace and the same outcome, up to the profile differences of D-TGT-02 (§15.6.1).
3. **Reference interpreter.** The harness evaluates each generated program with an interpreter of the dynamic semantics of [spec/06](../spec/06-expressions-and-statements.md) over the generator's own representation of the program, not over the compiler's HIR or MIR. The trace and outcome of every Wasm run MUST equal the interpreter's.
4. **Constant evaluation.** For each generated expression that is a constant expression, the generator also emits it as the initializer of a `const` item. The value that the compiler computes for the item at compile time MUST equal the value of the same expression evaluated at run time, and a panic in one MUST be the same panic in the other ([spec/10](../spec/10-errors-and-panics.md) §10.4.6).

### 15.10.3 The Standard Fact Checker

The result of the Standard Fact Checker is fixed by the procedure of [spec/09](../spec/09-contracts-and-refinements.md) §9.6.8, so its oracle is a second implementation of that procedure in the harness, written from spec/09 §9.6.3 to §9.6.8, together with brute-force evaluation. The harness generates obligations over at most three integer variables with small domains (`-8` to `8`), with the formula shapes of spec/09 §9.6.5, and their facts:

1. **Agreement.** The compiler's result (`Proven`, or `NotProven` with its reason) equals the oracle's for every obligation, as spec/09 §9.6.9 requires of every implementation.
2. **Soundness.** If the result is `Proven`, brute force over the domain finds no counterexample. A counterexample to a `Proven` result is a release-blocking defect of the specification's procedure or of both implementations.
3. **Limits.** Generated obligations near each limit check that the result switches to `NotProven(Limit)` exactly where spec/09 §9.6.8 places it.
4. **Independence.** The result of an obligation does not change when functions are added after its body or removed from after it, when other obligations of the same body fail, or with the target, the profile and `--jobs` (spec/09 §9.6.9). Its facts are kept at the same byte offsets, because the canonical order of spec/09 §9.6.7 depends on them.

Brute force also counts the `NotProven(Open)` results that have no counterexample. These are the incompleteness that spec/09 §9.6.8 accepts; the count is reported and never fails a test.

### 15.10.4 Other oracles

1. **WIT signatures.** The core signatures, sizes, alignments and offsets that component lowering computes are compared with those of `wit-parser` ([07](07-component-lowering.md) §7.13.2, "Signature agreement").
2. **Allocator.** The allocators are compared with the host model of [10](10-runtime.md) §10.11 item 1.
3. **Tree hash.** The `sai1:` tree hash of [package/03](../package/03-lock-file.md) §3.4 is computed by an implementation in the harness, written from §3.4.1 to §3.4.6 and sharing no code with the compiler's, on the package directories of the suite and on generated directories with the file names, sizes, nesting and the collisions of §3.4.4. Both MUST give the same digest, and both MUST reproduce the vector of §3.4.7.
4. **Resolution.** For generated dependency graphs of up to eight packages with up to six versions each, served by the Git fixture server, the resolution of [package/02](../package/02-resolver.md) §2.4 is compared with a second implementation of `Select` and `Check` in the harness, written from §2.4.2 and §2.4.3. Both MUST give the same selected graph or the same conflict, and every result MUST have the properties of §2.4.5: each selected version is one of the named versions and the least that is at least every visited minimum.
5. **Borrow checker.** The harness SHOULD translate the generated programs of §15.10.2 that use only the constructs of [spec/07](../spec/07-ownership-and-borrowing.md) §7.19 into Rust, by the translation of §7.19, and check them with the borrow checker of the pinned Rust toolchain of the build ([05](05-borrow-checker.md) §5.20.3 item 6). Every program that the compiler accepts MUST be accepted by Rust. A program that Rust accepts and the compiler rejects is listed in the report and does not fail the test, because spec/07 is stricter than Rust in places (spec/07 §7.1.3).

---

## 15.11 Benchmarks (reference implementation)

The benchmarks measure the performance targets of [01](01-architecture.md) §1.14.1 and enforce them as §1.14.2 states. They never affect a result or a conformance claim.

### 15.11.1 The benchmark corpus

1. The corpus is generated by `cargo xtask bench-corpus` from a fixed seed, so it is the same on every machine and is not checked in. Its generator version is part of the name of every measurement; a change of the generator starts a new history.
2. It consists of a single file of 20 MiB of SAI source for the lexing measurement; workspaces of 1, 10 and 100 kLOC (counted as in [01](01-architecture.md) §1.14.1 item 1), each with eight member packages and two path dependencies; and a fixed list of edits for the incremental measurement. The programs come from the type-directed generator of §15.10.2, extended with modules, `use` declarations, traits with impls, effects and doc comments, in proportions that the generator fixes.
3. Every workspace of the corpus is accepted without errors by the compiler being measured; a corpus that does not compile is a failed benchmark run, not a measurement.

### 15.11.2 Measurements

| Measurement | Procedure |
|---|---|
| lexing and parsing | `sai_syntax` parses the 20 MiB file on one worker thread, in process, through `sai_driver`; the reported value is bytes per second |
| new package | `sai new` and then `sai check`, cold, as a child process |
| edit in a function body | for each of 100 edits of the edit list, applied in order to the 100 kLOC workspace with a warm cache, the duration of `sai check`; the reported value is the 95th percentile |
| 100 kLOC cold | `sai check` with an empty `.sai/cache/` and a cold build of the dependencies before it |
| 10 kLOC dev build | `sai build --profile dev`, cold |
| language server | a `protocol` transcript (§15.4.8) that opens a file of the 100 kLOC workspace and sends 100 single-character changes; the time from each change notification to the publication of its syntax diagnostics and of its semantic diagnostics; the reported value for semantic diagnostics is the 95th percentile |
| peak memory | the peak resident set size of `sai check` of the 100 kLOC workspace, cold |

Phase times are recorded with a tracing subscriber ([01](01-architecture.md) §1.11 item 3) and reported with each measurement, so that a regression can be attributed to a phase.

### 15.11.3 Method

1. Benchmarks run on dedicated machines that meet the description of the reference machine ([01](01-architecture.md) §1.14.1), with no other job running.
2. Each measurement is taken 10 times after one discarded warm-up run, and the median of the 10 values is recorded, together with the toolchain's build ID ([01](01-architecture.md) §1.13.2).
3. A measurement is a **regression** if its median is more than 10% worse than the median of the previous 10 recorded medians on the main branch, or if it misses its target. A regression is measured a second time before it is reported, because unlike tests, a timing depends on the machine. A confirmed regression blocks the merge unless the change states it and its reason ([01](01-architecture.md) §1.14.2).

---

## 15.12 Compiler tests (reference implementation)

### 15.12.1 Unit tests and test-only crates

1. Each crate of [01](01-architecture.md) §1.3.1 has unit tests of its own invariants, run by `cargo test --workspace`. Phase crates are tested against small databases that implement only the query groups they use ([01](01-architecture.md) §1.3.2 rule 3).
2. The third-party crates that the tests, the harness, the fuzz targets and the benchmark runner may use, in addition to those of [01](01-architecture.md) §1.3.2 rule 4, are exactly: `wasm-wave` (WAVE values), `rustls` (the TLS of the Git fixture server, §15.2.7), `rand_chacha` (the deterministic random source of §15.5.4), `serde_json` (reading the IUT's NDJSON), `jsonschema` (§15.3.8), `ryu` (the float oracle of §15.10.1), `gix-pack` (generating local packed-object fixtures), and `libfuzzer-sys` and `arbitrary` (§15.7). None of them is a direct dependency of a shipped crate; `gix-pack` is already a transitive dependency of `gix`. The pack-generation APIs and the `generate` and `streaming-input` features are enabled only through `sai_package`'s dev-dependency and are absent from normal builds. The deterministic WASI host of §15.5.4 uses the same WASI implementation of the `wasmtime` project as `sai run` ([13](13-cli.md) §13.6.5), with its clocks, random source and network replaced as §15.5.4 states. The fixture server implements the subset of HTTP/1.1 that the Git smart HTTP protocol needs, and builds packs with `gix`.

### 15.12.2 Compiler-level tests

These tests live in `tests/impl/` and use the reference implementation's crates directly:

1. the test suites of the implementation chapters: the borrow-checker suite of [05](05-borrow-checker.md) §5.20.3 in `tests/impl/borrowck/` (fact goldens, diagnostic goldens, fixes, and the verify mode of §5.20.2), [07](07-component-lowering.md) §7.13.2, the effect and unsafe suites of [08](08-effect-and-capability-analysis.md) §8.12.2 in `tests/impl/effects/` and `tests/impl/unsafe/`, [10](10-runtime.md) §10.11, the size suite of [09](09-optimization-and-size.md) §9.11, and the WIT suite of [16](16-interfaces-and-wit.md) §16.11 in `tests/impl/wit/`;
2. the round trip of [spec/13](../spec/13-components-and-wit.md) §13.11.6, on every interface of the conformance suite: the SAI view of the generated WIT equals the interface after normalization;
3. the incremental tests and the metadata tests of §15.6.4 and §15.6.6;
4. **unsafe code**: the unit tests of `sai_intern` and of the archive reader of `sai_query` ([01](01-architecture.md) §1.3, §1.6.4) run under Miri, and the archive reader is also tested with truncated, misaligned and bit-flipped buffers that pass no checksum, and with buffers whose checksum is recomputed after a change, which the reader must reject by its bounds checks without undefined behavior;
5. **concurrency**: the query system is tested with `--jobs` from 1 to 16 on the conformance suite's multi-package tests, and with cancellation at every query boundary of a check, after which a new revision MUST produce the outputs of an uninterrupted run ([01](01-architecture.md) §1.4.6).

### 15.12.3 Internal compiler errors

1. A build of the reference implementation with the Cargo feature `ice-injection` (never enabled in the shipped binary) panics in a query named by a test. For each query group, a test injects a panic and requires exactly one `INTERNAL_COMPILER_ERROR` record with the data of [11](11-diagnostics.md) §11.4.6, no further diagnostic of the session's revision after it, and the exit code 101 ([13](13-cli.md) §13.3.4).
2. A test injects a formatter verification failure and requires the same, with the file left unchanged ([14](14-formatter.md) §14.11.2).
3. A test of the language server injects a panic in a request and requires the JSON-RPC error `-32603` and a server that keeps serving ([12](12-semantic-tooling.md) §12.5.2).

### 15.12.4 Package resolution and transport

1. The `ssh` transport of [package/04](../package/04-distribution-and-security.md) §4.2.3 is tested against an SSH server in the CI sandbox with fixture repositories (§15.2.7), and against a mock that refuses authentication, closes the connection, and sends malformed responses.
2. Fetch failures, retries and partial downloads into the global cache are tested by a fixture server that fails requests at chosen points; the cache MUST never contain an entry whose tree hash does not match ([package/04](../package/04-distribution-and-security.md) §4.4).
3. If `gix-protocol` is locally patched, its bounded ref-advertisement parser tests run explicitly in both adapter configurations: `cargo test -p gix-protocol --lib --features blocking-client,async-client,sha1,sha256` and `cargo test -p gix-protocol --lib --features async-client,sha1,sha256`. The workspace build does not enable the crate's optional protocol-client features, so `cargo test --workspace` alone does not run these cases.

### 15.12.5 The command line

1. `sai run` is tested on the command components of the conformance suite: the outcome and the standard streams equal those of the harness's execution where the program reads no clock and no random value ([13](13-cli.md) §13.6.5).
2. Every exit code of [13](13-cli.md) §13.3.4 is produced by at least one test, including 130 for an interrupt, sent to the process while a check is running, after which `.sai/cache/` is still valid for the next run ([01](01-architecture.md) §1.10.5).
3. `SOURCE_READ_FAILED` ([01](01-architecture.md) §1.7.2 item 3) is produced by a test that removes the read permission of a source file, on hosts where the test does not run with privileges that ignore it.
4. The commands that the conformance suite does not use (`sai new`, the dependency commands, `sai size`, `sai graph`, `sai diff`, `sai edit`, `sai bindgen wit`, `sai inspect`) have tests of their records against [13](13-cli.md) §13.8 and of their effect on the file system.

### 15.12.6 Merge gates and releases

1. **Planned v0.1 merge gate.** Once the target conformance infrastructure is implemented, every change to the compiler repository, including changes to `library/`, `tests/` and `docs/`, MUST pass the following checks on both an `x86_64` host and an `aarch64` host before merge:
   - `cargo build` and `cargo test --workspace`, and `cargo clippy` with warnings denied and the disallowed types and methods of [01](01-architecture.md) §1.8.3 rule 3;
   - `cargo xtask spec-tests`, which MUST leave `tests/spec/` unchanged (§15.8);
   - the canonical form of every test file and of the library source (§15.2.2 item 3);
   - `cargo xtask test` with the claim of §15.9.5: every applicable test passes, every staged test is `staged` or passes, and the reference implementation has no deviation (§15.3.4 item 3);
   - the coverage check of §15.9.2;
   - the equivalence tests of §15.6, the differential tests of §15.10 on their fixed seeds, the corpus replay of §15.7.2 item 3, and the compiler tests of §15.12.1 to §15.12.5;
   - the bootstrap and its fixpoint ([01](01-architecture.md) §1.12.2), for changes to the compiler or the library.
2. **Equal hosts.** The reports of the two hosts are compared with each other, after removing the `duration_us` fields. Any difference fails the gate, because results do not depend on the host ([spec/01](../spec/01-introduction.md) §1.6).
3. **No quarantine.** A failing test is never disabled, skipped or marked as expected to fail. Because a test's result is a function of the test and the IUT (§15.1.4 item 2), a failure is either a defect of the toolchain, which the change fixes, or a defect of the test or the specification, which the change corrects with a stated reason. Changed golden files are blessed and reviewed in the change (§15.4.1).
4. **Continuous jobs.** Fuzzing (§15.7.2) and the differential tests on fresh seeds (§15.10) run outside the gate. A finding of these jobs blocks the next release until it is fixed with its regression test. The benchmarks (§15.11) run for every change on their dedicated machines, beside the gate, and a confirmed regression blocks the merge as §15.11.3 item 3 states.
5. **Release.** A release additionally requires at least 24 CPU hours of fuzzing per target on the release candidate without a new finding (§15.7.2 item 2), a run of the full suite on both hosts with the released binary, and the publication of the claim report of §15.9.4.

---

## 15.13 Decisions introduced by this chapter

### N-J-38 The conformance suite is black-box

- **Rule:** The portable parts of the suite interact with an implementation only through the commands of §15.1.3, their NDJSON records and the artifacts they write. The harness executes artifacts in its own engine. Implementations with another command line are driven through a named adapter. Only the parts marked as applying to the reference implementation use its crates.
- **Rationale:** The specification defines conformance through observable behavior ([spec/01](../spec/01-introduction.md) §1.4), so a suite that used internal APIs would test one implementation's structure instead (P8). Running artifacts in the harness keeps clocks and randomness out of results.

### N-J-39 Hermetic and deterministic tests

- **Rule:** Every test runs in a fresh directory with a fixed environment (§15.1.4 item 1), is bounded by fuel rather than time, is never retried, and has a result that is a function of the test and the IUT. Reports are in path order.
- **Rationale:** The toolchain is deterministic (spec/01 §1.6), so a test that can give two results is a defect to find, not noise to tolerate (P8). Fuel makes the bound the same on every machine.

### N-J-40 Test files are canonical

- **Rule:** Every `.sai` and `sai.package` file of the suite is in the canonical form of [14](14-formatter.md), except formatter inputs and deliberately ill-formed files, and the harness never rewrites a test file.
- **Rationale:** Annotations are line-based (§15.3.1), and canonical files keep each annotation on the line of its construct. One form for all tests also makes them readable by agents and humans alike (P9).

### N-J-41 Expected diagnostics are matched by line and code

- **Rule:** Diagnostics are compared as multisets of `(file, line, severity, code)`. Acceptance and rejection must always be correct. The reference implementation must match exactly; other implementations get deviations for missing and unexpected codes, which do not fail a claim but are reported with it. An internal compiler error always fails.
- **Rationale:** Codes are a SHOULD ([spec/01](../spec/01-introduction.md) §1.4.2 item 3) and exact spans depend on how an implementation delimits a construct, so a portable check must be coarser than a golden file. Reporting deviations keeps the difference visible (P1).

### N-J-42 Golden files are blessed and reviewed

- **Rule:** Golden files hold the reference implementation's exact output and are written only by `cargo xtask bless`. The only golden comparisons that are portable are the formatter outputs and the WIT texts, whose bytes the specification defines.
- **Rationale:** Byte-exact comparison catches every change of output that agents depend on (P3), and blessing in the change makes every such change reviewed. Other implementations are not bound to the reference implementation's messages or spans ([spec/01](../spec/01-introduction.md) §1.5.2).

### N-J-43 A pinned, deterministic execution engine

- **Rule:** Artifacts are executed by a pinned `wasmtime` with the settings of §15.5.1: the feature set of the target, NaN canonicalization, fuel, a memory limit and a fixed stack. WASI components run against a deterministic host with fixed arguments, an empty environment, simulated clocks, seeded randomness and no network. Results must be the same on `x86_64` and `aarch64`.
- **Rationale:** Execution results are part of conformance ([spec/01](../spec/01-introduction.md) §1.4.2 item 4), so the engine must not add nondeterminism of its own (P8). A simulated host makes programs that read the clock testable.

### N-J-44 Configurations must agree

- **Rule:** Every test runs in every applicable target and profile, and the outcomes, outputs, host-call sequences and diagnostics must be equal across profiles, up to the differences of D-TGT-02, and across targets, up to `USize` and `ISize`. Each test is also run with one and eight jobs, from a second directory, and with doubled resources.
- **Rationale:** C-03 and P8 make a profile, a target, a thread count or a directory unable to change meaning. Checking every test in every configuration turns that rule into a property of the whole suite.

### N-J-45 Fuzzing is continuous; its corpus is a gate

- **Rule:** The fuzz targets of §15.7.1 run continuously with the budgets of §15.7.2 and are not a merge gate. Replaying their corpus is. Every finding becomes a regression test before its fix is merged.
- **Rationale:** A time-bounded random search cannot be a deterministic gate (N-J-39), but its findings can, and the robustness requirement ([spec/01](../spec/01-introduction.md) §1.4.2 item 8) needs a search that tests do not provide.

### N-J-46 The specification is tested

- **Rule:** The grammar, the library listings, every `sai`, `sai-package` and `sai-lock` example, the explain records and the diagnostic tables are turned into tests by `cargo xtask spec-tests` (§15.8). The generated tests are checked in and regenerated by the merge gate.
- **Rationale:** An example or a listing that does not compile misleads every reader, human or agent (P9). Generating the tests from the documents makes the specification and the reference implementation fail together when they drift apart.

### N-J-47 Coverage per stage and complete claims

- **Rule:** The suite is complete for a stage when it contains the tests of §15.9.1 and covers every section, code and panic kind of §15.9.2. A claim's stage range starts at 0, and a claim is substantiated only by a complete run of §15.9.4: every applicable test passes, every test of a later stage is staged or passes, and no test is `not-run`.
- **Rationale:** A suite that passes is only evidence if it covers the rules it stands for. Stages build on each other (README §4), so a range that omits an earlier stage has no meaning, and a partial run cannot substantiate a claim ([spec/01](../spec/01-introduction.md) §1.4.3).

### N-J-48 Differential oracles are independent

- **Rule:** Every oracle of §15.10 is code of the harness that shares no code with the compiler crates, except parsers of the data formats that the specification adopts by reference. A disagreement is investigated before either side is changed, and every disagreement found with a fresh seed becomes a conformance test.
- **Rationale:** An oracle that shares code with the compiler repeats its defects. Independent implementations of the specified procedures (the SFC, MVS, the tree hash) also test that the specification determines one result (P8).

---

## 15.14 Diagnostics

This chapter defines no diagnostic code. It checks the codes of all chapters (§15.3, §15.8.5); the codes below have a specific role in the suite.

| Code | Severity | Description |
|---|---|---|
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; context: the staged mode of tests above the claim (§15.3.5) and the staged library probes (§15.8.2) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; context: fails every test in which it occurs (§15.3.4 item 1), and is produced on purpose only by the injection tests of §15.12.3 |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | defined in [spec/01](../spec/01-introduction.md) §1.5.4; context: expected only by reference-only tests beyond a minimum (§15.9.2 items 3 and 5) |
| `EFFECT_NOT_DECLARED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.1; context: the effect probes of library signatures (§15.8.2 item 1) |
| `SEALED_TRAIT_IMPL` | error | defined in [spec/16](../spec/16-core-library.md) §16.3.5; context: the trait probes of sealed traits (§15.8.2 item 2) |
| `UNFORMATTED_FILE` | error | defined in [14](14-formatter.md) §14.11.4; context: the `--check` tests of the formatter (§15.4.3) |
| `SOURCE_READ_FAILED` | error | defined in [01](01-architecture.md) §1.7.2; context: covered only by the compiler test of §15.12.5 item 3 (§15.9.2 item 3) |
