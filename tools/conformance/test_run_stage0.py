import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from run_stage0 import ROOT_ID, _resolve_spec_reference, _run_iut, run_suite


class Stage0SmokeRunnerTests(unittest.TestCase):
    def setUp(self):
        if os.name == "nt":
            self.skipTest("fake executable tests currently use a POSIX launcher")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.conformance = self.base / "tests" / "conformance"
        self.fixture_dir = self.conformance / "spec-02-lexical-structure"
        self.fixture_dir.mkdir(parents=True)
        self.source = self.fixture_dir / "case.sai"
        self.script = self.base / "fake_iut.py"
        self.launcher = self.script
        self.receipt = self.base / "receipt.json"
        self.mode = "normal"
        self._write_fake()

    def _write_fake(self):
        script = f'''#!{sys.executable}
import json
import os
import sys
import time
from pathlib import Path

MODE = {self.mode!r}
RECEIPT = {str(self.receipt)!r}
ROOT = {ROOT_ID!r}

def line(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\\n"

def header(command, root):
    return {{"type":"header","format":"sai-diagnostics","version":1,
            "tool":{{"name":"sai","version":"0.1.0"}},
            "command":command,"root":root}}

def summary(errors=0, exit_code=0):
    return {{"type":"summary","errors":errors,"warnings":0,"infos":0,
            "shown_errors":errors,
            "suppressed":{{"cascade":0,"duplicate":0,"dependency":0}},
            "exit_code":exit_code}}

args = sys.argv[1:]
if args == ["--format=json", "explain", "--list"]:
    if MODE == "bad_registry":
        code = "UNREGISTERED_EXAMPLE"
    else:
        code = "EXPECTED_TOKEN"
    print(line(header("explain", None)).rstrip("\\n"))
    print(line({{"type":"result","kind":"explain.list","data":{{"codes":[
        {{"code":code,"class":"hard","summary":"A token was expected."}}
    ]}}}}).rstrip("\\n"))
    print(line(summary()).rstrip("\\n"))
    raise SystemExit(0)

root = Path.cwd()
source = (root / "src" / "lib.sai").read_bytes()
Path(RECEIPT).write_text(json.dumps({{
    "argv":args,
    "cwd":str(root),
    "environment":dict(os.environ),
    "manifest":(root / "sai.package").read_text(),
    "source":source.decode("utf-8"),
    "package_entries":sorted(path.name for path in root.iterdir()),
    "source_entries":sorted(path.name for path in (root / "src").iterdir()),
    "sandbox_entries":sorted(path.name for path in root.parent.iterdir()),
}}), encoding="utf-8")

if MODE == "timeout":
    time.sleep(10)
if MODE == "malformed":
    sys.stdout.buffer.write(b"{{bad json\\n")
    raise SystemExit(0)
if MODE == "stderr":
    sys.stderr.write("unexpected stderr")

is_fail = b"check-fail" in source
diagnostics = []
if is_fail:
    offset = source.index(b"@")
    diagnostics.append({{
        "type":"diagnostic","code":"EXPECTED_TOKEN","severity":"error",
        "level":None,"message":"expected a token","primary":{{
            "file":{{"path":"src/lib.sai","package":ROOT,"snapshot":None}},
            "start":{{"byte":offset,"line":4,"col":1,"utf16_col":1}},
            "end":{{"byte":offset+1,"line":4,"col":2,"utf16_col":2}}
        }},"primary_label":"","labels":[],"children":[],"fixes":[],
        "related":[],"data":{{}},"node":None,"symbol":None,"package":ROOT
    }})

records = [header("check", ROOT), *diagnostics]
if is_fail and MODE == "result_on_fail":
    records.append({{"type":"result","kind":"check","data":{{}}}})
if not is_fail and MODE != "missing_pass_result":
    result_data = {{"target":"wasm32-core","profile":"dev","tests":False,
                   "packages":1,"fixes_applied":0,"changed_files":[]}}
    if MODE == "bad_check_result":
        result_data.pop("changed_files")
    records.append({{"type":"result","kind":"check","data":result_data}})
if MODE == "wrong_header":
    records[0]["root"] = "wrong:root@0.1.0"
exit_code = 1 if is_fail else 0
if MODE == "summary_exit_mismatch":
    records.append(summary(errors=len(diagnostics), exit_code=0))
else:
    records.append(summary(errors=len(diagnostics), exit_code=exit_code))
for record in records:
    sys.stdout.write(line(record))
raise SystemExit(exit_code)
'''
        self.script.write_text(script, encoding="utf-8")
        self.launcher.chmod(0o755)

    def _fixture(self, kind="check-pass", *, extra="", body=None):
        if body is None:
            body = "pub fn value() -> Bool { true }\n"
        self.source.write_text(
            f"//@ kind: {kind}\n"
            "//@ stage: 0\n"
            "//@ spec: spec/02 §2.1\n"
            f"{extra}{body}",
            encoding="utf-8",
        )

    def _run(self, timeout=1):
        return run_suite(self.launcher, self.conformance, timeout=timeout)

    def test_pass_uses_exact_cli_environment_and_temp_package(self):
        self._fixture()
        report = self._run()
        self.assertEqual(report["mode"], "partial-smoke")
        self.assertIs(report["claim"], False)
        self.assertIs(report["iut_identity_verified"], False)
        self.assertEqual(report["outcome"], "pass")
        self.assertEqual(report["results"][0]["configuration"], {
            "target": None, "profile": None,
        })
        self.assertEqual(report["results"][0]["compatibility_execution"], {
            "manifest_target": "wasm32-core", "cli_profile_default": "dev",
        })
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["argv"], [
            "--format=json", "--max-errors=0", "check", "--jobs=1",
        ])
        self.assertEqual(receipt["manifest"],
                         "package conformance:test @ 0.1.0\n"
                         "compiler ^0.1\nsource \"src\"\n"
                         "targets { wasm32-core }\n")
        self.assertEqual(receipt["source"], self.source.read_text(encoding="utf-8"))
        expected_environment = {
            "NO_COLOR", "SAI_CACHE_DIR", "HOME",
        }
        platform_injected = {"LC_CTYPE", "__CF_USER_TEXT_ENCODING"} if sys.platform == "darwin" else set()
        self.assertLessEqual(set(receipt["environment"]), expected_environment | platform_injected)
        self.assertLessEqual(expected_environment, set(receipt["environment"]))
        self.assertEqual(receipt["environment"]["NO_COLOR"], "1")
        self.assertEqual(Path(receipt["environment"]["SAI_CACHE_DIR"]).resolve(),
                         (Path(receipt["cwd"]).parent / "cache").resolve())
        self.assertEqual(Path(receipt["environment"]["HOME"]).resolve(),
                         (Path(receipt["cwd"]).parent / "home").resolve())
        self.assertTrue(Path(receipt["cwd"]).is_absolute())
        self.assertEqual(Path(receipt["cwd"]).name, "package")
        self.assertEqual(receipt["package_entries"], ["sai.package", "src"])
        self.assertEqual(receipt["source_entries"], ["lib.sai"])
        self.assertEqual(receipt["sandbox_entries"], ["cache", "home", "package"])

    def test_runner_passes_only_the_documented_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            cwd = Path(temporary)
            fake_iut = self.base / "unused-iut"
            fake_iut.touch()
            completed = subprocess.CompletedProcess([], 0, b"", b"")
            with patch("run_stage0.subprocess.run", return_value=completed) as process:
                _run_iut(fake_iut, ["check"], cwd, 1)
            self.assertEqual(process.call_args.kwargs["env"], {
                "NO_COLOR": "1",
                "SAI_CACHE_DIR": str(cwd / "cache"),
                "HOME": str(cwd / "home"),
            })

    def test_native_child_observes_only_the_documented_environment(self):
        environment_utility = Path("/usr/bin/env")
        if not environment_utility.is_file():
            self.skipTest("/usr/bin/env is unavailable")
        with tempfile.TemporaryDirectory() as temporary:
            completed = _run_iut(environment_utility, [], Path(temporary), 1)
        self.assertEqual(completed.returncode, 0)
        observed = dict(
            line.split("=", 1)
            for line in completed.stdout.decode("utf-8").splitlines()
        )
        self.assertEqual(set(observed), {"NO_COLOR", "SAI_CACHE_DIR", "HOME"})

    def test_check_fail_matches_expected_diagnostic_and_has_no_result(self):
        self._fixture("check-fail", body="@ //~ error EXPECTED_TOKEN\n")
        report = self._run()
        self.assertEqual(report["outcome"], "pass")
        self.assertEqual(report["results"][0]["outcome"], "pass")
        self.assertEqual(report["results"][0]["failures"], [])

    def test_explicit_target_or_profile_and_unused_directives_fail_closed(self):
        for directive in (
            "targets: wasm32-core",
            "profiles: dev",
            "options: --tests",
            "reference-only: implementation-defined limit",
        ):
            with self.subTest(directive=directive):
                self._fixture(extra=f"//@ {directive}\n")
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertTrue(report["results"][0]["failures"])
                if directive.startswith("reference-only"):
                    self.assertIn(
                        "IUT identity is not verified",
                        report["results"][0]["failures"][0],
                    )

    def test_spec_reference_cannot_escape_docs_root(self):
        docs_root = Path(__file__).resolve().parents[2] / "docs"
        for document in ("/tmp/outside", "spec/../outside", r"spec\02", "C:/outside", "spec/\0bad"):
            with self.subTest(document=document):
                with self.assertRaises(ValueError):
                    _resolve_spec_reference({"document": document, "section": "2.1"}, docs_root)

    def test_spec_reference_symlink_cannot_escape_docs_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            docs_root = base / "docs"
            spec_dir = docs_root / "spec"
            spec_dir.mkdir(parents=True)
            outside = base / "outside.md"
            outside.write_text("## 2.1 Outside section\n", encoding="utf-8")
            (spec_dir / "02-lexical-structure.md").symlink_to(outside)
            with self.assertRaisesRegex(ValueError, "beneath docs root"):
                _resolve_spec_reference(
                    {"document": "spec/02", "section": "2.1"}, docs_root
                )

    def test_directory_fixture_is_reported_as_failure(self):
        self._fixture()
        directory_fixture = self.conformance / "other.t"
        directory_fixture.mkdir()
        (directory_fixture / "sai.package").write_text(PACKAGE_FOR_DISCOVERY, encoding="utf-8")
        report = self._run()
        self.assertEqual(report["failed"], 1)
        self.assertIn("directory fixtures are unsupported",
                      report["results"][0]["failures"][0])

    def test_wit_and_mir_sidecar_directories_fail_closed(self):
        self._fixture()
        for name in (
            "case.wit",
            "case.mir",
            "case.wasm32-core.dev.wit",
            "case.wasm64-wasi.release.mir",
        ):
            with self.subTest(name=name):
                sidecar = self.fixture_dir / name
                sidecar.mkdir()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(
                    "sidecar directories are unsupported",
                    " ".join(report["results"][0]["failures"]),
                )
                shutil.rmtree(sidecar)

    def test_empty_discovered_suite_is_failure(self):
        self.fixture_dir.rmdir()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertEqual(report["tests"], 0)
        self.assertIn("no Stage 0 smoke fixtures", report["failures"][0])

    def test_malformed_stream_missing_result_and_bad_result_schema_fail(self):
        self._fixture()
        for mode, expected in (
            ("malformed", "invalid record"),
            ("missing_pass_result", "successful check must have one result"),
            ("bad_check_result", "invalid result schema"),
            ("wrong_header", "wrong header or root"),
            ("stderr", "stderr must be empty"),
            ("bad_registry", "unregistered diagnostic code"),
        ):
            with self.subTest(mode=mode):
                self.mode = mode
                self._write_fake()
                if mode == "bad_registry":
                    self._fixture("check-fail", body="@ //~ error EXPECTED_TOKEN\n")
                else:
                    self._fixture()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(expected, " ".join(report["results"][0]["failures"]))
                self.mode = "normal"

    def test_summary_exit_mismatch_and_timeout_fail(self):
        self._fixture("check-fail", body="@ //~ error EXPECTED_TOKEN\n")
        self.mode = "summary_exit_mismatch"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("summary exit_code differs", report["results"][0]["failures"][0])

        self._fixture()
        self.mode = "timeout"
        self._write_fake()
        report = self._run(timeout=0.05)
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("timeout", report["results"][0]["failures"][0])


PACKAGE_FOR_DISCOVERY = (
    "package conformance:other @ 0.1.0\n"
    'source "src"\ntargets { wasm32-core }\n'
)


if __name__ == "__main__":
    unittest.main()
