import json
import tempfile
import unittest
from pathlib import Path

from coverage import (
    CoverageError,
    _build_inventory,
    _document_index,
    _leaf_sections,
    _parse_code_exemptions,
    _scan_diagnostic_tables,
    _parse_lint_codes,
    _parse_panic_kinds,
    _parse_section_exemptions,
    _markdown_table_cells,
    _scan_headings,
    _section_ids,
    _canonical_json,
    build_report,
    write_report,
)


class CoverageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self._write_repository()

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def scan_diagnostic_document(self, content, registry=None):
        path = self.write("docs/spec/02-document.md", content)
        document = "spec/02"
        return _scan_diagnostic_tables(
            {path.resolve(): document},
            {path.resolve(): _scan_headings(content, document)},
            set(registry or {"EXPECTED_TOKEN", "UNUSED_VARIABLE"}),
        )

    def _write_repository(self):
        for area, maximum in (("spec", 16), ("package", 4), ("impl", 15)):
            for number in range(1, maximum + 1):
                if area == "spec" and number == 1:
                    body = """# 1. Introduction
## 1.4 Designations
### 1.4.5 Implemented stages
#### 1.4.5.1 Feature stage
### 1.4.6 Other section
## 1.5 Limits
### 1.5.4 Resource limits
## 1.6 Determinism
### 1.6.1 Stable output
"""
                elif area == "spec" and number == 10:
                    body = """# 10. Errors
## 10.4 Panics
### 10.4.4 Panic messages
| Kind | Message |
|---|---|
| `Overflow(Add)` | text |
| `Contract(id), Invariant(id), Refinement(id)` | text |
| `StackOverflow`, `OutOfMemory` | text |
### 10.4.5 Panic locations
### 10.5 Reporting
"""
                elif area == "impl" and number == 12:
                    body = """# 12. Tooling
## 12.4 Graphs
### 12.4.1 Queries
## 12.5 Semantic protocol
### 12.5.1 Transport
### 12.5.2 Errors
"""
                elif area == "impl" and number == 13:
                    body = """# 13. CLI
## 13.3 Command output
### 13.3.1 Syntax
## 13.6 Commands
### 13.6.1 Check
## 13.7 Environment
### 13.7.1 Variables
## 13.8 Result records
### 13.8.1 Summary
"""
                elif area == "impl" and number == 15:
                    body = """# 15. Conformance
## 15.8 Generated tests
### 15.8.4 Diagnostic records
## 15.12 Compiler tests
### 15.12.2 Compiler-level tests
### 15.12.3 ICE injection
### 15.12.5 CLI
"""
                elif area == "spec" and number == 15:
                    body = """# 15. Attributes and lints
## 15.17 Diagnostic registry
### 15.17.2 The lints of v0.1
| Code | Default | Owner | Stage | Summary |
|---|---|---|---|---|
| `UNUSED_VARIABLE` | `warn` | this chapter | 0 | unused binding |
"""
                else:
                    body = (
                        f"# {number}. Document\n"
                        f"## {number}.1 Parent\n"
                        f"### {number}.1.1 Leaf\n"
                        f"### {number}.1.2 Leaf\n"
                    )
                self.write(f"docs/{area}/{number:02d}-document.md", body)

        self.write(
            "crates/sai_diagnostics/src/diagnostic_code_names.txt",
            "EXPECTED_TOKEN FEATURE_NOT_IMPLEMENTED IMPLEMENTATION_LIMIT_EXCEEDED "
            "INTERNAL_COMPILER_ERROR INVALID_ESCAPE REF_ONLY_CODE SIDECAR_ONLY "
            "SIDECAR_SOURCE_EXPECTED SIDECAR_SOURCE_FIXED SOURCE_READ_FAILED\n",
        )
        registry = self.root / "crates/sai_diagnostics/src/diagnostic_code_names.txt"
        registry.write_text(
            registry.read_text(encoding="utf-8") + "UNUSED_VARIABLE\n",
            encoding="utf-8",
        )
        self.write("tests/conformance/exempt-sections.txt", "")
        self.write("tests/conformance/exempt-codes.txt", "")
        self.write(
            "tests/conformance/cli/pass.sai",
            """//@ kind: check-fail
//@ stage: 0
//@ spec: spec/02 §2.1.1, 11 §11.1.1
//@ diag: error FEATURE_NOT_IMPLEMENTED
//@ diag: error FEATURE_NOT_IMPLEMENTED
const A: I32 = 1 //~ error EXPECTED_TOKEN
""",
        )
        self.write(
            "tests/conformance/cli/ref-only.sai",
            """//@ kind: check-fail
//@ stage: 0
//@ spec: spec/02 §2.1.1
//@ reference-only: depends on an implementation limit above the portable minimum
const B: I32 = 2 //~ error INVALID_ESCAPE
const C: I32 = 3 //~ error REF_ONLY_CODE
""",
        )
        self.write(
            "tests/conformance/cli/package.t/sai.package",
            """//@ kind: check-fail
//@ stage: 0
//@ spec: impl/13 §13.6.1
""",
        )
        self.write(
            "tests/conformance/cli/package.t/src/lib.sai",
            "const C: I32 = 3 //~ error EXPECTED_TOKEN\n",
        )
        self.write(
            "tests/conformance/cli/package.t/src/edge.expected.sai",
            "const D: I32 = 4 //~ error SIDECAR_SOURCE_EXPECTED\n",
        )
        self.write(
            "tests/conformance/cli/package.t/src/edge.fixed.sai",
            "const E: I32 = 5 //~ error SIDECAR_SOURCE_FIXED\n",
        )
        self.write(
            "tests/conformance/cli/package.t/expected/fixed.sai",
            "const Ignored: I32 = 4 //~ error SIDECAR_ONLY\n",
        )
        self.write(
            "tests/conformance/cli/package.t/module.wit/world.sai",
            "const AlsoIgnored: I32 = 4 //~ error INVALID_ESCAPE\n",
        )

    def write_golden(self, relative, *codes):
        records = [
            {
                "type": "header",
                "format": "sai-diagnostics",
                "version": 1,
                "tool": {"name": "sai", "version": "0.1.0"},
                "command": "check",
                "root": "src",
            }
        ]
        for code in codes:
            records.append({
                "type": "diagnostic",
                "code": code,
                "severity": "error",
                "level": None,
                "message": "expected diagnostic",
                "primary": None,
                "primary_label": "",
                "labels": [],
                "children": [],
                "fixes": [],
                "related": [],
                "data": {},
                "node": None,
                "symbol": None,
                "package": None,
            })
        records.append({
            "type": "summary",
            "errors": len(codes),
            "warnings": 0,
            "infos": 0,
            "shown_errors": len(codes),
            "suppressed": {"cascade": 0, "duplicate": 0, "dependency": 0},
            "exit_code": 1 if codes else 0,
        })
        content = "".join(
            json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
            for record in records
        )
        return self.write(relative, content)

    def test_heading_scan_ignores_fences_and_only_uses_numbered_leaves(self):
        headings = _scan_headings(
            """## 2.1 Parent
```sai
### 2.1.1 Not a section
```
### 2.1.1 Actual leaf
~~~text
## 2.1.2 Also not a section
~~~
### 2.1.2 Second leaf
""",
            "spec/02",
        )
        self.assertEqual(_section_ids(headings), {"2.1", "2.1.1", "2.1.2"})
        self.assertEqual(_leaf_sections(headings, None), ["2.1.1", "2.1.2"])
        self.assertEqual(_leaf_sections(headings, ("2.1.1",)), ["2.1.1"])

    def test_heading_scan_rejects_unterminated_fences_and_duplicate_numbers(self):
        with self.assertRaisesRegex(CoverageError, "unterminated fenced"):
            _scan_headings("```text\n## 2.1 Hidden\n", "spec/02")
        with self.assertRaisesRegex(CoverageError, "duplicate numbered"):
            _scan_headings("## 2.1 First\n## 2.1 Duplicate\n", "spec/02")

    def test_diagnostic_table_inventory_ignores_fences_and_records_locations(self):
        content = """# 2. Document
## 2.1 Parent
```text
| Code | Severity | Description |
|---|---|---|
| `NOT_REGISTERED` | invalid | ignored fenced table |
```
~~~text
| Code | Severity | Description |
|---|---|---|
| `NOT_REGISTERED` | invalid | also ignored |
~~~
### 2.1.1 Diagnostics
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | A pipe `|` within code is not a column. |
| `UNUSED_VARIABLE` | warning (lint), default `allow` | Unused binding. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            rows,
            [
                {
                    "document": "spec/02",
                    "section": "2.1.1",
                    "line": 16,
                    "code": "EXPECTED_TOKEN",
                    "severity": "error",
                    "description": "A pipe `|` within code is not a column.",
                },
                {
                    "document": "spec/02",
                    "section": "2.1.1",
                    "line": 17,
                    "code": "UNUSED_VARIABLE",
                    "severity": "warning (lint), default `allow`",
                    "description": "Unused binding.",
                },
            ],
        )

    def test_diagnostic_table_inventory_preserves_escaped_final_pipe(self):
        content = """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Description ending with an escaped pipe \\|
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["line"], row["description"]) for row in rows],
            [(5, "Description ending with an escaped pipe \\|")],
        )

    def test_diagnostic_table_inventory_ignores_indented_code_blocks(self):
        content = """# 2. Document
## 2.1 Section
    | Code | Severity | Description |
    |---|---|---|
    | `UNKNOWN_DIAGNOSTIC` | fatal | Indented example, not a table. |
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Actual diagnostic table. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["code"], row["line"]) for row in rows],
            [("EXPECTED_TOKEN", 8)],
        )

    def test_diagnostic_table_inventory_scans_tables_nested_in_list_items(self):
        content = """# 2. Document
## 2.1 Section
- Examples:
  - Nested diagnostic table:
    | Code | Severity | Description |
    |---|---|---|
    | `EXPECTED_TOKEN` | error | Nested table row. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["section"], row["line"], row["code"]) for row in rows],
            [("2.1", 7, "EXPECTED_TOKEN")],
        )

    def test_diagnostic_table_inventory_scans_header_on_list_marker_line(self):
        content = """# 2. Document
## 2.1 Section
- | Code | Severity | Description |
  |---|---|---|
  | `EXPECTED_TOKEN` | error | Header shares its list marker line. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["section"], row["line"], row["code"]) for row in rows],
            [("2.1", 5, "EXPECTED_TOKEN")],
        )

    def test_diagnostic_table_inventory_preserves_list_context_across_blank_lines(self):
        content = """# 2. Document
## 2.1 Section
- Examples:

  | Code | Severity | Description |
  |---|---|---|
  | `EXPECTED_TOKEN` | error | Nested table after a blank line. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["section"], row["line"], row["code"]) for row in rows],
            [("2.1", 7, "EXPECTED_TOKEN")],
        )

    def test_diagnostic_table_inventory_ignores_fences_nested_in_list_items(self):
        content = """# 2. Document
## 2.1 Section
- Parent:
  - Nested item:
    ```text
    | Code | Severity | Description |
    |---|---|---|
    | `NOT_REGISTERED` | fatal | Fenced nested example. |
    ```
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Actual diagnostic table. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["section"], row["line"], row["code"]) for row in rows],
            [("2.1", 12, "EXPECTED_TOKEN")],
        )

    def test_diagnostic_table_inventory_ignores_fence_on_list_marker_line(self):
        content = """# 2. Document
## 2.1 Section
- ```text
  | Code | Severity | Description |
  |---|---|---|
  | `NOT_REGISTERED` | fatal | Fenced example. |
### 2.1.1 Hidden heading inside the list-relative fence.
  ```
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Actual diagnostic table. |
"""
        rows = self.scan_diagnostic_document(content)
        self.assertEqual(
            [(row["section"], row["line"], row["code"]) for row in rows],
            [("2.1", 11, "EXPECTED_TOKEN")],
        )

    def test_diagnostic_header_with_indented_separator_is_not_a_table(self):
        content = """# 2. Document
## 2.1 Section
| Code | Severity | Description |
    |---|---|---|
    | `EXPECTED_TOKEN` | error | This is an indented code example. |
"""
        self.assertEqual(self.scan_diagnostic_document(content), [])

    def test_markdown_table_cells_only_treat_matched_backticks_as_code_spans(self):
        self.assertEqual(
            _markdown_table_cells("| left | middle | unmatched ` run | split here | right |"),
            ["left", "middle", "unmatched ` run", "split here", "right"],
        )
        self.assertEqual(
            _markdown_table_cells("| left | middle | matched `inside|span` here | right |"),
            ["left", "middle", "matched `inside|span` here", "right"],
        )
        self.assertEqual(
            _markdown_table_cells("| left | middle | matched `inside|span\\` here | right |"),
            ["left", "middle", "matched `inside|span\\` here", "right"],
        )
        self.assertEqual(
            _markdown_table_cells("| Code | Severity | Description ending in \\|"),
            ["Code", "Severity", "Description ending in \\|"],
        )
        self.assertEqual(
            _markdown_table_cells("| Code | Severity | Description ending in \\\\|"),
            ["Code", "Severity", "Description ending in \\\\"],
        )

    def test_diagnostic_table_inventory_sorts_documents_and_sections(self):
        second = self.write(
            "docs/spec/03-document.md",
            """# 3. Document
## 3.2 Later
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Later section. |
## 3.1 Earlier
| Code | Severity | Description |
|---|---|---|
| `UNUSED_VARIABLE` | info (lint) | Earlier section. |
""",
        )
        first = self.write(
            "docs/spec/02-document.md",
            """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | warning | First document. |
""",
        )
        rows = _scan_diagnostic_tables(
            {second.resolve(): "spec/03", first.resolve(): "spec/02"},
            {
                second.resolve(): _scan_headings(second.read_text(encoding="utf-8"), "spec/03"),
                first.resolve(): _scan_headings(first.read_text(encoding="utf-8"), "spec/02"),
            },
            {"EXPECTED_TOKEN", "UNUSED_VARIABLE"},
        )
        self.assertEqual(
            [(row["document"], row["section"]) for row in rows],
            [("spec/02", "2.1"), ("spec/03", "3.1"), ("spec/03", "3.2")],
        )

    def test_diagnostic_table_inventory_expands_rows_with_multiple_codes(self):
        rows = self.scan_diagnostic_document(
            """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `UNUSED_VARIABLE`, `EXPECTED_TOKEN` | info (lint) | Shared description. |
"""
        )
        self.assertEqual(
            [(row["code"], row["line"], row["severity"]) for row in rows],
            [
                ("EXPECTED_TOKEN", 5, "info (lint)"),
                ("UNUSED_VARIABLE", 5, "info (lint)"),
            ],
        )

    def test_diagnostic_table_inventory_accepts_documented_code_list_qualifiers(self):
        codes = {
            "UNKNOWN_OPERATION",
            "INVALID_OPERATION_ARGUMENT",
            "INVALID_OPERATION_TARGET",
            "FILE_HAS_SYNTAX_ERRORS",
            "OPERATION_INTRODUCES_ERRORS",
        }
        rows = self.scan_diagnostic_document(
            """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_OPERATION`, `INVALID_OPERATION_ARGUMENT`, `INVALID_OPERATION_TARGET`, `FILE_HAS_SYNTAX_ERRORS`, `OPERATION_INTRODUCES_ERRORS`, and the precondition codes of the operations | error | Operation validation errors. |
""",
            registry=codes,
        )
        self.assertEqual({row["code"] for row in rows}, codes)
        lexer_rows = self.scan_diagnostic_document(
            """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` and every other lexer error | error | Lexer errors. |
"""
        )
        self.assertEqual([row["code"] for row in lexer_rows], ["EXPECTED_TOKEN"])

    def test_diagnostic_table_inventory_rejects_malformed_rows_and_duplicates(self):
        with self.assertRaisesRegex(CoverageError, "invalid separator"):
            self.scan_diagnostic_document(
                """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|left|---|
| `EXPECTED_TOKEN` | error | Description. |
"""
            )
        with self.assertRaisesRegex(CoverageError, "exactly 3 columns"):
            self.scan_diagnostic_document(
                """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Description | unexpected cell |
"""
            )
        with self.assertRaisesRegex(CoverageError, "invalid diagnostic code"):
            self.scan_diagnostic_document(
                """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN`, UNUSED_VARIABLE | error | Malformed code list. |
"""
            )
        with self.assertRaisesRegex(CoverageError, "duplicates EXPECTED_TOKEN"):
            self.scan_diagnostic_document(
                """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | First row. |
| `EXPECTED_TOKEN` | error | First row. |
"""
            )

    def test_diagnostic_table_inventory_checks_registry_and_severity_vocabulary(self):
        with self.assertRaisesRegex(CoverageError, "unregistered diagnostic"):
            self.scan_diagnostic_document(
                """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_DIAGNOSTIC` | error | Unknown code. |
"""
            )
        with self.assertRaisesRegex(CoverageError, "invalid diagnostic severity"):
            self.scan_diagnostic_document(
                """# 2. Document
## 2.1 Section
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | fatal | Unknown severity. |
"""
            )

    def test_document_index_rejects_duplicate_chapter_numbers(self):
        self.write("docs/spec/02-other-name.md", "# 2. Other document\n")
        with self.assertRaisesRegex(CoverageError, "duplicate numbered document: spec/02"):
            _document_index(self.root / "docs")

    def test_covered_scopes_include_only_required_leaves(self):
        aliases, canonical = _document_index(self.root / "docs")
        inventory, _ = _build_inventory(self.root / "docs", aliases, canonical)
        self.assertEqual(
            [entry["id"] for entry in inventory["spec/01"]],
            ["spec/01 §1.4.5.1", "spec/01 §1.5.4", "spec/01 §1.6.1"],
        )
        self.assertEqual(
            [entry["id"] for entry in inventory["impl/12"]],
            ["impl/12 §12.5.1", "impl/12 §12.5.2"],
        )
        self.assertEqual(
            [entry["id"] for entry in inventory["impl/13"]],
            ["impl/13 §13.3.1", "impl/13 §13.6.1", "impl/13 §13.8.1"],
        )

    def test_report_resolves_aliases_counts_portable_codes_and_ignores_sidecars(self):
        self.write(
            "docs/spec/02-document.md",
            """# 2. Document
## 2.1 Parent
### 2.1.1 Leaf
| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | Expected a token. |
""",
        )
        report = build_report(self.root)
        self.assertEqual(
            report["diagnostic_tables"],
            [{
                "document": "spec/02",
                "section": "2.1.1",
                "line": 6,
                "code": "EXPECTED_TOKEN",
                "severity": "error",
                "description": "Expected a token.",
            }],
        )
        records = {test["path"]: test for test in report["fixtures"]}
        portable = records["tests/conformance/cli/pass.sai"]
        self.assertEqual(
            portable["spec"],
            ["spec/02 §2.1.1", "impl/11 §11.1.1"],
        )
        self.assertEqual(portable["diagnostic_codes"],
                         ["EXPECTED_TOKEN", "FEATURE_NOT_IMPLEMENTED"])
        directory = records["tests/conformance/cli/package.t"]
        self.assertEqual(directory["diagnostic_codes"], ["EXPECTED_TOKEN"])
        self.assertEqual(directory["diagnostic_json_goldens"], [])
        self.assertNotIn("SIDECAR_ONLY", directory["diagnostic_codes"])
        self.assertNotIn("SIDECAR_SOURCE_EXPECTED", directory["diagnostic_codes"])
        self.assertNotIn("SIDECAR_SOURCE_FIXED", directory["diagnostic_codes"])
        self.assertFalse(records["tests/conformance/cli/ref-only.sai"]["portable"])
        self.assertEqual(
            next(item for item in report["diagnostic_codes"]
                 if item["code"] == "INVALID_ESCAPE")["tests"],
            [],
        )
        self.assertEqual(
            next(item for item in report["diagnostic_codes"]
                 if item["code"] == "REF_ONLY_CODE")["tests"],
            [],
        )
        for code in ("SIDECAR_SOURCE_EXPECTED", "SIDECAR_SOURCE_FIXED"):
            self.assertEqual(
                next(item for item in report["diagnostic_codes"]
                     if item["code"] == code)["tests"],
                [],
            )
        self.assertFalse(report["claim"])
        self.assertFalse(report["stage_gate_enabled"])
        self.assertNotIn("UNUSED_VARIABLE", report["uncovered"]["hard_code_json_goldens"])

    def test_single_file_json_golden_covers_a_hard_code(self):
        self.write_golden("tests/conformance/cli/pass.json", "EXPECTED_TOKEN")
        report = build_report(self.root)
        entry = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "EXPECTED_TOKEN"
        )
        self.assertEqual(
            entry["goldens"], ["tests/conformance/cli/pass.json"]
        )
        fixture = next(
            item for item in report["fixtures"]
            if item["path"] == "tests/conformance/cli/pass.sai"
        )
        self.assertEqual(
            fixture["diagnostic_json_goldens"],
            [{
                "path": "tests/conformance/cli/pass.json",
                "diagnostic_codes": ["EXPECTED_TOKEN"],
            }],
        )

    def test_directory_json_golden_covers_a_hard_code(self):
        self.write_golden(
            "tests/conformance/cli/package.t/expected/diagnostics.json",
            "EXPECTED_TOKEN",
        )
        report = build_report(self.root)
        entry = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "EXPECTED_TOKEN"
        )
        self.assertEqual(
            entry["goldens"],
            ["tests/conformance/cli/package.t/expected/diagnostics.json"],
        )

    def test_lint_code_in_json_golden_is_excluded_from_hard_code_inventory(self):
        self.write_golden("tests/conformance/cli/pass.json", "UNUSED_VARIABLE")
        report = build_report(self.root)
        self.assertNotIn(
            "UNUSED_VARIABLE",
            {item["code"] for item in report["hard_code_json_goldens"]},
        )
        self.assertNotIn(
            "tests/conformance/cli/pass.json",
            next(
                item for item in report["hard_code_json_goldens"]
                if item["code"] == "EXPECTED_TOKEN"
            )["goldens"],
        )

    def test_missing_json_goldens_leave_sorted_hard_codes_uncovered(self):
        report = build_report(self.root)
        uncovered = report["uncovered"]["hard_code_json_goldens"]
        self.assertEqual(uncovered, sorted(uncovered))
        self.assertIn("EXPECTED_TOKEN", uncovered)
        self.assertNotIn("UNUSED_VARIABLE", uncovered)

    def test_malformed_json_golden_fails_closed(self):
        self.write(
            "tests/conformance/cli/pass.json",
            '{"type":"header","format":"sai-diagnostics"}\n',
        )
        with self.assertRaisesRegex(CoverageError, "invalid diagnostic JSON golden"):
            build_report(self.root)

    def test_unregistered_code_in_json_golden_fails_closed(self):
        self.write_golden("tests/conformance/cli/pass.json", "UNKNOWN_CODE")
        with self.assertRaisesRegex(CoverageError, "unregistered diagnostic"):
            build_report(self.root)

    def test_specialized_single_file_json_golden_is_inventoried(self):
        self.write_golden(
            "tests/conformance/cli/pass.wasm32-core.dev.json",
            "EXPECTED_TOKEN",
        )
        report = build_report(self.root)
        entry = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "EXPECTED_TOKEN"
        )
        self.assertEqual(
            entry["goldens"],
            ["tests/conformance/cli/pass.wasm32-core.dev.json"],
        )

    def test_unselected_single_file_json_fallback_does_not_count_as_coverage(self):
        self.write(
            "tests/conformance/cli/pass.sai",
            """//@ kind: check-fail
//@ stage: 1
//@ targets: wasm32-core
//@ profiles: dev
//@ spec: spec/02 §2.1.1
const A: I32 = 1 //~ error EXPECTED_TOKEN
""",
        )
        self.write_golden("tests/conformance/cli/pass.json", "EXPECTED_TOKEN")
        self.write_golden(
            "tests/conformance/cli/pass.wasm32-core.dev.json",
            "FEATURE_NOT_IMPLEMENTED",
        )
        report = build_report(self.root)
        expected_token = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "EXPECTED_TOKEN"
        )
        self.assertEqual(expected_token["goldens"], [])
        self.assertIn("EXPECTED_TOKEN", report["uncovered"]["hard_code_json_goldens"])
        fixture = next(
            item for item in report["fixtures"]
            if item["path"] == "tests/conformance/cli/pass.sai"
        )
        self.assertEqual(
            fixture["diagnostic_json_goldens"],
            [{
                "path": "tests/conformance/cli/pass.wasm32-core.dev.json",
                "diagnostic_codes": ["FEATURE_NOT_IMPLEMENTED"],
            }],
        )

    def test_profile_specialization_precedes_target_specialization(self):
        self.write(
            "tests/conformance/cli/pass.sai",
            """//@ kind: check-fail
//@ stage: 1
//@ targets: wasm32-core
//@ profiles: dev
//@ spec: spec/02 §2.1.1
const A: I32 = 1 //~ error EXPECTED_TOKEN
""",
        )
        self.write_golden(
            "tests/conformance/cli/pass.dev.json",
            "FEATURE_NOT_IMPLEMENTED",
        )
        self.write_golden(
            "tests/conformance/cli/pass.wasm32-core.json",
            "EXPECTED_TOKEN",
        )
        report = build_report(self.root)
        expected_token = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "EXPECTED_TOKEN"
        )
        self.assertEqual(expected_token["goldens"], [])

    def test_specialized_directory_json_golden_is_inventoried(self):
        self.write_golden(
            "tests/conformance/cli/package.t/expected/diagnostics.wasm32-core.dev.json",
            "EXPECTED_TOKEN",
        )
        report = build_report(self.root)
        entry = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "EXPECTED_TOKEN"
        )
        self.assertEqual(
            entry["goldens"],
            ["tests/conformance/cli/package.t/expected/diagnostics.wasm32-core.dev.json"],
        )

    def test_reference_only_json_golden_still_covers_hard_code(self):
        self.write_golden("tests/conformance/cli/ref-only.json", "REF_ONLY_CODE")
        report = build_report(self.root)
        entry = next(
            item for item in report["hard_code_json_goldens"]
            if item["code"] == "REF_ONLY_CODE"
        )
        self.assertEqual(entry["goldens"], ["tests/conformance/cli/ref-only.json"])
        fixture = next(
            item for item in report["fixtures"]
            if item["path"] == "tests/conformance/cli/ref-only.sai"
        )
        self.assertFalse(fixture["portable"])

    def test_lint_inventory_rejects_duplicates_and_unregistered_codes(self):
        aliases, canonical = _document_index(self.root / "docs")
        path = aliases["spec/15"]
        headings = _scan_headings(path.read_text(encoding="utf-8"), "spec/15")
        path.write_text(
            """# 15. Attributes and lints
## 15.17 Diagnostic registry
### 15.17.2 The lints of v0.1
| Code | Default |
|---|---|
| `UNUSED_VARIABLE` | `warn` |
| `UNUSED_VARIABLE` | `warn` |
""",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(CoverageError, "unique lint codes"):
            _parse_lint_codes(path, headings, {"UNUSED_VARIABLE"})

        path.write_text(
            """# 15. Attributes and lints
## 15.17 Diagnostic registry
### 15.17.2 The lints of v0.1
| Code | Default |
|---|---|
| `UNREGISTERED_LINT` | `warn` |
""",
            encoding="utf-8",
        )
        headings = _scan_headings(path.read_text(encoding="utf-8"), "spec/15")
        with self.assertRaisesRegex(CoverageError, "unregistered diagnostic"):
            _parse_lint_codes(path, headings, {"UNUSED_VARIABLE"})

    def test_panic_table_is_read_from_the_normative_section(self):
        aliases, canonical = _document_index(self.root / "docs")
        path = aliases["spec/10"]
        headings = _scan_headings(path.read_text(encoding="utf-8"), "spec/10")
        self.assertEqual(
            _parse_panic_kinds(path, headings),
            ["Overflow(Add)", "Contract(id)", "Invariant(id)", "Refinement(id)",
             "StackOverflow", "OutOfMemory"],
        )

    def test_section_exemptions_are_validated_and_bound_to_inventory(self):
        aliases, canonical = _document_index(self.root / "docs")
        inventory, headings = _build_inventory(self.root / "docs", aliases, canonical)
        path = self.write(
            "tests/conformance/exempt-sections.txt",
            "spec/02 §2.1.1 | generated §15.8.4\n",
        )
        self.assertEqual(
            _parse_section_exemptions(path, inventory, aliases, canonical, headings),
            {"spec/02 §2.1.1": "generated §15.8.4"},
        )
        path.write_text("spec/02 §2.1.1 | typo\n", encoding="utf-8")
        with self.assertRaisesRegex(CoverageError, "invalid section exemption"):
            _parse_section_exemptions(path, inventory, aliases, canonical, headings)
        path.write_text("spec/02 §2.1.99 | informative\n", encoding="utf-8")
        with self.assertRaisesRegex(CoverageError, "does not resolve"):
            _parse_section_exemptions(path, inventory, aliases, canonical, headings)
        path.write_text(
            "spec/02 §2.1.1 | generated §15.8.99\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(CoverageError, "does not resolve"):
            _parse_section_exemptions(path, inventory, aliases, canonical, headings)

    def test_code_exemptions_are_allowlisted_and_bind_to_their_required_tests(self):
        aliases, canonical = _document_index(self.root / "docs")
        _, headings = _build_inventory(self.root / "docs", aliases, canonical)
        path = self.write(
            "tests/conformance/exempt-codes.txt",
            "INTERNAL_COMPILER_ERROR | produced only by an injected compiler failure | §15.12.3\n"
            "IMPLEMENTATION_LIMIT_EXCEEDED | limits differ between implementations | §15.12.2\n"
            "SOURCE_READ_FAILED | host permissions cannot be made portable | §15.12.5\n",
        )
        self.assertEqual(
            _parse_code_exemptions(
                path,
                {
                    "EXPECTED_TOKEN", "INTERNAL_COMPILER_ERROR",
                    "IMPLEMENTATION_LIMIT_EXCEEDED", "SOURCE_READ_FAILED",
                },
                aliases, canonical, headings,
            ),
            {
                "INTERNAL_COMPILER_ERROR": {
                    "reason": "produced only by an injected compiler failure",
                    "test_section": "impl/15 §15.12.3",
                },
                "IMPLEMENTATION_LIMIT_EXCEEDED": {
                    "reason": "limits differ between implementations",
                    "test_section": "impl/15 §15.12.2",
                },
                "SOURCE_READ_FAILED": {
                    "reason": "host permissions cannot be made portable",
                    "test_section": "impl/15 §15.12.5",
                },
            },
        )
        path.write_text("NOT_REGISTERED | impossible | §15.12.3\n", encoding="utf-8")
        with self.assertRaisesRegex(CoverageError, "unregistered diagnostic"):
            _parse_code_exemptions(
                path, {"EXPECTED_TOKEN"}, aliases, canonical, headings
            )
        path.write_text("EXPECTED_TOKEN | impossible | §15.12.3\n", encoding="utf-8")
        with self.assertRaisesRegex(CoverageError, "not exemptible"):
            _parse_code_exemptions(
                path, {"EXPECTED_TOKEN"}, aliases, canonical, headings
            )
        path.write_text(
            "INTERNAL_COMPILER_ERROR | wrong test | §15.12.5\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(CoverageError, "must cite impl/15 §15.12.3"):
            _parse_code_exemptions(
                path,
                {"INTERNAL_COMPILER_ERROR", "SOURCE_READ_FAILED"},
                aliases, canonical, headings,
            )

    def test_missing_exemption_lists_are_fatal(self):
        (self.root / "tests/conformance/exempt-sections.txt").unlink()
        with self.assertRaisesRegex(CoverageError, "missing section exemption"):
            build_report(self.root)

    def test_report_bytes_are_stable_and_sorted(self):
        self.write_golden("tests/conformance/cli/pass.json", "EXPECTED_TOKEN")
        first = self.root / "target/conformance/coverage.json"
        second = self.root / "target/another/coverage.json"
        write_report(self.root, first)
        initial = first.read_bytes()
        write_report(self.root, second)
        write_report(self.root, first)
        self.assertEqual(initial, first.read_bytes())
        self.assertEqual(initial, second.read_bytes())
        report = build_report(self.root)
        self.assertEqual(initial, _canonical_json(report))
        self.assertEqual(
            [entry["path"] for entry in report["fixtures"]],
            sorted(entry["path"] for entry in report["fixtures"]),
        )

    def test_malformed_spec_reference_fails_closed(self):
        self.write(
            "tests/conformance/cli/bad.sai",
            """//@ kind: check-pass
//@ stage: 0
//@ spec: spec/02 §2.999
const A: I32 = 0
""",
        )
        with self.assertRaisesRegex(CoverageError, "spec section does not resolve"):
            build_report(self.root)


if __name__ == "__main__":
    unittest.main()
