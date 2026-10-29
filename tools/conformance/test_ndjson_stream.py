import io
import json
import subprocess
import sys
import unittest
from pathlib import Path

from ndjson_stream import StreamError, validate_stream


def header(command="check"):
    return {"type": "header", "format": "sai-diagnostics", "version": 1,
            "tool": {"name": "sai", "version": "1.0.0"}, "command": command, "root": None}


def summary(errors=0, shown_errors=0):
    return {"type": "summary", "errors": errors, "warnings": 0, "infos": 0,
            "shown_errors": shown_errors, "suppressed": {"cascade": 0, "duplicate": 0, "dependency": 0},
            "exit_code": 0}


def position(byte=0):
    return {"byte": byte, "line": 1, "col": 1, "utf16_col": 1}


def location():
    return {"file": {"path": "src/あ.sai", "package": None, "snapshot": None},
            "start": position(), "end": position(1)}


def diagnostic():
    return {"type": "diagnostic", "code": "EXPECTED_TOKEN", "severity": "error", "level": None,
            "message": "expected a token", "primary": location(), "primary_label": "",
            "labels": [], "children": [], "fixes": [], "related": [], "data": {},
            "node": None, "symbol": None, "package": None}


def encode(records):
    return b"".join((json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
                    for record in records)


class StreamTests(unittest.TestCase):
    def validate(self, records):
        return validate_stream(io.BytesIO(encode(records)))

    def invalid(self, records):
        with self.assertRaises(StreamError):
            self.validate(records)

    def test_basic_and_nested_valid_records(self):
        record = diagnostic()
        record["level"] = {"value": "warn", "source": "cli", "location": None}
        record["labels"] = [{"location": location(), "message": "label"}]
        record["children"] = [{"kind": "note", "message": "文字\n\t\b\r\f\x00", "location": None}]
        record["fixes"] = [{"title": "fix", "applicability": "has-placeholders",
                            "operation": {"name": "replace", "node": "expr:ABCD", "args": {}},
                            "edits": [{"file": location()["file"], "start": position(),
                                       "end": position(1), "new_text": "x"}],
                            "placeholders": [{"edit": 0, "start": 0, "end": 1, "label": "x"}]}]
        record["related"] = [{"kind": "effect_chain", "effect": "io", "steps": [
            {"name": "f", "symbol": "symbol://acme%3Apkg/path#ABCDEF", "location": None, "relation": "calls"}]}]
        records = [header(), record, summary(1, 1)]
        self.assertEqual(self.validate(records), records)

    def test_common_schema_required_and_types(self):
        for field in diagnostic():
            record = diagnostic()
            del record[field]
            with self.subTest(field=field):
                self.invalid([header(), record, summary(1, 1)])
        for field, value in [("code", "A"), ("code", "INVALID_code"), ("code", "A_" + "A" * 39),
                             ("severity", "warn"), ("message", ""), ("labels", {}),
                             ("node", "expr:abcd"), ("symbol", "wrong"), ("data", []),
                             ("primary_label", None)]:
            record = diagnostic()
            record[field] = value
            with self.subTest(field=field, value=value):
                self.invalid([header(), record, summary(1, 1)])

    def test_nested_schema_constraints(self):
        bad_locations = []
        for path, value in [("byte", True), ("line", 0), ("col", -1), ("utf16_col", None)]:
            loc = location()
            loc["start"][path] = value
            bad_locations.append(loc)
        loc = location()
        loc["file"]["path"] = ""
        bad_locations.append(loc)
        for loc in bad_locations:
            record = diagnostic()
            record["primary"] = loc
            self.invalid([header(), record, summary(1, 1)])
        for related in [{"kind": "effect_chain", "effect": "io", "steps": []},
                        {"kind": "declared_here", "location": None, "what": "x"},
                        {"kind": "use_after_move", "place": "x", "moved_at": location(),
                         "move_kind": "assign", "used_at": location(), "in_loop": 1}]:
            record = diagnostic()
            record["related"] = [related]
            self.invalid([header(), record, summary(1, 1)])
        record = diagnostic()
        record["fixes"] = [{"title": "f", "applicability": "machine-applicable",
                            "operation": {"name": "bad-name", "node": "expr:ABCD", "args": {}},
                            "edits": [], "placeholders": []}]
        self.invalid([header(), record, summary(1, 1)])

    def test_version_extension_and_known_order(self):
        record = diagnostic()
        record["extra"] = {"future": [1, True, None]}
        record["children"] = [{"kind": "future_kind", "message": "m", "location": None}]
        record["related"] = [{"kind": "future_kind", "payload": "opaque"}]
        self.validate([header(), record, summary(1, 1)])
        record = diagnostic()
        record["primary"] = {"start": position(), "file": location()["file"], "end": position(1)}
        self.invalid([header(), record, summary(1, 1)])
        invalid_header = header()
        invalid_header["version"] = True
        self.invalid([invalid_header, summary()])
        invalid_header["version"] = 2
        self.invalid([invalid_header, summary()])

    def test_wire_form_rejects_noncanonical_and_malformed(self):
        valid = encode([header(), summary()])
        invalid = [b"", b"\xef\xbb\xbf" + valid, valid[:-1], valid.replace(b'"type":', b'"type" :', 1),
                   valid.replace(b'"header"', b'"he\\u0061der"', 1), valid.replace(b'\n', b'\r\n', 1),
                   valid.replace(b'"version":1', b'"version":1,"version":1', 1),
                   valid.replace(b'"version":1', b'"version":1.0', 1),
                   valid.replace(b'"version":1', b'"version":NaN', 1),
                   valid.replace(b'"root":null', b'"root":"\\ud800"', 1),
                   valid.replace(b'"root":null', b'"root":"\xff"', 1),
                   valid.replace(b'"root":null', b'"root":null,"extra":-1', 1),
                   valid.replace(b'"root":null', b'"root":null,"extra":{"a":1,"a":2}', 1),
                   b'[]\n', b'\n']
        for stream in invalid:
            with self.subTest(stream=stream):
                with self.assertRaises(StreamError):
                    validate_stream(io.BytesIO(stream))

    def test_stream_shape_and_counts(self):
        result = {"type": "result", "kind": "build", "data": {}}
        diag = diagnostic()
        for records in [[], [summary()], [header()], [header(), header(), summary()],
                        [header(), summary(), summary()], [header(), result, diag, summary(1, 1)],
                        [header(), diag, summary()], [header(), diag, summary(0, 1)],
                        [header(), {"type": "unknown"}, summary()]]:
            self.invalid(records)
        self.validate([header(), diag, result, summary(1, 1)])
        self.validate([header("test"), {"type": "result", "kind": "test"}, diag, summary(1, 1)])
        self.invalid([header("test"), result, diag, summary(1, 1)])

    def test_summary_schema_and_result_kind(self):
        for name in summary():
            value = summary()
            del value[name]
            self.invalid([header(), value])
        for value in [True, -1, "0"]:
            end = summary()
            end["suppressed"]["cascade"] = value
            self.invalid([header(), end])
        self.invalid([header(), {"type": "result", "kind": "Bad"}, summary()])
        for severity in ("warning", "info"):
            record = diagnostic()
            record["severity"] = severity
            self.invalid([header(), record, summary()])

    def test_diagnostic_counts(self):
        for severity, field in (("warning", "warnings"), ("info", "infos")):
            record = diagnostic()
            record["severity"] = severity
            for count in (0, 1, 2):
                end = summary()
                end[field] = count
                with self.subTest(severity=severity, count=count):
                    if count == 1:
                        self.validate([header(), record, end])
                    else:
                        self.invalid([header(), record, end])
            end = summary()
            end[field] = 1
            self.invalid([header(), end])

        error = diagnostic()
        self.validate([header(), error, summary(1, 1)])
        self.validate([header(), error, summary(2, 1)])
        for total, shown in ((0, 1), (1, 0), (1, 2), (2, 0)):
            with self.subTest(errors=total, shown_errors=shown):
                self.invalid([header(), error, summary(total, shown)])
        self.invalid([header(), summary(2, 0)])
        self.validate([header(), summary()])

        warning = diagnostic()
        warning["severity"] = "warning"
        info = diagnostic()
        info["severity"] = "info"
        end = summary(3, 1)
        end["warnings"] = 1
        end["infos"] = 1
        end["suppressed"] = {"cascade": 4, "duplicate": 2, "dependency": 1}
        self.validate([header(), error, warning, info, end])

    def test_related_variants_and_future_relation(self):
        step = {"name": "f", "symbol": None, "location": None, "relation": "future_relation"}
        variants = [
            {"kind": "capability_chain", "capability": "fs", "effect": "io", "steps": [step], "policy": None},
            {"kind": "requirement_chain", "steps": [step]},
            {"kind": "borrow_conflict", "place": "x", "loan": location(), "loan_kind": "shared",
             "access": location(), "access_kind": "read", "later_use": None},
            {"kind": "use_after_move", "place": "x", "moved_at": location(), "move_kind": "capture",
             "used_at": location(), "in_loop": False},
            {"kind": "declared_here", "location": location(), "what": "x"},
        ]
        record = diagnostic()
        record["related"] = variants
        self.validate([header(), record, summary(1, 1)])
        variants[0]["steps"] = [step] * 33
        self.invalid([header(), record, summary(1, 1)])

    def test_cli_never_reports_partial_as_success(self):
        script = Path(__file__).with_name("ndjson_stream.py")
        result = subprocess.run([sys.executable, "-B", str(script)], input=encode([header(), summary()]),
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["status"], "partial")
        result = subprocess.run([sys.executable, "-B", str(script)], input=b"not json\n",
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b"invalid stream", result.stderr)


if __name__ == "__main__":
    unittest.main()
