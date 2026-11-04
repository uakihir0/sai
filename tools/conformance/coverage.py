#!/usr/bin/env python3
"""Write a deterministic, report-only conformance coverage inventory.

Usage: python3 -B tools/conformance/coverage.py [--root REPOSITORY]
       [--output target/conformance/coverage.json]

This validates every fixture's directives and specification references, then
reports section, diagnostic-code, and panic-kind coverage. It does not run the
compiler or establish a conformance claim. Stage gating remains disabled until
the repository records which implementation stages are complete.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import stat
import sys
from pathlib import Path

import expectation_extractor
import fixture_discovery
import ndjson_stream


CODE_TEXT = r"[A-Z][A-Z0-9]*(?:_[A-Z][A-Z0-9]*)+"
CODE = re.compile(CODE_TEXT + r"\Z")
DIAGNOSTIC_CODE_CELL_SUFFIXES = (
    ", and the precondition codes of the operations",
    ", and every other `parser` row",
    ", and every other `validation` row",
    " and every other lexer error",
)
DIAGNOSTIC_SEVERITIES = {
    "error",
    "warning",
    "info",
    "warning (lint)",
    "info (lint)",
    "warning (lint), default `allow`",
}
SECTION = re.compile(r"[0-9]+(?:\.[0-9]+)*\Z")
SPEC_REFERENCE = re.compile(
    r"([A-Za-z0-9][A-Za-z0-9._-]*(?:/[A-Za-z0-9][A-Za-z0-9._-]*)*)"
    r"[ \t]+§([0-9]+(?:\.[0-9]+)*)\Z"
)
HEADING = re.compile(r"^(#{1,6})[ \t]+([0-9]+(?:\.[0-9]+)*)(?:[ \t]+.*)?[ \t]*$")
DIAGNOSTIC_FENCE = re.compile(r"^([ \t]*)(`{3,}|~{3,})(.*)$")
LIST_ITEM_MARKER = re.compile(r"^([ \t]*)([-+*]|[0-9]{1,9}[.)])([ \t]+)")
DOC_FILENAME = re.compile(r"([0-9]{2})-[A-Za-z0-9._-]+\.md\Z")
PANIC_KIND = re.compile(r"`([^`]+)`")
SECTION_EXEMPTION = re.compile(
    r"(.+?)[ \t]*\|[ \t]*(informative|decisions|diagnostics|generated §15\.8\.[0-9]+)[ \t]*\Z"
)
CODE_EXEMPTION = re.compile(
    rf"({CODE_TEXT})[ \t]*\|[ \t]*(.+?)[ \t]*\|[ \t]*§(15\.12(?:\.[0-9]+)*)[ \t]*\Z"
)
CODE_EXEMPTION_TEST_SECTIONS = {
    "INTERNAL_COMPILER_ERROR": "15.12.3",
    "IMPLEMENTATION_LIMIT_EXCEEDED": "15.12.2",
    "SOURCE_READ_FAILED": "15.12.5",
}
LINT_TABLE_HEADING = "15.17.2"

FULL_DOCUMENTS = tuple(
    [f"spec/{number:02d}" for number in range(2, 17)]
    + [f"package/{number:02d}" for number in range(1, 5)]
    + ["impl/11", "impl/14"]
)
SCOPED_DOCUMENTS = {
    "spec/01": ("1.4.5", "1.5.4", "1.6"),
    "impl/12": ("12.5",),
    # §15.1.3 names the command interface and the records returned by those
    # commands. These live in §§13.3, 13.6, and 13.8 of the CLI chapter.
    "impl/13": ("13.3", "13.6", "13.8"),
}
EXPECTED_EXEMPT_SECTIONS = Path("tests/conformance/exempt-sections.txt")
EXPECTED_EXEMPT_CODES = Path("tests/conformance/exempt-codes.txt")
EXPECTED_REGISTRY = Path("crates/sai_diagnostics/src/diagnostic_code_names.txt")
EXPECTED_CONFORMANCE = Path("tests/conformance")
EXPECTED_REPORT = Path("target/conformance/coverage.json")


class CoverageError(ValueError):
    """A malformed fixture, specification inventory, or exemption list."""


def _read_regular_utf8(path: Path, description: str) -> str:
    try:
        info = path.lstat()
    except OSError:
        raise CoverageError(f"missing {description}: {path.as_posix()}") from None
    if not stat.S_ISREG(info.st_mode):
        raise CoverageError(f"{description} is not a regular file: {path.as_posix()}")
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        raise CoverageError(f"could not read UTF-8 {description}: {path.as_posix()}") from None


def _document_index(docs_root: Path) -> tuple[dict[str, Path], dict[Path, str]]:
    """Resolve directive aliases to canonical document keys."""
    aliases: dict[str, Path] = {}
    canonical: dict[Path, str] = {}
    for area in ("spec", "package", "impl"):
        directory = docs_root / area
        try:
            entries = sorted(directory.iterdir(), key=lambda path: path.name.encode("utf-8"))
        except OSError:
            raise CoverageError(f"missing specification area: docs/{area}") from None
        for path in entries:
            match = DOC_FILENAME.fullmatch(path.name)
            if match is None:
                continue
            number = match[1]
            key = f"{area}/{number}"
            if key in aliases:
                raise CoverageError(f"duplicate numbered document: {key}")
            _read_regular_utf8(path, "specification document")
            aliases[key] = path
            aliases[f"{area}/{path.stem}"] = path
            canonical[path.resolve()] = key
            if area == "impl":
                aliases[number] = path
                aliases[path.stem] = path
    if not canonical:
        raise CoverageError("no numbered specification documents found")
    return aliases, canonical


def _scan_headings(markdown: str, document: str) -> list[dict[str, object]]:
    """Return numbered headings outside fenced code blocks."""
    result = []
    fence_character: str | None = None
    fence_length = 0
    fence_list_content_indent: int | None = None
    list_content_indents: list[int] = []
    for line_number, line in enumerate(markdown.splitlines(), 1):
        if fence_character is not None:
            fence_match = _diagnostic_fence_parts(
                line, fence_list_content_indent
            )
            if fence_match is not None:
                marker, suffix = fence_match
                if (
                    marker[0] == fence_character
                    and len(marker) >= fence_length
                    and not suffix.strip()
                ):
                    fence_character = None
                    fence_length = 0
                    fence_list_content_indent = None
            continue
        if line.strip():
            line_indent = _leading_indentation_columns(line)
            while list_content_indents and line_indent < list_content_indents[-1]:
                list_content_indents.pop()
        active_list_indent = (
            list_content_indents[-1] if list_content_indents else None
        )
        fence_match = _diagnostic_fence_parts(line, active_list_indent)
        if fence_match is not None:
            marker, _ = fence_match
            fence_character = marker[0]
            fence_length = len(marker)
            fence_list_content_indent = active_list_indent
            continue
        item_content_indent = _list_item_content_indent(line)
        if item_content_indent is not None:
            list_content_indents.append(item_content_indent)
            list_item = LIST_ITEM_MARKER.match(line)
            assert list_item is not None
            item_content = line[list_item.end():]
            fence_match = (
                _diagnostic_fence_parts(item_content, 0)
                if item_content.strip()
                else None
            )
            if fence_match is not None:
                marker, _ = fence_match
                fence_character = marker[0]
                fence_length = len(marker)
                fence_list_content_indent = item_content_indent
                continue
        heading = HEADING.fullmatch(line)
        if heading is not None:
            result.append({
                "section": heading[2],
                "level": len(heading[1]),
                "line": line_number,
            })
    if fence_character is not None:
        raise CoverageError(f"unterminated fenced block in {document}")
    sections = [entry["section"] for entry in result]
    if len(sections) != len(set(sections)):
        raise CoverageError(f"duplicate numbered heading in {document}")
    return result


def _section_ids(headings: list[dict[str, object]]) -> set[str]:
    return {str(entry["section"]) for entry in headings}


def _leaf_sections(
    headings: list[dict[str, object]],
    roots: tuple[str, ...] | None,
) -> list[str]:
    sections = _section_ids(headings)
    if roots is None:
        selected = sections
    else:
        for root in roots:
            if root not in sections:
                raise CoverageError(f"covered scope section does not resolve: §{root}")
        selected = {
            section for section in sections
            if any(section == root or section.startswith(root + ".") for root in roots)
        }
    leaves = [
        section for section in selected
        if not any(
            candidate.startswith(section + ".")
            for candidate in selected
        )
    ]
    return sorted(leaves, key=lambda section: tuple(int(part) for part in section.split(".")))


def _build_inventory(
    docs_root: Path,
    aliases: dict[str, Path],
    canonical: dict[Path, str],
) -> tuple[dict[str, list[dict[str, object]]], dict[Path, list[dict[str, object]]]]:
    by_document: dict[str, list[dict[str, object]]] = {}
    headings_by_path: dict[Path, list[dict[str, object]]] = {}
    for path, document in sorted(canonical.items(), key=lambda item: item[1]):
        headings = _scan_headings(_read_regular_utf8(path, "specification document"), document)
        headings_by_path[path] = headings

    required_documents = set(FULL_DOCUMENTS) | set(SCOPED_DOCUMENTS)
    for document in sorted(required_documents):
        path = aliases.get(document)
        if path is None:
            raise CoverageError(f"covered document does not resolve: {document}")
        headings = headings_by_path[path.resolve()]
        roots = SCOPED_DOCUMENTS.get(document)
        leaves = _leaf_sections(headings, roots)
        by_document[document] = [
            {"document": document, "section": section,
             "id": f"{document} §{section}", "tests": [], "exemption": None}
            for section in leaves
        ]
    return by_document, headings_by_path


def _resolve_reference(
    reference: object,
    aliases: dict[str, Path],
    canonical: dict[Path, str],
    headings_by_path: dict[Path, list[dict[str, object]]],
) -> tuple[str, str]:
    if not isinstance(reference, dict):
        raise CoverageError("spec directive entry is not an object")
    document = reference.get("document")
    section = reference.get("section")
    if (not isinstance(document, str) or not isinstance(section, str)
            or not expectation_extractor.root_relative(document)
            or SECTION.fullmatch(section) is None):
        raise CoverageError("invalid spec section reference")
    path = aliases.get(document)
    if path is None:
        raise CoverageError(f"spec document does not resolve: {document}")
    path_key = path.resolve()
    canonical_document = canonical.get(path_key)
    if canonical_document is None:
        raise CoverageError(f"spec document has no canonical name: {document}")
    if section not in _section_ids(headings_by_path[path_key]):
        raise CoverageError(f"spec section does not resolve: {document} §{section}")
    return canonical_document, section


def _parse_section_exemptions(
    path: Path,
    inventory: dict[str, list[dict[str, object]]],
    aliases: dict[str, Path],
    canonical: dict[Path, str],
    headings_by_path: dict[Path, list[dict[str, object]]],
) -> dict[str, str]:
    text = _read_regular_utf8(path, "section exemption list")
    known = {
        entry["id"]: entry
        for entries in inventory.values()
        for entry in entries
    }
    result: dict[str, str] = {}
    for line_number, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        match = SECTION_EXEMPTION.fullmatch(line)
        if match is None:
            raise CoverageError(f"invalid section exemption at {path}:{line_number}")
        section_match = SPEC_REFERENCE.fullmatch(match[1].strip())
        if section_match is None:
            raise CoverageError(f"invalid section reference in {path}:{line_number}")
        document, section = _resolve_reference(
            {"document": section_match[1], "section": section_match[2]},
            aliases, canonical, headings_by_path,
        )
        identifier = f"{document} §{section}"
        if identifier not in known:
            raise CoverageError(f"section exemption is outside covered inventory: {identifier}")
        if identifier in result:
            raise CoverageError(f"duplicate section exemption: {identifier}")
        reason = match[2]
        generated = re.fullmatch(r"generated §([0-9]+(?:\.[0-9]+)*)", reason)
        if generated is not None:
            generated_document, generated_section = _resolve_reference(
                {"document": "impl/15", "section": generated[1]},
                aliases, canonical, headings_by_path,
            )
            if generated_document != "impl/15" or not generated_section.startswith("15.8."):
                raise CoverageError(
                    f"generated section exemption must cite impl/15 §15.8.x: {identifier}"
                )
        result[identifier] = reason
    return result


def _parse_code_exemptions(
    path: Path,
    registry: set[str],
    aliases: dict[str, Path],
    canonical: dict[Path, str],
    headings_by_path: dict[Path, list[dict[str, object]]],
) -> dict[str, dict[str, str]]:
    text = _read_regular_utf8(path, "diagnostic-code exemption list")
    result: dict[str, dict[str, str]] = {}
    for line_number, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        match = CODE_EXEMPTION.fullmatch(line)
        if match is None:
            raise CoverageError(f"invalid code exemption at {path}:{line_number}")
        code, reason, section = match.groups()
        if code not in registry:
            raise CoverageError(f"exemption names an unregistered diagnostic: {code}")
        expected_section = CODE_EXEMPTION_TEST_SECTIONS.get(code)
        if expected_section is None:
            raise CoverageError(f"diagnostic is not exemptible from portable coverage: {code}")
        if section != expected_section:
            raise CoverageError(
                f"{code} exemption must cite impl/15 §{expected_section}"
            )
        if code in result:
            raise CoverageError(f"duplicate code exemption: {code}")
        document, section = _resolve_reference(
            {"document": "impl/15", "section": section},
            aliases, canonical, headings_by_path,
        )
        if document != "impl/15" or not section.startswith("15.12"):
            raise CoverageError(f"code exemption must point to impl/15 §15.12: {code}")
        result[code] = {"reason": reason, "test_section": f"impl/15 §{section}"}
    return result


def _load_registry(path: Path) -> list[str]:
    text = _read_regular_utf8(path, "diagnostic code registry")
    codes = text.split()
    if not codes or any(CODE.fullmatch(code) is None for code in codes):
        raise CoverageError(f"invalid diagnostic code registry: {path.as_posix()}")
    if codes != sorted(set(codes)):
        raise CoverageError(f"diagnostic code registry must be unique and sorted: {path.as_posix()}")
    return codes


def _markdown_table_cells(line: str) -> list[str] | None:
    """Split one Markdown table row without splitting escaped or code-span pipes."""
    text = line.strip()
    cells: list[str] = []
    start = 0
    code_delimiter = 0
    index = 0
    while index < len(text):
        character = text[index]
        if code_delimiter == 0 and character == "\\" and index + 1 < len(text):
            index += 2
            continue
        if character == "`":
            end = index + 1
            while end < len(text) and text[end] == "`":
                end += 1
            delimiter_length = end - index
            if code_delimiter == 0:
                if _has_matching_backtick_run(text, end, delimiter_length):
                    code_delimiter = delimiter_length
            elif delimiter_length == code_delimiter:
                code_delimiter = 0
            index = end
            continue
        if character == "|" and code_delimiter == 0:
            cells.append(text[start:index].strip())
            start = index + 1
        index += 1

    if not cells:
        return None
    cells.append(text[start:].strip())
    if text.startswith("|"):
        cells.pop(0)
    if text.endswith("|"):
        preceding = len(text) - 2
        backslashes = 0
        while preceding >= 0 and text[preceding] == "\\":
            backslashes += 1
            preceding -= 1
        if backslashes % 2 == 0:
            cells.pop()
    return cells


def _has_matching_backtick_run(text: str, start: int, delimiter_length: int) -> bool:
    """Return whether a backtick run has an equal-length closer on this line."""
    index = start
    while index < len(text):
        if text[index] != "`":
            index += 1
            continue
        end = index + 1
        while end < len(text) and text[end] == "`":
            end += 1
        if end - index == delimiter_length:
            return True
        index = end
    return False


def _leading_indentation_columns(line: str) -> int:
    indentation = re.match(r"[ \t]*", line)
    return len(indentation[0].expandtabs(4)) if indentation is not None else 0


def _list_item_content_indent(line: str) -> int | None:
    """Return the content column for conventional space-separated list markers."""
    marker = LIST_ITEM_MARKER.match(line)
    if marker is None:
        return None
    prefix = marker[1] + marker[2] + marker[3]
    return len(prefix.expandtabs(4))


def _is_indented_code_line(line: str, list_content_indent: int | None = None) -> bool:
    """Recognize indented code relative to the active list item, when present."""
    minimum_indent = 4 if list_content_indent is None else list_content_indent + 4
    return _leading_indentation_columns(line) >= minimum_indent


def _diagnostic_fence_parts(
    line: str, list_content_indent: int | None = None
) -> tuple[str, str] | None:
    """Return a fence marker and its suffix at top level or in a list item."""
    match = DIAGNOSTIC_FENCE.fullmatch(line)
    if match is None:
        return None
    indentation = len(match[1].expandtabs(4))
    if list_content_indent is None:
        if indentation > 3:
            return None
    elif not list_content_indent <= indentation <= list_content_indent + 3:
        return None
    marker, suffix = match[2], match[3]
    if marker.startswith("`") and "`" in suffix:
        return None
    return marker, suffix


def _is_table_separator(cells: list[str]) -> bool:
    return len(cells) == 3 and all(
        re.fullmatch(r":?-{3,}:?", cell) is not None for cell in cells
    )


def _scan_diagnostic_tables(
    canonical: dict[Path, str],
    headings_by_path: dict[Path, list[dict[str, object]]],
    registry: set[str],
) -> list[dict[str, object]]:
    """Inventory exact diagnostic tables outside fenced and indented code.

    List indentation follows conventional space-separated markers; this is not
    a full CommonMark container parser and does not resolve lazy continuations.
    """
    records: list[dict[str, object]] = []
    for path, document in sorted(canonical.items(), key=lambda item: item[1]):
        text = _read_regular_utf8(path, "specification document")
        lines = text.splitlines()
        headings = headings_by_path[path]
        headings_by_line = {
            int(heading["line"]): str(heading["section"])
            for heading in headings
        }
        section: str | None = None
        fence_character: str | None = None
        fence_length = 0
        fence_list_content_indent: int | None = None
        list_content_indents: list[int] = []
        line_index = 0
        while line_index < len(lines):
            line = lines[line_index]
            line_number = line_index + 1
            heading_section = headings_by_line.get(line_number)
            if heading_section is not None:
                section = heading_section

            if fence_character is None and line.strip():
                line_indent = _leading_indentation_columns(line)
                while list_content_indents and line_indent < list_content_indents[-1]:
                    list_content_indents.pop()
            active_list_indent = (
                list_content_indents[-1] if list_content_indents else None
            )
            if fence_character is not None:
                fence_match = _diagnostic_fence_parts(line, fence_list_content_indent)
                if fence_match is not None:
                    marker, suffix = fence_match
                    if (
                        marker[0] == fence_character
                        and len(marker) >= fence_length
                        and not suffix.strip()
                    ):
                        fence_character = None
                        fence_length = 0
                        fence_list_content_indent = None
                line_index += 1
                continue
            fence_match = _diagnostic_fence_parts(line, active_list_indent)
            if fence_match is not None:
                marker, _ = fence_match
                fence_character = marker[0]
                fence_length = len(marker)
                fence_list_content_indent = active_list_indent
                line_index += 1
                continue
            if _is_indented_code_line(line, active_list_indent):
                line_index += 1
                continue
            list_item = LIST_ITEM_MARKER.match(line)
            item_content_indent = _list_item_content_indent(line)
            table_line = line
            if item_content_indent is not None:
                list_content_indents.append(item_content_indent)
                active_list_indent = item_content_indent
                assert list_item is not None
                table_line = line[list_item.end():]
                fence_match = (
                    _diagnostic_fence_parts(table_line, 0)
                    if table_line.strip()
                    else None
                )
                fence_container_indent = item_content_indent
            else:
                fence_match = _diagnostic_fence_parts(line, active_list_indent)
                fence_container_indent = active_list_indent
            if fence_match is not None:
                marker, _ = fence_match
                fence_character = marker[0]
                fence_length = len(marker)
                fence_list_content_indent = fence_container_indent
                line_index += 1
                continue
            active_list_indent = (
                list_content_indents[-1] if list_content_indents else None
            )

            cells = _markdown_table_cells(table_line)
            if cells != ["Code", "Severity", "Description"]:
                line_index += 1
                continue

            table_location = f"{document} §{section or '?'}:{line_number}"
            if section is None:
                raise CoverageError(
                    f"{table_location} diagnostic table is outside a numbered section"
                )
            if line_index + 1 >= len(lines):
                raise CoverageError(f"{table_location} diagnostic table has no separator")
            if _is_indented_code_line(lines[line_index + 1], active_list_indent):
                line_index += 1
                continue
            separator = _markdown_table_cells(lines[line_index + 1])
            if separator is None or not _is_table_separator(separator):
                raise CoverageError(f"{table_location} diagnostic table has an invalid separator")

            table_rows: dict[tuple[str, str, str], int] = {}
            row_index = line_index + 2
            while row_index < len(lines):
                if _is_indented_code_line(lines[row_index], active_list_indent):
                    break
                row_cells = _markdown_table_cells(lines[row_index])
                if row_cells is None:
                    break
                row_line = row_index + 1
                row_location = f"{document} §{section}:{row_line}"
                if len(row_cells) != 3:
                    raise CoverageError(
                        f"{row_location} diagnostic table row must have exactly 3 columns"
                    )
                code_cell = row_cells[0]
                codes = [
                    match[1]
                    for match in re.finditer(rf"`({CODE_TEXT})`", code_cell)
                ]
                code_list = ", ".join(f"`{code}`" for code in codes)
                if (not codes or (
                    code_cell != code_list
                    and not any(
                        code_cell == code_list + suffix
                        for suffix in DIAGNOSTIC_CODE_CELL_SUFFIXES
                    )
                )):
                    raise CoverageError(f"{row_location} has an invalid diagnostic code")
                severity = row_cells[1]
                description = row_cells[2]
                if severity not in DIAGNOSTIC_SEVERITIES:
                    raise CoverageError(
                        f"{row_location} has invalid diagnostic severity: {severity}"
                    )
                if not description:
                    raise CoverageError(f"{row_location} has an empty diagnostic description")
                for code in codes:
                    row_identity = (code, severity, description)
                    if row_identity in table_rows:
                        raise CoverageError(
                            f"{row_location} duplicates {code} diagnostic row "
                            f"(first listed at line {table_rows[row_identity]})"
                        )
                    table_rows[row_identity] = row_line
                    if code not in registry:
                        raise CoverageError(
                            f"{row_location} names unregistered diagnostic: {code}"
                        )
                    records.append({
                        "document": document,
                        "section": section,
                        "line": row_line,
                        "code": code,
                        "severity": severity,
                        "description": description,
                    })
                row_index += 1
            if not table_rows:
                raise CoverageError(
                    f"{table_location} diagnostic table has no data rows"
                )
            line_index = row_index

        if fence_character is not None:
            raise CoverageError(f"unterminated fenced block in {document}")

    return sorted(
        records,
        key=lambda record: (
            str(record["document"]),
            tuple(int(part) for part in str(record["section"]).split(".")),
            int(record["line"]),
            str(record["code"]),
        ),
    )


def _parse_lint_codes(
    doc_path: Path,
    headings: list[dict[str, object]],
    registry: set[str],
) -> set[str]:
    heading = next(
        (entry for entry in headings if entry["section"] == LINT_TABLE_HEADING),
        None,
    )
    if heading is None:
        raise CoverageError("spec/15 §15.17.2 does not resolve")
    lines = _read_regular_utf8(doc_path, "lint registry specification").splitlines()
    start = int(heading["line"])
    end = len(lines)
    for candidate in headings:
        if int(candidate["line"]) > start and int(candidate["level"]) <= int(heading["level"]):
            end = int(candidate["line"]) - 1
            break

    table_lines = []
    in_table = False
    for line in lines[start:end]:
        if line.lstrip().startswith("|"):
            in_table = True
            table_lines.append(line)
        elif in_table:
            break
    if len(table_lines) < 3:
        raise CoverageError("could not extract lint table from spec/15 §15.17.2")

    header = [cell.strip() for cell in table_lines[0].strip().strip("|").split("|")]
    separator = [cell.strip() for cell in table_lines[1].strip().strip("|").split("|")]
    if not header or header[0] != "Code" or not separator or not re.fullmatch(r":?-{3,}:?", separator[0]):
        raise CoverageError("spec/15 §15.17.2 has an invalid lint table header")

    codes: list[str] = []
    for line in table_lines[2:]:
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if not cells:
            continue
        match = re.fullmatch(r"`([^`]+)`", cells[0])
        if match is None or CODE.fullmatch(match[1]) is None:
            raise CoverageError("spec/15 §15.17.2 has an invalid lint code")
        codes.append(match[1])
    if not codes or len(codes) != len(set(codes)):
        raise CoverageError("spec/15 §15.17.2 must list unique lint codes")
    unknown = sorted(set(codes) - registry)
    if unknown:
        raise CoverageError(
            "spec/15 §15.17.2 names unregistered diagnostic(s): " + ", ".join(unknown)
        )
    return set(codes)


def _diagnostic_json_golden_paths(
    root: Path,
    fixture: dict[str, str],
    directives: dict[str, object],
) -> list[Path]:
    fixture_path = root / EXPECTED_CONFORMANCE / fixture["path"].removeprefix("conformance/")
    if fixture["kind"] == "single-file":
        root_file = root / EXPECTED_CONFORMANCE / fixture["root_file"].removeprefix("conformance/")
        base = root_file.with_suffix("")
    else:
        base = fixture_path / "expected" / "diagnostics"

    # Formatting commands do not select a target or profile. Stage 0 check
    # tests are also targetless when their directives leave both unrestricted;
    # higher-stage claims may additionally select their permitted matrix.
    configurations = []
    if directives["kind"] in ("fmt", "fmt-refuse"):
        configurations.append((None, None))
    elif (
        directives["stage"] == 0
        and directives["kind"] in ("check-pass", "check-fail")
        and not {"targets", "profiles"} & set(directives["specified"])
    ):
        configurations.append((None, None))
        configurations.extend(
            (target, profile)
            for target in sorted(directives["targets"])
            for profile in sorted(directives["profiles"])
        )
    else:
        configurations.extend(
            (target, profile)
            for target in sorted(directives["targets"])
            for profile in sorted(directives["profiles"])
        )

    selected = set()
    ordinary = base.with_name(f"{base.name}.json")
    for target, profile in configurations:
        candidates = []
        if target is not None and profile is not None:
            candidates.append(base.with_name(f"{base.name}.{target}.{profile}.json"))
        if profile is not None:
            candidates.append(base.with_name(f"{base.name}.{profile}.json"))
        if target is not None:
            candidates.append(base.with_name(f"{base.name}.{target}.json"))
        candidates.append(ordinary)
        for candidate in candidates:
            if candidate.exists():
                selected.add(candidate)
                break
    return sorted(selected, key=lambda path: path.relative_to(root).as_posix())


def _codes_in_diagnostic_json_golden(path: Path, registry: set[str]) -> set[str]:
    try:
        info = path.lstat()
    except OSError:
        raise CoverageError(f"missing diagnostic JSON golden: {path.as_posix()}") from None
    if not stat.S_ISREG(info.st_mode):
        raise CoverageError(f"diagnostic JSON golden is not a regular file: {path.as_posix()}")
    try:
        data = path.read_bytes()
        records = ndjson_stream.validate_stream(data.splitlines(keepends=True))
    except (OSError, ndjson_stream.StreamError) as error:
        raise CoverageError(
            f"invalid diagnostic JSON golden {path.as_posix()}: {error}"
        ) from None
    codes = {record["code"] for record in records if record["type"] == "diagnostic"}
    unknown = sorted(codes - registry)
    if unknown:
        raise CoverageError(
            f"{path.as_posix()} names unregistered diagnostic(s): {', '.join(unknown)}"
        )
    return codes


def _fixture_source_files(fixture_root: Path) -> list[Path]:
    """List package sources, excluding expected outputs and WIT/MIR sidecars."""
    result = []
    for directory, names, files in os.walk(fixture_root):
        names[:] = sorted(
            (name for name in names if name not in {"expected"}
             and not name.endswith((".wit", ".mir"))),
            key=lambda name: os.fsencode(name),
        )
        for name in sorted(files, key=os.fsencode):
            if (name.endswith(".sai")
                    and not name.endswith(fixture_discovery.SOURCE_FILE_SIDECARS)):
                result.append(Path(directory) / name)
    return sorted(result, key=lambda path: path.relative_to(fixture_root).as_posix().encode("utf-8"))


def _codes_for_fixture(
    root: Path,
    fixture: dict[str, str],
    directives: dict[str, object],
    expected: list[dict[str, object]],
    registry: set[str],
) -> list[str]:
    fixture_path = root / EXPECTED_CONFORMANCE / fixture["path"].removeprefix("conformance/")
    codes = {entry["code"] for entry in expected}
    if fixture["kind"] == "directory":
        # Directory-test directives belong only to sai.package. Diagnostic
        # annotations in source files are still expectations of this fixture.
        for source_path in _fixture_source_files(fixture_path):
            relative = source_path.relative_to(fixture_path).as_posix()
            try:
                source_expected = expectation_extractor.extract_bytes(
                    source_path.read_bytes(), relative, lint_levels={code: "info" for code in registry}
                )
            except (OSError, UnicodeError, ValueError) as error:
                raise CoverageError(f"{fixture['path']}: {error}") from None
            codes.update(entry["code"] for entry in source_expected)
    unknown = sorted(codes - registry)
    if unknown:
        raise CoverageError(f"{fixture['path']} expects unregistered code(s): {', '.join(unknown)}")
    return sorted(codes)


def _parse_panic_kinds(
    doc_path: Path,
    headings: list[dict[str, object]],
) -> list[str]:
    panic_heading = next(
        (entry for entry in headings if entry["section"] == "10.4.4"),
        None,
    )
    if panic_heading is None:
        raise CoverageError("spec/10 §10.4.4 does not resolve")
    lines = _read_regular_utf8(doc_path, "panic-kind specification").splitlines()
    start = int(panic_heading["line"])
    end = len(lines)
    for heading in headings:
        if int(heading["line"]) > start and int(heading["level"]) <= int(panic_heading["level"]):
            end = int(heading["line"]) - 1
            break
    kinds = []
    for line in lines[start:end]:
        if not line.lstrip().startswith("|"):
            continue
        cells = line.strip().strip("|").split("|")
        if not cells:
            continue
        for cell in PANIC_KIND.findall(cells[0]):
            kinds.extend(part.strip() for part in cell.split(","))
    if not kinds or len(kinds) != len(set(kinds)):
        raise CoverageError("could not extract a unique panic-kind table from spec/10 §10.4.4")
    return kinds


def build_report(root: Path) -> dict[str, object]:
    root = root.resolve(strict=True)
    docs_root = root / "docs"
    aliases, canonical = _document_index(docs_root)
    inventory, headings_by_path = _build_inventory(docs_root, aliases, canonical)
    registry_codes = _load_registry(root / EXPECTED_REGISTRY)
    diagnostic_tables = _scan_diagnostic_tables(
        canonical, headings_by_path, set(registry_codes)
    )
    section_exemptions = _parse_section_exemptions(
        root / EXPECTED_EXEMPT_SECTIONS, inventory, aliases, canonical, headings_by_path
    )
    code_exemptions = _parse_code_exemptions(
        root / EXPECTED_EXEMPT_CODES, set(registry_codes), aliases, canonical, headings_by_path
    )
    for entries in inventory.values():
        for entry in entries:
            entry["exemption"] = section_exemptions.get(str(entry["id"]))

    conformance_root = root / EXPECTED_CONFORMANCE
    try:
        fixtures = fixture_discovery.discover(conformance_root)
    except (fixture_discovery.DiscoveryError, ValueError) as error:
        raise CoverageError(f"fixture discovery failed: {error}") from None
    if not fixtures:
        raise CoverageError("conformance fixture set is empty")

    # The report covers diagnostic identity, not severity. Supplying a neutral
    # lint level lets the shared extractor validate syntax while the conformance
    # runner remains responsible for checking actual lint levels and outcomes.
    neutral_lint_levels = {code: "info" for code in registry_codes}
    lint_doc = aliases.get("spec/15")
    if lint_doc is None:
        raise CoverageError("covered lint document spec/15 does not resolve")
    lint_codes = _parse_lint_codes(
        lint_doc, headings_by_path[lint_doc.resolve()], set(registry_codes)
    )
    hard_codes = set(registry_codes) - lint_codes
    all_sections = {
        entry["id"]: entry
        for entries in inventory.values()
        for entry in entries
    }
    section_tests: dict[str, set[str]] = {identifier: set() for identifier in all_sections}
    code_tests: dict[str, set[str]] = {code: set() for code in registry_codes}
    hard_code_json_goldens: dict[str, set[str]] = {code: set() for code in hard_codes}
    test_records = []

    for fixture in fixtures:
        fixture_relative = fixture["path"].removeprefix("conformance/")
        report_path = f"tests/conformance/{fixture_relative}"
        root_file = (
            root / EXPECTED_CONFORMANCE / fixture["root_file"].removeprefix("conformance/")
        )
        extractor_path = "src/lib.sai" if fixture["kind"] == "single-file" else "sai.package"
        try:
            extracted = expectation_extractor.extract_bytes_with_directives(
                root_file.read_bytes(), extractor_path, lint_levels=neutral_lint_levels
            )
        except (OSError, UnicodeError, ValueError) as error:
            raise CoverageError(f"{report_path}: {error}") from None
        directives = extracted["directives"]
        resolved_specs = []
        for reference in directives["spec"]:
            document, section = _resolve_reference(
                reference, aliases, canonical, headings_by_path
            )
            resolved_specs.append(f"{document} §{section}")
        if len(resolved_specs) != len(set(resolved_specs)):
            raise CoverageError(f"{report_path} repeats a spec section reference")
        codes = _codes_for_fixture(
            root, fixture, directives, extracted["expected"], set(registry_codes)
        )
        golden_records = []
        for golden_path in _diagnostic_json_golden_paths(root, fixture, directives):
            golden_codes = _codes_in_diagnostic_json_golden(
                golden_path, set(registry_codes)
            )
            relative_golden_path = golden_path.relative_to(root).as_posix()
            golden_records.append({
                "path": relative_golden_path,
                "diagnostic_codes": sorted(golden_codes),
            })
            for code in golden_codes & hard_codes:
                hard_code_json_goldens[code].add(relative_golden_path)
        portable = directives["reference_only"] is None
        if portable:
            for identifier in resolved_specs:
                if identifier in section_tests:
                    section_tests[identifier].add(report_path)
            for code in codes:
                if code not in code_exemptions:
                    code_tests[code].add(report_path)
        test_records.append({
            "path": report_path,
            "kind": directives["kind"],
            "stage": directives["stage"],
            "portable": portable,
            "reference_only_reason": directives["reference_only"],
            "spec": resolved_specs,
            "diagnostic_codes": codes,
            "diagnostic_json_goldens": golden_records,
        })

    for identifier, entry in all_sections.items():
        entry["tests"] = sorted(section_tests[identifier])
    section_records = [
        entry for document in sorted(inventory)
        for entry in inventory[document]
    ]
    code_records = [
        {
            "code": code,
            "tests": sorted(code_tests[code]),
            "exemption": code_exemptions.get(code),
        }
        for code in registry_codes
    ]
    panic_doc = aliases.get("spec/10")
    if panic_doc is None:
        raise CoverageError("covered panic-kind document spec/10 does not resolve")
    panic_kinds = _parse_panic_kinds(panic_doc, headings_by_path[panic_doc.resolve()])
    panic_records = [{"kind": kind, "tests": []} for kind in panic_kinds]
    hard_code_json_records = [
        {"code": code, "goldens": sorted(hard_code_json_goldens[code])}
        for code in sorted(hard_codes)
    ]

    uncovered = {
        "sections": [
            entry["id"] for entry in section_records
            if not entry["tests"] and entry["exemption"] is None
        ],
        "codes": [
            entry["code"] for entry in code_records
            if not entry["tests"] and entry["exemption"] is None
        ],
        "panic_kinds": [entry["kind"] for entry in panic_records if not entry["tests"]],
        "hard_code_json_goldens": [
            entry["code"] for entry in hard_code_json_records if not entry["goldens"]
        ],
    }
    return {
        "schema_version": 1,
        "status": "report-only",
        "claim": False,
        "stage_gate_enabled": False,
        "stage_gate_note": (
            "The repository does not yet declare implemented stages; uncovered "
            "lists report the full covered inventory and are not a merge gate."
        ),
        "fixture_scope_note": (
            "This initial inventory reads tests/conformance. Generated tests/spec "
            "are not included; their generator and inventory are not implemented."
        ),
        "panic_kind_attribution": (
            "The current run-panic marker identifies a location, not a kind, and "
            "panic reports contain no kind field; no kind is inferred."
        ),
        "fixtures": test_records,
        "sections": section_records,
        "diagnostic_tables": diagnostic_tables,
        "diagnostic_codes": code_records,
        "hard_code_json_goldens": hard_code_json_records,
        "panic_kinds": panic_records,
        "uncovered": uncovered,
    }


def _canonical_json(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
            + "\n").encode("utf-8")


def write_report(root: Path, output: Path) -> None:
    report = build_report(root)
    destination = output if output.is_absolute() else root / output
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(destination.name + ".tmp")
    try:
        temporary.write_bytes(_canonical_json(report))
        os.replace(temporary, destination)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, default=EXPECTED_REPORT)
    args = parser.parse_args()
    try:
        write_report(args.root, args.output)
    except (CoverageError, OSError, RuntimeError, ValueError) as error:
        print(f"coverage report failed: {ascii(str(error))}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
