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
malformed inputs. Expectation-only mode checks directive placement and
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

These components are not the full harness and cannot substantiate a
conformance claim. The caller must resolve package paths, validate NDJSON,
run the IUT and manage the staged command sequence and
configurations. The matcher only judges supplied normalized diagnostics and
process exit code; it does not validate the complete command protocol.

Run its isolated tests with
`python3 -B -m unittest discover -s tools/conformance -p 'test_*.py'`.
