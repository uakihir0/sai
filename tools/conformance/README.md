# Conformance diagnostic matcher

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

This is not the full harness and cannot substantiate a conformance claim. The
caller must discover tests, parse annotations and directives, resolve package
paths, validate NDJSON, run the IUT and manage the staged command sequence and
configurations. This component only judges the supplied normalized diagnostics
and process exit code; it does not validate the complete command protocol.

Run its isolated tests with
`python3 -B -m unittest discover -s tools/conformance -p 'test_*.py'`.
