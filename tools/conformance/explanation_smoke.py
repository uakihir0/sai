#!/usr/bin/env python3
"""Run a narrow, non-claim smoke for selected Stage 0 and Stage 1 explain records."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import tempfile
from collections import Counter
from pathlib import Path

import expectation_extractor
import run_stage0
from run_stage0 import SmokeError


ROOT_ID = run_stage0.ROOT_ID
RECORDS = {
    "DUPLICATE_BINDING": {
        "class": "hard",
        "severity": "error",
        "stage": 0,
        "owner": "spec/04-modules-and-names.md",
    },
    "INVALID_ESCAPE": {
        "class": "hard",
        "severity": "error",
        "stage": 0,
        "owner": "spec/02-lexical-structure.md",
    },
    "LITERAL_OUT_OF_RANGE": {
        "class": "hard",
        "severity": "error",
        "stage": 0,
        "owner": "spec/02-lexical-structure.md",
    },
    "MALFORMED_NUMBER": {
        "class": "hard",
        "severity": "error",
        "stage": 0,
        "owner": "spec/02-lexical-structure.md",
    },
    "TYPE_MISMATCH": {
        "class": "hard",
        "severity": "error",
        "stage": 1,
        "owner": "spec/05-types.md",
    },
    "UNKNOWN_FIELD": {
        "class": "hard",
        "severity": "error",
        "stage": 1,
        "owner": "spec/06-expressions-and-statements.md",
    },
}
DATA_FIELDS = {
    "code", "class", "severity", "default_level", "stage", "owner", "since",
    "status", "superseded_by", "summary", "markdown",
}
TABLE_HEADER = ["Class", "Severity", "Stage", "Owner", "Since", "Status"]
SECTIONS = ("Summary", "Explanation", "Example", "Fix", "See also")
PACKAGE_MANIFEST = run_stage0.PACKAGE_MANIFEST
TIMEOUT_SECONDS = run_stage0.TIMEOUT_SECONDS
APPENDIX_RELATIVE_PATH = "spec/appendix-a-diagnostic-codes.md"
CHECK_GATE_HELP = (
    "root and active source-dependency syntax parsing, module-tree validation, "
    "target filtering, Phase 1 declarations, and root core export-name checks are "
    "implemented, including qualified top-level type-member visibility diagnostics; "
    "import and use resolution, type checking, dependency and workspace analysis, "
    "generated WIT for source dependencies, lints remain unavailable"
)


def _compact_summary(summary: str) -> str:
    """Apply the compact-list summary transform from impl/11 §11.13.1."""
    try:
        summary.encode("utf-8", "strict")
    except UnicodeEncodeError:
        raise SmokeError("explanation summary contains non-scalar text") from None
    summary = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", summary)
    summary = re.sub(r"`{1,2}", "", summary).strip()
    if summary.endswith("."):
        summary = summary[:-1].rstrip()
    if len(summary) <= 80:
        return summary
    prefix = summary[:79]
    whitespace = next(
        (index for index in range(len(prefix) - 1, -1, -1) if prefix[index].isspace()),
        None,
    )
    if whitespace is not None:
        prefix = prefix[:whitespace]
    return prefix.rstrip() + "…"


def _section_map(markdown: str, *, code: str) -> tuple[dict[str, str], dict[str, list[str]]]:
    if not isinstance(markdown, str):
        raise SmokeError("explanation markdown is not text")
    try:
        markdown.encode("utf-8", "strict")
    except UnicodeEncodeError:
        raise SmokeError("explanation markdown contains non-scalar text") from None
    lines = markdown.splitlines()
    if not lines or lines[0] != f"# {code}":
        raise SmokeError("explanation markdown heading does not match its code")

    headings: list[tuple[str, int]] = []
    fence_character: str | None = None
    fence_width = 0
    for index, line in enumerate(lines):
        fence = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence is not None:
            marker = fence[1]
            if fence_character is None:
                fence_character = marker[0]
                fence_width = len(marker)
            elif (
                marker[0] == fence_character
                and len(marker) >= fence_width
                and not line[fence.end():].strip()
            ):
                fence_character = None
                fence_width = 0
            continue
        if fence_character is not None:
            continue
        heading = re.fullmatch(r" {0,3}##[ \t]+(.+?)[ \t]*", line)
        if heading is not None:
            name = heading[1]
            if name not in SECTIONS:
                raise SmokeError("explanation markdown contains an unsupported section")
            headings.append((name, index))
        elif re.match(r"^ {0,3}#[ \t]+", line) and index != 0:
            raise SmokeError("explanation markdown contains an extra top-level heading")
    if fence_character is not None:
        raise SmokeError("explanation markdown contains an unclosed code fence")

    names = [name for name, _ in headings]
    if (
        names[:3] != ["Summary", "Explanation", "Example"]
        or names[3:] not in ([], ["Fix"], ["See also"], ["Fix", "See also"])
        or len(names) != len(set(names))
    ):
        raise SmokeError("explanation markdown sections are missing or out of order")
    sections = {}
    for offset, (name, start) in enumerate(headings):
        end = headings[offset + 1][1] if offset + 1 < len(headings) else len(lines)
        sections[name] = "\n".join(lines[start + 1:end]).strip()

    table_index = next(
        (index for index, line in enumerate(lines)
         if run_stage0._table_cells(line) == TABLE_HEADER),
        None,
    )
    if table_index != 2 or table_index + 2 >= len(lines):
        raise SmokeError("explanation metadata table is missing or misplaced")
    separator = run_stage0._table_cells(lines[table_index + 1])
    metadata_row = run_stage0._table_cells(lines[table_index + 2])
    if (
        separator is None
        or len(separator) != len(TABLE_HEADER)
        or any(not re.fullmatch(r":?-{3,}:?", cell) for cell in separator)
        or metadata_row is None
        or len(metadata_row) != len(TABLE_HEADER)
    ):
        raise SmokeError("explanation metadata table is malformed")
    metadata = dict(zip(TABLE_HEADER, metadata_row))
    if any(not value for value in sections.values()):
        raise SmokeError("explanation markdown contains an empty section")
    return sections, metadata


def _source_fence(section: str, *, role: str) -> tuple[str, str]:
    lines = section.splitlines()
    opening = re.fullmatch(r"( {0,3})(`{3,}|~{3,})(.*)", lines[0]) if lines else None
    if len(lines) < 3 or opening is None:
        raise SmokeError(f"explanation {role} is not one supported fenced block")
    indentation = len(opening[1])
    marker = opening[2]
    info_string = opening[3].strip()
    if marker[0] == "`" and "`" in info_string:
        raise SmokeError(f"explanation {role} has an invalid backtick fence")
    closing = re.fullmatch(r" {0,3}(`{3,}|~{3,})[ \t]*", lines[-1])
    if (
        closing is None
        or closing[1][0] != marker[0]
        or len(closing[1]) < len(marker)
    ):
        raise SmokeError(f"explanation {role} is not one supported fenced block")
    if role == "Example":
        if info_string not in ("sai", "sai noncanonical"):
            raise SmokeError("explanation Example has an unsupported fence")
    elif info_string != "sai":
        raise SmokeError("explanation Fix must use a canonical sai fence")
    source_lines = [
        line[min(indentation, len(line) - len(line.lstrip(" "))):]
        if line.startswith(" ") else line
        for line in lines[1:-1]
    ]
    source = "\n".join(source_lines) + "\n"
    if not source.strip():
        raise SmokeError(f"explanation {role} source is empty")
    return source, info_string


def _parse_record(
    markdown: str, *, code: str,
) -> tuple[dict, str, str, str, str]:
    sections, table = _section_map(markdown, code=code)
    example, example_info = _source_fence(sections["Example"], role="Example")
    if "Fix" not in sections:
        raise SmokeError("selected explanation record has no Fix")
    fix, _ = _source_fence(sections["Fix"], role="Fix")
    return table, sections["Summary"], example, example_info, fix


def _appendix_summary(docs_root: Path, *, code: str) -> str:
    appendix_path = docs_root / APPENDIX_RELATIVE_PATH
    try:
        lines = appendix_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        raise SmokeError("diagnostic-code appendix cannot be read") from None
    header = ["Code", "Severity", "Owner", "Summary"]
    if not any(run_stage0._table_cells(line) == header for line in lines):
        raise SmokeError("diagnostic-code appendix summary table is missing")
    rows = [
        cells for line in lines
        if (cells := run_stage0._table_cells(line)) is not None
        and len(cells) == len(header)
        and cells[0] == f"`{code}`"
    ]
    if len(rows) != 1 or not rows[0][3]:
        raise SmokeError(f"diagnostic-code appendix has no unique summary for {code}")
    return rows[0][3]


def _validate_record(
    completed: subprocess.CompletedProcess,
    *,
    code: str,
    list_entry: dict,
    docs_root: Path,
) -> tuple[str, str, str, list[dict]]:
    records = run_stage0._records(
        completed.stdout, exit_code=completed.returncode, stderr=completed.stderr,
    )
    header = records[0]
    run_stage0._validate_empty_summary(records[-1], command=f"sai explain {code}")
    if (
        completed.returncode != 0
        or len(records) != 3
        or set(header) != {"type", "format", "version", "tool", "command", "root"}
        or header.get("command") != "explain"
        or header.get("root") is not None
        or records[1].get("kind") != "explain"
        or set(records[1]) != {"type", "kind", "data"}
        or records[2]["type"] != "summary"
        or records[2]["errors"] != 0
        or records[2]["warnings"] != 0
        or records[2]["infos"] != 0
        or records[2]["exit_code"] != 0
    ):
        raise SmokeError(f"sai explain {code} returned an invalid record stream")
    data = records[1].get("data")
    if not isinstance(data, dict) or set(data) != DATA_FIELDS:
        raise SmokeError("sai explain returned an invalid record schema")
    expected_metadata = RECORDS[code]
    if (
        data["code"] != code
        or data["class"] != expected_metadata["class"]
        or data["class"] != list_entry["class"]
        or data["severity"] != expected_metadata["severity"]
        or data["default_level"] is not None
        or type(data["stage"]) is not int
        or data["stage"] != expected_metadata["stage"]
        or data["owner"] != expected_metadata["owner"]
        or data["since"] != "0.1.0"
        or data["status"] != "active"
        or data["superseded_by"] is not None
        or not isinstance(data["summary"], str)
        or not data["summary"]
        or not isinstance(data["markdown"], str)
    ):
        raise SmokeError("sai explain metadata does not match its list entry or selected record")
    if data["summary"] != _appendix_summary(docs_root, code=code):
        raise SmokeError("sai explain full summary does not match Appendix A")
    if _compact_summary(data["summary"]) != list_entry["summary"]:
        raise SmokeError("sai explain compact summary does not match its list entry")
    markdown_metadata, summary_text, example, example_info, fix = _parse_record(
        data["markdown"], code=code,
    )
    expected_table = {
        "Class": data["class"],
        "Severity": data["severity"],
        "Stage": str(data["stage"]),
        "Owner": data["owner"],
        "Since": data["since"],
        "Status": data["status"],
    }
    if (
        markdown_metadata != expected_table
        or summary_text != data["summary"]
    ):
        raise SmokeError("explanation markdown metadata or Summary disagrees with JSON")
    spec_path = docs_root.joinpath(*expected_metadata["owner"].split("/"))
    try:
        resolved_spec = spec_path.resolve(strict=True)
        resolved_spec.relative_to(docs_root.resolve(strict=True))
    except (OSError, RuntimeError, ValueError):
        raise SmokeError("explanation owner does not resolve under docs/") from None
    if not resolved_spec.is_file():
        raise SmokeError("explanation owner is not a documentation file")
    expected = expectation_extractor.extract_explanation_example(
        example, section="Example", info_string=example_info,
    )
    if (
        len(expected) != 1
        or expected[0]["severity"] != "error"
        or expected[0]["code"] != code
    ):
        raise SmokeError("explanation Example must mark exactly its own error code")
    if expectation_extractor.extract(fix, "src/lib.sai"):
        raise SmokeError("explanation Fix contains diagnostic expectations")
    return example, example_info, fix, expected


def _run_package_command(
    iut: Path,
    source: str,
    command: list[str],
    *,
    timeout: float,
) -> subprocess.CompletedProcess:
    with tempfile.TemporaryDirectory(prefix="sai-explanation-") as temporary:
        sandbox_root = Path(temporary)
        package_root = sandbox_root / "package"
        source_dir = package_root / "src"
        source_dir.mkdir(parents=True)
        (package_root / "sai.package").write_bytes(PACKAGE_MANIFEST.encode("utf-8"))
        (source_dir / "lib.sai").write_bytes(source.encode("utf-8", "strict"))
        return run_stage0._run_iut(
            iut,
            ["--format=json", "--max-errors=0", *command],
            package_root,
            timeout,
            state_root=sandbox_root,
        )


def _command_diagnostics(
    completed: subprocess.CompletedProcess,
    *,
    command: str,
    registered_codes: set[str],
) -> tuple[list[dict], list[dict]]:
    records = run_stage0._records(
        completed.stdout,
        exit_code=completed.returncode,
        stderr=completed.stderr,
        max_errors=0,
    )
    header = records[0]
    if (
        header.get("command") != command
        or header.get("root") != ROOT_ID
        or completed.returncode not in (0, 1)
    ):
        raise SmokeError(f"sai {command} returned the wrong command, root, or exit code")
    diagnostics = [record for record in records if record["type"] == "diagnostic"]
    results = [record for record in records if record["type"] == "result"]
    actual = run_stage0._actual_diagnostics(records, registered_codes)
    errors = any(item["severity"] == "error" for item in actual)
    if completed.returncode == 0:
        if errors or len(results) != 1 or results[0].get("kind") != command:
            raise SmokeError(f"successful sai {command} must have one result and no errors")
    elif errors is False or results:
        raise SmokeError(f"failed sai {command} must have errors and no result")
    if command == "check":
        run_stage0._validate_check_result(
            records,
            kind="check-pass" if completed.returncode == 0 else "check-fail",
            exit_code=completed.returncode,
            diagnostics=actual,
        )
    elif completed.returncode == 0:
        if set(results[0]) != {"type", "kind", "data"}:
            raise SmokeError("sai build returned an invalid result record")
        data = results[0].get("data")
        if (
            not isinstance(data, dict)
            or set(data) != {
                "target", "profile", "artifact", "component_kind", "world",
                "wasm_features", "outputs",
            }
            or data["target"] != "wasm32-core"
            or data["profile"] != "dev"
            or data["artifact"] != "core_module"
            or data["component_kind"] is not None
            or data["world"] is not None
            or data["wasm_features"] != []
            or not isinstance(data["outputs"], list)
            or not data["outputs"]
            or any(
                not isinstance(output, dict)
                or set(output) != {"path", "bytes", "sha256"}
                or not isinstance(output["path"], str)
                or not output["path"].startswith("out/wasm32-core/dev/")
                or type(output["bytes"]) is not int
                or output["bytes"] < 0
                or not isinstance(output["sha256"], str)
                or not re.fullmatch(r"[0-9a-f]{64}", output["sha256"])
                for output in data["outputs"]
            )
        ):
            raise SmokeError("sai build returned an invalid core-module result")
    return diagnostics, actual


def _allowed_feature_gate(record: dict, *, code: str, command: str) -> bool:
    return (
        code in {"INVALID_ESCAPE", "MALFORMED_NUMBER"}
        and command == "check"
        and record.get("code") == "FEATURE_NOT_IMPLEMENTED"
        and record.get("severity") == "error"
        and record.get("level") is None
        and record.get("message") == "stage 0 feature `sai check` is not implemented"
        and record.get("primary") is None
        and record.get("primary_label") == ""
        and record.get("labels") == []
        and record.get("children") == [{
            "kind": "help",
            "message": CHECK_GATE_HELP,
            "location": None,
        }]
        and record.get("fixes") == []
        and record.get("related") == []
        and record.get("data") == {"stage": 0, "feature": "sai check"}
        and record.get("node") is None
        and record.get("symbol") is None
        and record.get("package") in (None, ROOT_ID)
    )


def _validate_example_diagnostics(
    records: list[dict],
    *,
    code: str,
    command: str,
    expected: list[dict],
) -> None:
    target_diagnostics = [
        record for record in records
        if record["type"] == "diagnostic" and record["code"] == code
    ]
    if not target_diagnostics or any(record["severity"] != "error" for record in target_diagnostics):
        raise SmokeError(f"sai {command} did not report {code} as an error")
    gate_count = sum(
        _allowed_feature_gate(record, code=code, command=command)
        for record in records if record["type"] == "diagnostic"
    )
    if gate_count > 1:
        raise SmokeError(f"sai {command} repeated the accepted Stage 0 feature gate")
    expected_other = Counter(
        (item["file"], item["line"], item["severity"], item["code"])
        for item in expected if item["code"] != code
    )
    actual_other = Counter()
    for record in records:
        if record["type"] != "diagnostic" or record["code"] == code:
            continue
        if _allowed_feature_gate(record, code=code, command=command):
            continue
        primary = record["primary"]
        if primary is None:
            actual_other[(None, None, record["severity"], record["code"])] += 1
        else:
            actual_other[(
                primary["file"]["path"],
                primary["start"]["line"],
                record["severity"],
                record["code"],
            )] += 1
    if expected_other != actual_other:
        raise SmokeError(f"sai {command} produced diagnostics not marked by the Example")


def _validate_fallback_diagnostics(
    check_records: list[dict],
    build_records: list[dict],
    *,
    code: str,
    expected: list[dict],
) -> None:
    check_diagnostics = [
        record for record in check_records if record["type"] == "diagnostic"
    ]
    build_diagnostics = [
        record for record in build_records if record["type"] == "diagnostic"
    ]
    gates = [
        record for record in check_diagnostics
        if _allowed_feature_gate(record, code=code, command="check")
    ]
    if len(gates) > 1:
        raise SmokeError("sai check repeated the accepted Stage 0 feature gate")

    def diagnostic_counts(diagnostics: list[dict], *, command: str) -> Counter:
        counts = Counter()
        for record in diagnostics:
            if record["code"] == code or _allowed_feature_gate(
                record, code=code, command=command,
            ):
                continue
            primary = record["primary"]
            if primary is None:
                counts[(None, None, record["severity"], record["code"])] += 1
            else:
                counts[(
                    primary["file"]["path"],
                    primary["start"]["line"],
                    record["severity"],
                    record["code"],
                )] += 1
        return counts

    target_diagnostics = [
        record
        for record in check_diagnostics + build_diagnostics
        if record["code"] == code
    ]
    if (
        not target_diagnostics
        or any(record["severity"] != "error" for record in target_diagnostics)
    ):
        raise SmokeError(f"sai check/build did not report only {code} errors")

    checked = diagnostic_counts(check_diagnostics, command="check")
    built = diagnostic_counts(build_diagnostics, command="build")
    combined = Counter({
        key: max(checked[key], built[key])
        for key in checked.keys() | built.keys()
    })
    marked = Counter(
        (item["file"], item["line"], item["severity"], item["code"])
        for item in expected if item["code"] != code
    )
    if not any(
        item["code"] == code and item["severity"] == "error"
        for item in expected
    ):
        raise SmokeError("explanation Example does not mark its own error code")
    if combined != marked:
        raise SmokeError("sai check/build diagnostics do not match Example markers")


def _run_example_check(
    iut: Path,
    *,
    code: str,
    source: str,
    expected: list[dict],
    registered_codes: set[str],
    timeout: float,
) -> None:
    check = _run_package_command(
        iut, source, ["check", "--jobs=1"], timeout=timeout,
    )
    check_records, _ = _command_diagnostics(
        check, command="check", registered_codes=registered_codes,
    )
    if any(
        item["type"] == "diagnostic" and item["code"] == code
        for item in check_records
    ):
        _validate_example_diagnostics(
            check_records, code=code, command="check", expected=expected,
        )
        return

    build = _run_package_command(
        iut,
        source,
        ["build", "--target", "wasm32-core"],
        timeout=timeout,
    )
    build_records, _ = _command_diagnostics(
        build, command="build", registered_codes=registered_codes,
    )
    _validate_fallback_diagnostics(
        check_records,
        build_records,
        code=code,
        expected=expected,
    )


def _run_fix_check(
    iut: Path,
    *,
    source: str,
    registered_codes: set[str],
    timeout: float,
) -> None:
    completed = _run_package_command(
        iut, source, ["check", "--jobs=1"], timeout=timeout,
    )
    _, actual = _command_diagnostics(
        completed, command="check", registered_codes=registered_codes,
    )
    if completed.returncode != 0 or actual:
        raise SmokeError("explanation Fix produced a diagnostic")


def _run_fmt(
    iut: Path,
    *,
    source: str,
    info_string: str,
    expected: list[dict],
    registered_codes: set[str],
    docs_root: Path,
    timeout: float,
) -> None:
    command = [
        "--format=json", "--max-errors=0", "fmt", "--stdin-kind", "source", "-",
    ]
    source_bytes = source.encode("utf-8", "strict")

    def invoke(input_bytes: bytes) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory(prefix="sai-explanation-fmt-") as temporary:
            sandbox_root = Path(temporary)
            package_root = sandbox_root / "package"
            package_root.mkdir()
            return run_stage0._run_iut(
                iut,
                command,
                package_root,
                timeout,
                state_root=sandbox_root,
                stdin_data=input_bytes,
            )

    first = invoke(source_bytes)
    if info_string == "sai noncanonical" and first.returncode == 1:
        run_stage0._validate_fmt_refusal(
            first,
            expected=expected,
            registered_codes=registered_codes,
            refusal_codes=run_stage0._formatter_refusal_codes(docs_root),
        )
        return
    if info_string == "sai":
        run_stage0._validate_fmt_output(
            first,
            expected_output=source_bytes,
            input_bytes=source_bytes,
            registered_codes=registered_codes,
            fixed_point=True,
        )
        return
    if first.returncode != 0:
        raise SmokeError("marked explanation Example was refused with the wrong result")
    _, data, summary, diagnostics = run_stage0._formatter_envelope(
        first, registered_codes=registered_codes,
    )
    if (
        diagnostics
        or summary["suppressed"] != {"cascade": 0, "duplicate": 0, "dependency": 0}
        or data["check"] is not False
        or type(data["files"]) is not int
        or data["files"] != 1
        or not isinstance(data["changed"], list)
        or any(not isinstance(path, str) for path in data["changed"])
        or data["refused"] != []
        or not isinstance(data["output"], str)
        or summary["exit_code"] != 0
    ):
        raise SmokeError("accepted noncanonical explanation Example has an invalid fmt result")
    try:
        formatted = data["output"].encode("utf-8", "strict")
    except UnicodeEncodeError:
        raise SmokeError("formatter output for explanation Example is not UTF-8 text") from None
    if data["changed"] != ([] if formatted == source_bytes else ["<stdin>"]):
        raise SmokeError("formatter changed list disagrees with explanation Example output")
    second = invoke(formatted)
    run_stage0._validate_fmt_output(
        second,
        expected_output=formatted,
        input_bytes=formatted,
        registered_codes=registered_codes,
        fixed_point=True,
    )


def _run_record(
    iut: Path,
    code: str,
    list_entry: dict,
    registered_codes: set[str],
    *,
    docs_root: Path,
    timeout: float,
) -> dict:
    result = {
        "type": "test",
        "test": f"sai explain {code}",
        "mode": "partial-smoke",
        "claim": False,
        "outcome": "fail",
        "failures": [],
    }
    try:
        with tempfile.TemporaryDirectory(prefix="sai-explanation-record-") as temporary:
            completed = run_stage0._run_iut(
                iut,
                ["--format=json", "explain", code],
                Path(temporary),
                timeout,
            )
        source, info_string, fix, expected = _validate_record(
            completed, code=code, list_entry=list_entry, docs_root=docs_root,
        )
        _run_example_check(
            iut,
            code=code,
            source=source,
            expected=expected,
            registered_codes=registered_codes,
            timeout=timeout,
        )
        _run_fix_check(
            iut, source=fix, registered_codes=registered_codes, timeout=timeout,
        )
        _run_fmt(
            iut,
            source=source,
            info_string=info_string,
            expected=expected,
            registered_codes=registered_codes,
            docs_root=docs_root,
            timeout=timeout,
        )
        _run_fmt(
            iut,
            source=fix,
            info_string="sai",
            expected=[],
            registered_codes=registered_codes,
            docs_root=docs_root,
            timeout=timeout,
        )
        result["outcome"] = "pass"
    except (SmokeError, ValueError, run_stage0.ndjson_stream.StreamError) as error:
        result["failures"] = [str(error)]
    except Exception:
        result["failures"] = ["unexpected explanation smoke-runner failure"]
    return result


def run_suite(iut: str | Path, *, timeout: float = TIMEOUT_SECONDS) -> dict:
    """Smoke selected complete Stage 0 and Stage 1 explanation records without a claim."""
    report = {
        "type": "summary",
        "mode": "partial-smoke",
        "claim": False,
        "iut_identity_verified": False,
        "outcome": "fail",
        "tests": 0,
        "passed": 0,
        "failed": 0,
        "results": [],
        "failures": [],
        "limitations": [
            "Only DUPLICATE_BINDING, INVALID_ESCAPE, LITERAL_OUT_OF_RANGE, and "
            "MALFORMED_NUMBER (Stage 0) and TYPE_MISMATCH and UNKNOWN_FIELD "
            "(Stage 1) are checked; "
            "other registered explanation records are not checked.",
            "This runner does not validate other registered explanation records, "
            "staged applicability, diagnostic tables, or the full registry checks "
            "required by impl/15 §§15.8.4–15.8.5.",
            "The supplied executable is treated as the reference implementation; "
            "its identity is not verified.",
        ],
    }
    try:
        requested_iut = Path(iut)
        if not requested_iut.is_absolute():
            raise SmokeError("IUT path must be absolute")
        iut_path = requested_iut.resolve(strict=True)
        if not iut_path.is_file():
            raise SmokeError("IUT path is not a regular file")
        if not isinstance(timeout, (int, float)) or timeout <= 0 or timeout > TIMEOUT_SECONDS:
            raise SmokeError("timeout must be greater than zero and at most 600 seconds")
        registry = run_stage0._read_registry_entries(iut_path, timeout=timeout)
        missing = sorted(set(RECORDS) - set(registry))
        if missing:
            raise SmokeError("selected explanation codes are missing from --list: "
                             + ", ".join(missing))
        docs_root = Path(__file__).resolve().parents[2] / "docs"
        report["results"] = [
            _run_record(
                iut_path,
                code,
                registry[code],
                set(registry),
                docs_root=docs_root,
                timeout=timeout,
            )
            for code in RECORDS
        ]
        report["tests"] = len(report["results"])
        report["passed"] = sum(item["outcome"] == "pass" for item in report["results"])
        report["failed"] = report["tests"] - report["passed"]
        report["outcome"] = "pass" if report["failed"] == 0 else "fail"
    except (SmokeError, OSError, ValueError) as error:
        report["failures"] = [str(error)]
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--iut",
        required=True,
        help="absolute path to the reference sai executable; identity is not verified",
    )
    arguments = parser.parse_args()
    report = run_suite(arguments.iut)
    print(json.dumps(report, ensure_ascii=False, separators=(",", ":"), allow_nan=False))
    return 0 if report["outcome"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
