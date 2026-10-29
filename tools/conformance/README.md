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
for the root file's diagnostic directives and `--lint-levels` with a JSON map
of lint codes to resolved levels. The output can supply the matcher's
`expected` array. Invalid input or unsupported syntax exits with status 2.

The scanner is deliberately incomplete: it rejects package-path candidates,
format strings, execution expectations, non-ASCII code outside literals and
comments, and ambiguous malformed inputs. Directive placement and repetition
are checked, but other directive values are not validated. An unsupported
test must not be treated as passing or as having no expectations.

## NDJSON envelope

`python3 tools/conformance/ndjson_stream.py` reads a binary NDJSON stream from
standard input and checks common version 1 wire-format and record constraints.
Even when these checks succeed, it reports `partial` and exits 2: callers must
still check command-specific schemas, the error limit, diagnostic ordering,
registered codes, source-dependent constraints, process status and stderr.

These components are not the full harness and cannot substantiate a
conformance claim. The caller must discover tests, resolve package paths,
validate NDJSON, run the IUT and manage the staged command sequence and
configurations. The matcher only judges supplied normalized diagnostics and
process exit code; it does not validate the complete command protocol.

Run its isolated tests with
`python3 -B -m unittest discover -s tools/conformance -p 'test_*.py'`.
