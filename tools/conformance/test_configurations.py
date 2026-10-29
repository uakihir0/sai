import json
import subprocess
import sys
import unittest
from pathlib import Path

from configurations import select


def request(**changes):
    value = {"claim_stage": 1, "test_stage": 0, "kind": "check-pass",
             "claimed_targets": ["wasm32-core"], "implementation": "reference",
             "reference_only": False}
    value.update(changes)
    return value


def configuration(target, profile):
    return {"target": target, "profile": profile}


class ConfigurationTests(unittest.TestCase):
    def test_stage_zero_checks_run_once_without_target_or_profile(self):
        for kind in ("check-pass", "check-fail"):
            for implementation in ("reference", "other"):
                with self.subTest(kind=kind, implementation=implementation):
                    self.assertEqual(select(request(claim_stage=0, claimed_targets=[], kind=kind,
                                                    implementation=implementation)),
                                     {"selection": "configurations",
                                      "configurations": [configuration(None, None)]})

    def test_stage_zero_nonchecks_are_not_applicable(self):
        for kind in ("fmt", "fmt-refuse", "protocol"):
            with self.subTest(kind=kind):
                self.assertEqual(select(request(claim_stage=0, claimed_targets=[], kind=kind)),
                                 {"selection": "not-applicable", "configurations": []})

    def test_kind_stage_lower_bounds_are_validated_before_selection(self):
        for kind, minimum in (("build-pass", 1), ("build-fail", 1), ("run-pass", 1),
                              ("run-fail", 4), ("run-panic", 1), ("run-trap", 1),
                              ("test", 3)):
            for claim_stage in (0, 7):
                with self.subTest(kind=kind, claim_stage=claim_stage):
                    targets = [] if claim_stage == 0 else (["wasm32-wasi"] if kind == "run-fail" else ["wasm32-core"])
                    restrictions = {"test_targets": ["wasm32-wasi"]} if kind == "run-fail" else {}
                    with self.assertRaisesRegex(ValueError, "below the minimum"):
                        select(request(kind=kind, test_stage=minimum - 1,
                                       claim_stage=claim_stage, claimed_targets=targets, **restrictions))
                    result = select(request(kind=kind, test_stage=minimum,
                                            claim_stage=claim_stage, claimed_targets=targets, **restrictions))
                    self.assertEqual(result["selection"], "staged-selection-required"
                                     if claim_stage == 0 else "configurations")

    def test_stage_zero_nonchecks_with_later_stage_require_staged_selection(self):
        for kind, stage in (("build-pass", 1), ("run-pass", 1), ("test", 3),
                            ("run-fail", 4), ("fmt", 1), ("protocol", 6)):
            with self.subTest(kind=kind):
                restrictions = {"test_targets": ["wasm32-wasi"]} if kind == "run-fail" else {}
                self.assertEqual(select(request(claim_stage=0, claimed_targets=[],
                                                kind=kind, test_stage=stage, **restrictions)),
                                 {"selection": "staged-selection-required", "configurations": []})

    def test_run_fail_requires_explicit_wasi_only_targets(self):
        for targets in (None, [], ["wasm32-core"], ["wasm64-core"],
                        ["wasm32-wasi", "wasm32-core"]):
            with self.subTest(targets=targets), self.assertRaisesRegex(ValueError, "WASI-only"):
                restriction = {} if targets is None else {"test_targets": targets}
                select(request(kind="run-fail", test_stage=4, claim_stage=4,
                               claimed_targets=["wasm32-wasi"], **restriction))
        result = select(request(kind="run-fail", test_stage=4, claim_stage=4,
                                claimed_targets=["wasm32-core", "wasm32-wasi"],
                                test_targets=["wasm32-wasi"]))
        self.assertEqual(result, {"selection": "configurations", "configurations": [
            configuration("wasm32-wasi", "dev"), configuration("wasm32-wasi", "release"),
            configuration("wasm32-wasi", "tiny")]})
        self.assertEqual(select(request(kind="run-fail", test_stage=4, claim_stage=1,
                                        test_targets=["wasm32-wasi"]))["selection"],
                         "staged-selection-required")

    def test_stage_zero_filters_are_not_silently_ignored(self):
        for restriction in ({"test_targets": []}, {"test_targets": ["wasm32-core"]},
                            {"test_profiles": ["dev"]}):
            with self.subTest(restriction=restriction), self.assertRaisesRegex(ValueError, "unsupported"):
                select(request(claim_stage=0, claimed_targets=[], **restriction))

    def test_later_stage_requires_separate_selection(self):
        for stage in (0, 1, 3, 6):
            targets = [] if stage == 0 else ["wasm32-core"]
            with self.subTest(stage=stage):
                result = select(request(claim_stage=stage, test_stage=stage + 1,
                                        claimed_targets=targets, implementation="other",
                                        reference_only=True))
                self.assertEqual(result, {"selection": "staged-selection-required", "configurations": []})

    def test_stage_boundaries_and_deterministic_product_order(self):
        targets = ["wasm64-wasi", "wasm64-core", "wasm32-wasi", "wasm32-core"]
        for stage, eligible_targets, eligible_profiles in (
            (1, ["wasm32-core"], ["dev", "release"]),
            (2, ["wasm32-core", "wasm64-core"], ["dev", "release", "tiny"]),
            (3, ["wasm32-core", "wasm64-core"], ["dev", "release", "tiny"]),
            (4, ["wasm32-core", "wasm32-wasi", "wasm64-core", "wasm64-wasi"],
             ["dev", "release", "tiny"]),
            (7, ["wasm32-core", "wasm32-wasi", "wasm64-core", "wasm64-wasi"],
             ["dev", "release", "tiny"]),
        ):
            with self.subTest(stage=stage):
                expected = [configuration(target, profile) for target in eligible_targets
                            for profile in eligible_profiles]
                self.assertEqual(select(request(claim_stage=stage, test_stage=stage,
                                                claimed_targets=targets)),
                                 {"selection": "configurations", "configurations": expected})

    def test_claim_and_directive_intersections(self):
        result = select(request(claim_stage=4, claimed_targets=["wasm32-core", "wasm32-wasi"],
                                test_targets=["wasm64-wasi", "wasm32-wasi"],
                                test_profiles=["tiny", "dev"]))
        self.assertEqual(result, {"selection": "configurations",
                                  "configurations": [configuration("wasm32-wasi", "dev"),
                                                     configuration("wasm32-wasi", "tiny")]})
        for change in ({"claimed_targets": []}, {"test_targets": []}, {"test_profiles": []},
                       {"test_targets": ["wasm64-core"]}, {"test_profiles": ["tiny"]}):
            with self.subTest(change=change):
                self.assertEqual(select(request(**change)),
                                 {"selection": "not-applicable", "configurations": []})

    def test_reference_only_is_not_applicable_to_other_iuts(self):
        for stage, targets in ((0, []), (1, ["wasm32-core"])):
            with self.subTest(stage=stage):
                self.assertEqual(select(request(claim_stage=stage, claimed_targets=targets,
                                                implementation="other", reference_only=True)),
                                 {"selection": "not-applicable", "configurations": []})
                self.assertEqual(select(request(claim_stage=stage, claimed_targets=targets,
                                                implementation="reference", reference_only=True))["selection"],
                                 "configurations")

    def test_invalid_requests_and_field_types(self):
        for change in ({"claim_stage": True}, {"test_stage": -1}, {"test_stage": 8},
                       {"claim_stage": 1.0}, {"kind": "check"}, {"implementation": None},
                       {"reference_only": 1}, {"claimed_targets": None},
                       {"claimed_targets": ["unknown"]}, {"claimed_targets": ["wasm32-core"] * 2},
                       {"test_targets": "wasm32-core"}, {"test_profiles": ["DEV"]},
                       {"test_profiles": [None]}, {"test_targets": [{}]},
                       {"claim_stage": 0}, {"typo": True}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                select(request(**change))
        for value in (None, [], {}, {1: 2}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                select(value)

    def test_cli_selection_and_malformed_input(self):
        script = Path(__file__).with_name("configurations.py")
        result = subprocess.run([sys.executable, "-B", str(script)], input=json.dumps(request()).encode(),
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout), select(request()))
        staged = request(test_stage=2)
        result = subprocess.run([sys.executable, "-B", str(script)], input=json.dumps(staged).encode(),
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["selection"], "staged-selection-required")
        for bad in (b"", b"\xff", b'{"kind":1,"kind":2}', b"NaN", b"1e999999", b"[" * 2000):
            with self.subTest(bad=bad[:40]):
                result = subprocess.run([sys.executable, "-B", str(script)], input=bad,
                                        capture_output=True, check=False)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")
                self.assertIn(b"invalid input", result.stderr)


if __name__ == "__main__":
    unittest.main()
