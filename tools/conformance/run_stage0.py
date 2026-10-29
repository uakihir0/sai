#!/usr/bin/env python3
"""Run a deliberately partial Stage 0 smoke suite against a black-box IUT.

This runner accepts only reference-implementation, single-file Stage 0
check-pass/check-fail and source-fmt/fmt-refuse tests. Its results are useful
smoke evidence only; it never reports a conformance claim.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path, PurePosixPath

import configurations
import diagnostic_matcher
import expectation_extractor
import fixture_discovery
import ndjson_stream


ROOT_ID = "conformance:test@0.1.0"
PACKAGE_MANIFEST = (
    "package conformance:test @ 0.1.0\n"
    "compiler ^0.1\n"
    'source "src"\n'
    "targets { wasm32-core }\n"
)
TIMEOUT_SECONDS = 600
ALLOWED_DIRECTIVES = {"kind", "stage", "spec", "diag"}
SEVERITY_RANK = {"error": 0, "warning": 1, "info": 2}
CODE_PATTERN = re.compile(r"[A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)+\Z")
DOCUMENT_AREA = {"spec", "package", "impl"}
SIDECAR_SUFFIXES = fixture_discovery.FILE_SIDECARS + fixture_discovery.SOURCE_FILE_SIDECARS


class SmokeError(ValueError):
    """A stable, user-facing rejection of an unsupported or invalid case."""


def _canonical_json(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"),
                      allow_nan=False).encode("utf-8")


def _records(stdout: bytes, *, exit_code: int, stderr: bytes,
             max_errors: int | None = None) -> list[dict]:
    if not isinstance(stdout, bytes):
        raise SmokeError("IUT stdout was not captured as bytes")
    if max_errors is None:
        return ndjson_stream.validate_stream(
            stdout.splitlines(keepends=True), process_exit_code=exit_code, stderr=stderr
        )
    return ndjson_stream.validate_stream(
        stdout.splitlines(keepends=True), process_exit_code=exit_code,
        stderr=stderr, max_errors=max_errors,
    )


def _run_iut(iut: Path, argv: list[str], cwd: Path, timeout: float, *,
             state_root: Path | None = None,
             stdin_data: bytes | None = None) -> subprocess.CompletedProcess:
    state_root = cwd if state_root is None else state_root
    cache = state_root / "cache"
    home = state_root / "home"
    cache.mkdir(exist_ok=True)
    home.mkdir(exist_ok=True)
    environment = {
        "NO_COLOR": "1",
        "SAI_CACHE_DIR": str(cache),
        "HOME": str(home),
    }
    try:
        process_options = {
            "cwd": cwd,
            "env": environment,
            "stdout": subprocess.PIPE,
            "stderr": subprocess.PIPE,
            "timeout": timeout,
            "check": False,
        }
        if stdin_data is None:
            process_options["stdin"] = subprocess.DEVNULL
        else:
            process_options["input"] = stdin_data
        return subprocess.run([str(iut), *argv], **process_options)
    except subprocess.TimeoutExpired:
        raise SmokeError("IUT exceeded the smoke-run timeout") from None
    except OSError:
        raise SmokeError("could not start the IUT") from None


def _read_registry(iut: Path, *, timeout: float) -> set[str]:
    with tempfile.TemporaryDirectory(prefix="sai-stage0-registry-") as temporary:
        cwd = Path(temporary)
        completed = _run_iut(iut, ["--format=json", "explain", "--list"], cwd, timeout)
    records = _records(completed.stdout, exit_code=completed.returncode,
                       stderr=completed.stderr)
    if completed.returncode != 0:
        raise SmokeError("sai explain --list did not succeed")
    header = records[0]
    if header.get("command") != "explain" or header.get("root") is not None:
        raise SmokeError("sai explain --list returned the wrong header")
    diagnostics = [record for record in records if record["type"] == "diagnostic"]
    results = [record for record in records if record["type"] == "result"]
    summaries = [record for record in records if record["type"] == "summary"]
    if diagnostics or len(results) != 1 or len(summaries) != 1:
        raise SmokeError("sai explain --list returned the wrong record shape")
    result = results[0]
    if result.get("kind") != "explain.list" or set(result) != {"type", "kind", "data"}:
        raise SmokeError("sai explain --list returned an invalid result")
    data = result.get("data")
    if not isinstance(data, dict) or set(data) != {"codes"} or not isinstance(data["codes"], list):
        raise SmokeError("sai explain --list returned an invalid code registry")

    codes = []
    for entry in data["codes"]:
        if (not isinstance(entry, dict) or set(entry) != {"code", "class", "summary"}
                or not isinstance(entry["code"], str)
                or not CODE_PATTERN.fullmatch(entry["code"])
                or entry["class"] not in ("hard", "lint")
                or not isinstance(entry["summary"], str)):
            raise SmokeError("sai explain --list returned an invalid code registry")
        codes.append(entry["code"])
    if codes != sorted(set(codes), key=lambda code: code.encode("utf-8")):
        raise SmokeError("sai explain --list returned an unordered or duplicate registry")
    summary = summaries[0]
    if (summary["errors"] != 0 or summary["warnings"] != 0 or summary["infos"] != 0
            or summary["exit_code"] != 0):
        raise SmokeError("sai explain --list returned a nonempty summary")
    return set(codes)


def _resolve_spec_reference(reference: dict, docs_root: Path) -> None:
    document = reference.get("document")
    section = reference.get("section")
    if (not isinstance(document, str) or not isinstance(section, str)
            or not expectation_extractor.root_relative(document)
            or not section or not re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", section)):
        raise SmokeError("invalid spec section reference")
    parts = PurePosixPath(document).parts
    if len(parts) >= 2 and parts[0] in DOCUMENT_AREA and re.fullmatch(r"[0-9]{2}", parts[1]):
        candidates = list((docs_root / parts[0]).glob(parts[1] + "-*.md"))
        if len(parts) > 2:
            raise SmokeError("nested numbered spec document references are unsupported")
        document_path = candidates[0] if len(candidates) == 1 else None
    else:
        document_path = docs_root.joinpath(*parts).with_suffix(".md")
    if document_path is None:
        raise SmokeError(f"spec document does not resolve: {document}")
    try:
        canonical_docs_root = docs_root.resolve(strict=True)
        canonical_document = document_path.resolve(strict=True)
        canonical_document.relative_to(canonical_docs_root)
    except (OSError, RuntimeError, ValueError):
        raise SmokeError(f"spec document does not resolve beneath docs root: {document}") from None
    if not canonical_document.is_file():
        raise SmokeError(f"spec document does not resolve: {document}")
    heading = re.compile(r"^#{1,6}[ \t]+(" + re.escape(section) + r")(?:[ \t]+|$)")
    try:
        headings = canonical_document.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        raise SmokeError(f"could not read spec document: {document}") from None
    if not any(heading.match(line) for line in headings):
        raise SmokeError(f"spec section does not resolve: {document} §{section}")


def _validate_directives(extracted: dict, docs_root: Path) -> tuple[dict, list[dict], dict]:
    if not isinstance(extracted, dict) or set(extracted) != {"expected", "directives"}:
        raise SmokeError("expectation extractor returned an invalid value")
    expected = extracted["expected"]
    directives = extracted["directives"]
    if not isinstance(expected, list) or not isinstance(directives, dict):
        raise SmokeError("expectation extractor returned an invalid value")

    specified = directives.get("specified")
    if (not isinstance(specified, list)
            or any(not isinstance(name, str) for name in specified)
            or len(specified) != len(set(specified))):
        raise SmokeError("directive presence metadata is missing or invalid")
    if "targets" in specified or "profiles" in specified:
        raise SmokeError("Stage 0 target/profile directives are unsupported")
    if "reference-only" in specified:
        raise SmokeError(
            "reference-only fixtures are unsupported because IUT identity is not verified"
        )
    unsupported = set(specified) - ALLOWED_DIRECTIVES
    if unsupported:
        raise SmokeError("unsupported directive(s): " + ", ".join(sorted(unsupported)))
    if not {"kind", "stage", "spec"} <= set(specified):
        raise SmokeError("Stage 0 tests require kind, stage, and spec directives")
    kind = directives.get("kind")
    if kind not in ("check-pass", "check-fail", "fmt", "fmt-refuse"):
        raise SmokeError("only check-pass, check-fail, fmt, and fmt-refuse are supported")
    if directives.get("stage") != 0:
        raise SmokeError("only Stage 0 tests are supported")
    if not isinstance(directives.get("spec"), list) or not directives["spec"]:
        raise SmokeError("at least one spec section reference is required")
    for reference in directives["spec"]:
        _resolve_spec_reference(reference, docs_root)

    if kind in ("fmt", "fmt-refuse"):
        if kind == "fmt" and (expected or "diag" in specified):
            raise SmokeError("source fmt fixtures do not support diagnostic expectations")
        if kind == "fmt-refuse":
            refusal_codes = _formatter_refusal_codes(docs_root)
            if not expected or "diag" not in specified:
                raise SmokeError("source fmt-refuse fixtures require expected error diagnostics")
            for diagnostic in expected:
                if (
                    not isinstance(diagnostic, dict)
                    or diagnostic.get("file") != "src/lib.sai"
                    or type(diagnostic.get("line")) is not int
                    or diagnostic["line"] < 1
                    or diagnostic.get("severity") != "error"
                    or diagnostic.get("code") not in refusal_codes
                ):
                    raise SmokeError(
                        "source fmt-refuse expectations must be located formatter "
                        "refusal errors"
                    )
        # Stage 0's formatter command is targetless; configurations.select()
        # intentionally selects configurations only for Stage 0 checks.
        selected = {
            "selection": "formatter-smoke",
            "configurations": [{"target": None, "profile": None}],
        }
    else:
        selection_request = {
            "claim_stage": 0,
            "test_stage": 0,
            "kind": kind,
            "claimed_targets": [],
            "implementation": "reference",
            "reference_only": directives.get("reference_only") is not None,
        }
        selected = configurations.select(selection_request)
        if selected != {
            "selection": "configurations",
            "configurations": [{"target": None, "profile": None}],
        }:
            raise SmokeError("Stage 0 check did not select exactly one targetless configuration")
    for diagnostic in expected:
        if not isinstance(diagnostic, dict):
            raise SmokeError("invalid expected diagnostic")
        if diagnostic.get("file") not in (None, "src/lib.sai"):
            raise SmokeError("single-file checks can expect diagnostics only in src/lib.sai")
    return directives, expected, selected


def _reject_sidecars(source: Path, fixture_path: str, *, allow_expected: bool = False) -> None:
    stem = source.name[:-4]
    try:
        entries = list(source.parent.iterdir())
    except OSError:
        raise SmokeError(f"could not inspect fixture siblings: {fixture_path}") from None
    sources = {
        entry.name[:-4] for entry in entries
        if entry.is_file() and entry.name.endswith(".sai")
        and not entry.name.endswith(fixture_discovery.SOURCE_FILE_SIDECARS)
    }
    for entry in entries:
        if entry.is_dir() and entry.name.endswith((".wit", ".mir")):
            candidate = entry.name[:-4]
            if stem in fixture_discovery._owners(candidate, sources):
                raise SmokeError("sidecar directories are unsupported by the Stage 0 smoke runner")
        for suffix in SIDECAR_SUFFIXES:
            if entry.name.endswith(suffix):
                candidate = entry.name[:-len(suffix)]
                if stem in fixture_discovery._owners(candidate, sources):
                    if allow_expected and suffix == ".expected.sai" and entry.name == (
                        stem + ".expected.sai"
                    ):
                        continue
                    raise SmokeError("sidecars are unsupported by the Stage 0 smoke runner")


def _read_expected_format(source: Path, fixture_path: str) -> bytes:
    expected = source.with_name(source.stem + ".expected.sai")
    try:
        info = expected.lstat()
    except OSError:
        raise SmokeError(f"source fmt fixture requires {source.stem}.expected.sai: {fixture_path}") from None
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise SmokeError("formatter expected-output sidecar is not a regular file")
    try:
        contents = expected.read_bytes()
        contents.decode("utf-8", "strict")
    except (OSError, UnicodeError):
        raise SmokeError("formatter expected-output sidecar is not readable UTF-8") from None
    return contents


def _validate_fmt_output(
    completed: subprocess.CompletedProcess,
    *,
    expected_output: bytes,
    input_bytes: bytes,
    registered_codes: set[str],
    fixed_point: bool,
) -> None:
    records = _records(
        completed.stdout,
        exit_code=completed.returncode,
        stderr=completed.stderr,
        max_errors=0,
    )
    header = records[0]
    if (
        set(header) != {"type", "format", "version", "tool", "command", "root"}
        or header.get("command") != "fmt"
        or header.get("root") is not None
        or set(header.get("tool", {})) != {"name", "version"}
    ):
        raise SmokeError("sai fmt returned the wrong header")
    if (
        len(records) < 3
        or records[-2]["type"] != "result"
        or sum(record["type"] == "result" for record in records) != 1
        or any(record["type"] != "diagnostic" for record in records[1:-2])
    ):
        raise SmokeError("sai fmt returned the wrong record shape")
    summary = records[-1]
    if (
        set(summary) != {
            "type", "errors", "warnings", "infos", "shown_errors", "suppressed", "exit_code",
        }
        or set(summary["suppressed"]) != {"cascade", "duplicate", "dependency"}
    ):
        raise SmokeError("sai fmt returned an invalid summary schema")
    actual_diagnostics = _formatter_diagnostics(records, registered_codes)
    result = records[-2]
    if result.get("kind") != "fmt" or set(result) != {"type", "kind", "data"}:
        raise SmokeError("sai fmt returned an invalid result kind")
    data = result.get("data")
    fields = {"check", "files", "changed", "refused", "output"}
    if not isinstance(data, dict) or set(data) != fields:
        raise SmokeError("sai fmt returned an invalid result schema")
    if (
        data["check"] is not False
        or type(data["files"]) is not int
        or data["files"] != 1
        or not isinstance(data["changed"], list)
        or any(not isinstance(path, str) for path in data["changed"])
        or data["changed"] not in ([], ["<stdin>"])
        or not isinstance(data["refused"], list)
        or any(not isinstance(path, str) for path in data["refused"])
        or data["refused"] not in ([], ["<stdin>"])
        or (data["output"] is not None and not isinstance(data["output"], str))
    ):
        raise SmokeError("sai fmt result does not match the source stdin fixture")
    if completed.returncode != 0:
        if completed.returncode != 1:
            raise SmokeError("sai fmt returned an abnormal exit code")
        if (
            data["output"] is not None
            or data["changed"]
            or data["refused"] != ["<stdin>"]
            or not actual_diagnostics
            or any(item["severity"] != "error" for item in actual_diagnostics)
        ):
            raise SmokeError("sai fmt returned an invalid refusal")
        raise SmokeError("source fmt fixture was refused by the formatter")
    if actual_diagnostics:
        raise SmokeError("source fmt returned unexpected diagnostics")
    if data["refused"] or data["output"] is None:
        raise SmokeError("sai fmt refused the source stdin fixture")
    try:
        actual_output = data["output"].encode("utf-8", "strict")
    except UnicodeError:
        raise SmokeError("sai fmt output is not valid UTF-8 text") from None
    expected_changed = [] if input_bytes == actual_output else ["<stdin>"]
    if fixed_point and data["changed"]:
        raise SmokeError("formatting expected output was not a fixed point")
    if data["changed"] != expected_changed:
        raise SmokeError("sai fmt changed list does not match its input and output")
    if actual_output != expected_output:
        if fixed_point:
            raise SmokeError("formatting expected output changed its bytes")
        raise SmokeError("sai fmt output does not match the expected bytes")
    # _records validates the summary values and its exit/count relationship.
    if summary["exit_code"] != 0:
        raise SmokeError("sai fmt summary reports an unsuccessful command")


def _run_fmt_fixture(
    source_path: Path,
    fixture_path: str,
    source_bytes: bytes,
    iut: Path,
    registered_codes: set[str],
    *,
    timeout: float,
) -> None:
    expected_output = _read_expected_format(source_path, fixture_path)
    command = [
        "--format=json",
        "--max-errors=0",
        "fmt",
        "--stdin-kind",
        "source",
        "-",
    ]
    with tempfile.TemporaryDirectory(prefix="sai-stage0-fmt-") as temporary:
        sandbox_root = Path(temporary)
        package_root = sandbox_root / "package"
        package_root.mkdir()
        first = _run_iut(
            iut,
            command,
            package_root,
            timeout,
            state_root=sandbox_root,
            stdin_data=source_bytes,
        )
        _validate_fmt_output(
            first,
            expected_output=expected_output,
            input_bytes=source_bytes,
            registered_codes=registered_codes,
            fixed_point=False,
        )
        second = _run_iut(
            iut,
            command,
            package_root,
            timeout,
            state_root=sandbox_root,
            stdin_data=expected_output,
        )
        _validate_fmt_output(
            second,
            expected_output=expected_output,
            input_bytes=expected_output,
            registered_codes=registered_codes,
            fixed_point=True,
        )
        check_command = [
            "--format=json",
            "--max-errors=0",
            "fmt",
            "--check",
            "--stdin-kind",
            "source",
            "-",
        ]
        for check_input in (source_bytes, expected_output):
            checked = _run_iut(
                iut,
                check_command,
                package_root,
                timeout,
                state_root=sandbox_root,
                stdin_data=check_input,
            )
            _validate_fmt_check(
                checked,
                input_bytes=check_input,
                expected_output=expected_output,
                registered_codes=registered_codes,
            )

        source_dir = package_root / "src"
        source_dir.mkdir()
        manifest_file = package_root / "sai.package"
        manifest_file.write_text(
            PACKAGE_MANIFEST,
            encoding="utf-8",
            newline="\n",
        )
        source_file = source_dir / "lib.sai"
        expected_sources = ("src/lib.sai",)
        manifest_state = _file_state(manifest_file)
        package_check_command = [
            "--format=json",
            "--max-errors=0",
            "fmt",
            "--check",
        ]
        path_check_command = [*package_check_command, "src/lib.sai"]
        for check_input in (source_bytes, expected_output):
            for check_command, file_count in (
                (package_check_command, 2),
                (path_check_command, 1),
            ):
                source_file.write_bytes(check_input)
                if _package_sai_inventory(package_root) != expected_sources:
                    raise SmokeError(
                        "generated formatter package has an unexpected source inventory"
                    )
                source_state = _file_state(source_file)
                checked = _run_iut(
                    iut,
                    check_command,
                    package_root,
                    timeout,
                    state_root=sandbox_root,
                )
                _validate_fmt_check(
                    checked,
                    input_bytes=check_input,
                    expected_output=expected_output,
                    registered_codes=registered_codes,
                    file_path="src/lib.sai",
                    file_count=file_count,
                )
                package_sources = _package_sai_inventory(package_root)
                if package_sources != expected_sources:
                    raise SmokeError(
                        f"sai fmt --check changed the package source inventory: "
                        f"{package_sources!r}"
                    )
                if _file_state(source_file) != source_state:
                    raise SmokeError(
                        "sai fmt --check modified or replaced the source fixture"
                    )
                if source_file.read_bytes() != check_input:
                    raise SmokeError("sai fmt --check modified the source fixture")
                if _file_state(manifest_file) != manifest_state:
                    raise SmokeError(
                        "sai fmt --check modified or replaced the generated package contract"
                    )
                if manifest_file.read_bytes() != PACKAGE_MANIFEST.encode("utf-8"):
                    raise SmokeError(
                        "sai fmt --check modified the generated package contract"
                    )


def _package_sai_inventory(package_root: Path) -> tuple[str, ...]:
    paths = (
        path.relative_to(package_root).as_posix()
        for path in package_root.rglob("*.sai")
        if path.is_file()
    )
    return tuple(sorted(paths))


def _file_state(path: Path) -> tuple[int, int, int, int, int, int, int] | None:
    try:
        info = path.lstat()
    except OSError:
        return None
    return (
        info.st_mode,
        info.st_dev,
        info.st_ino,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
        info.st_nlink,
    )


def _split_lf_lines(source: bytes) -> list[bytes]:
    """Split only after LF, preserving each terminator and any preceding CR."""
    lines = []
    start = 0
    while start < len(source):
        newline = source.find(b"\n", start)
        if newline < 0:
            lines.append(source[start:])
            break
        lines.append(source[start:newline + 1])
        start = newline + 1
    return lines


def _first_differing_lf_line(
    input_bytes: bytes,
    expected_output: bytes,
) -> tuple[int, bytes | None, int, int] | None:
    """Return line, expected content, and input span for the first byte difference."""
    input_lines = _split_lf_lines(input_bytes)
    output_lines = _split_lf_lines(expected_output)
    offset = 0
    for index in range(max(len(input_lines), len(output_lines))):
        input_line = input_lines[index] if index < len(input_lines) else None
        output_line = output_lines[index] if index < len(output_lines) else None
        if input_line == output_line:
            if input_line is not None:
                offset += len(input_line)
            continue
        if input_line is None:
            start = end = len(input_bytes)
        else:
            start = offset
            end = start + len(input_line)
            if input_line.endswith(b"\n"):
                end -= 1
                if end > start and input_bytes[end - 1] == 13:
                    end -= 1
        if output_line is None:
            expected_line = None
        else:
            expected_line = (
                output_line[:-1] if output_line.endswith(b"\n") else output_line
            )
        return index + 1, expected_line, start, end
    return None


def _source_position(input_bytes: bytes, offset: int) -> dict[str, int]:
    """Map an original-file byte offset to its diagnostic position."""
    bom_length = 3 if input_bytes.startswith(b"\xef\xbb\xbf") else 0
    if offset <= bom_length:
        return {"line": 1, "col": 1, "utf16_col": 1}

    line = 1
    line_start = bom_length
    cursor = bom_length
    while cursor < offset:
        byte = input_bytes[cursor]
        if byte == 13:
            terminator_end = (
                cursor + 2
                if input_bytes[cursor:cursor + 2] == b"\r\n"
                else cursor + 1
            )
            if terminator_end > offset:
                break
            line += 1
            line_start = terminator_end
            cursor = terminator_end
        elif byte == 10:
            line += 1
            line_start = cursor + 1
            cursor += 1
        else:
            cursor += 1

    prefix = input_bytes[line_start:offset].decode("utf-8", "strict")
    return {
        "line": line,
        "col": offset - line_start + 1,
        "utf16_col": 1 + sum(2 if ord(character) > 0xFFFF else 1 for character in prefix),
    }


def _formatter_envelope(
    completed: subprocess.CompletedProcess,
    *,
    registered_codes: set[str],
    primary_file: str = "<stdin>",
) -> tuple[list[dict], dict, dict, list[dict]]:
    records = _records(
        completed.stdout,
        exit_code=completed.returncode,
        stderr=completed.stderr,
        max_errors=0,
    )
    header = records[0]
    if (
        set(header) != {"type", "format", "version", "tool", "command", "root"}
        or header.get("command") != "fmt"
        or header.get("root") is not None
        or not isinstance(header.get("tool"), dict)
        or set(header["tool"]) != {"name", "version"}
    ):
        raise SmokeError("sai fmt returned the wrong header")
    if (
        len(records) < 3
        or records[-2]["type"] != "result"
        or sum(record["type"] == "result" for record in records) != 1
        or any(record["type"] != "diagnostic" for record in records[1:-2])
    ):
        raise SmokeError("sai fmt returned the wrong record shape")
    summary = records[-1]
    if (
        set(summary) != {
            "type", "errors", "warnings", "infos", "shown_errors", "suppressed", "exit_code",
        }
        or not isinstance(summary.get("suppressed"), dict)
        or set(summary["suppressed"]) != {"cascade", "duplicate", "dependency"}
    ):
        raise SmokeError("sai fmt returned an invalid summary schema")
    diagnostics = records[1:-2]
    expected_diagnostic_fields = {
        "type", "code", "severity", "level", "message", "primary", "primary_label",
        "labels", "children", "fixes", "related", "data", "node", "symbol", "package",
    }
    if any(set(item) != expected_diagnostic_fields for item in diagnostics):
        raise SmokeError("sai fmt returned an invalid diagnostic schema")
    actual = _formatter_diagnostics(
        records,
        registered_codes,
        primary_file=primary_file,
    )
    result = records[-2]
    if result.get("kind") != "fmt" or set(result) != {"type", "kind", "data"}:
        raise SmokeError("sai fmt returned an invalid result kind")
    data = result.get("data")
    fields = {"check", "files", "changed", "refused", "output"}
    if not isinstance(data, dict) or set(data) != fields:
        raise SmokeError("sai fmt returned an invalid result schema")
    return actual, data, summary, diagnostics


def _validate_fmt_check(
    completed: subprocess.CompletedProcess,
    *,
    input_bytes: bytes,
    expected_output: bytes,
    registered_codes: set[str],
    file_path: str = "<stdin>",
    file_count: int = 1,
) -> None:
    actual, data, summary, diagnostics = _formatter_envelope(
        completed,
        registered_codes=registered_codes,
        primary_file=file_path,
    )
    difference = _first_differing_lf_line(input_bytes, expected_output)
    if (
        type(data["check"]) is not bool
        or data["check"] is not True
        or type(data["files"]) is not int
        or data["files"] != file_count
        or not isinstance(data["changed"], list)
        or data["changed"] != ([] if difference is None else [file_path])
        or not isinstance(data["refused"], list)
        or data["refused"] != []
        or data["output"] is not None
    ):
        raise SmokeError("sai fmt --check returned an invalid fixture result")
    if summary["suppressed"] != {
        "cascade": 0,
        "duplicate": 0,
        "dependency": 0,
    }:
        raise SmokeError("sai fmt --check suppressed unexpected diagnostics")
    if difference is None:
        if diagnostics or actual:
            raise SmokeError("canonical formatter output produced a --check diagnostic")
        if completed.returncode != 0 or summary["exit_code"] != 0:
            raise SmokeError("sai fmt --check returned an unexpected exit code")
        return
    if completed.returncode != 1 or summary["exit_code"] != 1:
        raise SmokeError("sai fmt --check returned an unexpected exit code")
    if len(diagnostics) != 1 or len(actual) != 1:
        raise SmokeError("noncanonical formatter input did not produce exactly one diagnostic")

    line, expected_line, start_byte, end_byte = difference
    diagnostic = diagnostics[0]
    if (
        diagnostic["code"] != "UNFORMATTED_FILE"
        or diagnostic["severity"] != "error"
        or diagnostic["message"] != "file is not formatted"
        or diagnostic["level"] is not None
        or diagnostic["primary_label"] != ""
        or diagnostic["labels"] != []
        or diagnostic["fixes"] != []
        or diagnostic["related"] != []
        or diagnostic["node"] is not None
        or diagnostic["symbol"] is not None
        or diagnostic["package"] is not None
        or diagnostic["children"] != [{
            "kind": "help",
            "message": "run `sai fmt <path>` to format this file",
            "location": None,
        }]
        or diagnostic["data"] != {
            "line": line,
            "expected": (
                None if expected_line is None else expected_line.decode("utf-8", "strict")
            ),
        }
    ):
        raise SmokeError("UNFORMATTED_FILE does not match the first differing line")
    primary = diagnostic["primary"]
    expected_start = {
        "byte": start_byte,
        **_source_position(input_bytes, start_byte),
    }
    expected_end = {
        "byte": end_byte,
        **_source_position(input_bytes, end_byte),
    }
    if (
        primary is None
        or primary["file"] != {"path": file_path, "package": None, "snapshot": None}
        or primary["start"] != expected_start
        or primary["end"] != expected_end
        or expected_start["line"] != line
        or expected_end["line"] != line
    ):
        raise SmokeError("UNFORMATTED_FILE has the wrong primary position or byte span")


def _validate_fmt_refusal(
    completed: subprocess.CompletedProcess,
    *,
    expected: list[dict],
    registered_codes: set[str],
    refusal_codes: set[str],
) -> None:
    actual, data, summary, _ = _formatter_envelope(
        completed,
        registered_codes=registered_codes,
    )
    if completed.returncode == 0:
        raise SmokeError("source fmt-refuse fixture was accepted by the formatter")
    if (
        type(data["check"]) is not bool
        or data["check"] is not False
        or type(data["files"]) is not int
        or data["files"] != 1
        or not isinstance(data["changed"], list)
        or data["changed"] != []
        or not isinstance(data["refused"], list)
        or data["refused"] != ["<stdin>"]
        or data["output"] is not None
    ):
        raise SmokeError("sai fmt refusal result does not match the source stdin fixture")
    if completed.returncode != 1 or summary["exit_code"] != 1:
        raise SmokeError("sai fmt-refuse returned an abnormal exit code")
    if not actual:
        raise SmokeError("sai fmt-refuse returned no diagnostic")
    if any(
        item["severity"] != "error" or item["code"] not in refusal_codes
        for item in actual
    ):
        raise SmokeError(
            "formatter refusal included a non-parser or unsupported diagnostic"
        )
    expected_counts = Counter(
        (item["file"], item["line"], item["severity"], item["code"])
        for item in expected
    )
    actual_counts = Counter(
        (item["file"], item["line"], item["severity"], item["code"])
        for item in actual
    )
    if expected_counts != actual_counts:
        raise SmokeError("formatter refusal does not match the expected error multiset")


def _actual_diagnostics(records: list[dict], codes: set[str]) -> list[dict]:
    actual = []
    for record in records:
        if record["type"] != "diagnostic":
            continue
        code = record["code"]
        if code not in codes:
            raise SmokeError(f"unregistered diagnostic code: {code}")
        package = record.get("package")
        if package not in (None, ROOT_ID):
            raise SmokeError("diagnostics from dependencies are unsupported")
        primary = record["primary"]
        if primary is None:
            file = line = None
            start_byte = end_byte = 0
        else:
            source = primary["file"]
            file_path = source["path"]
            if source.get("package") != ROOT_ID:
                raise SmokeError("diagnostic primary location is outside the root package")
            if file_path not in ("src/lib.sai", "sai.package"):
                raise SmokeError("diagnostic primary path is outside the supported fixture")
            file = file_path
            line = primary["start"]["line"]
            start_byte = primary["start"]["byte"]
            end_byte = primary["end"]["byte"]
        severity = {"warn": "warning"}.get(record["severity"], record["severity"])
        actual.append({
            "file": file,
            "line": line,
            "severity": severity,
            "code": code,
            "_order": (
                0 if primary is None else 1,
                0,
                0 if file == "sai.package" else 1 if file == "sai.lock" else 2,
                file.encode("utf-8") if file is not None else b"",
                start_byte,
                end_byte,
                SEVERITY_RANK[severity],
                code.encode("utf-8"),
                record["message"].encode("utf-8"),
                _canonical_json(record),
            ),
        })
    if [item["_order"] for item in actual] != sorted(item["_order"] for item in actual):
        raise SmokeError("diagnostics are not in specification order")
    return [{key: value for key, value in item.items() if key != "_order"} for item in actual]


def _validate_check_result(records: list[dict], *, kind: str, exit_code: int,
                           diagnostics: list[dict]) -> None:
    results = [record for record in records if record["type"] == "result"]
    has_error = any(item["severity"] == "error" for item in diagnostics)
    if exit_code == 0:
        if kind != "check-pass" or has_error or len(results) != 1:
            raise SmokeError("successful check must have one result and match check-pass")
    else:
        if exit_code != 1 or kind != "check-fail" or not has_error or results:
            raise SmokeError("failed check must have errors and no result, matching check-fail")
    if not results:
        return
    result = results[0]
    if result.get("kind") != "check" or set(result) != {"type", "kind", "data"}:
        raise SmokeError("sai check returned an invalid result kind")
    data = result.get("data")
    expected_fields = {
        "target", "profile", "tests", "packages", "fixes_applied", "changed_files",
    }
    if not isinstance(data, dict) or set(data) != expected_fields:
        raise SmokeError("sai check returned an invalid result schema")
    if (data["target"] != "wasm32-core" or data["profile"] != "dev"
            or type(data["tests"]) is not bool or data["tests"] is not False
            or type(data["packages"]) is not int or data["packages"] != 1
            or type(data["fixes_applied"]) is not int or data["fixes_applied"] != 0
            or data["changed_files"] != []):
        raise SmokeError("sai check result does not match the smoke package")


def _read_markdown_section(
    docs_root: Path,
    *,
    relative_path: str,
    heading: str,
    level: int,
) -> list[str]:
    path = docs_root / relative_path
    try:
        resolved_docs = docs_root.resolve(strict=True)
        resolved_path = path.resolve(strict=True)
        resolved_path.relative_to(resolved_docs)
        lines = resolved_path.read_text(encoding="utf-8").splitlines()
    except (OSError, RuntimeError, UnicodeError, ValueError):
        raise SmokeError("could not read formatter refusal diagnostic definitions") from None
    marker = "#" * level + " " + heading
    start = None
    end = len(lines)
    heading_pattern = re.compile(r"^(#{1," + str(level) + r"})[ \t]+")
    for index, line in enumerate(lines):
        if line == marker:
            start = index + 1
            continue
        if start is not None and heading_pattern.match(line):
            end = index
            break
    if start is None:
        raise SmokeError("formatter refusal diagnostic definition section is missing")
    return lines[start:end]


def _table_cells(line: str) -> list[str] | None:
    stripped = line.strip()
    if not stripped.startswith("|") or not stripped.endswith("|"):
        return None
    cells = []
    start = 1
    in_code = False
    for index in range(1, len(stripped) - 1):
        char = stripped[index]
        if char == "`":
            in_code = not in_code
        elif char == "|" and not in_code:
            cells.append(stripped[start:index].strip())
            start = index + 1
    cells.append(stripped[start:-1].strip())
    return cells


def _formatter_refusal_codes(docs_root: Path) -> set[str]:
    code_pattern = re.compile(r"`([A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)+)`")
    lexical_lines = _read_markdown_section(
        docs_root,
        relative_path="spec/02-lexical-structure.md",
        heading="2.12 Lexical diagnostics",
        level=2,
    )
    parser_lines = _read_markdown_section(
        docs_root,
        relative_path="impl/02-frontend-ast-hir.md",
        heading="2.5.12 Syntactic validation",
        level=3,
    )
    diagnostic_lines = _read_markdown_section(
        docs_root,
        relative_path="impl/02-frontend-ast-hir.md",
        heading="2.12 Diagnostics",
        level=2,
    )
    codes: set[str] = set()
    lexical_codes: set[str] = set()
    parser_codes: set[str] = set()
    diagnostic_severities: dict[str, str] = {}
    found_lexical_table = False
    for line in lexical_lines:
        cells = _table_cells(line)
        if not cells:
            continue
        if cells == ["Code", "Severity", "Description"]:
            found_lexical_table = True
            continue
        if not cells[0].startswith("`"):
            continue
        if len(cells) != 3:
            raise SmokeError("formatter lexical diagnostic table has an unsupported row")
        matches = code_pattern.findall(cells[0])
        if len(matches) != 1 or "`" + matches[0] + "`" != cells[0]:
            raise SmokeError("formatter lexical diagnostic table has an unsupported row")
        if cells[1] not in ("error", "warning", "warning (lint)"):
            raise SmokeError("formatter lexical diagnostic severity is unsupported")
        if cells[1] == "error" and matches[0] != "LITERAL_OUT_OF_RANGE":
            lexical_codes.add(matches[0])

    found_parser_table = False
    for line in parser_lines:
        cells = _table_cells(line)
        if not cells:
            continue
        if cells == ["Code", "Where", "Check"]:
            found_parser_table = True
            continue
        if not cells[0].startswith("`"):
            continue
        if len(cells) != 3:
            raise SmokeError("formatter parser diagnostic table has an unsupported row")
        row_codes = code_pattern.findall(cells[0])
        if not row_codes:
            raise SmokeError("formatter parser diagnostic table has an unsupported row")
        remainder = code_pattern.sub("", cells[0]).replace(",", "").strip()
        if remainder:
            raise SmokeError("formatter parser diagnostic code list is malformed")
        # Only an exact parser phase is accepted. Rows with mixed phases such
        # as "parser for ..., validation for ..." are excluded as whole rows.
        if cells[1] == "parser":
            parser_codes.update(row_codes)

    found_diagnostic_table = False
    for line in diagnostic_lines:
        cells = _table_cells(line)
        if not cells:
            continue
        if cells == ["Code", "Severity", "Description"]:
            found_diagnostic_table = True
            continue
        if not cells[0].startswith("`"):
            continue
        if len(cells) != 3:
            raise SmokeError("formatter diagnostic severity table has an unsupported row")
        row_codes = code_pattern.findall(cells[0])
        if not row_codes:
            raise SmokeError("formatter diagnostic severity table has an unsupported row")
        remainder = code_pattern.sub("", cells[0]).replace(",", "").strip()
        if remainder or cells[1] not in ("error", "warning", "warning (lint)"):
            raise SmokeError("formatter diagnostic severity row is malformed")
        for code in row_codes:
            previous = diagnostic_severities.setdefault(code, cells[1])
            if previous != cells[1]:
                raise SmokeError("formatter diagnostic severity is inconsistent")

    if (
        not found_lexical_table
        or not found_parser_table
        or not found_diagnostic_table
    ):
        raise SmokeError("formatter refusal diagnostic tables are incomplete")
    unknown_parser_codes = parser_codes - diagnostic_severities.keys()
    if unknown_parser_codes:
        raise SmokeError(
            "formatter parser diagnostics are missing severity definitions"
        )
    # The implementation diagnostic table is authoritative for parser severity.
    # A new parser-phase warning must remain excluded even if it is absent from
    # the lexical table (which contains only lexer-owned severities).
    parser_codes = {
        code for code in parser_codes
        if diagnostic_severities[code] == "error"
    }
    parser_codes.discard("CLAUSE_ORDER")
    codes = lexical_codes | parser_codes
    required = {
        "INVALID_UTF8",
        "EXPECTED_TOKEN",
        "EXPECTED_EXPRESSION",
        "UNEXPECTED_NEWLINE",
        "IMPLEMENTATION_LIMIT_EXCEEDED",
    }
    excluded = {
        "LITERAL_OUT_OF_RANGE",
        "CLAUSE_ORDER",
        "MISSING_FN_BODY",
        "UNKNOWN_ATTRIBUTE",
        "INFER_IN_SIGNATURE",
        "NAMED_ARGUMENT_ON_FUNCTION",
        "INVALID_ASSIGN_TARGET",
        "INVARIANT_OUTSIDE_DEFINING_MODULE",
        "OR_PATTERN_BINDING_MISMATCH",
        "FEATURE_NOT_IMPLEMENTED",
        "GENERIC_INTERFACE_ITEM",
        "INTERFACE_FN_CLAUSE",
        "EFFECTS_ON_INTERFACE_FN",
    }
    if not required <= codes or codes & excluded:
        raise SmokeError("formatter refusal diagnostic tables do not match the supported taxonomy")
    return codes


def _formatter_diagnostics(
    records: list[dict],
    codes: set[str],
    *,
    primary_file: str = "<stdin>",
) -> list[dict]:
    actual = []
    for record in records:
        if record["type"] != "diagnostic":
            continue
        code = record["code"]
        if code == "INTERNAL_COMPILER_ERROR":
            raise SmokeError("formatter reported an internal compiler error")
        if code not in codes:
            raise SmokeError(f"unregistered diagnostic code: {code}")
        if record.get("package") is not None:
            raise SmokeError("formatter preflight diagnostic has an unexpected package")
        primary = record["primary"]
        if primary is None:
            file = line = None
        else:
            source = primary["file"]
            if (
                source["path"] != primary_file
                or source.get("package") is not None
                or source.get("snapshot") is not None
            ):
                raise SmokeError("formatter diagnostic is outside the selected fixture file")
            file = "src/lib.sai"
            line = primary["start"]["line"]
        severity = {"warn": "warning"}.get(record["severity"], record["severity"])
        actual.append({
            "file": file,
            "line": line,
            "severity": severity,
            "code": code,
        })
    return actual


def _validate_formatter_preflight(
    completed: subprocess.CompletedProcess,
    *,
    expected: list[dict],
    registered_codes: set[str],
    formatter_refusal_codes: set[str],
) -> None:
    records = _records(
        completed.stdout,
        exit_code=completed.returncode,
        stderr=completed.stderr,
        max_errors=0,
    )
    header = records[0]
    if header.get("command") != "fmt" or header.get("root") is not None:
        raise SmokeError("sai fmt --check returned the wrong header")
    if (
        len(records) < 3
        or records[-2]["type"] != "result"
        or sum(record["type"] == "result" for record in records) != 1
        or any(record["type"] != "diagnostic" for record in records[1:-2])
    ):
        raise SmokeError("sai fmt --check returned no result")
    result = records[-2]
    if result.get("kind") != "fmt" or set(result) != {"type", "kind", "data"}:
        raise SmokeError("sai fmt --check returned an invalid result kind")
    data = result.get("data")
    fields = {"check", "files", "changed", "refused", "output"}
    if not isinstance(data, dict) or set(data) != fields:
        raise SmokeError("sai fmt --check returned an invalid result schema")
    if (
        data["check"] is not True
        or type(data["files"]) is not int
        or data["files"] != 1
        or not isinstance(data["changed"], list)
        or not isinstance(data["refused"], list)
        or data["output"] is not None
    ):
        raise SmokeError("sai fmt --check result does not match the stdin fixture")

    actual = _formatter_diagnostics(records, registered_codes)
    expected_parse_errors = [
        item for item in expected
        if item["severity"] == "error" and item["code"] in formatter_refusal_codes
    ]
    if completed.returncode == 0:
        if expected_parse_errors:
            raise SmokeError(
                "formatter accepted source with explicitly expected refusal errors"
            )
        if actual or data["changed"] or data["refused"]:
            raise SmokeError("sai fmt --check did not confirm canonical source")
        return

    if completed.returncode != 1:
        raise SmokeError("sai fmt --check returned an abnormal exit code")
    if data["changed"] == ["<stdin>"] and not data["refused"]:
        raise SmokeError("source is not in canonical formatter form")
    if data["changed"] or data["refused"] != ["<stdin>"]:
        raise SmokeError("sai fmt --check returned an unexpected refusal")
    if not actual or any(
        item["severity"] != "error" or item["code"] not in formatter_refusal_codes
        for item in actual
    ):
        raise SmokeError("formatter refusal included a non-parser or unsupported diagnostic")

    expected_parse_counts = Counter(
        (item["file"], item["line"], item["severity"], item["code"])
        for item in expected_parse_errors
    )
    actual_parse_counts = Counter(
        (item["file"], item["line"], item["severity"], item["code"])
        for item in actual
    )
    if not expected_parse_errors or expected_parse_counts != actual_parse_counts:
        raise SmokeError(
            "formatter refusal does not match explicitly expected errors"
        )


def _run_fixture(fixture: dict, conformance_root: Path, iut: Path, codes: set[str],
                 *, timeout: float, docs_root: Path) -> dict:
    path = fixture["path"]
    output = {
        "type": "test",
        "test": path,
        "mode": "partial-smoke",
        "claim": False,
        "configuration": {"target": None, "profile": None},
        "compatibility_execution": None,
        "outcome": "fail",
        "deviations": [],
        "failures": [],
    }
    try:
        if fixture["kind"] != "single-file":
            raise SmokeError("directory fixtures are unsupported")
        parts = PurePosixPath(fixture["root_file"]).parts
        if not parts or parts[0] != "conformance":
            raise SmokeError("discovery returned an invalid fixture path")
        source_path = conformance_root.joinpath(*parts[1:])
        try:
            info = source_path.lstat()
        except OSError:
            raise SmokeError(f"could not inspect fixture source: {path}") from None
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise SmokeError("fixture source is not a regular file")
        try:
            source_bytes = source_path.read_bytes()
        except OSError:
            raise SmokeError(f"could not read fixture source: {path}") from None
        extracted = expectation_extractor.extract_bytes_with_directives(
            source_bytes, "src/lib.sai"
        )
        directives, expected, selection = _validate_directives(extracted, docs_root)
        output["configuration"] = selection["configurations"][0]
        output["reference_only"] = directives["reference_only"] is not None
        if directives["kind"] == "fmt":
            _reject_sidecars(source_path, path, allow_expected=True)
            _run_fmt_fixture(
                source_path,
                path,
                source_bytes,
                iut,
                codes,
                timeout=timeout,
            )
            output["outcome"] = "pass"
            return output
        if directives["kind"] == "fmt-refuse":
            _reject_sidecars(source_path, path)
            refusal_codes = _formatter_refusal_codes(docs_root)
            command = [
                "--format=json",
                "--max-errors=0",
                "fmt",
                "--stdin-kind",
                "source",
                "-",
            ]
            with tempfile.TemporaryDirectory(prefix="sai-stage0-fmt-refuse-") as temporary:
                sandbox_root = Path(temporary)
                package_root = sandbox_root / "package"
                package_root.mkdir()
                completed = _run_iut(
                    iut,
                    command,
                    package_root,
                    timeout,
                    state_root=sandbox_root,
                    stdin_data=source_bytes,
                )
            _validate_fmt_refusal(
                completed,
                expected=expected,
                registered_codes=codes,
                refusal_codes=refusal_codes,
            )
            output["outcome"] = "pass"
            return output
        output["compatibility_execution"] = {
            "manifest_target": "wasm32-core",
            "cli_profile_default": "dev",
        }
        _reject_sidecars(source_path, path)
        if any(item["file"] == "sai.package" for item in expected):
            raise SmokeError("manifest diagnostics are unsupported for generated packages")

        with tempfile.TemporaryDirectory(prefix="sai-stage0-test-") as temporary:
            sandbox_root = Path(temporary)
            package_root = sandbox_root / "package"
            package_root.mkdir()
            formatter = _run_iut(
                iut,
                [
                    "--format=json",
                    "--max-errors=0",
                    "fmt",
                    "--check",
                    "--stdin-kind",
                    "source",
                    "-",
                ],
                package_root,
                timeout,
                state_root=sandbox_root,
                stdin_data=source_bytes,
            )
            _validate_formatter_preflight(
                formatter,
                expected=expected,
                registered_codes=codes,
                formatter_refusal_codes=_formatter_refusal_codes(docs_root),
            )
            source_dir = package_root / "src"
            source_dir.mkdir()
            (package_root / "sai.package").write_text(PACKAGE_MANIFEST, encoding="utf-8", newline="\n")
            (source_dir / "lib.sai").write_bytes(source_bytes)
            completed = _run_iut(
                iut,
                ["--format=json", "--max-errors=0", "check", "--jobs=1"],
                package_root,
                timeout,
                state_root=sandbox_root,
            )
        records = _records(
            completed.stdout, exit_code=completed.returncode,
            stderr=completed.stderr, max_errors=0,
        )
        header = records[0]
        if header.get("command") != "check" or header.get("root") != ROOT_ID:
            raise SmokeError("sai check returned the wrong header or root")
        if completed.returncode not in (0, 1):
            raise SmokeError("sai check returned an abnormal exit code")
        actual = _actual_diagnostics(records, codes)
        _validate_check_result(
            records, kind=directives["kind"], exit_code=completed.returncode,
            diagnostics=actual,
        )
        comparison = diagnostic_matcher.compare({
            "kind": directives["kind"],
            "implementation": "reference",
            "exit_code": completed.returncode,
            "expected": expected,
            "actual": actual,
        })
        output["outcome"] = comparison["outcome"]
        output["deviations"] = comparison["deviations"]
        output["failures"] = comparison["failures"]
    except (SmokeError, ValueError, ndjson_stream.StreamError) as error:
        output["failures"] = [str(error)]
    except Exception:
        output["failures"] = ["unexpected smoke-runner failure"]
    return output


def run_suite(iut: os.PathLike | str, conformance_directory: os.PathLike | str,
              *, timeout: float = TIMEOUT_SECONDS) -> dict:
    """Run discovered Stage 0 check and source-fmt fixtures without a claim."""
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
            "Stage 0 selects one targetless/profileless configuration; the current CLI "
            "uses the generated package's wasm32-core/dev defaults for execution.",
            "The caller must supply this repository's reference sai binary; the runner "
            "cannot verify the identity of the executable passed with --iut.",
            "Formatter coverage is limited to single-file source fmt goldens, their "
            "fixed-point, source-stdin, generated package-wide, and fmt --check with "
            "one explicit path, and refusal cases. Package-contract formatting "
            "variants and goldens, workspaces, multi-source package checks, multiple "
            "explicit path arguments, layout variants, and complete "
            "formatter-rule coverage are not covered.",
            "This smoke subset does not establish section, diagnostic, or suite coverage "
            "and cannot substantiate a conformance claim.",
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
        conformance_root = Path(conformance_directory)
        fixtures = fixture_discovery.discover(conformance_root)
        if not fixtures:
            raise SmokeError("no Stage 0 smoke fixtures were discovered")
        codes = _read_registry(iut_path, timeout=timeout)
        docs_root = Path(__file__).resolve().parents[2] / "docs"
        results = [
            _run_fixture(fixture, conformance_root, iut_path, codes,
                         timeout=timeout, docs_root=docs_root)
            for fixture in fixtures
        ]
        report["results"] = results
        report["tests"] = len(results)
        report["passed"] = sum(result["outcome"] in ("pass", "pass-with-deviations")
                               for result in results)
        report["failed"] = len(results) - report["passed"]
        report["outcome"] = "pass" if report["failed"] == 0 else "fail"
    except (SmokeError, fixture_discovery.DiscoveryError, OSError, ValueError) as error:
        report["failures"] = [str(error)]
        report["outcome"] = "fail"
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--iut",
        required=True,
        help="absolute path to this repository's reference sai executable; identity is not verified",
    )
    parser.add_argument("conformance_directory", type=Path)
    arguments = parser.parse_args()
    report = run_suite(arguments.iut, arguments.conformance_directory)
    print(json.dumps(report, ensure_ascii=False, separators=(",", ":"), allow_nan=False))
    return 0 if report["outcome"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
