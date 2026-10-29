#!/usr/bin/env python3
"""Run a deliberately partial Stage 0 smoke suite against a black-box IUT.

This runner accepts only reference-implementation, single-file Stage 0
check-pass/check-fail tests. Its results are useful smoke evidence only; it
never reports a conformance claim.
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
ALLOWED_DIRECTIVES = {"kind", "stage", "spec", "diag", "reference-only"}
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
             state_root: Path | None = None) -> subprocess.CompletedProcess:
    state_root = cwd if state_root is None else state_root
    cache = state_root / "cache"
    home = state_root / "home"
    cache.mkdir()
    home.mkdir()
    environment = {
        "NO_COLOR": "1",
        "SAI_CACHE_DIR": str(cache),
        "HOME": str(home),
    }
    try:
        return subprocess.run(
            [str(iut), *argv],
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
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
    unsupported = set(specified) - ALLOWED_DIRECTIVES
    if unsupported:
        raise SmokeError("unsupported directive(s): " + ", ".join(sorted(unsupported)))
    if not {"kind", "stage", "spec"} <= set(specified):
        raise SmokeError("Stage 0 tests require kind, stage, and spec directives")
    if directives.get("kind") not in ("check-pass", "check-fail"):
        raise SmokeError("only check-pass and check-fail are supported")
    if directives.get("stage") != 0:
        raise SmokeError("only Stage 0 tests are supported")
    if not isinstance(directives.get("spec"), list) or not directives["spec"]:
        raise SmokeError("at least one spec section reference is required")
    for reference in directives["spec"]:
        _resolve_spec_reference(reference, docs_root)

    selection_request = {
        "claim_stage": 0,
        "test_stage": 0,
        "kind": directives["kind"],
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


def _reject_sidecars(source: Path, fixture_path: str) -> None:
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
                    raise SmokeError("sidecars are unsupported by the Stage 0 smoke runner")


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


def _run_fixture(fixture: dict, conformance_root: Path, iut: Path, codes: set[str],
                 *, timeout: float, docs_root: Path) -> dict:
    path = fixture["path"]
    output = {
        "type": "test",
        "test": path,
        "mode": "partial-smoke",
        "claim": False,
        "configuration": {"target": None, "profile": None},
        "compatibility_execution": {"manifest_target": "wasm32-core", "cli_profile_default": "dev"},
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
        _reject_sidecars(source_path, path)
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
        if any(item["file"] == "sai.package" for item in expected):
            raise SmokeError("manifest diagnostics are unsupported for generated packages")

        with tempfile.TemporaryDirectory(prefix="sai-stage0-test-") as temporary:
            sandbox_root = Path(temporary)
            package_root = sandbox_root / "package"
            package_root.mkdir()
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
    """Run discovered Stage 0 check fixtures and return a non-claim report."""
    report = {
        "type": "summary",
        "mode": "partial-smoke",
        "claim": False,
        "outcome": "fail",
        "tests": 0,
        "passed": 0,
        "failed": 0,
        "results": [],
        "failures": [],
        "limitations": [
            "Stage 0 selects one targetless/profileless configuration; the current CLI "
            "uses the generated package's wasm32-core/dev defaults for execution.",
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
    parser.add_argument("--iut", required=True, help="absolute path to the IUT executable")
    parser.add_argument("conformance_directory", type=Path)
    arguments = parser.parse_args()
    report = run_suite(arguments.iut, arguments.conformance_directory)
    print(json.dumps(report, ensure_ascii=False, separators=(",", ":"), allow_nan=False))
    return 0 if report["outcome"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
