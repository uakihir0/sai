#!/usr/bin/env python3
"""Compare normalized diagnostics for one conformance-check command."""

from __future__ import annotations

import json
import re
import sys
from collections import Counter
from decimal import Decimal, DecimalException


SEVERITIES = {"error", "warning", "info"}
KINDS = {"check-pass", "check-fail", "build-fail-check"}
IMPLEMENTATIONS = {"reference", "other"}


def is_root_relative_path(file: str) -> bool:
    if "\\" in file or "\0" in file or re.match(r"^[A-Za-z]:", file):
        return False
    return all(segment not in ("", ".", "..") for segment in file.split("/"))


def diagnostic_tuple(diagnostic: object) -> tuple[str | None, int | None, str, str]:
    if not isinstance(diagnostic, dict):
        raise ValueError("each diagnostic must be an object")
    if not all(key in diagnostic for key in ("file", "line", "severity", "code")):
        raise ValueError("diagnostics require file, line, severity, and code")
    file = diagnostic["file"]
    line = diagnostic["line"]
    severity = diagnostic["severity"]
    code = diagnostic["code"]
    if file is None:
        if line is not None:
            raise ValueError("a locationless diagnostic must have a null line")
    elif (
        not isinstance(file, str)
        or not is_root_relative_path(file)
        or type(line) is not int
        or line < 1
    ):
        raise ValueError("a located diagnostic requires a root-relative file and a positive line")
    if not isinstance(severity, str) or severity not in SEVERITIES:
        raise ValueError("invalid diagnostic severity")
    if not isinstance(code, str) or not code:
        raise ValueError("diagnostic code must be a nonempty string")
    return file, line, severity, code


def tuple_record(value: tuple[str | None, int | None, str, str]) -> dict:
    file, line, severity, code = value
    return {"file": file, "line": line, "severity": severity, "code": code}


def sort_key(value: tuple[str | None, int | None, str, str]) -> tuple:
    file, line, severity, code = value
    return (file is not None, file or "", line or 0, severity, code)


def validate_unicode(value: object) -> None:
    pending = [value]
    while pending:
        item = pending.pop()
        if isinstance(item, str):
            try:
                item.encode("utf-8", errors="strict")
            except UnicodeEncodeError:
                raise ValueError("input contains an unpaired Unicode surrogate") from None
        elif isinstance(item, dict):
            pending.extend(item.keys())
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)


def compare(request: object) -> dict:
    validate_unicode(request)
    if not isinstance(request, dict):
        raise ValueError("input must be an object")
    kind = request.get("kind")
    implementation = request.get("implementation")
    exit_code = request.get("exit_code")
    expected = request.get("expected")
    actual = request.get("actual")
    claim_stage = request.get("staged_claim_stage")
    if not isinstance(kind, str) or kind not in KINDS:
        raise ValueError("invalid kind")
    if not isinstance(implementation, str) or implementation not in IMPLEMENTATIONS:
        raise ValueError("invalid implementation")
    if type(exit_code) is not int:
        raise ValueError("exit_code must be an integer")
    if not isinstance(expected, list) or not isinstance(actual, list):
        raise ValueError("expected and actual must be arrays")
    if "staged_claim_stage" in request and (type(claim_stage) is not int or not 0 <= claim_stage <= 7):
        raise ValueError("staged_claim_stage must be an integer from 0 to 7")

    expected_counts = Counter(diagnostic_tuple(diagnostic) for diagnostic in expected)
    actual_tuples = [diagnostic_tuple(diagnostic) for diagnostic in actual]
    actual_counts = Counter(actual_tuples)
    failures = []
    if exit_code == 101 or any(code == "INTERNAL_COMPILER_ERROR" for _, _, _, code in actual_tuples):
        failures.append("internal-compiler-error")
        return {"outcome": "fail", "failures": failures, "deviations": []}

    if exit_code not in (0, 1):
        return {"outcome": "fail", "failures": ["abnormal-exit"], "deviations": []}

    has_error = any(severity == "error" for _, _, severity, _ in actual_tuples)
    if exit_code != (1 if has_error else 0):
        return {"outcome": "fail", "failures": ["exit-code-mismatch"], "deviations": []}

    if claim_stage is not None:
        for diagnostic, (_, _, severity, code) in zip(actual, actual_tuples):
            data = diagnostic.get("data")
            stage = data.get("stage") if isinstance(data, dict) else None
            if code == "FEATURE_NOT_IMPLEMENTED" and severity == "error" and type(stage) is int and claim_stage < stage <= 7:
                return {"outcome": "staged", "failures": [], "deviations": []}

    if has_error != (kind == "check-fail"):
        failures.append("wrong-acceptance-outcome")

    deviations = []
    for label, difference in (
        ("missing", expected_counts - actual_counts),
        ("unexpected", actual_counts - expected_counts),
    ):
        for diagnostic in sorted(difference, key=sort_key):
            for _ in range(difference[diagnostic]):
                deviations.append({"kind": label, "diagnostic": tuple_record(diagnostic)})

    if implementation == "reference" and deviations:
        failures.append("diagnostic-mismatch")
    outcome = "fail" if failures else "pass-with-deviations" if deviations else "pass"
    return {"outcome": outcome, "failures": failures, "deviations": deviations}


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        validate_unicode(key)
        if key in result:
            raise ValueError(f"duplicate JSON key: {key!a}")
        result[key] = value
    return result


def reject_constant(value: str) -> None:
    raise ValueError(f"invalid JSON constant: {value}")


def main() -> int:
    try:
        source = sys.stdin.buffer.read().decode("utf-8", errors="strict")
        result = compare(
            json.loads(
                source,
                object_pairs_hook=unique_object,
                parse_constant=reject_constant,
                parse_float=Decimal,
            )
        )
    except (ValueError, DecimalException) as error:
        print(f"invalid input: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=True, separators=(",", ":")))
    return 1 if result["outcome"] == "fail" else 0


if __name__ == "__main__":
    raise SystemExit(main())
