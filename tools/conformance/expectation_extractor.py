#!/usr/bin/env python3
"""Extract one source file's normalized diagnostic expectations.

Usage: python3 expectation_extractor.py SOURCE --path ROOT_RELATIVE_PATH
       [--directives] [--validate-test-directives] [--lint-levels JSON_OBJECT]

This is a conservative lexical scanner, not an IUT lexer or a complete suite
validator. Format strings, execution annotations, and ambiguous or unfinished
literals are rejected instead of guessed. Lint defaults must come from the
IUT's explain records; allow-level lints cannot be expected. Expectation-only
mode checks directive placement and repetition but does not validate their
values. `--validate-test-directives` additionally requires and validates the
complete test header, returning normalized directive metadata. Lexically
ill-formed tests whose comment boundaries cannot be established are
unsupported. Non-ASCII code outside comments and literals is
unsupported; the scanner does not depend on Python's Unicode identifier tables.
Invalid UTF-8 is supported only as isolated bytes outside comments and literals.
Invalid escapes, manifest line continuations, and a backslash immediately
followed by a lone CR (including in comments and raw strings) are rejected.
Outside manifests, only complete package paths with unambiguous boundaries are
supported. Ambiguous newline or bracket recovery causes conservative rejection.
Output is a JSON object with an `expected` array in the diagnostic matcher's
input format; strict mode also includes normalized `directives`. Errors exit
with status 2.
See docs/impl/15-conformance-and-testing.md §15.3.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


CODE = r"[A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)+"
DIAGNOSTIC = re.compile(rf"(error|warning|info)[ \t]+({CODE})\Z")
DIRECTIVE_DIAGNOSTIC = re.compile(rf"(error|warning|info)[ \t]+({CODE})(?:[ \t]+at[ \t]+(.+):([1-9][0-9]*))?\Z")
SIMPLE_CODE = re.compile(rf"({CODE})(?=$|[ \t(;,])")
POSSIBLE_PACKAGE_PATH = re.compile(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*:")
PACKAGE_PATH = re.compile(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*:[a-z][a-z0-9]*(?:-[a-z0-9]+)*(?:/[a-z][a-z0-9_]*(?:-[a-z0-9]+)*)*")
UNICODE_ESCAPE = re.compile(r"\\u\{([0-9a-fA-F]{1,6})\}")
SPEC_REFERENCE = re.compile(r"([A-Za-z0-9][A-Za-z0-9._-]*(?:/[A-Za-z0-9][A-Za-z0-9._-]*)*)[ \t]+§([0-9]+(?:\.[0-9]+)*)\Z")
CALL_EXPORT = re.compile(r"[A-Za-z_][A-Za-z0-9_.:/@#-]*\Z")
DIRECTIVES = {
    "kind", "stage", "spec", "targets", "profiles", "options", "component",
    "call", "args", "seed", "diag", "fuel", "memory-limit", "stderr-fails",
    "reference-only",
}
LEVELS = {"info": "info", "warn": "warning", "deny": "error"}
KINDS = {
    "check-pass", "check-fail", "build-pass", "build-fail", "run-pass",
    "run-fail", "run-panic", "run-trap", "test", "fmt", "fmt-refuse",
    "protocol",
}
TARGETS = {"wasm32-core", "wasm64-core", "wasm32-wasi", "wasm64-wasi"}
PROFILES = {"dev", "release", "tiny"}
OPTION_FLAGS = {"--tests", "--deny-warnings", "--locked", "--offline", "--frozen"}
OPTION_VALUE_FLAGS = {"--allow", "--warn", "--deny"}


def root_relative(path: object) -> bool:
    return (
        isinstance(path, str)
        and "\\" not in path
        and "\0" not in path
        and not re.match(r"^[A-Za-z]:", path)
        and all(part not in ("", ".", "..") for part in path.split("/"))
    )


def is_identifier_start(character: str) -> bool:
    return character == "_" or "a" <= character <= "z" or "A" <= character <= "Z"


def is_identifier_continue(character: str) -> bool:
    return is_identifier_start(character) or "0" <= character <= "9"


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key in lint levels")
        result[key] = value
    return result


def _parse_json_string_arguments(value: str) -> list[str]:
    decoder = json.JSONDecoder()
    result = []
    position = 0
    while position < len(value):
        while position < len(value) and value[position] in " \t":
            position += 1
        if position == len(value):
            break
        try:
            argument, end = decoder.raw_decode(value, position)
        except (json.JSONDecodeError, RecursionError):
            raise ValueError("args must contain space-separated JSON strings") from None
        if not isinstance(argument, str):
            raise ValueError("each args entry must be a JSON string")
        if any(0xD800 <= ord(character) <= 0xDFFF for character in argument):
            raise ValueError("args strings must contain Unicode scalar values")
        result.append(argument)
        position = end
        if position < len(value) and value[position] not in " \t":
            raise ValueError("args entries must be separated by spaces")
    if not result:
        raise ValueError("args must contain at least one JSON string")
    return result


def _split_call_arguments(value: str) -> list[str]:
    if not value.strip():
        return []
    result = []
    start = 0
    stack = []
    quote = None
    escaped = False
    pairs = {")": "(", "]": "[", "}": "{"}
    for index, character in enumerate(value):
        if quote is not None:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = None
            continue
        if character in ('"', "'"):
            quote = character
        elif character in "([{":
            stack.append(character)
        elif character in ")]}":
            if not stack or stack.pop() != pairs[character]:
                raise ValueError("call directive has unbalanced delimiters")
        elif character == "," and not stack:
            argument = value[start:index].strip()
            if not argument:
                raise ValueError("call directive has an empty argument")
            result.append(argument)
            start = index + 1
    if quote is not None or escaped or stack:
        raise ValueError("call directive has an unterminated string or delimiter")
    final = value[start:].strip()
    if not final:
        raise ValueError("call directive has an empty argument")
    result.append(final)
    return result


def _validate_call_value(value: str) -> None:
    stack = []
    quote = None
    escaped = False
    pairs = {")": "(", "]": "[", "}": "{"}
    for character in value:
        if quote is not None:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = None
            continue
        if character in ('"', "'"):
            quote = character
        elif character in "([{":
            stack.append(character)
        elif character in ")]}":
            if not stack or stack.pop() != pairs[character]:
                raise ValueError("call directive has unbalanced delimiters")
    if quote is not None or escaped or stack:
        raise ValueError("call directive has an unterminated string or delimiter")


def _parse_call(value: str) -> dict:
    value = value.strip()
    open_paren = value.find("(")
    if open_paren <= 0:
        raise ValueError("call must have the form <export>(<value>, ...) -> <result>")
    export_text = value[:open_paren]
    export = export_text.strip()
    if export_text != export or not CALL_EXPORT.fullmatch(export):
        raise ValueError("call has an invalid export name")

    stack = []
    quote = None
    escaped = False
    pairs = {")": "(", "]": "[", "}": "{"}
    close_paren = None
    for index in range(open_paren, len(value)):
        character = value[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = None
            continue
        if character in ('"', "'"):
            quote = character
        elif character in "([{":
            stack.append(character)
        elif character in ")]}":
            if not stack or stack.pop() != pairs[character]:
                raise ValueError("call directive has unbalanced delimiters")
            if not stack:
                close_paren = index
                break
    if close_paren is None or quote is not None or escaped:
        raise ValueError("call directive has an unterminated delimiter or string")

    trailer = value[close_paren + 1:]
    result_match = re.fullmatch(r"[ \t]*->[ \t]*(.+?)[ \t]*", trailer)
    if result_match is None:
        raise ValueError("call must have a result after `->`")
    arguments = _split_call_arguments(value[open_paren + 1:close_paren])
    result = result_match[1]
    _validate_call_value(result)
    return {"export": export, "arguments": arguments, "result": result}


def _parse_options(value: str) -> list[str]:
    tokens = value.split()
    if not tokens:
        raise ValueError("options must contain at least one option")
    parsed = []
    index = 0
    while index < len(tokens):
        option = tokens[index]
        parsed.append(option)
        index += 1
        if option in OPTION_FLAGS:
            continue
        if option in OPTION_VALUE_FLAGS:
            if index >= len(tokens) or not re.fullmatch(CODE, tokens[index]):
                raise ValueError(f"{option} requires a diagnostic code")
            parsed.append(tokens[index])
            index += 1
            continue
        if option == "--wasm-feature":
            if index >= len(tokens) or tokens[index] not in {"tail-call", "simd128"}:
                raise ValueError("--wasm-feature requires tail-call or simd128")
            parsed.append(tokens[index])
            index += 1
            continue
        raise ValueError(f"unsupported test option: {option}")
    return parsed


def _parse_directive(name: str, value: str) -> object:
    if name == "kind":
        if value not in KINDS:
            raise ValueError("unsupported test kind")
        return value
    if name == "stage":
        if not re.fullmatch(r"[0-9]+", value) or int(value) > 7:
            raise ValueError("stage must be a decimal integer from 0 to 7")
        return int(value)
    if name == "spec":
        references = [reference.strip() for reference in value.split(",")]
        if not references or any(not reference for reference in references):
            raise ValueError("spec must contain one or more section references")
        result = []
        for reference in references:
            match = SPEC_REFERENCE.fullmatch(reference)
            if match is None or not root_relative(match[1]):
                raise ValueError("spec entries must have the form `<document> §<section>`")
            result.append({"document": match[1], "section": match[2]})
        return result
    if name in ("targets", "profiles"):
        values = value.split()
        allowed = TARGETS if name == "targets" else PROFILES
        if not values or any(item not in allowed for item in values):
            raise ValueError(f"{name} must contain supported names separated by spaces")
        if len(set(values)) != len(values):
            raise ValueError(f"{name} contains a duplicate name")
        return values
    if name == "options":
        return _parse_options(value)
    if name == "component":
        if value:
            raise ValueError("component is an empty flag")
        return True
    if name == "call":
        return _parse_call(value)
    if name == "args":
        return _parse_json_string_arguments(value)
    if name == "seed":
        if not re.fullmatch(r"[0-9]+", value):
            raise ValueError("seed must be a decimal integer")
        return int(value)
    if name == "diag":
        diagnostic = DIRECTIVE_DIAGNOSTIC.fullmatch(value)
        if diagnostic is None:
            raise ValueError("invalid diagnostic directive")
        severity, code, target_file, target_line = diagnostic.groups()
        if target_file is not None and not root_relative(target_file):
            raise ValueError("diagnostic directive path is not root-relative")
        return {"file": target_file, "line": int(target_line) if target_line else None,
                "severity": severity, "code": code}
    if name in ("fuel", "memory-limit"):
        if not re.fullmatch(r"[0-9]+", value):
            raise ValueError(f"{name} must be a decimal integer")
        return int(value)
    if name == "stderr-fails":
        if value != "first-write":
            raise ValueError("stderr-fails supports only first-write")
        return value
    if name == "reference-only":
        if not value:
            raise ValueError("reference-only requires a reason")
        return value
    raise ValueError(f"unsupported directive: {name}")


def extract(source: str, path: str, *, directives: bool = False, lint_levels: dict | None = None) -> list[dict]:
    expected, _ = _extract(source, path, directives=directives, lint_levels=lint_levels,
                           invalid_bytes=False)
    return expected


def extract_bytes(source: bytes, path: str, *, directives: bool = False, lint_levels: dict | None = None) -> list[dict]:
    if not isinstance(source, bytes):
        raise ValueError("source must be bytes")
    expected, _ = _extract(source.decode("utf-8", errors="surrogateescape"), path,
                           directives=directives, lint_levels=lint_levels, invalid_bytes=True)
    return expected


def extract_with_directives(source: str, path: str, *, lint_levels: dict | None = None) -> dict:
    """Extract expectations and validate the complete test directive header."""
    expected, metadata = _extract(source, path, directives=True, lint_levels=lint_levels,
                                  invalid_bytes=False, validate_test_directives=True)
    return {"expected": expected, "directives": metadata}


def extract_bytes_with_directives(source: bytes, path: str, *, lint_levels: dict | None = None) -> dict:
    """Byte-preserving counterpart of :func:`extract_with_directives`."""
    if not isinstance(source, bytes):
        raise ValueError("source must be bytes")
    expected, metadata = _extract(source.decode("utf-8", errors="surrogateescape"), path,
                                  directives=True, lint_levels=lint_levels, invalid_bytes=True,
                                  validate_test_directives=True)
    return {"expected": expected, "directives": metadata}


def _extract(source: str, path: str, *, directives: bool, lint_levels: dict | None,
             invalid_bytes: bool, validate_test_directives: bool = False) -> tuple[list[dict], dict | None]:
    if not isinstance(source, str) or not root_relative(path) or type(directives) is not bool:
        raise ValueError("invalid source, root-relative path, or directives flag")
    try:
        source.encode("utf-8", errors="surrogateescape" if invalid_bytes else "strict")
        path.encode("utf-8", errors="strict")
    except UnicodeEncodeError:
        raise ValueError("source and path must contain Unicode scalar values") from None
    if lint_levels is None:
        lint_levels = {}
    if not isinstance(lint_levels, dict):
        raise ValueError("lint levels must be an object")
    if any(not isinstance(code, str) or not re.fullmatch(CODE, code) or level not in ("allow", "info", "warn", "deny") for code, level in lint_levels.items()):
        raise ValueError("invalid lint level entry")
    is_manifest = path == "sai.package" or path.endswith("/sai.package")
    if not is_manifest and not path.endswith(".sai"):
        raise ValueError("source path must name a .sai file or sai.package")
    if re.search(r"\\\r(?!\n)", source):
        raise ValueError("backslash followed by a lone CR is not supported")
    source = source.removeprefix("\ufeff").replace("\r\n", "\n").replace("\r", "\n")
    position = 0
    line = 1
    code_on_line = False
    seen_token = False
    seen_directives = set()
    expected = []
    previous_introducer = None
    line_break_since_previous = False
    brackets = []
    brackets_unknown = False
    directive_values = {}
    parsed_diagnostics = []

    def fail(message: str) -> None:
        raise ValueError(f"line {line}: {message}")

    def invalid_character(character: str) -> bool:
        return "\udc80" <= character <= "\udcff"

    def require_valid_text(text: str) -> None:
        if any(invalid_character(character) for character in text):
            fail("invalid UTF-8 within a comment or literal is not supported")

    def emit(severity: str, code: str, target_file: str | None, target_line: int | None) -> None:
        expected.append({"file": target_file, "line": target_line, "severity": severity, "code": code})

    def process_comment(comment: str) -> None:
        if comment.startswith("//~"):
            value = comment[3:]
            caret_count = len(value) - len(value.lstrip("^"))
            value = value[caret_count:].strip()
            target_line = line - caret_count
            if target_line < 1 or (not caret_count and not code_on_line):
                fail("annotation must refer to a valid code line")
            if value == "panic" or value.startswith("test-failed:"):
                fail("execution expectations are not supported")
            parts = value.split(",")
            for part in parts:
                match = DIAGNOSTIC.fullmatch(part.strip())
                if match is None:
                    fail("invalid diagnostic annotation")
                emit(match[1], match[2], path, target_line)
            return
        for label in ("rejected", "lint"):
            prefix = f"// {label}:"
            if comment.startswith(prefix):
                match = SIMPLE_CODE.match(comment[len(prefix):].strip())
                if match is None or not code_on_line:
                    fail(f"invalid {label} annotation")
                code = match[1]
                if label == "rejected":
                    severity = "error"
                else:
                    level = lint_levels.get(code)
                    if not isinstance(level, str) or level not in LEVELS:
                        fail(f"missing or unsupported default level for lint {code}")
                    severity = LEVELS[level]
                emit(severity, code, path, line)
                return
        if comment.startswith("//@"):
            if not directives or seen_token:
                fail("directive outside the directive source or after a token")
            match = re.fullmatch(r"//@\s*([a-z][a-z-]*)\s*:\s*(.*?)\s*", comment)
            if match is None or match[1] not in DIRECTIVES:
                fail("invalid or unknown directive")
            name, value = match.groups()
            if name not in ("diag", "call") and name in seen_directives:
                fail("repeated non-repeatable directive")
            seen_directives.add(name)
            if validate_test_directives or name == "diag":
                try:
                    parsed = _parse_directive(name, value)
                except ValueError as error:
                    fail(str(error))
                if name == "diag":
                    parsed_diagnostics.append(parsed)
                    emit(parsed["severity"], parsed["code"], parsed["file"], parsed["line"])
                else:
                    directive_values.setdefault(name, []).append(parsed)

    def skip_escape(start: int, limit: int, *, byte: bool, continuation: bool) -> int:
        if start + 1 >= limit:
            fail("unterminated escape")
        escaped = source[start + 1]
        if invalid_character(escaped):
            fail("invalid UTF-8 within a literal is not supported")
        if escaped in "nrt\\'\"0":
            return start + 2
        if escaped == "\n" and continuation and not is_manifest:
            return start + 2
        if escaped == "u" and not byte:
            match = UNICODE_ESCAPE.match(source, start, limit)
            if match:
                scalar = int(match[1], 16)
                if scalar <= 0x10ffff and not 0xd800 <= scalar <= 0xdfff:
                    return match.end()
        if escaped == "x" and byte and start + 4 <= limit and re.fullmatch(r"[0-9a-fA-F]{2}", source[start + 2:start + 4]):
            return start + 4
        fail("invalid or unsupported escape")

    def skip_literal(start: int, quote: str, *, byte: bool = False) -> int:
        cursor = start + 1
        while cursor < len(source):
            if invalid_character(source[cursor]):
                fail("invalid UTF-8 within a literal is not supported")
            if source[cursor] == quote:
                return cursor + 1
            if source[cursor] == "\n":
                fail("unterminated literal")
            if source[cursor] == "\\":
                cursor = skip_escape(cursor, len(source), byte=byte, continuation=quote == '"')
                continue
            cursor += 1
        fail("unterminated literal")

    if not is_manifest and source.startswith("#!"):
        end = source.find("\n")
        position = len(source) if end < 0 else end
        require_valid_text(source[:position])

    while position < len(source):
        character = source[position]
        if invalid_character(character):
            end = position + 1
            while end < len(source) and invalid_character(source[end]):
                end += 1
            before = position == 0 or source[position - 1] in " \t\n"
            after = end == len(source) or source[end] in " \t\n" or source.startswith(("//", "/*"), end)
            if not before or not after:
                fail("invalid UTF-8 is supported only as an isolated token")
            seen_token = True
            code_on_line = True
            previous_introducer = None
            line_break_since_previous = False
            position = end
            continue
        if character == "\n":
            line += 1
            code_on_line = False
            line_break_since_previous = True
            position += 1
            continue
        if character in " \t":
            position += 1
            continue
        if source.startswith("//", position):
            end = source.find("\n", position)
            end = len(source) if end < 0 else end
            require_valid_text(source[position:end])
            if source.startswith("//!", position) or (source.startswith("///", position) and not source.startswith("////", position)):
                previous_introducer = None
                line_break_since_previous = False
                if not is_manifest:
                    seen_token = True
            process_comment(source[position:end])
            position = end
            continue
        if source.startswith("/*", position):
            depth = 1
            position += 2
            while position < len(source) and depth:
                if source.startswith("/*", position):
                    depth += 1
                    position += 2
                elif source.startswith("*/", position):
                    depth -= 1
                    position += 2
                else:
                    if invalid_character(source[position]):
                        fail("invalid UTF-8 within a block comment is not supported")
                    if source[position] == "\n":
                        line += 1
                        code_on_line = False
                        line_break_since_previous = True
                    position += 1
            if depth:
                fail("unterminated block comment")
            continue

        if ord(character) >= 128:
            fail("non-ASCII code outside comments and literals is not supported")
        if character.isspace() or ord(character) < 32 or ord(character) == 127:
            fail("unsupported whitespace or control outside a literal or comment")

        seen_token = True
        code_on_line = True
        start = position
        introducer = previous_introducer
        line_break = line_break_since_previous
        previous_introducer = None
        line_break_since_previous = False
        if not is_manifest and "0" <= character <= "9":
            position += 1
            while position < len(source) and is_identifier_continue(source[position]):
                position += 1
            continue
        if is_identifier_start(character):
            path_context = (introducer in ("use", "export") or
                            (introducer in ("import", "include") and
                             (not line_break or brackets_unknown or (brackets and brackets[-1] in "(["))))
            if not is_manifest and path_context and POSSIBLE_PACKAGE_PATH.match(source, position):
                path_match = PACKAGE_PATH.match(source, position)
                safe_newline = not line_break or (not brackets_unknown and brackets and brackets[-1] in "([")
                if (not safe_newline or path_match is None or
                        (path_match.end() < len(source) and source[path_match.end()] not in " \t\n.,{}")):
                    fail("package-path candidates with ambiguous boundaries are not supported")
                position = path_match.end()
                continue
            position += 1
            while position < len(source) and is_identifier_continue(source[position]):
                position += 1
            word = source[start:position]
            if word == "r" and not is_manifest and position < len(source) and source[position] == "#":
                raw_start = position + 1
                if raw_start < len(source) and is_identifier_start(source[raw_start]):
                    position = raw_start + 1
                    while position < len(source) and is_identifier_continue(source[position]):
                        position += 1
                    continue
            if is_manifest and word in ("f", "r", "br", "b") and position < len(source) and source[position] in "#\"'":
                fail("source-language literal syntax is not supported in a manifest")
            if word == "f" and position < len(source) and source[position] == '"':
                fail("format strings are not supported")
            if word == "b" and position < len(source) and source[position] in "\"'":
                position = skip_literal(position, source[position], byte=True)
                line += source[start:position].count("\n")
                continue
            if word in ("r", "br"):
                quote = position
                while quote < len(source) and source[quote] == "#":
                    quote += 1
                if quote < len(source) and source[quote] == '"':
                    if quote - position > 255:
                        fail("raw string delimiter is too long")
                    closing = '"' + "#" * (quote - position)
                    end = source.find(closing, quote + 1)
                    if end < 0:
                        fail("unterminated raw string")
                    position = end + len(closing)
                    require_valid_text(source[start:position])
                    line += source[start:position].count("\n")
                    continue
            if word in ("use", "export", "import", "include"):
                previous_introducer = word
            continue
        if character == '"':
            if source.startswith('"""', position):
                if is_manifest:
                    fail("multiline strings are not supported in a manifest")
                if not source.startswith('"""\n', position):
                    fail("malformed multiline string")
                cursor = position + 4
                while cursor < len(source):
                    end = source.find("\n", cursor)
                    end = len(source) if end < 0 else end
                    if source[cursor:end].lstrip(" \t") == '"""':
                        body = position + 4
                        while body < cursor:
                            if source[body] == "\\":
                                body = skip_escape(body, cursor, byte=False, continuation=False)
                            else:
                                body += 1
                        position = end
                        break
                    cursor = end + 1
                else:
                    fail("unterminated multiline string")
            else:
                position = skip_literal(position, '"')
            require_valid_text(source[start:position])
            line += source[start:position].count("\n")
            continue
        if character == "'":
            if is_manifest:
                fail("character or label syntax is not supported in a manifest")
            if position + 1 < len(source) and ord(source[position + 1]) >= 128:
                fail("non-ASCII character or label syntax is not supported")
            if position + 1 < len(source) and is_identifier_start(source[position + 1]):
                position += 2
                while position < len(source) and is_identifier_continue(source[position]):
                    position += 1
                if position < len(source) and source[position] == "'":
                    position += 1
            else:
                position = skip_literal(position, "'")
            continue
        if character in "([{":
            brackets.append(character)
        elif character in ")]}" and not brackets_unknown:
            if not brackets or brackets[-1] != {')': '(', ']': '[', '}': '{'}[character]:
                brackets_unknown = True
            else:
                brackets.pop()
        position += 1
    if not validate_test_directives:
        return expected, None

    missing = {"kind", "stage", "spec"} - directive_values.keys()
    if missing:
        raise ValueError("missing required directive(s): " + ", ".join(sorted(missing)))

    def one(name: str) -> object:
        return directive_values[name][0]

    metadata = {
        "specified": sorted(seen_directives),
        "kind": one("kind"),
        "stage": one("stage"),
        "spec": one("spec"),
        "targets": one("targets") if "targets" in directive_values else sorted(TARGETS),
        "profiles": one("profiles") if "profiles" in directive_values else ["dev", "release", "tiny"],
        "options": one("options") if "options" in directive_values else [],
        "component": "component" in directive_values,
        "call": directive_values.get("call", []),
        "args": one("args") if "args" in directive_values else [],
        "seed": one("seed") if "seed" in directive_values else None,
        "diag": parsed_diagnostics,
        "fuel": one("fuel") if "fuel" in directive_values else 10_000_000_000,
        "memory_limit": one("memory-limit") if "memory-limit" in directive_values else 4096,
        "stderr_fails": one("stderr-fails") if "stderr-fails" in directive_values else None,
        "reference_only": one("reference-only") if "reference-only" in directive_values else None,
    }
    return expected, metadata


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--path", required=True)
    parser.add_argument("--directives", action="store_true")
    parser.add_argument("--validate-test-directives", action="store_true")
    parser.add_argument("--lint-levels", default="{}")
    args = parser.parse_args()
    try:
        levels = json.loads(args.lint_levels, object_pairs_hook=unique_object)
        if not isinstance(levels, dict):
            raise ValueError("lint levels must be an object")
        source = args.source.read_bytes()
        if args.validate_test_directives:
            result = extract_bytes_with_directives(source, args.path, lint_levels=levels)
        else:
            result = {"expected": extract_bytes(source, args.path, directives=args.directives,
                                                lint_levels=levels)}
    except (OSError, ValueError) as error:
        print(f"invalid input: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
