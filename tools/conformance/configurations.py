#!/usr/bin/env python3
"""Select normal test configurations, not a conformance verdict.

Usage: python3 configurations.py < request.json

The request explicitly supplies claim_stage, test_stage, kind, claimed_targets,
implementation (reference or other), and reference_only. Optional test_targets
and test_profiles are lists restricting the directives; omission means all.
The claim starts at stage 0 and profiles are derived from its highest stage.
Configurations are sorted by target then profile in ASCII order, not by stage.

Later-stage tests, including reference-only tests for other IUTs, return
staged-selection-required as specified by §15.9.3's classification order,
without selecting a staged configuration or deciding its outcome. The CLI
exits 2 for that result. Kind/stage checks cover only known lower bounds;
the caller remains responsible for validating all other test metadata.
Run-fail tests require explicit, nonempty WASI-only test_targets; command
components cannot run on core targets. Other component metadata is not checked.
For stage-0 check tests, explicit
target/profile restrictions are unsupported: the no-target special case and
directive exclusion rules do not specify their interaction unambiguously here.
This tool does not parse directives, validate claims or test completeness, run
tests, select staged fallback configurations, or establish conformance.
"""

from __future__ import annotations

import json
import sys


TARGETS = {"wasm32-core": 1, "wasm64-core": 2, "wasm32-wasi": 4, "wasm64-wasi": 4}
PROFILES = {"dev": 1, "release": 1, "tiny": 2}
KIND_MIN_STAGES = {"check-pass": 0, "check-fail": 0, "build-pass": 1,
                   "build-fail": 1, "run-pass": 1, "run-fail": 4,
                   "run-panic": 1, "run-trap": 1, "test": 3,
                   "fmt": 0, "fmt-refuse": 0, "protocol": 0}
REQUIRED = {"claim_stage", "test_stage", "kind", "claimed_targets", "implementation", "reference_only"}
OPTIONAL = {"test_targets", "test_profiles"}


def _stage(value: object, name: str) -> int:
    if type(value) is not int or not 0 <= value <= 7:
        raise ValueError(f"{name} must be an integer from 0 to 7")
    return value


def _names(value: object, supported: dict[str, int], name: str) -> set[str]:
    if not isinstance(value, list):
        raise ValueError(f"{name} must be a list")
    if any(not isinstance(item, str) or item not in supported for item in value):
        raise ValueError(f"{name} contains an unsupported value")
    if len(set(value)) != len(value):
        raise ValueError(f"{name} contains duplicates")
    return set(value)


def select(request: object) -> dict:
    """Return configuration selection only; unsupported cases never pass."""
    if not isinstance(request, dict) or any(not isinstance(key, str) for key in request):
        raise ValueError("request must be an object with string keys")
    if REQUIRED - request.keys() or request.keys() - REQUIRED - OPTIONAL:
        raise ValueError("request has missing or unsupported fields")
    claim_stage = _stage(request["claim_stage"], "claim_stage")
    test_stage = _stage(request["test_stage"], "test_stage")
    kind = request["kind"]
    if not isinstance(kind, str) or kind not in KIND_MIN_STAGES:
        raise ValueError("unsupported test kind")
    if test_stage < KIND_MIN_STAGES[kind]:
        raise ValueError(f"test_stage is below the minimum for {kind}")
    implementation = request["implementation"]
    if not isinstance(implementation, str) or implementation not in ("reference", "other"):
        raise ValueError("unsupported implementation")
    if type(request["reference_only"]) is not bool:
        raise ValueError("reference_only must be boolean")
    claimed_targets = _names(request["claimed_targets"], TARGETS, "claimed_targets")
    test_targets = (_names(request["test_targets"], TARGETS, "test_targets")
                    if "test_targets" in request else set(TARGETS))
    test_profiles = (_names(request["test_profiles"], PROFILES, "test_profiles")
                     if "test_profiles" in request else set(PROFILES))
    if kind == "run-fail" and ("test_targets" not in request or not test_targets
                               or not test_targets <= {"wasm32-wasi", "wasm64-wasi"}):
        raise ValueError("run-fail requires explicit nonempty WASI-only test_targets")
    if claim_stage == 0 and claimed_targets:
        raise ValueError("a stage-0 claim has no targets")

    if test_stage > claim_stage:
        return {"selection": "staged-selection-required", "configurations": []}
    if request["reference_only"] and implementation != "reference":
        return {"selection": "not-applicable", "configurations": []}
    if claim_stage == 0:
        if kind not in ("check-pass", "check-fail"):
            return {"selection": "not-applicable", "configurations": []}
        if "test_targets" in request or "test_profiles" in request:
            raise ValueError("stage-0 check with explicit target/profile restrictions is unsupported")
        return {"selection": "configurations", "configurations": [{"target": None, "profile": None}]}

    targets = sorted(name for name in claimed_targets & test_targets if TARGETS[name] <= claim_stage)
    profiles = sorted(name for name in test_profiles if PROFILES[name] <= claim_stage)
    configurations = [{"target": target, "profile": profile}
                      for target in targets for profile in profiles]
    return {"selection": "configurations" if configurations else "not-applicable",
            "configurations": configurations}


def _unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key!a}")
        result[key] = value
    return result


def _reject_number(value: str) -> None:
    raise ValueError(f"unsupported JSON number: {value!a}")


def main() -> int:
    try:
        source = sys.stdin.buffer.read().decode("utf-8", "strict")
        request = json.loads(source, object_pairs_hook=_unique_object,
                             parse_float=_reject_number, parse_constant=_reject_number)
        result = select(request)
    except (ValueError, RecursionError) as error:
        print(f"invalid input: {ascii(str(error))}", file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=True, separators=(",", ":")))
    return 2 if result["selection"] == "staged-selection-required" else 0


if __name__ == "__main__":
    raise SystemExit(main())
