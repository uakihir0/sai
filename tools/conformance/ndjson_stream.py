"""Validate the common v1 NDJSON envelope, not complete conformance.

``validate_stream`` consumes a binary line iterator and returns the records.
It raises ``StreamError`` on a detected violation. A return is only a partial
validation: command-specific result schemas, diagnostic ordering and other
context-dependent rules still need to be checked by the caller.
"""

import json
import re
import sys


class StreamError(ValueError):
    """A detected wire-format or common-schema violation."""


CODE = re.compile(r"[A-Z][A-Z0-9]*(_[A-Z][A-Z0-9]*)+\Z")
NODE = re.compile(r"[a-z][a-z_]*:[0-9A-F]{4,16}\Z")
SYMBOL = re.compile(r"symbol://[a-z][a-z0-9]*(-[a-z0-9]+)*%3A[a-z][a-z0-9]*(-[a-z0-9]+)*(?:/(?:[A-Za-z0-9._~!$&'()*+,;=:@]|%[0-9A-F]{2})+)+#[0-9A-F]{6}\Z")
RESULT_KIND = re.compile(r"[a-z][a-z_.]*\Z")
OPERATION_NAME = re.compile(r"[a-z][a-z_]*\Z")


def optional(spec):
    return ("nullable", spec)


def array(spec, minimum=0, maximum=None):
    return ("array", spec, minimum, maximum)


def fields(**specs):
    return ("object", specs)


POSITION = fields(byte="uint", line="pos1", col="pos1", utf16_col="pos1")
SOURCE = fields(path="nonempty", package=optional("str"), snapshot=optional("uint"))
LOCATION = fields(file=SOURCE, start=POSITION, end=POSITION)
OPT_LOCATION = optional(LOCATION)
LEVEL = optional(fields(value=("enum", ("info", "warn", "deny")),
                        source=("enum", ("default", "attribute", "manifest", "policy", "cli", "deny_warnings")),
                        location=OPT_LOCATION))
LABEL = fields(location=LOCATION, message="str")
CHILD = fields(kind="str", message="str", location=OPT_LOCATION)
EDIT = fields(file=SOURCE, start=POSITION, end=POSITION, new_text="str")
PLACEHOLDER = fields(edit="uint", start="uint", end="uint", label="str")
OPERATION = fields(name="operation_name", node="node", args="any_object")
FIX = fields(title="str", applicability=("enum", ("machine-applicable", "maybe-incorrect", "has-placeholders")),
             operation=OPERATION, edits=array(EDIT, 1), placeholders=array(PLACEHOLDER))
STEP = fields(name="str", symbol=optional("symbol"), location=OPT_LOCATION, relation="str")
STEPS = array(STEP, 1, 32)
RELATED = {
    "effect_chain": fields(kind="str", effect="str", steps=STEPS),
    "capability_chain": fields(kind="str", capability="str", effect="str", steps=STEPS, policy=OPT_LOCATION),
    "borrow_conflict": fields(kind="str", place="str", loan=LOCATION,
                              loan_kind=("enum", ("shared", "mutable")), access=LOCATION,
                              access_kind=("enum", ("read", "write", "move", "borrow", "borrow_mut", "storage_dead")),
                              later_use=OPT_LOCATION),
    "use_after_move": fields(kind="str", place="str", moved_at=LOCATION,
                             move_kind=("enum", ("assign", "argument", "return", "capture", "pattern")),
                             used_at=LOCATION, in_loop="bool"),
    "requirement_chain": fields(kind="str", steps=STEPS),
    "declared_here": fields(kind="str", location=LOCATION, what="str"),
}
HEADER = fields(type=("const", "header"), format=("const", "sai-diagnostics"),
                version=("const_int", 1), tool=fields(name=("const", "sai"), version="str"),
                command="str", root=optional("str"))
DIAGNOSTIC = fields(type=("const", "diagnostic"), code="code",
                    severity=("enum", ("error", "warning", "info")), level=LEVEL,
                    message="nonempty", primary=OPT_LOCATION, primary_label="str",
                    labels=array(LABEL), children=array(CHILD), fixes=array(FIX),
                    related=array("related"), data="any_object", node=optional("node"),
                    symbol=optional("symbol"), package=optional("str"))
RESULT = fields(type=("const", "result"), kind="result_kind")
SUMMARY = fields(type=("const", "summary"), errors="uint", warnings="uint", infos="uint",
                 shown_errors="uint", suppressed=fields(cascade="uint", duplicate="uint", dependency="uint"),
                 exit_code="uint")
SCHEMAS = {"header": HEADER, "diagnostic": DIAGNOSTIC, "result": RESULT, "summary": SUMMARY}


def check(value, spec, path):
    if isinstance(spec, tuple):
        tag = spec[0]
        if tag == "nullable":
            if value is not None:
                check(value, spec[1], path)
            return
        if tag == "array":
            if not isinstance(value, list) or len(value) < spec[2] or (spec[3] is not None and len(value) > spec[3]):
                raise StreamError(f"{path}: invalid array length or type")
            for index, item in enumerate(value):
                check(item, spec[1], f"{path}[{index}]")
            return
        if tag == "object":
            if not isinstance(value, dict):
                raise StreamError(f"{path}: expected object")
            properties = spec[1]
            missing = properties.keys() - value.keys()
            if missing:
                raise StreamError(f"{path}: missing {sorted(missing)}")
            actual_order = [key for key in value if key in properties]
            if actual_order != list(properties):
                raise StreamError(f"{path}: members out of schema order")
            for key, child_spec in properties.items():
                check(value[key], child_spec, f"{path}.{key}")
            return
        if tag == "enum":
            if not isinstance(value, str) or value not in spec[1]:
                raise StreamError(f"{path}: invalid enum")
            return
        if tag == "const":
            if not isinstance(value, str) or value != spec[1]:
                raise StreamError(f"{path}: invalid constant")
            return
        if tag == "const_int":
            if type(value) is not int or value != spec[1]:
                raise StreamError(f"{path}: unsupported version")
            return
    elif spec == "related":
        if not isinstance(value, dict) or not isinstance(value.get("kind"), str):
            raise StreamError(f"{path}: related entry needs a string kind")
        check(value, RELATED.get(value["kind"], fields(kind="str")), path)
        return
    elif spec == "any_object":
        if isinstance(value, dict):
            return
    elif spec == "bool":
        if type(value) is bool:
            return
    elif spec in ("uint", "pos1"):
        if type(value) is int and value >= (1 if spec == "pos1" else 0):
            return
    elif isinstance(value, str):
        if spec == "str" or (spec == "nonempty" and value):
            return
        patterns = {"code": CODE, "node": NODE, "symbol": SYMBOL,
                    "result_kind": RESULT_KIND, "operation_name": OPERATION_NAME}
        if spec in patterns and patterns[spec].fullmatch(value) and (spec != "code" or len(value) <= 40):
            return
    raise StreamError(f"{path}: invalid {spec}")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise StreamError(f"duplicate member: {key!a}")
        result[key] = value
    return result


def reject_number(value):
    raise StreamError(f"non-integer or non-JSON number: {value}")


def check_numbers(value):
    pending = [value]
    while pending:
        item = pending.pop()
        if type(item) is int and item < 0:
            raise StreamError("negative number")
        if isinstance(item, dict):
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)


def parse_line(line):
    if not isinstance(line, bytes) or not line.endswith(b"\n"):
        raise StreamError("each record must be bytes terminated by LF")
    try:
        text = line[:-1].decode("utf-8", "strict")
        value = json.loads(text, object_pairs_hook=unique_object,
                           parse_float=reject_number, parse_constant=reject_number)
        check_numbers(value)
        canonical = json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
        if canonical != text:
            raise StreamError("record is not canonically serialized")
        canonical.encode("utf-8", "strict")
    except (UnicodeError, json.JSONDecodeError, RecursionError, ValueError) as error:
        raise StreamError(f"invalid record: {error}") from error
    if not isinstance(value, dict):
        raise StreamError("record must be an object")
    return value


def validate_stream(lines):
    """Return records after partial v1 validation; never a conformance verdict."""
    records = []
    seen_result = False
    interleavable = True
    shown = {"error": 0, "warning": 0, "info": 0}
    for number, line in enumerate(lines, 1):
        try:
            record = parse_line(line)
            kind = record.get("type")
            if not isinstance(kind, str) or kind not in SCHEMAS:
                raise StreamError("unknown record type")
            check(record, SCHEMAS[kind], "record")
            if not records and kind != "header":
                raise StreamError("header must be first")
            if records and (kind == "header" or records[-1]["type"] == "summary"):
                raise StreamError("duplicate header or record after summary")
            if kind == "result":
                seen_result = True
                interleavable &= record["kind"] == "test"
            if kind == "diagnostic":
                if seen_result and not (records[0]["command"] == "test" and interleavable):
                    raise StreamError("diagnostic follows result without defined interleaving")
                shown[record["severity"]] += 1
            if kind == "summary":
                if (record["shown_errors"] != shown["error"] or
                        record["errors"] < shown["error"] or
                        (record["errors"] > 0 and shown["error"] == 0) or
                        record["warnings"] != shown["warning"] or
                        record["infos"] != shown["info"]):
                    raise StreamError("inconsistent diagnostic counts")
            records.append(record)
        except StreamError as error:
            raise StreamError(f"line {number}: {error}") from error
    if not records or records[-1]["type"] != "summary":
        raise StreamError("missing header or final summary")
    return records


def main():
    try:
        records = validate_stream(sys.stdin.buffer)
    except StreamError as error:
        print(f"invalid stream: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"status": "partial", "records": len(records),
                      "unchecked": ["command-specific result schemas", "diagnostic order and registry",
                                    "source-dependent semantics", "process exit and stderr", "golden comparison"]}))
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
