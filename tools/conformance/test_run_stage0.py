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
        codes = [code]
    else:
        codes = [
            "EXPECTED_TOKEN",
            "FEATURE_NOT_IMPLEMENTED",
            "INTERNAL_COMPILER_ERROR",
            "INVALID_ASSIGN_TARGET",
            "INVARIANT_OUTSIDE_DEFINING_MODULE",
            "LITERAL_OUT_OF_RANGE",
            "NAMED_ARGUMENT_ON_FUNCTION",
            "OR_PATTERN_BINDING_MISMATCH",
            "UNFORMATTED_FILE",
        ]
    print(line(header("explain", None)).rstrip("\\n"))
    print(line({{"type":"result","kind":"explain.list","data":{{"codes":[
        {{"code":code,"class":"hard","summary":"A diagnostic."}}
        for code in codes
    ]}}}}).rstrip("\\n"))
    print(line(summary()).rstrip("\\n"))
    raise SystemExit(0)

if args == [
    "--format=json", "--max-errors=0", "fmt", "--check",
    "--stdin-kind", "source", "-",
]:
    formatter_input = sys.stdin.buffer.read()
    try:
        receipt = json.loads(Path(RECEIPT).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        receipt = {{}}
    receipt.update({{
        "formatter_argv":args,
        "formatter_input":formatter_input.decode("utf-8"),
    }})
    receipt.setdefault("calls", []).append("fmt")
    Path(RECEIPT).write_text(json.dumps(receipt), encoding="utf-8")
    if MODE == "formatter_malformed":
        sys.stdout.buffer.write(b"{{bad json\\n")
        raise SystemExit(0)
    if MODE == "formatter_extra_refusal" or (
        MODE != "formatter_accepts_expected_parse_error"
        and b"@ //~ error EXPECTED_TOKEN" in formatter_input
    ):
        diagnostic = {{
            "type":"diagnostic","code":"EXPECTED_TOKEN","severity":"error",
            "level":None,"message":"expected a token","primary":{{
                "file":{{"path":"<stdin>","package":None,"snapshot":None}},
                "start":{{"byte":0,"line":4,"col":1,"utf16_col":1}},
                "end":{{"byte":1,"line":4,"col":2,"utf16_col":2}}
            }},"primary_label":"","labels":[],"children":[],"fixes":[],
            "related":[],"data":{{}},"node":None,"symbol":None,"package":None
        }}
        diagnostics = [diagnostic]
        if MODE == "formatter_extra_refusal":
            extra = json.loads(json.dumps(diagnostic))
            extra["primary"]["start"]["line"] = 5
            extra["primary"]["end"]["line"] = 5
            diagnostics.append(extra)
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":1,"changed":[],"refused":["<stdin>"],"output":None
        }}}}
        status = 1
        errors = len(diagnostics)
    elif MODE == "formatter_unexpected_refusal":
        diagnostic = {{
            "type":"diagnostic","code":"FEATURE_NOT_IMPLEMENTED","severity":"error",
            "level":None,"message":"unsupported formatting shape","primary":None,
            "primary_label":"","labels":[],"children":[],"fixes":[],
            "related":[],"data":{{}},"node":None,"symbol":None,"package":None
        }}
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":1,"changed":[],"refused":["<stdin>"],"output":None
        }}}}
        status = 1
        errors = 1
        diagnostics = [diagnostic]
    elif MODE == "formatter_semantic_refusal":
        diagnostic = {{
            "type":"diagnostic","code":"LITERAL_OUT_OF_RANGE","severity":"error",
            "level":None,"message":"literal is outside its type's range","primary":{{
                "file":{{"path":"<stdin>","package":None,"snapshot":None}},
                "start":{{"byte":0,"line":5,"col":1,"utf16_col":1}},
                "end":{{"byte":1,"line":5,"col":2,"utf16_col":2}}
            }},"primary_label":"","labels":[],"children":[],"fixes":[],
            "related":[],"data":{{}},"node":None,"symbol":None,"package":None
        }}
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":1,"changed":[],"refused":["<stdin>"],"output":None
        }}}}
        status = 1
        errors = 1
        diagnostics = [diagnostic]
    elif MODE == "formatter_resolution_refusal":
        diagnostic = {{
            "type":"diagnostic","code":"NAMED_ARGUMENT_ON_FUNCTION","severity":"error",
            "level":None,"message":"named arguments require a callable interface","primary":{{
                "file":{{"path":"<stdin>","package":None,"snapshot":None}},
                "start":{{"byte":0,"line":6,"col":1,"utf16_col":1}},
                "end":{{"byte":1,"line":6,"col":2,"utf16_col":2}}
            }},"primary_label":"","labels":[],"children":[],"fixes":[],
            "related":[],"data":{{}},"node":None,"symbol":None,"package":None
        }}
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":1,"changed":[],"refused":["<stdin>"],"output":None
        }}}}
        status = 1
        errors = 1
        diagnostics = [diagnostic]
    elif MODE == "formatter_unformatted":
        diagnostic = {{
            "type":"diagnostic","code":"UNFORMATTED_FILE","severity":"error",
            "level":None,"message":"file is not formatted","primary":{{
                "file":{{"path":"<stdin>","package":None,"snapshot":None}},
                "start":{{"byte":0,"line":4,"col":1,"utf16_col":1}},
                "end":{{"byte":1,"line":4,"col":2,"utf16_col":2}}
            }},"primary_label":"","labels":[],"children":[],"fixes":[],
            "related":[],"data":{{"line":4,"expected":"formatted"}}, "node":None,
            "symbol":None,"package":None
        }}
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":1,"changed":["<stdin>"],"refused":[],"output":None
        }}}}
        status = 1
        errors = 1
        diagnostics = [diagnostic]
    else:
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":1,"changed":[],"refused":[],"output":None
        }}}}
        status = 0
        errors = 0
        diagnostics = []
    print(line(header("fmt", None)).rstrip("\\n"))
    for diagnostic in diagnostics:
        print(line(diagnostic).rstrip("\\n"))
    print(line(result).rstrip("\\n"))
    print(line(summary(errors=errors, exit_code=status)).rstrip("\\n"))
    raise SystemExit(status)

root = Path.cwd()
source = (root / "src" / "lib.sai").read_bytes()
try:
    receipt = json.loads(Path(RECEIPT).read_text(encoding="utf-8"))
except (OSError, json.JSONDecodeError):
    receipt = {{}}
receipt.update({{
    "argv":args,
    "cwd":str(root),
    "environment":dict(os.environ),
    "manifest":(root / "sai.package").read_text(),
    "source":source.decode("utf-8"),
    "package_entries":sorted(path.name for path in root.iterdir()),
    "source_entries":sorted(path.name for path in (root / "src").iterdir()),
    "sandbox_entries":sorted(path.name for path in root.parent.iterdir()),
}})
receipt.setdefault("calls", []).append("check")
Path(RECEIPT).write_text(json.dumps(receipt), encoding="utf-8")

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
        self.assertEqual(receipt["formatter_argv"], [
            "--format=json", "--max-errors=0", "fmt", "--check",
            "--stdin-kind", "source", "-",
        ])
        self.assertEqual(receipt["calls"], ["fmt", "check"])
        self.assertEqual(receipt["manifest"],
                         "package conformance:test @ 0.1.0\n"
                         "compiler ^0.1\nsource \"src\"\n"
                         "targets { wasm32-core }\n")
        self.assertEqual(receipt["source"], self.source.read_text(encoding="utf-8"))
        self.assertEqual(receipt["formatter_input"], self.source.read_text(encoding="utf-8"))
        self.assertEqual(
            self.source.read_bytes(),
            receipt["formatter_input"].encode("utf-8"),
            "formatter preflight must not rewrite the fixture",
        )
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
        self.mode = "formatter_expected_refusal"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "pass")
        self.assertEqual(report["results"][0]["outcome"], "pass")
        self.assertEqual(report["results"][0]["failures"], [])

    def test_formatter_must_refuse_source_with_expected_syntax_error(self):
        self._fixture("check-fail", body="@ //~ error EXPECTED_TOKEN\n")
        self.mode = "formatter_accepts_expected_parse_error"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "accepted source with explicitly expected lexical or syntax errors",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

    def test_unexpected_formatter_refusal_fails_before_check(self):
        self._fixture()
        self.mode = "formatter_unexpected_refusal"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "only by lexical or syntax errors",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

    def test_expected_type_dependent_literal_error_is_not_a_parse_refusal(self):
        self._fixture(
            "check-fail",
            extra="//@ diag: error LITERAL_OUT_OF_RANGE at src/lib.sai:5\n",
            body="pub const OVERFLOW: I8 = 128\n",
        )
        self.mode = "formatter_semantic_refusal"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "only by lexical or syntax errors",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

    def test_expected_name_resolution_error_is_not_a_parse_refusal(self):
        self._fixture(
            "check-fail",
            extra="//@ diag: error NAMED_ARGUMENT_ON_FUNCTION at src/lib.sai:6\n",
            body=(
                "pub fn target(value: Bool) {}\n"
                "pub fn caller() { target(other = true) }\n"
            ),
        )
        self.mode = "formatter_resolution_refusal"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "only by lexical or syntax errors",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

    def test_formatter_refusal_with_unexpected_parse_error_fails(self):
        self._fixture("check-fail", body="@ //~ error EXPECTED_TOKEN\n")
        self.mode = "formatter_extra_refusal"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "does not match explicitly expected",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

    def test_noncanonical_source_fails_before_check_without_rewriting(self):
        self._fixture(body="pub fn value()->Bool{true}\n")
        original = self.source.read_bytes()
        self.mode = "formatter_unformatted"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "not in canonical formatter form",
            " ".join(report["results"][0]["failures"]),
        )
        self.assertEqual(self.source.read_bytes(), original)
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])
        self.assertEqual(receipt["formatter_input"].encode("utf-8"), original)

    def test_malformed_formatter_stream_fails_closed(self):
        self._fixture()
        self.mode = "formatter_malformed"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "invalid record",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

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
