import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

from diagnostic_matcher import compare


def diagnostic(code, severity="error", file="src/lib.sai", line=2, **extra):
    return {"file": file, "line": line, "severity": severity, "code": code, **extra}


def request(**changes):
    result = {
        "kind": "check-pass",
        "implementation": "reference",
        "exit_code": 0,
        "expected": [],
        "actual": [],
    }
    result.update(changes)
    return result


class DiagnosticMatcherTests(unittest.TestCase):
    def test_check_pass_and_fail(self):
        self.assertEqual(compare(request())["outcome"], "pass")
        error = diagnostic("EXPECTED_TOKEN")
        self.assertEqual(
            compare(request(kind="check-fail", expected=[error], actual=[error], exit_code=1))["outcome"],
            "pass",
        )
        self.assertEqual(
            compare(request(kind="build-fail-check", expected=[error], actual=[error], exit_code=1))["failures"],
            ["wrong-acceptance-outcome"],
        )
        self.assertEqual(compare(request(kind="check-fail"))["failures"], ["wrong-acceptance-outcome"])

    def test_multiset_and_reference_mismatch(self):
        error = diagnostic("EXPECTED_TOKEN")
        result = compare(request(kind="check-fail", expected=[error, error], actual=[error], exit_code=1))
        self.assertEqual(result["outcome"], "fail")
        self.assertEqual(result["failures"], ["diagnostic-mismatch"])
        self.assertEqual(result["deviations"], [{"kind": "missing", "diagnostic": error}])
        result = compare(request(kind="check-fail", expected=[error], actual=[error, error, error], exit_code=1))
        self.assertEqual([entry["kind"] for entry in result["deviations"]], ["unexpected", "unexpected"])

    def test_other_implementation_reports_sorted_deviations(self):
        expected = [diagnostic("EXPECTED_TOKEN"), diagnostic("MISSING_ROOT", file=None, line=None)]
        actual = [diagnostic("OTHER_ERROR", file="src/z.sai", line=9)]
        result = compare(request(kind="check-fail", implementation="other", expected=expected, actual=actual, exit_code=1))
        self.assertEqual(result["outcome"], "pass-with-deviations")
        self.assertEqual(
            [(entry["kind"], entry["diagnostic"]["code"]) for entry in result["deviations"]],
            [("missing", "MISSING_ROOT"), ("missing", "EXPECTED_TOKEN"), ("unexpected", "OTHER_ERROR")],
        )

    def test_wrong_outcome_fails_even_for_other_implementation(self):
        error = diagnostic("EXPECTED_TOKEN")
        result = compare(request(implementation="other", actual=[error], exit_code=1))
        self.assertEqual(result["outcome"], "fail")
        self.assertEqual(result["failures"], ["wrong-acceptance-outcome"])
        self.assertEqual(result["deviations"][0]["kind"], "unexpected")

    def test_staged_requires_error_and_stage_above_claim(self):
        feature = diagnostic("FEATURE_NOT_IMPLEMENTED", data={"stage": 1})
        result = compare(request(actual=[feature], exit_code=1, staged_claim_stage=0))
        self.assertEqual(result["outcome"], "staged")
        self.assertEqual(compare(request(staged_claim_stage=0))["outcome"], "pass")
        for changed in (
            diagnostic("FEATURE_NOT_IMPLEMENTED", data={"stage": 0}),
            diagnostic("FEATURE_NOT_IMPLEMENTED", data={"stage": True}),
            diagnostic("FEATURE_NOT_IMPLEMENTED", severity="warning", data={"stage": 1}),
            diagnostic("FEATURE_NOT_IMPLEMENTED", data={"stage": 8}),
            diagnostic("FEATURE_NOT_IMPLEMENTED"),
        ):
            with self.subTest(changed=changed):
                exit_code = 1 if changed["severity"] == "error" else 0
                self.assertNotEqual(compare(request(actual=[changed], exit_code=exit_code, staged_claim_stage=0))["outcome"], "staged")

    def test_exit_failures_precede_staged_and_diagnostic_matching(self):
        error = diagnostic("EXPECTED_TOKEN")
        feature = diagnostic("FEATURE_NOT_IMPLEMENTED", data={"stage": 1})
        for exit_code in (2, 3, 4, 130, -11, 255):
            for actual in ([], [feature]):
                with self.subTest(exit_code=exit_code, actual=actual):
                    result = compare(request(exit_code=exit_code, actual=actual, staged_claim_stage=0))
                    self.assertEqual(result["outcome"], "fail")
                    self.assertEqual(result["failures"], ["abnormal-exit"])
        for value in (
            request(kind="check-fail", expected=[error], actual=[error], exit_code=0),
            request(actual=[], exit_code=1),
            request(actual=[feature], exit_code=0, staged_claim_stage=0),
        ):
            with self.subTest(value=value):
                self.assertEqual(compare(value)["failures"], ["exit-code-mismatch"])

    def test_root_relative_diagnostic_paths(self):
        for file in ("src/lib.sai", "sai.package", "deps/日本語.sai", "src/a:b.sai"):
            with self.subTest(file=file):
                warning = diagnostic("WARN_CODE", severity="warning", file=file, line=1)
                self.assertEqual(compare(request(expected=[warning], actual=[warning]))["outcome"], "pass")
        for file in ("", "/tmp/test.sai", "../test.sai", "src/../test.sai", "./test.sai", "src//test.sai", "src/", "C:/test.sai", "C:test.sai", "\\\\host\\share", "src\\test.sai", "src/\0test.sai"):
            for field in ("expected", "actual"):
                with self.subTest(file=file, field=field):
                    with self.assertRaises(ValueError):
                        compare(request(**{field: [diagnostic("BAD", file=file)]}))

    def test_ice_takes_precedence_over_staged(self):
        feature = diagnostic("FEATURE_NOT_IMPLEMENTED", data={"stage": 2})
        ice = diagnostic("INTERNAL_COMPILER_ERROR", file=None, line=None)
        for changes in ({"exit_code": 101}, {"actual": [feature, ice]}):
            with self.subTest(changes=changes):
                result = compare(request(staged_claim_stage=0, **changes))
                self.assertEqual(result["outcome"], "fail")
                self.assertEqual(result["failures"], ["internal-compiler-error"])

    def test_invalid_inputs_are_rejected(self):
        for changes in (
            {"kind": "run-pass"},
            {"implementation": "unknown"},
            {"exit_code": True},
            {"staged_claim_stage": True},
            {"staged_claim_stage": None},
            {"expected": [diagnostic("BAD", file=None, line=2)]},
            {"actual": [diagnostic("BAD", line=True)]},
            {"actual": [{"severity": "error", "code": "BAD"}]},
        ):
            with self.subTest(changes=changes):
                with self.assertRaises(ValueError):
                    compare(request(**changes))

    def test_cli_contract(self):
        script = Path(__file__).with_name("diagnostic_matcher.py")
        for value, expected_exit in ((request(), 0), (request(kind="check-fail"), 1), ({}, 2)):
            with self.subTest(expected_exit=expected_exit):
                process = subprocess.run(
                    [sys.executable, "-B", str(script)],
                    input=json.dumps(value),
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(process.returncode, expected_exit)
                if expected_exit == 2:
                    self.assertIn("invalid input:", process.stderr)
                else:
                    self.assertEqual(process.stderr, "")
                    self.assertIn("outcome", json.loads(process.stdout))

    def test_cli_rejects_non_json_constants_and_duplicate_keys(self):
        script = Path(__file__).with_name("diagnostic_matcher.py")
        valid = json.dumps(request())
        for value in (
            valid[:-1] + ', "extra": NaN}',
            valid[:-1] + ', "extra": Infinity}',
            valid[:-1] + ', "extra": -Infinity}',
            valid[:-1] + ', "kind": "check-pass"}',
            valid[:-1] + ', "extra": {"nested": 1, "nested": 2}}',
            valid[:-1] + ', "staged_claim_stage": null}',
        ):
            with self.subTest(value=value):
                process = subprocess.run(
                    [sys.executable, "-B", str(script)], input=value,
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(process.returncode, 2)
                self.assertEqual(process.stdout, "")
                self.assertIn("invalid input:", process.stderr)
        process = subprocess.run(
            [sys.executable, "-B", str(script)], input=valid[:-1] + ', "extra": 1e400}',
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(process.returncode, 0)

    def test_cli_rejects_invalid_utf8_unpaired_surrogates_and_oversized_exponents(self):
        script = Path(__file__).with_name("diagnostic_matcher.py")
        valid = json.dumps(request()).encode("ascii")
        malformed = (
            valid[:-1] + b', "extra": "\xff"}',
            valid[:-1] + b', "extra": "\xed\xa0\x80"}',
            valid[:-1] + b', "extra": "\\ud800"}',
            valid[:-1] + b', "extra": ["\\udfff"]}',
            valid[:-1] + b', "extra": {"\\ud800": 1}}',
            valid[:-1] + b', "extra": {"\\ud800": 1, "\\ud800": 2}}',
            valid[:-1] + b', "extra": 1e' + b"9" * 200 + b"}",
            valid[:-1] + b', "extra": 1e-' + b"9" * 200 + b"}",
        )
        for value in malformed:
            with self.subTest(value=value):
                process = subprocess.run(
                    [sys.executable, "-B", str(script)], input=value,
                    capture_output=True, check=False,
                    env={**os.environ, "PYTHONIOENCODING": "utf-8:surrogateescape"},
                )
                self.assertEqual(process.returncode, 2)
                self.assertEqual(process.stdout, b"")
                self.assertIn(b"invalid input:", process.stderr)
                self.assertNotIn(b"Traceback", process.stderr)

    def test_valid_unicode_and_surrogate_pair_are_preserved(self):
        script = Path(__file__).with_name("diagnostic_matcher.py")
        expected = diagnostic("UNICODE_ERROR", file="src/😀.sai")
        value = request(kind="check-fail", implementation="other", expected=[expected],
                        actual=[diagnostic("OTHER_ERROR")], exit_code=1)
        process = subprocess.run(
            [sys.executable, "-B", str(script)], input=json.dumps(value).encode("ascii"),
            capture_output=True, check=False,
        )
        self.assertEqual(process.returncode, 0)
        self.assertEqual(process.stderr, b"")
        result = json.loads(process.stdout.decode("utf-8"))
        self.assertEqual(result["outcome"], "pass-with-deviations")
        self.assertEqual(result["deviations"][0]["diagnostic"]["file"], "src/😀.sai")

    def test_output_is_portable_across_stdout_encodings(self):
        script = Path(__file__).with_name("diagnostic_matcher.py")
        expected = diagnostic("ERROR", file="src/日本語-café.sai")
        value = request(kind="check-fail", implementation="other", expected=[expected],
                        actual=[diagnostic("OTHER_ERROR")], exit_code=1)
        for encoding in ("ascii", "latin-1"):
            with self.subTest(encoding=encoding):
                process = subprocess.run(
                    [sys.executable, "-B", str(script)], input=json.dumps(value).encode("ascii"),
                    capture_output=True, check=False,
                    env={**os.environ, "PYTHONIOENCODING": encoding},
                )
                self.assertEqual(process.returncode, 0)
                self.assertEqual(process.stderr, b"")
                result = json.loads(process.stdout.decode("utf-8"))
                self.assertEqual(result["deviations"][0]["diagnostic"]["file"], "src/日本語-café.sai")

    def test_compare_rejects_unpaired_surrogates_in_any_input_string(self):
        for value in (
            request(expected=[diagnostic("\ud800")]),
            request(actual=[diagnostic("ERROR", file="src/\udfff.sai")]),
            {**request(), "extra": {"\ud800": 1}},
        ):
            with self.subTest(value=ascii(value)):
                with self.assertRaises(ValueError):
                    compare(value)


if __name__ == "__main__":
    unittest.main()
