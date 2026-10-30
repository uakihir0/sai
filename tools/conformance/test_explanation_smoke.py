import json
import os
import tempfile
import unittest
from pathlib import Path
from subprocess import CompletedProcess
from unittest.mock import patch

from explanation_smoke import (
    CHECK_GATE_HELP,
    RECORDS,
    _appendix_summary,
    _compact_summary,
    _parse_record,
    _source_fence,
    _validate_fallback_diagnostics,
    _validate_example_diagnostics,
    _validate_record,
    run_suite,
)
import run_stage0


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
EXPLANATIONS = REPOSITORY_ROOT / "crates" / "sai_diagnostics" / "src" / "explanations"


def _header(command, root):
    return {
        "type": "header",
        "format": "sai-diagnostics",
        "version": 1,
        "tool": {"name": "sai", "version": "0.1.0"},
        "command": command,
        "root": root,
    }


def _summary(errors=0, exit_code=0):
    return {
        "type": "summary",
        "errors": errors,
        "warnings": 0,
        "infos": 0,
        "shown_errors": errors,
        "suppressed": {"cascade": 0, "duplicate": 0, "dependency": 0},
        "exit_code": exit_code,
    }


def _diagnostic(
    code, *, line=1, path="src/lib.sai", data=None,
    file_package=run_stage0.ROOT_ID, message="test diagnostic",
):
    primary = None
    if path is not None:
        primary = {
            "file": {"path": path, "package": file_package, "snapshot": None},
            "start": {"byte": 0, "line": line, "col": 1, "utf16_col": 1},
            "end": {"byte": 1, "line": line, "col": 2, "utf16_col": 2},
        }
    return {
        "type": "diagnostic",
        "code": code,
        "severity": "error",
        "level": None,
        "message": message,
        "primary": primary,
        "primary_label": "",
        "labels": [],
        "children": (
            [{"kind": "help", "message": CHECK_GATE_HELP, "location": None}]
            if code == "FEATURE_NOT_IMPLEMENTED" else []
        ),
        "fixes": [],
        "related": [],
        "data": {} if data is None else data,
        "node": None,
        "symbol": None,
        "package": None,
    }


def _stdout(*records):
    return b"".join(
        json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        for record in records
    )


class ExplanationMarkdownTests(unittest.TestCase):
    def test_current_selected_records_have_expected_structure(self):
        for code, expected in RECORDS.items():
            with self.subTest(code=code):
                markdown = (EXPLANATIONS / f"{code.lower()}.md").read_text(encoding="utf-8")
                table, summary, example, info, fix = _parse_record(markdown, code=code)
                self.assertEqual(table["Class"], expected["class"])
                self.assertEqual(table["Severity"], expected["severity"])
                self.assertEqual(table["Stage"], str(expected["stage"]))
                self.assertEqual(table["Owner"], expected["owner"])
                self.assertEqual(info, "sai noncanonical" if code == "INVALID_ESCAPE" else "sai")
                self.assertTrue(summary)
                self.assertTrue(example.endswith("\n"))
                self.assertTrue(fix.endswith("\n"))

    def test_source_fence_accepts_commonmark_tildes_and_longer_closers(self):
        self.assertEqual(
            _source_fence("~~~sai\npub const X: U32 = 1\n~~~~\n", role="Fix"),
            ("pub const X: U32 = 1\n", "sai"),
        )
        self.assertEqual(
            _source_fence("  ````sai\n  pub const X: U32 = 1\n   ````\n", role="Fix"),
            ("pub const X: U32 = 1\n", "sai"),
        )

    def test_source_fence_rejects_mismatched_closer(self):
        with self.assertRaisesRegex(ValueError, "one supported fenced block"):
            _source_fence("~~~sai\npub const X: U32 = 1\n```\n", role="Fix")

    def test_full_summary_matches_appendix_not_only_compact_list_prefix(self):
        code = "DUPLICATE_BINDING"
        canonical = _appendix_summary(REPOSITORY_ROOT / "docs", code=code)
        self.assertEqual(
            canonical,
            "Two bindings of one name in one scope: module scope, block, parameter "
            "list, generic parameter list, pattern, interface (§4.4.3, D-MOD-03, N-A-8)",
        )

    def test_rejects_out_of_order_sections(self):
        markdown = (
            "# SAMPLE\n\n"
            "| Class | Severity | Stage | Owner | Since | Status |\n"
            "|---|---|---|---|---|---|\n"
            "| hard | error | 0 | spec/02-lexical-structure.md | 0.1.0 | active |\n\n"
            "## Explanation\n\nText.\n\n## Summary\n\nText.\n\n"
            "## Example\n\n```sai\npub const VALUE: U32 = 1\n```\n\n"
            "## Fix\n\n```sai\npub const VALUE: U32 = 2\n```\n"
        )
        with self.assertRaisesRegex(ValueError, "sections are missing or out of order"):
            _parse_record(markdown, code="SAMPLE")

    def test_list_summary_transform_removes_markdown_and_caps_unicode(self):
        self.assertEqual(_compact_summary("See [the guide](docs/guide.md) and `code`."), "See the guide and code")
        compact = _compact_summary("測" * 90)
        self.assertEqual(len(compact), 80)
        self.assertTrue(compact.endswith("…"))


class ExampleDiagnosticTests(unittest.TestCase):
    def test_literal_out_of_range_does_not_accept_the_check_stage_gate(self):
        expected = [{
            "file": "src/lib.sai",
            "line": 1,
            "severity": "error",
            "code": "LITERAL_OUT_OF_RANGE",
        }]
        records = [
            _diagnostic("LITERAL_OUT_OF_RANGE", line=1),
            _diagnostic(
                "FEATURE_NOT_IMPLEMENTED",
                path=None,
                data={"stage": 0, "feature": "sai check"},
                message="stage 0 feature `sai check` is not implemented",
            ),
        ]
        with self.assertRaisesRegex(ValueError, "not marked by the Example"):
            _validate_example_diagnostics(
                records,
                code="LITERAL_OUT_OF_RANGE",
                command="check",
                expected=expected,
            )

    def test_invalid_escape_allows_only_the_known_check_stage_gate(self):
        expected = [{
            "file": "src/lib.sai",
            "line": 1,
            "severity": "error",
            "code": "INVALID_ESCAPE",
        }]
        records = [
            _diagnostic("INVALID_ESCAPE", line=1),
            _diagnostic(
                "FEATURE_NOT_IMPLEMENTED",
                path=None,
                data={"stage": 0, "feature": "sai check"},
                message="stage 0 feature `sai check` is not implemented",
            ),
        ]
        _validate_example_diagnostics(
            records, code="INVALID_ESCAPE", command="check", expected=expected,
        )

    def test_rejects_repeated_known_stage_gate(self):
        expected = [{
            "file": "src/lib.sai",
            "line": 1,
            "severity": "error",
            "code": "INVALID_ESCAPE",
        }]
        gate = _diagnostic(
            "FEATURE_NOT_IMPLEMENTED",
            path=None,
            data={"stage": 0, "feature": "sai check"},
            message="stage 0 feature `sai check` is not implemented",
        )
        with self.assertRaisesRegex(ValueError, "repeated the accepted Stage 0 feature gate"):
            _validate_example_diagnostics(
                [
                    _diagnostic("INVALID_ESCAPE"),
                    gate,
                    gate,
                ],
                code="INVALID_ESCAPE",
                command="check",
                expected=expected,
            )

    def test_rejects_unmarked_example_diagnostics(self):
        expected = [{
            "file": "src/lib.sai",
            "line": 1,
            "severity": "error",
            "code": "DUPLICATE_BINDING",
        }]
        records = [
            _diagnostic("DUPLICATE_BINDING", line=1),
            _diagnostic("UNKNOWN_FIELD", line=2),
        ]
        with self.assertRaisesRegex(ValueError, "not marked by the Example"):
            _validate_example_diagnostics(
                records, code="DUPLICATE_BINDING", command="check", expected=expected,
            )


class EmptyCommandSummaryTests(unittest.TestCase):
    def test_empty_explain_summary_requires_no_suppressed_diagnostics(self):
        summary = {
            **_summary(),
            "type": "summary",
            "shown_errors": 0,
            "suppressed": {"cascade": 0, "duplicate": 1, "dependency": 0},
        }
        with self.assertRaisesRegex(ValueError, "returned a nonempty summary"):
            run_stage0._validate_empty_summary(summary, command="sai explain --list")

    def test_registry_list_rejects_suppressed_diagnostic_counts(self):
        summary = {
            **_summary(),
            "shown_errors": 0,
            "suppressed": {"cascade": 0, "duplicate": 1, "dependency": 0},
        }
        completed = CompletedProcess(
            [],
            0,
            _stdout(
                _header("explain", None),
                {"type": "result", "kind": "explain.list", "data": {"codes": []}},
                summary,
            ),
            b"",
        )
        with patch.object(run_stage0, "_run_iut", return_value=completed):
            with self.assertRaisesRegex(ValueError, "returned a nonempty summary"):
                run_stage0._read_registry_entries(Path("/fake/sai"), timeout=1)

    def test_empty_summary_ignores_future_optional_members(self):
        run_stage0._validate_empty_summary(
            {
                **_summary(),
                "type": "summary",
                "shown_errors": 0,
                "suppressed": {"cascade": 0, "duplicate": 0, "dependency": 0},
                "future_optional_member": True,
            },
            command="sai explain --list",
        )


class ExplanationSmokeRunnerTests(unittest.TestCase):
    def setUp(self):
        if os.name == "nt":
            self.skipTest("mocked executable resolution currently uses a POSIX file mode")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.iut = self.root / "fake-sai"
        self.iut.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.iut.chmod(0o755)
        self.calls = []
        self.build_duplicate = False

    def _explanation_data(self, code):
        markdown = (EXPLANATIONS / f"{code.lower()}.md").read_text(encoding="utf-8")
        _, summary, _, _, _ = _parse_record(markdown, code=code)
        metadata = RECORDS[code]
        return {
            "code": code,
            **metadata,
            "default_level": None,
            "since": "0.1.0",
            "status": "active",
            "superseded_by": None,
            "summary": summary,
            "markdown": markdown,
        }

    def _registry_response(self):
        entries = [{
            "code": "FEATURE_NOT_IMPLEMENTED",
            "class": "hard",
            "summary": "A requested feature is not implemented.",
        }]
        for code in sorted(RECORDS):
            data = self._explanation_data(code)
            entries.append({
                "code": code,
                "class": data["class"],
                "summary": _compact_summary(data["summary"]),
            })
        entries.sort(key=lambda entry: entry["code"].encode("utf-8"))
        return CompletedProcess(
            [],
            0,
            _stdout(
                _header("explain", None),
                {"type": "result", "kind": "explain.list", "data": {"codes": entries}},
                _summary(),
            ),
            b"",
        )

    def _record_response(self, code):
        return CompletedProcess(
            [],
            0,
            _stdout(
                _header("explain", None),
                {"type": "result", "kind": "explain", "data": self._explanation_data(code)},
                _summary(),
            ),
            b"",
        )

    def _check_response(self, code, *, failed, include_gate=False):
        diagnostics = []
        if include_gate:
            diagnostics.append(_diagnostic(
                "FEATURE_NOT_IMPLEMENTED",
                path=None,
                data={"stage": 0, "feature": "sai check"},
                message="stage 0 feature `sai check` is not implemented",
            ))
        if failed:
            if code == "DUPLICATE_BINDING":
                diagnostics.append(_diagnostic(code, line=2))
            elif code == "TYPE_MISMATCH":
                diagnostics.append(_diagnostic(code, line=1))
            else:
                diagnostics.append(_diagnostic(code, line=1))
        exit_code = 1 if diagnostics else 0
        records = [_header("check", run_stage0.ROOT_ID), *diagnostics]
        if exit_code == 0:
            records.append({
                "type": "result",
                "kind": "check",
                "data": {
                    "target": "wasm32-core",
                    "profile": "dev",
                    "tests": False,
                    "packages": 1,
                    "fixes_applied": 0,
                    "changed_files": [],
                },
            })
        records.append(_summary(errors=len(diagnostics), exit_code=exit_code))
        return CompletedProcess([], exit_code, _stdout(*records), b"")

    def _fmt_response(self, source):
        if b"\\q" in source:
            diagnostics = [_diagnostic(
                "INVALID_ESCAPE", path="<stdin>", file_package=None,
            )]
            result = {
                "type": "result",
                "kind": "fmt",
                "data": {
                    "check": False,
                    "files": 1,
                    "changed": [],
                    "refused": ["<stdin>"],
                    "output": None,
                },
            }
            return CompletedProcess(
                [], 1,
                _stdout(
                    _header("fmt", None), *diagnostics, result,
                    _summary(errors=1, exit_code=1),
                ),
                b"",
            )
        result = {
            "type": "result",
            "kind": "fmt",
            "data": {
                "check": False,
                "files": 1,
                "changed": [],
                "refused": [],
                "output": source.decode("utf-8"),
            },
        }
        return CompletedProcess(
            [], 0, _stdout(_header("fmt", None), result, _summary()), b"",
        )

    def _run_iut(self, iut, argv, cwd, timeout, *, state_root=None, stdin_data=None):
        self.calls.append(tuple(argv))
        if argv == ["--format=json", "explain", "--list"]:
            return self._registry_response()
        if len(argv) == 3 and argv[:2] == ["--format=json", "explain"]:
            return self._record_response(argv[2])
        if argv[-3:] == ["--stdin-kind", "source", "-"]:
            return self._fmt_response(stdin_data or b"")
        if argv[-2:] == ["check", "--jobs=1"]:
            source = (Path(cwd) / "src" / "lib.sai").read_bytes()
            if b"VALUE: U32 = 1" in source:
                if self.build_duplicate:
                    return self._check_response("DUPLICATE_BINDING", failed=False)
                return self._check_response("DUPLICATE_BINDING", failed=True)
            if b"COUNT: I32 = true" in source:
                return self._check_response("TYPE_MISMATCH", failed=True)
            if b"\\q" in source:
                return self._check_response(
                    "INVALID_ESCAPE", failed=True, include_gate=True,
                )
            if b"VALUE: U8 = 999" in source:
                return self._check_response("LITERAL_OUT_OF_RANGE", failed=True)
            return self._check_response("DUPLICATE_BINDING", failed=False)
        if len(argv) >= 2 and argv[-2:] == ["--target", "wasm32-core"]:
            source = (Path(cwd) / "src" / "lib.sai").read_bytes()
            if b"VALUE: U32 = 1" in source:
                code, line = "DUPLICATE_BINDING", 2
            elif b"COUNT: I32 = true" in source:
                code, line = "TYPE_MISMATCH", 1
            else:
                code, line = "INVALID_ESCAPE", 1
            diagnostic = _diagnostic(code, line=line)
            return CompletedProcess(
                [], 1,
                _stdout(
                    _header("build", run_stage0.ROOT_ID),
                    diagnostic,
                    _summary(errors=1, exit_code=1),
                ),
                b"",
            )
        raise AssertionError(f"unexpected command: {argv!r}")

    def test_runs_selected_records_through_check_fix_and_formatter(self):
        with patch.object(run_stage0, "_run_iut", side_effect=self._run_iut):
            report = run_suite(self.iut)
        self.assertEqual(report["mode"], "partial-smoke")
        self.assertIs(report["claim"], False)
        self.assertEqual((report["tests"], report["passed"], report["failed"]), (4, 4, 0))
        self.assertEqual(
            [result["test"] for result in report["results"]],
            [f"sai explain {code}" for code in RECORDS],
        )
        self.assertEqual({metadata["stage"] for metadata in RECORDS.values()}, {0, 1})
        self.assertEqual(report["outcome"], "pass")
        self.assertTrue(any(call[1:3] == ("explain", "--list") for call in self.calls))
        self.assertFalse(any("build" in call for call in self.calls))
        self.assertTrue(all(result["outcome"] == "pass" for result in report["results"]))

    def test_rejects_json_and_markdown_summary_drift_beyond_compact_prefix(self):
        code = "DUPLICATE_BINDING"
        data = self._explanation_data(code)
        original = data["summary"]
        altered = original + " A changed sentence beyond the compact list prefix."
        data["summary"] = altered
        data["markdown"] = data["markdown"].replace(
            f"## Summary\n\n{original}",
            f"## Summary\n\n{altered}",
            1,
        )
        _, _, _, _, _ = _parse_record(data["markdown"], code=code)
        completed = CompletedProcess(
            [],
            0,
            _stdout(
                _header("explain", None),
                {"type": "result", "kind": "explain", "data": data},
                _summary(),
            ),
            b"",
        )
        entry = {
            "code": code,
            "class": data["class"],
            "summary": _compact_summary(original),
        }
        self.assertEqual(_compact_summary(altered), entry["summary"])
        with self.assertRaisesRegex(ValueError, "does not match Appendix A"):
            _validate_record(
                completed,
                code=code,
                list_entry=entry,
                docs_root=REPOSITORY_ROOT / "docs",
            )

    def test_record_validation_allows_a_fix_that_removes_a_source_line(self):
        code = "DUPLICATE_BINDING"
        data = self._explanation_data(code)
        data["markdown"] = data["markdown"].replace(
            "```sai\npub const VALUE: U32 = 0\n"
            "pub const OTHER: U32 = 1\n```",
            "```sai\npub const VALUE: U32 = 0\n```",
            1,
        )
        completed = CompletedProcess(
            [],
            0,
            _stdout(
                _header("explain", None),
                {"type": "result", "kind": "explain", "data": data},
                _summary(),
            ),
            b"",
        )
        entry = {
            "code": code,
            "class": data["class"],
            "summary": _compact_summary(data["summary"]),
        }
        _, _, fix, _ = _validate_record(
            completed,
            code=code,
            list_entry=entry,
            docs_root=REPOSITORY_ROOT / "docs",
        )
        self.assertEqual(fix, "pub const VALUE: U32 = 0\n")

    def test_fallback_accepts_other_marked_check_diagnostic_before_build_target(self):
        expected = [
            {
                "file": "src/lib.sai",
                "line": 1,
                "severity": "error",
                "code": "UNKNOWN_FIELD",
            },
            {
                "file": "src/lib.sai",
                "line": 2,
                "severity": "error",
                "code": "DUPLICATE_BINDING",
            },
        ]
        _validate_fallback_diagnostics(
            [_diagnostic("UNKNOWN_FIELD", line=1)],
            [_diagnostic("DUPLICATE_BINDING", line=4)],
            code="DUPLICATE_BINDING",
            expected=expected,
        )

    def test_fallback_rejects_check_gate_reported_by_build(self):
        gate = _diagnostic(
            "FEATURE_NOT_IMPLEMENTED",
            path=None,
            data={"stage": 0, "feature": "sai check"},
            message="stage 0 feature `sai check` is not implemented",
        )
        with self.assertRaisesRegex(ValueError, "do not match Example markers"):
            _validate_fallback_diagnostics(
                [],
                [_diagnostic("INVALID_ESCAPE"), gate],
                code="INVALID_ESCAPE",
                expected=[{
                    "file": "src/lib.sai",
                    "line": 1,
                    "severity": "error",
                    "code": "INVALID_ESCAPE",
                }],
            )

    def test_fallback_rejects_non_error_occurrences_of_target_code(self):
        warning = _diagnostic("INVALID_ESCAPE")
        warning["severity"] = "warning"
        with self.assertRaisesRegex(ValueError, "did not report only INVALID_ESCAPE errors"):
            _validate_fallback_diagnostics(
                [],
                [_diagnostic("INVALID_ESCAPE"), warning],
                code="INVALID_ESCAPE",
                expected=[{
                    "file": "src/lib.sai",
                    "line": 1,
                    "severity": "error",
                    "code": "INVALID_ESCAPE",
                }],
            )

    def test_uses_build_when_check_does_not_report_the_example_code(self):
        self.build_duplicate = True
        with patch.object(run_stage0, "_run_iut", side_effect=self._run_iut):
            report = run_suite(self.iut)
        self.assertEqual(report["outcome"], "pass")
        self.assertIn(("--format=json", "--max-errors=0", "build", "--target", "wasm32-core"),
                      self.calls)


if __name__ == "__main__":
    unittest.main()
