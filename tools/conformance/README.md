# Standalone conformance components

This standalone component implements the diagnostic multiset comparison,
acceptance outcome, ICE precedence, and single-command staged decision of
`docs/impl/15-conformance-and-testing.md` §15.3.4–§15.3.5. It uses only the
Python standard library and does not import compiler internals.

Run `python3 tools/conformance/diagnostic_matcher.py < request.json`. It reads
one JSON object and emits one JSON result. Exit status is 0 for `pass`,
`pass-with-deviations`, or `staged`; 1 for `fail`; and 2 for malformed input.
Input must be valid UTF-8 JSON without unpaired Unicode surrogates, duplicate
object keys, or non-JSON numeric constants such as `NaN` and `Infinity`.
Numbers outside the parser's supported range are malformed input.

```json
{
  "kind": "check-fail",
  "implementation": "reference",
  "exit_code": 1,
  "expected": [{"file": "src/lib.sai", "line": 2, "severity": "error", "code": "EXPECTED_TOKEN"}],
  "actual": [{"file": "src/lib.sai", "line": 2, "severity": "error", "code": "EXPECTED_TOKEN"}]
}
```

Kinds are `check-pass`, `check-fail`, and `build-fail-check` (the preliminary
check of a build-fail test). Implementations are `reference` or `other`.
Diagnostics use root-relative `file` and 1-based `line`, or null for both when
locationless. A file path uses `/` separators and has no empty, `.` or `..`
segments, backslashes, drive prefix, or NUL. `actual` entries may include
`data`. In staged mode, set
`staged_claim_stage` to 0–7; a qualifying `FEATURE_NOT_IMPLEMENTED` error has
`data: {"stage": N}`. Omit this field for an ordinary comparison; explicit
null is invalid.

For these check commands, an error diagnostic requires IUT exit code 1 and no
error requires 0. Other exits fail, as does an inconsistent 0 or 1, before
staged status can be reported. An ICE diagnostic or exit 101 always fails.

## Expectation extraction

`python3 tools/conformance/expectation_extractor.py SOURCE --path ROOT_RELATIVE_PATH`
extracts diagnostic annotations from a single source file. Add `--directives`
for the root file's diagnostic directives. Add `--validate-test-directives` to
require and validate the full `kind`, `stage`, and `spec` header and receive
normalized metadata alongside the matcher's `expected` array. This strict mode
includes optional defaults for targets, profiles, options, component, calls,
args, seed, diagnostics, fuel, memory, stderr failure and reference-only reason.
`--lint-levels` supplies a JSON map of lint codes to resolved levels.
Invalid input or unsupported syntax exits with status 2.

The normalized `directives` object uses `kind`, numeric `stage`, `spec` entries
of `{document, section}`, arrays for `targets`, `profiles`, `options`, `call`,
`args` and `diag`, a boolean `component`, and nullable optional values. Defaults
are all four targets, `dev release tiny`, empty options/calls/args/diagnostics,
`component: false`, no seed or reference-only reason, fuel `10000000000`,
memory limit `4096`, and no stderr failure. `--path` remains the source path
used for diagnostic locations.
The sorted `specified` array lists directive names explicitly written in the
test, including repeatable and empty-flag directives; it distinguishes an
omitted default such as `targets` from an explicitly written value.

The scanner is deliberately incomplete: it rejects package paths with
ambiguous boundaries or newline contexts, format strings, execution
expectations, non-ASCII code outside literals and comments, and ambiguous
malformed inputs. In a strictly validated leading `fmt-refuse` fixture, the
scanner permits invalid literal escapes so the refusal fixture can reach the
IUT. It also permits an unterminated block comment only when the explicit error
expectation is `UNTERMINATED_COMMENT` at that comment's opening line; malformed
literal boundaries and other unsupported inputs still fail closed.
Expectation-only mode checks directive placement and
repetition but does not validate values. Strict mode validates directive
names, repeatability, placement and the documented value forms, including the
allowed target/profile/option names, comma-separated section-reference syntax,
JSON-string arguments, and the call expression's export/argument/result
structure. It does not check that referenced specification sections exist,
resolve a call against a built artifact, validate call value encodings, or
enforce cross-field applicability rules; the configuration selector and future
coverage/execution harness must do those checks. An unsupported test must not
be treated as passing or as having no expectations.

## NDJSON envelope

`python3 tools/conformance/ndjson_stream.py` reads a binary NDJSON stream from
standard input and checks common version 1 wire-format and record constraints.
The optional `--max-errors`, `--process-exit-code`, and `--stderr-file` inputs
check the observed error limit, exit-code agreement, and empty stderr. Even
when these checks succeed, it reports `partial` and exits 2: callers must still
check command-specific schemas, diagnostic ordering, registered codes,
source-dependent constraints, exit-code semantics and stdout write failures.

## Fixture discovery

`python3 -B tools/conformance/fixture_discovery.py tests/conformance` lists
single-file and directory fixtures in deterministic path order. It inspects
paths without executing tests. It rejects ambiguous test/sidecar layouts,
including possible tests inside `.wit/` or `.mir/` directories, non-ASCII names,
UNC roots, unsafe entries, and nonportable name collisions. Thus it does not
yet support every valid suite. It assumes the tree is not modified concurrently
and does not validate directives or fixture contents. Windows junction
behavior still needs a Windows runtime test.

### Single-file formatter and fix sidecars

The discovery pass accepts `<name>.expected.sai` and `<name>.fixed.sai` when
exactly one existing `<name>.sai` owns the sidecar. It also accepts the
supported `.<target>`, `.<profile>`, and `.<target>.<profile>` specializations.
These suffixes are reserved for sidecars: an orphan, wrong-type entry, or
specialized name that matches multiple source fixtures is rejected rather than
classified as a test.

## Configuration selection

`python3 -B tools/conformance/configurations.py < request.json` selects a
bounded subset of normal test configurations. It rejects stage-0 checks with
explicit target or profile restrictions and requires explicit, nonempty
WASI-only test targets for `run-fail`. Later-stage tests return
`staged-selection-required` with exit status 2 instead of a selected result.
The caller must validate the remaining test metadata and implementation
capabilities, including optional `wasm64-wasi` component support. This helper
does not run tests, resolve staged fallback configurations, or establish a
conformance claim.

## Stage 0 partial smoke runner

`python3 -B tools/conformance/run_stage0.py --iut /absolute/path/to/sai tests/conformance`
runs a deliberately limited set of Stage 0 checks and formatter cases through
the IUT's command line. It accepts single-file `check-pass`, `check-fail`,
source `fmt`, and source `fmt-refuse` fixtures with `kind`, `stage`, and
resolvable `spec` directives, plus a Stage 3 package `fmt` directory fixture
rooted at `sai.package`. Source `fmt` fixtures require exactly one sibling
`<name>.expected.sai`; the package fixture requires exactly
`expected/sai.package`. A single-file `check-pass` or `check-fail` fixture may
also have one ordinary `<name>.json` sidecar. `fmt-refuse` requires explicit,
nonempty expected error diagnostics and rejects every sidecar; specialized
goldens and other sidecars are unsupported, as are directory fixtures other
than the package `fmt` fixture. The runner rejects explicit target/profile
restrictions, unsupported directives, an empty fixture set, and inputs whose
specification references do not resolve beneath `docs/`.

Every accepted fixture runs in a fresh sandbox with only `NO_COLOR=1`,
`SAI_CACHE_DIR=<sandbox>/cache`, and `HOME=<sandbox>/home` in the IUT
environment. For a check fixture, the runner copies the source unchanged,
creates the generated package manifest, invokes
`sai --format=json --max-errors=0 check --jobs=1`, validates the diagnostic
registry and JSON stream, checks diagnostic order and result semantics, and
compares expected diagnostics. When the fixture has an ordinary `.json`
sidecar, it also compares the complete NDJSON stream with that golden after
normalizing only the header's complete `tool` member and `duration_us` in
`test` result records as specified by §15.4.4. Record and member order and
every other value remain part of the comparison. This currently covers
`impl-13-cli/stage0-check-pass.json` and
`spec-03-grammar/stage0-check-fail.json`; the smoke report records the golden
path and pass/fail outcome whenever a golden is present and checked.

For a `fmt` fixture it passes the unchanged source bytes on standard input to
`sai --format=json --max-errors=0 fmt --stdin-kind source -`, compares the JSON
`output` bytes with `<name>.expected.sai`, then formats the expected bytes again
and requires the same output with an empty `changed` list. It then runs
sai_test_harness's `fmt-layout-variants` binary through `cargo run --locked` to
derive 16 deterministic layouts from the fixture path relative to `tests/`.
The reference lexer selects safe token boundaries and the syntax tree supplies
parser-skipped positions across signatures, declaration clauses and bodies,
match-arm lists, tests, and properties. It varies eligible breaks in both
directions: inserting a safe newline or compacting an optional existing
newline. The reference parser validates each generated layout against the
original syntax tree. The pseudo-random stream is seeded with BLAKE3 of the
test path. Comments and blank lines are retained. Sources without mutable
layout positions may yield repeated entries; the runner still submits all 16
inputs to the IUT and compares each result with the expected bytes. A Rust
toolchain with `cargo` must be available when the runner processes a `fmt`
fixture.
It then runs
`sai --format=json --max-errors=0 fmt --check --stdin-kind source -` on the
input and expected bytes. Expected bytes must pass without diagnostics; an input
that differs must report exactly one `UNFORMATTED_FILE` on the first differing
line, with its expected-line data and complete primary byte, line, UTF-8 column,
and UTF-16 column positions matching `docs/impl/14-formatter.md` §14.11.4.
Suppressed cascade, duplicate, and dependency diagnostic counts must be zero.
The runner also copies the input and expected output, in separate runs, as the
only source file in a generated package and invokes no-argument
`sai fmt --check`; the generated `sai.package` is checked alongside it. It
requires the `.sai` inventory to remain unchanged and compares each selected
file's metadata and bytes to ensure `--check` neither modifies nor replaces the
source or manifest. It does not run `sai check` or the canonical-input preflight
for `fmt`. It also runs
`sai --format=json --max-errors=0 fmt --check src/lib.sai` on both byte
sequences, requires exactly one selected source, and checks the same canonical
pass result or exact first-difference diagnostic. Multiple explicit path
arguments are not covered. For
`fmt-refuse`, it passes the unchanged
source bytes to the same formatter command and requires a strict refusal result
(`exit 1`, one file, no output, and `<stdin>` in `refused`) whose nonempty error
multiset exactly matches the fixture's expectations. It derives allowed
refusal codes from the lexical error table and parser-only rows in the
syntactic-validation table; parser severities come from the implementation
diagnostics table, and unknown severities fail closed. Validation and mixed
parser/validation codes are excluded. This kind also bypasses canonical-input
preflight and `sai check`. The runner generates an exactly 16 MiB valid source
with a canonical block comment and final newline, and requires formatter
acceptance with byte-identical output. It also generates a valid-UTF-8 source
input of 16 MiB + 1 byte beginning with an unterminated block comment and
requires the exact `IMPLEMENTATION_LIMIT_EXCEEDED` diagnostic, actual byte
length, one refused `<stdin>` input, no changed files, no output, and no other
diagnostic. The malformed prefix makes the probe verify that the size check
precedes lexing. Both inputs are created in memory and are not stored as
fixtures.
A timeout is a failure, never a pass.

For the package `fmt` directory fixture, the runner pipes the `sai.package`
bytes to `fmt --stdin-kind package -`, compares the exact output with
`expected/sai.package`, and checks the expected fixed point. Its package
reference generator varies whitespace and safe line breaks before `from`,
`dir`, `only`, and `grant`, and next to braces. It preserves comments and
blank-line gaps, requires the same significant token sequence, and checks that
the package parser accepts every variant. The runner submits 16 variants,
checks both input byte sequences with package-stdin `fmt --check`, and checks
the disk fixture through `fmt --check sai.package`; every explicit-path check
must leave the manifest bytes and file identity unchanged.

The runner treats the supplied executable as the repository's reference
implementation, but cannot verify its identity from the black-box protocol.
Callers must supply the reference binary, and the report sets
`iut_identity_verified` to `false`. It rejects `reference-only` fixtures because
it cannot establish that identity.

The conceptual configuration is targetless and profileless, as Stage 0
requires. The current CLI still executes this generated package as
`wasm32-core`/`dev`: the package contract requires a nonempty target list and
the CLI has no targetless Stage 0 mode. The report exposes this compatibility
execution and always sets `mode` to `partial-smoke` and `claim` to `false`.
This runner is useful as smoke evidence only; it does not validate suite
coverage, all goldens, all directive applicability, or the complete conformance
protocol, and cannot substantiate a conformance claim. Its JSON golden
comparison is limited to ordinary sidecars on single-file Stage 0 check
fixtures; it does not cover formatter goldens, target/profile specializations,
or directory fixtures.
Formatter coverage is limited to single-file source and one package-manifest
`fmt` fixture's byte comparisons, fixed-point checks, 16 deterministic layout
variants, source-stdin, generated
package-wide `fmt --check` and `fmt --check` with one explicit path,
exact-diagnostic `fmt-refuse` cases, and a generated source-size refusal probe.
Fixed-point and `fmt --check` checks apply only to `fmt` pass fixtures.

The same run also executes a table of 18 selected CLI usage-error cases from
`impl/13-cli.md` §13.3.5: missing and unknown commands, missing and unknown
subcommands, unknown option, missing command-specific, global, and `new`
option values, invalid option values, mixed errors whose order follows the
canonical diagnostic key rather than argv order, synthetic option conflicts
that identify the later option, repeated missing option values that must not be
deduplicated, an unexpected positional, conflicting global options, all-error
reporting with `--max-errors=1`, and `NO_PACKAGE_FOUND` from both `check` and
`fmt`. Each case requires status 2,
empty stderr, a valid JSON stream with the expected command header and
diagnostics, `primary: null`, exact `data.argument_index` and
`data.argument`, missing option names in their messages, a matching summary, no result record, and empty isolated
cache and home directories. The coverage is deliberately selected; it does
not replace the full command-line and lint coverage required by §15.9.1.

## Stage 0 and Stage 1 explanation-record partial smoke

`python3 -B tools/conformance/run_explanation_smoke.py --iut /absolute/path/to/sai`
checks the currently selected complete explanation records:
`DUPLICATE_BINDING`, `INVALID_ESCAPE`, `LITERAL_OUT_OF_RANGE`, and
`MALFORMED_NUMBER` (Stage 0), plus `TYPE_MISMATCH` (Stage 1). It verifies their
`--list` entries, JSON metadata, Markdown heading/table/section structure, and
marked Example and Fix fences. It checks
each Example with the generated single-file package,
tries `sai check` before the documented `sai build` fallback, checks each Fix
with `sai check`, and verifies formatter fixed points or the exact marked
lexical refusal behavior.

## Conformance coverage inventory (report-only)

`python3 -B tools/conformance/coverage.py --root . --output target/conformance/coverage.json`
validates the specification references and diagnostic expectations in every
discovered fixture, inventories the lowest-level numbered headings in the covered
document scopes of §15.9.2, and writes a deterministic JSON report containing
section-to-test mappings and uncovered sections, diagnostic codes, panic kinds,
and hard diagnostic codes without a JSON golden. It reads the lint list from
spec/15 §15.17.2, selects ordinary and target/profile-specialized JSON sidecars
for configurations allowed by each fixture's directives using §15.2.5's
first-existing precedence, validates their common NDJSON envelope and registered
diagnostic codes, and keeps this nonportable golden accounting separate from
portable annotation coverage. Reference-only fixtures' selected goldens still
count. This initial inventory reads `tests/conformance`; generated tests in
`tests/spec/` are not present yet. Directory fixtures use directives from
their root `sai.package` and scan their source `.sai` files for diagnostic
annotations; reserved `.expected.sai` and `.fixed.sai` outputs, `expected/`
directories, and `.wit` and `.mir` trees do not count as tests.

The inventory is intentionally report-only. It does not run the IUT, check the
required-content matrix of §15.9.1, compare goldens with IUT output, verify
code-specific diagnostic data shapes, or generate or check the complete
§1.5.4 limit-test matrix. The smoke runner's two source-formatter boundary
probes do not cover the other limits or compiler acceptance at those limits.
The repository does not yet record its implemented stages, so coverage is not a
CI merge gate.
`run-panic` annotations identify a location but not a panic kind, and panic
reports carry no kind field; therefore the report leaves every panic kind
uncovered instead of inferring one from source text.

The exemption-list files are required inputs; add rows only after review.
Blank lines and `#` comments are ignored. A section exemption uses
`<document> §<section> | <reason>`, where the reason is `informative`,
`decisions`, `diagnostics`, or `generated §15.8.<n>`; generated-section anchors
must resolve in `impl/15`. A diagnostic-code exemption uses
`<CODE> | <reason> | §15.12.<section>` and is limited to
`INTERNAL_COMPILER_ERROR` (§15.12.3), `SOURCE_READ_FAILED` (§15.12.5), and
`IMPLEMENTATION_LIMIT_EXCEEDED` (§15.12.2). These formats make omissions
explicit; they do not waive the need to review the exemption itself.

For `INVALID_ESCAPE` and `MALFORMED_NUMBER`, the runner accepts only the exact
current `sai check` `FEATURE_NOT_IMPLEMENTED` gate alongside the required
diagnostic. It still rejects every other unmarked diagnostic. This reflects the
present Stage 0 checker boundary; each Example must produce its own marked
diagnostic. `LITERAL_OUT_OF_RANGE` has no gate allowance: its Example must
produce only the marked diagnostic.

This runner does not check the remaining registered records, general staged
applicability, diagnostic tables, or the registry requirements in `impl/15`
§§15.8.4–15.8.5. It reports `mode: partial-smoke`, `claim: false`, and
`iut_identity_verified: false`.
Package-contract formatting variants and goldens, package workspaces,
multi-source package checks, multiple explicit path arguments, complete
formatter-rule coverage, and every refusal condition remain unsupported.

The subprocess environment mapping contains exactly the three listed entries.
On macOS, Python test executables may add `LC_CTYPE` and
`__CF_USER_TEXT_ENCODING` during their own interpreter initialization; a native
child-process check verifies the environment received through direct execution.

The standalone components and this limited runner are not the full harness.
The matcher only judges supplied normalized diagnostics and process exit code;
it does not validate the complete command protocol. The normative harness
still needs to implement package-path resolution, every supported test kind,
staged command sequences, configuration coverage, execution, goldens, and the
suite claim.

Run its isolated tests with
`python3 -B -m unittest discover -s tools/conformance -p 'test_*.py'`.
