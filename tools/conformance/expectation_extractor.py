#!/usr/bin/env python3
"""Extract one source file's normalized diagnostic expectations.

Usage: python3 expectation_extractor.py SOURCE --path ROOT_RELATIVE_PATH
       [--directives] [--lint-levels JSON_OBJECT]

This is a conservative lexical scanner, not an IUT lexer or a complete suite
validator. Format strings, execution annotations, and ambiguous or unfinished
literals are rejected instead of guessed. Lint defaults must come from the
IUT's explain records; allow-level lints cannot be expected. Other recognized
directives are checked for placement and repetition, but their values are not
validated. Lexically ill-formed tests whose comment boundaries cannot be
established are unsupported. Non-ASCII code outside comments and literals is
unsupported; the scanner does not depend on Python's Unicode identifier tables.
Invalid UTF-8 is supported only as isolated bytes outside comments and literals.
Invalid escapes, manifest line continuations, and a backslash immediately
followed by a lone CR (including in comments and raw strings) are rejected.
Outside manifests, only complete package paths with unambiguous boundaries are
supported. Ambiguous newline or bracket recovery causes conservative rejection.
Output is a JSON object with an `expected` array
in the diagnostic matcher's input format. Errors exit with status 2.
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
DIRECTIVES = {
    "kind", "stage", "spec", "targets", "profiles", "options", "component",
    "call", "args", "seed", "diag", "fuel", "memory-limit", "stderr-fails",
    "reference-only",
}
LEVELS = {"info": "info", "warn": "warning", "deny": "error"}


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


def extract(source: str, path: str, *, directives: bool = False, lint_levels: dict | None = None) -> list[dict]:
    return _extract(source, path, directives=directives, lint_levels=lint_levels, invalid_bytes=False)


def extract_bytes(source: bytes, path: str, *, directives: bool = False, lint_levels: dict | None = None) -> list[dict]:
    if not isinstance(source, bytes):
        raise ValueError("source must be bytes")
    return _extract(source.decode("utf-8", errors="surrogateescape"), path,
                    directives=directives, lint_levels=lint_levels, invalid_bytes=True)


def _extract(source: str, path: str, *, directives: bool, lint_levels: dict | None, invalid_bytes: bool) -> list[dict]:
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
            if name == "diag":
                diagnostic = DIRECTIVE_DIAGNOSTIC.fullmatch(value)
                if diagnostic is None:
                    fail("invalid diagnostic directive")
                severity, code, target_file, target_line = diagnostic.groups()
                if target_file is not None and not root_relative(target_file):
                    fail("diagnostic directive path is not root-relative")
                emit(severity, code, target_file, int(target_line) if target_line else None)

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
    return expected


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--path", required=True)
    parser.add_argument("--directives", action="store_true")
    parser.add_argument("--lint-levels", default="{}")
    args = parser.parse_args()
    try:
        levels = json.loads(args.lint_levels, object_pairs_hook=unique_object)
        if not isinstance(levels, dict):
            raise ValueError("lint levels must be an object")
        result = extract_bytes(args.source.read_bytes(), args.path, directives=args.directives, lint_levels=levels)
    except (OSError, ValueError) as error:
        print(f"invalid input: {error}", file=sys.stderr)
        return 2
    print(json.dumps({"expected": result}, ensure_ascii=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
