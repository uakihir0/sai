import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from run_stage0 import (
    ROOT_ID,
    SmokeError,
    _first_differing_lf_line,
    _formatter_refusal_codes,
    _resolve_spec_reference,
    _source_position,
    _run_iut,
    run_suite,
)


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
        self.expected_format = None
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
EXPECTED_FORMATTED = {self.expected_format!r}

def line(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\\n"

def header(command, root):
    return {{"type":"header","format":"sai-diagnostics","version":1,
            "tool":{{"name":"sai","version":"0.1.0"}},
            "command":command,"root":root}}

def summary(errors=0, warnings=0, infos=0, exit_code=0):
    return {{"type":"summary","errors":errors,"warnings":warnings,"infos":infos,
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
            "MISSING_FN_BODY",
            "NAMED_ARGUMENT_ON_FUNCTION",
            "OR_PATTERN_BINDING_MISMATCH",
            "UNFORMATTED_FILE",
            "UNKNOWN_ATTRIBUTE",
        ]
    print(line(header("explain", None)).rstrip("\\n"))
    print(line({{"type":"result","kind":"explain.list","data":{{"codes":[
        {{"code":code,"class":"hard","summary":"A diagnostic."}}
        for code in codes
    ]}}}}).rstrip("\\n"))
    print(line(summary()).rstrip("\\n"))
    raise SystemExit(0)

stdin_check_args = [
    "--format=json", "--max-errors=0", "fmt", "--check",
    "--stdin-kind", "source", "-",
]
package_check_args = ["--format=json", "--max-errors=0", "fmt", "--check"]
is_package_check = args == package_check_args
if args == stdin_check_args or is_package_check:
    formatter_input = (
        Path("src/lib.sai").read_bytes() if is_package_check
        else sys.stdin.buffer.read()
    )
    formatter_path = "src/lib.sai" if is_package_check else "<stdin>"
    formatter_file_count = 2 if is_package_check else 1
    try:
        receipt = json.loads(Path(RECEIPT).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        receipt = {{}}
    receipt.update({{
        "formatter_argv":args,
        "formatter_input":formatter_input.decode("utf-8"),
    }})
    is_fmt_fixture_check = MODE == "fmt_fixture" or MODE.startswith("fmt_check_")
    receipt.setdefault("calls", []).append(
        "fmt-check" if is_fmt_fixture_check else "fmt"
    )
    if is_fmt_fixture_check:
        receipt.setdefault("formatter_check_inputs", []).append(
            formatter_input.decode("utf-8")
        )
        receipt.setdefault("formatter_check_argvs", []).append(args)
        receipt.setdefault("formatter_check_paths", []).append(formatter_path)
    Path(RECEIPT).write_text(json.dumps(receipt), encoding="utf-8")
    if is_fmt_fixture_check:
        if MODE == "fmt_check_malformed":
            sys.stdout.buffer.write(b"{{bad json\\n")
            raise SystemExit(0)
        def split_lf(value):
            lines = []
            start = 0
            while start < len(value):
                newline = value.find(b"\\n", start)
                if newline < 0:
                    lines.append(value[start:])
                    break
                lines.append(value[start:newline + 1])
                start = newline + 1
            return lines

        def position(value, offset):
            bom_length = 3 if value.startswith(b"\\xef\\xbb\\xbf") else 0
            if offset <= bom_length:
                return {{
                    "byte":offset,"line":1,"col":1,"utf16_col":1
                }}
            line = 1
            line_start = bom_length
            cursor = bom_length
            while cursor < offset:
                byte = value[cursor]
                if byte == 13:
                    terminator_end = (
                        cursor + 2 if value[cursor:cursor + 2] == b"\\r\\n"
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
            prefix = value[line_start:offset].decode("utf-8")
            return {{
                "byte":offset,"line":line,"col":offset-line_start+1,
                "utf16_col":1+sum(2 if ord(character)>0xffff else 1
                                   for character in prefix)
            }}

        input_lines = split_lf(formatter_input)
        output_lines = split_lf(EXPECTED_FORMATTED)
        difference = next((
            index for index in range(max(len(input_lines), len(output_lines)))
            if (input_lines[index] if index < len(input_lines) else None)
            != (output_lines[index] if index < len(output_lines) else None)
        ), None)
        forced_canonical_diagnostic = (
            MODE == "fmt_check_canonical_diagnostic"
            or (
                is_package_check
                and MODE == "fmt_check_package_manifest_diagnostic"
                and formatter_input == EXPECTED_FORMATTED
            )
        )
        diagnostics = []
        if difference is not None or forced_canonical_diagnostic:
            line_number = 1 if difference is None else difference + 1
            input_line = (
                None if difference is None or difference >= len(input_lines)
                else input_lines[difference]
            )
            output_line = (
                None if difference is None or difference >= len(output_lines)
                else output_lines[difference]
            )
            start_byte = (
                len(formatter_input) if input_line is None
                else sum(len(item) for item in input_lines[:difference])
            )
            end_byte = start_byte if input_line is None else start_byte + len(input_line)
            if input_line is not None and input_line.endswith(b"\\n"):
                end_byte -= 1
                if end_byte > start_byte and formatter_input[end_byte - 1] == 13:
                    end_byte -= 1
            expected_line = None if output_line is None else output_line.removesuffix(b"\\n")
            start_position = position(formatter_input, start_byte)
            end_position = position(formatter_input, end_byte)
            diagnostic = {{
                "type":"diagnostic","code":"UNFORMATTED_FILE","severity":"error",
                "level":None,"message":"file is not formatted","primary":{{
                    "file":{{"path":formatter_path,"package":None,"snapshot":None}},
                    "start":start_position,
                    "end":end_position
                }},"primary_label":"","labels":[],
                "children":[{{"kind":"help","message":"run `sai fmt <path>` to format this file",
                             "location":None}}],
                "fixes":[],"related":[],
                "data":{{"line":line_number,"expected":(
                    None if expected_line is None else expected_line.decode("utf-8")
                )}},
                "node":None,"symbol":None,"package":None
            }}
            if MODE == "fmt_check_wrong_code":
                diagnostic["code"] = "EXPECTED_TOKEN"
            if MODE == "fmt_check_wrong_severity":
                diagnostic["severity"] = "warning"
            if MODE == "fmt_check_wrong_message":
                diagnostic["message"] = "different"
            if MODE == "fmt_check_wrong_line":
                diagnostic["primary"]["start"]["line"] += 1
                diagnostic["primary"]["end"]["line"] += 1
            if MODE == "fmt_check_wrong_span":
                diagnostic["primary"]["end"]["byte"] += 1
            if MODE == "fmt_check_wrong_column":
                diagnostic["primary"]["start"]["col"] += 1
            if MODE == "fmt_check_wrong_utf16_column":
                diagnostic["primary"]["end"]["utf16_col"] += 1
            if MODE == "fmt_check_wrong_data":
                diagnostic["data"]["expected"] = "different"
            if is_package_check and MODE == "fmt_check_package_wrong_path":
                diagnostic["primary"]["file"]["path"] = "src/other.sai"
            if (
                is_package_check
                and MODE == "fmt_check_package_manifest_diagnostic"
                and formatter_input == EXPECTED_FORMATTED
            ):
                diagnostic["primary"]["file"]["path"] = "sai.package"
            diagnostics = [diagnostic]
            if MODE == "fmt_check_extra_diagnostic":
                diagnostics.append(json.loads(json.dumps(diagnostic)))
            if MODE == "fmt_check_missing_diagnostic":
                diagnostics = []

        errors = sum(item["severity"] == "error" for item in diagnostics)
        warnings = sum(item["severity"] == "warning" for item in diagnostics)
        if MODE == "fmt_check_missing_diagnostic":
            status = 0
        else:
            status = 1 if errors else 0
        changed = [] if formatter_input == EXPECTED_FORMATTED else [formatter_path]
        refused = []
        output = None
        if MODE == "fmt_check_wrong_result":
            changed = []
            refused = [formatter_path]
            output = "unexpected"
        files = formatter_file_count
        if is_package_check and MODE == "fmt_check_package_wrong_files":
            files = 1
        if is_package_check and MODE == "fmt_check_package_wrong_changed":
            changed = ["src/other.sai"]
        result = {{"type":"result","kind":"fmt","data":{{
            "check":True,"files":files,"changed":changed,"refused":refused,"output":output
        }}}}
        if is_package_check and MODE == "fmt_check_package_mutates_source":
            Path("src/lib.sai").write_bytes(b"modified by fake IUT\\n")
        if is_package_check and MODE == "fmt_check_package_rewrites_identical_source":
            source_path = Path("src/lib.sai")
            source_path.write_bytes(source_path.read_bytes())
        if is_package_check and MODE == "fmt_check_package_adds_source":
            Path("src/extra.sai").write_bytes(b"added by fake IUT\\n")
        if is_package_check and MODE == "fmt_check_package_mutates_manifest":
            Path("sai.package").write_bytes(b"modified by fake IUT\\n")
        print(line(header("fmt", None)).rstrip("\\n"))
        for diagnostic in diagnostics:
            print(line(diagnostic).rstrip("\\n"))
        print(line(result).rstrip("\\n"))
        summary_record = summary(errors=errors, warnings=warnings, exit_code=status)
        if MODE == "fmt_check_suppressed":
            summary_record["suppressed"]["cascade"] = 1
        print(line(summary_record).rstrip("\\n"))
        raise SystemExit(status)
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
    elif MODE == "formatter_unexpected_validation_refusal":
        diagnostic = {{
            "type":"diagnostic","code":"MISSING_FN_BODY","severity":"error",
            "level":None,"message":"function requires a body","primary":{{
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

if args == [
    "--format=json", "--max-errors=0", "fmt", "--stdin-kind", "source", "-",
]:
    formatter_input = sys.stdin.buffer.read()
    try:
        receipt = json.loads(Path(RECEIPT).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        receipt = {{}}
    receipt.setdefault("formatter_inputs", []).append(formatter_input.decode("utf-8"))
    receipt.setdefault("calls", []).append("fmt")
    receipt["fmt_argv"] = args
    Path(RECEIPT).write_text(json.dumps(receipt), encoding="utf-8")
    if MODE == "fmt_malformed":
        sys.stdout.buffer.write(b"{{bad json\\n")
        raise SystemExit(0)

    diagnostic = {{
        "type":"diagnostic","code":"EXPECTED_TOKEN","severity":"error",
        "level":None,"message":"expected a token","primary":{{
            "file":{{"path":"<stdin>","package":None,"snapshot":None}},
            "start":{{"byte":0,"line":1,"col":1,"utf16_col":1}},
            "end":{{"byte":1,"line":1,"col":2,"utf16_col":2}}
        }},"primary_label":"","labels":[],"children":[],"fixes":[],
        "related":[],"data":{{}},"node":None,"symbol":None,"package":None
    }}
    if MODE.startswith("fmt_refuse"):
        warnings = 0
        if MODE == "fmt_refuse_malformed":
            sys.stdout.buffer.write(b"{{bad json\\n")
            raise SystemExit(0)
        if MODE == "fmt_refuse_accepts":
            diagnostics = []
            status = 0
            errors = 0
            result_data = {{
                "check":False,"files":1,"changed":[],
                "refused":[],"output":formatter_input.decode("utf-8")
            }}
        else:
            diagnostic["primary"]["start"]["line"] = formatter_input.count(b"\\n") + 1
            diagnostic["primary"]["end"]["line"] = formatter_input.count(b"\\n") + 1
            if MODE == "fmt_refuse_wrong_line":
                diagnostic["primary"]["start"]["line"] -= 1
                diagnostic["primary"]["end"]["line"] -= 1
            if MODE == "fmt_refuse_validation_diagnostic":
                diagnostic["code"] = "MISSING_FN_BODY"
                diagnostic["message"] = "function requires a body"
            if MODE == "fmt_refuse_warning":
                diagnostic["severity"] = "warning"
            if MODE == "fmt_refuse_internal_error":
                diagnostic["code"] = "INTERNAL_COMPILER_ERROR"
                diagnostic["message"] = "internal compiler error"
            diagnostics = [diagnostic]
            if MODE == "fmt_refuse_extra_diagnostic":
                diagnostics.append(json.loads(json.dumps(diagnostic)))
            status = 101 if MODE == "fmt_refuse_internal_error" else 1
            errors = sum(item["severity"] == "error" for item in diagnostics)
            warnings = sum(item["severity"] == "warning" for item in diagnostics)
            result_data = {{
                "check":False,"files":1,"changed":[],
                "refused":["<stdin>"],"output":None
            }}
            if MODE == "fmt_refuse_bad_schema":
                result_data.pop("output")
        result = {{"type":"result","kind":"fmt","data":result_data}}
        print(line(header("fmt", None)).rstrip("\\n"))
        for item in diagnostics:
            print(line(item).rstrip("\\n"))
        print(line(result).rstrip("\\n"))
        print(line(summary(errors=errors, warnings=warnings, exit_code=status)).rstrip("\\n"))
        raise SystemExit(status)

    if MODE == "fmt_refused":
        output = None
        changed = []
        refused = ["<stdin>"]
        diagnostics = [diagnostic]
        status = 1
        result_data = {{
            "check":False,"files":1,"changed":changed,
            "refused":refused,"output":output
        }}
    elif MODE == "fmt_internal_error":
        diagnostic["code"] = "INTERNAL_COMPILER_ERROR"
        diagnostic["message"] = "internal compiler error"
        output = None
        changed = []
        refused = ["<stdin>"]
        diagnostics = [diagnostic]
        status = 101
        result_data = {{
            "check":False,"files":1,"changed":changed,
            "refused":refused,"output":output
        }}
    else:
        output_bytes = EXPECTED_FORMATTED
        if MODE == "fmt_output_mismatch":
            output_bytes = b"different output\\n"
        if MODE == "fmt_fixed_point_failure" and len(receipt["formatter_inputs"]) > 1:
            output_bytes = EXPECTED_FORMATTED + b" "
        output = output_bytes.decode("utf-8")
        changed = [] if formatter_input == output_bytes else ["<stdin>"]
        refused = []
        diagnostics = []
        status = 0
        if MODE == "fmt_unexpected_diagnostic":
            info = json.loads(json.dumps(diagnostic))
            info["severity"] = "info"
            info["code"] = "EXPECTED_TOKEN"
            diagnostics = [info]
        if MODE == "fmt_bad_schema":
            result_data = {{
                "check":False,"files":1,"changed":changed,
                "refused":refused
            }}
        else:
            result_data = {{
                "check":False,"files":1,"changed":changed,
                "refused":refused,"output":output
            }}
    result = {{"type":"result","kind":"fmt","data":result_data}}
    errors = sum(item["severity"] == "error" for item in diagnostics)
    infos = sum(item["severity"] == "info" for item in diagnostics)
    command_header = header("fmt", None)
    if MODE == "fmt_wrong_header":
        command_header["command"] = "check"
    if MODE == "fmt_wrong_root":
        command_header["root"] = ROOT
    print(line(command_header).rstrip("\\n"))
    for item in diagnostics:
        print(line(item).rstrip("\\n"))
    print(line(result).rstrip("\\n"))
    summary_record = summary(errors=errors, exit_code=status)
    if infos != 0:
        summary_record = {{
              "type":"summary","errors":errors,"warnings":0,"infos":infos,
              "shown_errors":errors,
              "suppressed":{{"cascade":0,"duplicate":0,"dependency":0}},
              "exit_code":status
        }}
    if MODE == "fmt_extra_summary":
        summary_record["unexpected"] = True
    print(line(summary_record).rstrip("\\n"))
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
    if MODE == "validation_only_check":
        offset = source.index(b"pub fn missing")
        code = "MISSING_FN_BODY"
        line_number = source[:offset].count(b"\\n") + 1
    else:
        offset = source.index(b"@")
        code = "EXPECTED_TOKEN"
        line_number = 4
    diagnostics.append({{
        "type":"diagnostic","code":code,"severity":"error",
        "level":None,"message":"expected a token","primary":{{
            "file":{{"path":"src/lib.sai","package":ROOT,"snapshot":None}},
            "start":{{"byte":offset,"line":line_number,"col":1,"utf16_col":1}},
            "end":{{"byte":offset+1,"line":line_number,"col":2,"utf16_col":2}}
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

    def _fixture(self, kind="check-pass", *, extra="", body=None, spec="spec/02 §2.1"):
        if body is None:
            body = "pub fn value() -> Bool { true }\n"
        self.source.write_text(
            f"//@ kind: {kind}\n"
            "//@ stage: 0\n"
            f"//@ spec: {spec}\n"
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
            "accepted source with explicitly expected refusal errors",
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
            "non-parser or unsupported",
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
            "non-parser or unsupported",
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
            "non-parser or unsupported",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt"])

    def test_formatter_refusal_taxonomy_uses_exact_parser_phase_rows(self):
        codes = _formatter_refusal_codes(Path(__file__).resolve().parents[2] / "docs")
        self.assertTrue({
            "INVALID_UTF8",
            "EXPECTED_TOKEN",
            "EXPECTED_EXPRESSION",
            "UNEXPECTED_NEWLINE",
            "IMPLEMENTATION_LIMIT_EXCEEDED",
        } <= codes)
        self.assertFalse({
            "LITERAL_OUT_OF_RANGE",
            "CLAUSE_ORDER",
            "MISSING_FN_BODY",
            "UNKNOWN_ATTRIBUTE",
            "INFER_IN_SIGNATURE",
            "NAMED_ARGUMENT_ON_FUNCTION",
            "INVALID_ASSIGN_TARGET",
            "FEATURE_NOT_IMPLEMENTED",
            "GENERIC_INTERFACE_ITEM",
            "INTERFACE_FN_CLAUSE",
            "EFFECTS_ON_INTERFACE_FN",
            "OR_PATTERN_BINDING_MISMATCH",
            "INVARIANT_OUTSIDE_DEFINING_MODULE",
            "MISPLACED_DOC_COMMENT",
        } & codes)

    def test_formatter_refusal_taxonomy_uses_fail_closed_parser_severities(self):
        required = (
            "INVALID_UTF8",
            "EXPECTED_TOKEN",
            "EXPECTED_EXPRESSION",
            "UNEXPECTED_NEWLINE",
            "IMPLEMENTATION_LIMIT_EXCEEDED",
        )

        def write_docs(root, parser_rows):
            lexical = root / "spec" / "02-lexical-structure.md"
            implementation = root / "impl" / "02-frontend-ast-hir.md"
            lexical.parent.mkdir(parents=True, exist_ok=True)
            implementation.parent.mkdir(parents=True, exist_ok=True)
            lexical.write_text(
                "## 2.12 Lexical diagnostics\n\n"
                "| Code | Severity | Description |\n"
                "|---|---|---|\n"
                + "".join(f"| `{code}` | error | sample |\n" for code in required),
                encoding="utf-8",
            )
            implementation.write_text(
                "### 2.5.12 Syntactic validation\n\n"
                "| Code | Where | Check |\n"
                "|---|---|---|\n"
                + "".join(
                    f"| `{code}` | parser | sample |\n" for code in parser_rows
                )
                + "\n## 2.12 Diagnostics\n\n"
                "| Code | Severity | Description |\n"
                "|---|---|---|\n"
                "| `EXPECTED_TOKEN` | error | sample |\n"
                "| `FUTURE_PARSER_WARNING` | warning | sample |\n",
                encoding="utf-8",
            )

        with tempfile.TemporaryDirectory() as temporary:
            docs = Path(temporary) / "docs"
            write_docs(docs, ("EXPECTED_TOKEN", "FUTURE_PARSER_WARNING"))
            codes = _formatter_refusal_codes(docs)
            self.assertNotIn("FUTURE_PARSER_WARNING", codes)

        with tempfile.TemporaryDirectory() as temporary:
            docs = Path(temporary) / "docs"
            write_docs(docs, ("EXPECTED_TOKEN", "UNREGISTERED_PARSER_CODE"))
            with self.assertRaisesRegex(
                SmokeError,
                "missing severity definitions",
            ):
                _formatter_refusal_codes(docs)

    def test_expected_validation_error_does_not_force_formatter_preflight_refusal(self):
        self._fixture(
            "check-fail",
            extra=(
                "//@ diag: error MISSING_FN_BODY at src/lib.sai:5\n"
            ),
            body="pub fn missing();\n",
            spec="impl/02 §2.5.12",
        )
        self.mode = "validation_only_check"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "pass")
        self.assertEqual(report["results"][0]["failures"], [])
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["calls"], ["fmt", "check"])

    def test_fake_validation_only_formatter_refusal_is_rejected_before_check(self):
        self._fixture()
        self.mode = "formatter_unexpected_validation_refusal"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("non-parser or unsupported",
                      " ".join(report["results"][0]["failures"]))
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

    def _fmt_fixture(self, *, body="pub fn value()->Bool{true}\n"):
        self._fixture("fmt", body=body)
        expected_path = self.source.with_name("case.expected.sai")
        if expected_path.is_symlink() or expected_path.is_file():
            expected_path.unlink()
        elif expected_path.is_dir():
            shutil.rmtree(expected_path)
        expected = (
            "//@ kind: fmt\n"
            "//@ stage: 0\n"
            "//@ spec: impl/14 §14.4.2\n"
            "pub fn value() -> Bool {\n"
            "    true\n"
            "}\n"
        ).encode("utf-8")
        expected_path.write_bytes(expected)
        self.expected_format = expected
        self.mode = "fmt_fixture"
        self._write_fake()

    def _fmt_refuse_fixture(
        self, *, expected_line=6, extra="", body="pub fn broken() -> Bool { true\n",
    ):
        self.source.write_text(
            "//@ kind: fmt-refuse\n"
            "//@ stage: 0\n"
            "//@ spec: impl/02 §2.5.11, impl/14 §14.2.2\n"
            f"//@ diag: error EXPECTED_TOKEN at src/lib.sai:{expected_line}\n"
            + extra
            + body,
            encoding="utf-8",
        )
        self.mode = "fmt_refuse"
        self._write_fake()

    def test_fmt_refuse_accepts_exact_error_multiset_without_preflight_or_check(self):
        self._fmt_refuse_fixture()
        original = self.source.read_bytes()
        report = self._run()
        self.assertEqual(report["outcome"], "pass")
        self.assertEqual(report["results"][0]["outcome"], "pass")
        self.assertIsNone(report["results"][0]["compatibility_execution"])
        self.assertIs(report["claim"], False)
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["fmt_argv"], [
            "--format=json", "--max-errors=0", "fmt", "--stdin-kind", "source", "-",
        ])
        self.assertEqual(receipt["calls"], ["fmt"])
        self.assertEqual(receipt["formatter_inputs"], [original.decode("utf-8")])
        self.assertEqual(self.source.read_bytes(), original)

    def test_fmt_refuse_rejects_missing_wrong_and_extra_diagnostics(self):
        self._fmt_refuse_fixture()
        self.source.write_text(
            "//@ kind: fmt-refuse\n"
            "//@ stage: 0\n"
            "//@ spec: impl/02 §2.5.11\n"
            "pub fn broken() -> Bool { true\n",
            encoding="utf-8",
        )
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("require expected error diagnostics",
                      " ".join(report["results"][0]["failures"]))

        for mode, extra, reason in (
            ("fmt_refuse", "",
             "does not match the expected error multiset"),
            ("fmt_refuse_extra_diagnostic", "", "does not match the expected error multiset"),
            ("fmt_refuse_validation_diagnostic", "", "non-parser or unsupported"),
            ("fmt_refuse_warning", "", "non-parser or unsupported"),
        ):
            with self.subTest(mode=mode, extra=extra):
                expected_line = 5 if mode == "fmt_refuse" else 6
                self._fmt_refuse_fixture(expected_line=expected_line, extra=extra)
                self.mode = mode
                self._write_fake()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(reason, " ".join(report["results"][0]["failures"]))

        self._fmt_refuse_fixture()
        self.mode = "fmt_refuse_wrong_line"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("does not match the expected error multiset",
                      " ".join(report["results"][0]["failures"]))

    def test_fmt_refuse_rejects_acceptance_bad_schema_malformed_stream_and_ice(self):
        for mode, reason in (
            ("fmt_refuse_accepts", "was accepted by the formatter"),
            ("fmt_refuse_bad_schema", "invalid result schema"),
            ("fmt_refuse_malformed", "invalid record"),
            ("fmt_refuse_internal_error", "internal compiler error"),
        ):
            with self.subTest(mode=mode):
                self._fmt_refuse_fixture()
                self.mode = mode
                self._write_fake()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(reason, " ".join(report["results"][0]["failures"]))

    def test_fmt_refuse_rejects_every_sidecar(self):
        self._fmt_refuse_fixture()
        (self.fixture_dir / "case.expected.sai").write_bytes(b"unexpected\n")
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("sidecars are unsupported",
                      " ".join(report["results"][0]["failures"]))
        self.assertFalse(self.receipt.exists())

    def test_fmt_compares_original_bytes_and_expected_fixed_point(self):
        self._fmt_fixture()
        original = self.source.read_bytes()
        report = self._run()
        self.assertEqual(report["outcome"], "pass")
        self.assertEqual(report["results"][0]["outcome"], "pass")
        self.assertEqual(report["results"][0]["configuration"], {
            "target": None, "profile": None,
        })
        self.assertIsNone(report["results"][0]["compatibility_execution"])
        self.assertIs(report["claim"], False)
        self.assertIn(
            "generated package with one source file",
            report["limitations"][2],
        )
        self.assertIn(
            "workspace and multi-source package checks, path-based fmt --check",
            report["limitations"][2],
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["fmt_argv"], [
            "--format=json", "--max-errors=0", "fmt", "--stdin-kind", "source", "-",
        ])
        self.assertEqual(
            receipt["calls"],
            ["fmt", "fmt", "fmt-check", "fmt-check", "fmt-check", "fmt-check"],
        )
        self.assertEqual(receipt["formatter_inputs"], [
            original.decode("utf-8"),
            self.expected_format.decode("utf-8"),
        ])
        stdin_check_argv = [
            "--format=json",
            "--max-errors=0",
            "fmt",
            "--check",
            "--stdin-kind",
            "source",
            "-",
        ]
        package_check_argv = ["--format=json", "--max-errors=0", "fmt", "--check"]
        self.assertEqual(receipt["formatter_check_argvs"], [
            stdin_check_argv, stdin_check_argv, package_check_argv, package_check_argv,
        ])
        self.assertEqual(receipt["formatter_check_inputs"], [
            original.decode("utf-8"),
            self.expected_format.decode("utf-8"),
            original.decode("utf-8"),
            self.expected_format.decode("utf-8"),
        ])
        self.assertEqual(receipt["formatter_check_paths"], [
            "<stdin>", "<stdin>", "src/lib.sai", "src/lib.sai",
        ])
        self.assertEqual(self.source.read_bytes(), original)

    def test_fmt_package_check_rejects_bad_file_inventory_and_manifest(self):
        cases = (
            ("fmt_check_package_wrong_files", "invalid fixture result"),
            ("fmt_check_package_wrong_changed", "invalid fixture result"),
            (
                "fmt_check_package_wrong_path",
                "outside the selected fixture file",
            ),
            (
                "fmt_check_package_manifest_diagnostic",
                "outside the selected fixture file",
            ),
            (
                "fmt_check_package_mutates_source",
                "modified or replaced the source fixture",
            ),
            (
                "fmt_check_package_rewrites_identical_source",
                "modified or replaced the source fixture",
            ),
            (
                "fmt_check_package_adds_source",
                "changed the package source inventory",
            ),
            (
                "fmt_check_package_mutates_manifest",
                "modified or replaced the generated package contract",
            ),
        )
        for mode, expected_failure in cases:
            with self.subTest(mode=mode):
                self._fmt_fixture()
                self.mode = mode
                self._write_fake()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(
                    expected_failure,
                    " ".join(report["results"][0]["failures"]),
                )

    def test_fmt_check_line_contract_uses_lf_bytes_and_empty_missing_lines(self):
        cases = (
            (
                b"fn f(){}\n",
                b"fn f() {}\n",
                (1, b"fn f() {}", 0, 8),
            ),
            (
                b"fn f(){}\r\n",
                b"fn f() {}\n",
                (1, b"fn f() {}", 0, 8),
            ),
            (
                b"\xef\xbb\xbffn f(){}\n",
                b"fn f() {}\n",
                (1, b"fn f() {}", 0, 11),
            ),
            (
                b"first\n",
                b"first\nsecond\n",
                (2, b"second", 6, 6),
            ),
            (
                b"first\nsecond\n",
                b"first\n",
                (2, None, 6, 12),
            ),
            (
                b"fn f() {}\n",
                b"fn f() {}",
                (1, b"fn f() {}", 0, 9),
            ),
            (
                b"fn f() {}",
                b"fn f() {}\n",
                (1, b"fn f() {}", 0, 9),
            ),
            (
                b"\n",
                b"",
                (1, None, 0, 0),
            ),
            (
                b"",
                b"first\n",
                (1, b"first", 0, 0),
            ),
            (b"same\n", b"same\n", None),
        )
        for input_bytes, expected_output, expected in cases:
            with self.subTest(input_bytes=input_bytes, expected_output=expected_output):
                self.assertEqual(
                    _first_differing_lf_line(input_bytes, expected_output),
                    expected,
                )

    def test_fmt_check_position_columns_follow_original_utf8_source_bytes(self):
        source = b"\xef\xbb\xbfabc\r\n" + "é😀\n".encode("utf-8")
        cases = (
            (0, {"line": 1, "col": 1, "utf16_col": 1}),
            (3, {"line": 1, "col": 1, "utf16_col": 1}),
            (6, {"line": 1, "col": 4, "utf16_col": 4}),
            (8, {"line": 2, "col": 1, "utf16_col": 1}),
            (10, {"line": 2, "col": 3, "utf16_col": 2}),
            (14, {"line": 2, "col": 7, "utf16_col": 4}),
        )
        for offset, expected in cases:
            with self.subTest(offset=offset):
                self.assertEqual(_source_position(source, offset), expected)

    def test_fmt_check_rejects_malformed_and_inconsistent_responses(self):
        for mode, reason in (
            ("fmt_check_malformed", "invalid record"),
            ("fmt_check_wrong_code", "first differing line"),
            ("fmt_check_wrong_severity", "unexpected exit code"),
            ("fmt_check_wrong_message", "first differing line"),
            ("fmt_check_wrong_line", "wrong primary position or byte span"),
            ("fmt_check_wrong_span", "wrong primary position or byte span"),
            ("fmt_check_wrong_column", "wrong primary position or byte span"),
            ("fmt_check_wrong_utf16_column", "wrong primary position or byte span"),
            ("fmt_check_wrong_data", "first differing line"),
            ("fmt_check_extra_diagnostic", "exactly one diagnostic"),
            ("fmt_check_missing_diagnostic", "unexpected exit code"),
            ("fmt_check_wrong_result", "invalid fixture result"),
            ("fmt_check_suppressed", "suppressed unexpected diagnostics"),
        ):
            with self.subTest(mode=mode):
                self._fmt_fixture()
                self.mode = mode
                self._write_fake()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(reason, " ".join(report["results"][0]["failures"]))

    def test_fmt_check_rejects_unformatted_diagnostic_on_canonical_expected_output(self):
        self._fmt_fixture()
        self.mode = "fmt_check_canonical_diagnostic"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn(
            "canonical formatter output produced a --check diagnostic",
            " ".join(report["results"][0]["failures"]),
        )
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(receipt["formatter_check_inputs"], [
            self.source.read_text(encoding="utf-8"),
            self.expected_format.decode("utf-8"),
        ])

    def test_fmt_requires_only_its_exact_expected_sidecar(self):
        self._fmt_fixture()
        expected = self.source.with_name("case.expected.sai")
        expected.unlink()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("requires case.expected.sai",
                      " ".join(report["results"][0]["failures"]))

        self._fmt_fixture()
        (self.fixture_dir / "case.fixed.sai").write_bytes(b"extra\n")
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("sidecars are unsupported",
                      " ".join(report["results"][0]["failures"]))

        self._fmt_fixture()
        (self.fixture_dir / "case.stdin").write_bytes(b"extra\n")
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("sidecars are unsupported",
                      " ".join(report["results"][0]["failures"]))

    def test_fmt_wrong_type_or_symlink_expected_sidecar_fails_closed(self):
        self._fmt_fixture()
        expected = self.source.with_name("case.expected.sai")
        expected.unlink()
        expected.mkdir()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("sidecar is not a file",
                      " ".join(report["failures"]))

        if hasattr(os, "symlink"):
            self._fmt_fixture()
            expected = self.source.with_name("case.expected.sai")
            expected.unlink()
            target = self.fixture_dir / "outside.txt"
            target.write_bytes(self.expected_format)
            expected.symlink_to(target)
            report = self._run()
            self.assertEqual(report["outcome"], "fail")
            self.assertTrue(report["failures"])

    def test_fmt_rejects_output_that_differs_from_expected(self):
        self._fmt_fixture()
        self.mode = "fmt_output_mismatch"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("does not match the expected bytes",
                      " ".join(report["results"][0]["failures"]))
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(len(receipt["formatter_inputs"]), 1)

    def test_fmt_rejects_non_fixed_point_expected_output(self):
        self._fmt_fixture()
        self.mode = "fmt_fixed_point_failure"
        self._write_fake()
        report = self._run()
        self.assertEqual(report["outcome"], "fail")
        self.assertIn("not a fixed point",
                      " ".join(report["results"][0]["failures"]))
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        self.assertEqual(len(receipt["formatter_inputs"]), 2)

    def test_fmt_malformed_refused_diagnostic_and_schema_fail_closed(self):
        for mode, reason in (
            ("fmt_malformed", "invalid record"),
            ("fmt_refused", "was refused"),
            ("fmt_internal_error", "formatter reported an internal compiler error"),
            ("fmt_unexpected_diagnostic", "unexpected diagnostics"),
            ("fmt_bad_schema", "invalid result schema"),
            ("fmt_wrong_header", "wrong header"),
            ("fmt_wrong_root", "wrong header"),
            ("fmt_extra_summary", "invalid summary schema"),
        ):
            with self.subTest(mode=mode):
                self._fmt_fixture()
                self.mode = mode
                self._write_fake()
                report = self._run()
                self.assertEqual(report["outcome"], "fail")
                self.assertIn(
                    reason,
                    " ".join(report["results"][0]["failures"]),
                )

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
