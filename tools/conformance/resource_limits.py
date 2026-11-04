#!/usr/bin/env python3
"""Read numeric implementation limits from spec/01 §1.5.4."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re


HEADING = re.compile(r"^(#{1,6})[ \t]+1\.5\.4(?:[ \t]+.*)?[ \t]*$")
TABLE_ROW = re.compile(r"^\|(.+)\|[ \t]*$")
MINIMUM = re.compile(
    r"(?P<value>[1-9][0-9]{0,2}(?:,[0-9]{3})*|[1-9][0-9]*)(?:[ \t]+(?P<unit>bytes|KiB|MiB))?"
)
UNIT_MULTIPLIERS = {"bytes": 1, "KiB": 1024, "MiB": 1024 * 1024}
EXPECTED_LIMIT_NAMES = frozenset(
    {
        "size of a source file",
        "length of an identifier",
        "length of a string or byte-string literal",
        "nesting depth of expressions, blocks, patterns, and types (combined)",
        "parameters of one function",
        "generic parameters of one item",
        "fields of one struct, or variants of one enum",
        "elements of one tuple type",
        "statements in one block",
        "modules in one package",
        "depth of nested monomorphization (an instance requiring another instance)",
        "packages in one resolved graph",
    }
)


class LimitTableError(ValueError):
    """The implementation-limit table is missing or malformed."""


@dataclass(frozen=True)
class ResourceLimit:
    name: str
    minimum: int
    unit: str | None
    source_minimum: str


def _cells(line: str) -> list[str] | None:
    match = TABLE_ROW.fullmatch(line.strip())
    if match is None:
        return None
    return [cell.strip() for cell in match[1].split("|")]


def parse_implementation_limits(markdown: str) -> tuple[ResourceLimit, ...]:
    """Parse and validate the two-column table under §1.5.4."""
    if not isinstance(markdown, str):
        raise LimitTableError("implementation-limit document must be text")

    lines = markdown.splitlines()
    headings = [
        (index, len(match[1]))
        for index, line in enumerate(lines)
        if (match := HEADING.fullmatch(line))
    ]
    if len(headings) != 1:
        raise LimitTableError("expected one §1.5.4 implementation-limits heading")

    heading_index, heading_level = headings[0]
    section_end = len(lines)
    for index in range(heading_index + 1, len(lines)):
        line = lines[index]
        if line.startswith("#"):
            level = len(line) - len(line.lstrip("#"))
            if 1 <= level <= 6 and level <= heading_level:
                section_end = index
                break

    table_headers = [
        index
        for index in range(heading_index + 1, section_end)
        if _cells(lines[index]) == ["Limit", "Minimum"]
    ]
    if len(table_headers) != 1:
        raise LimitTableError("expected one Limit | Minimum table in §1.5.4")

    table_index = table_headers[0]
    if table_index + 1 >= section_end:
        raise LimitTableError("implementation-limit table has no separator")
    separator = _cells(lines[table_index + 1])
    if separator is None or len(separator) != 2 or any(
        not re.fullmatch(r":?-{3,}:?", cell) for cell in separator
    ):
        raise LimitTableError("implementation-limit table has an invalid separator")

    limits = []
    seen_names = set()
    for line in lines[table_index + 2:section_end]:
        cells = _cells(line)
        if cells is None:
            break
        if len(cells) != 2 or not all(cells):
            raise LimitTableError("implementation-limit rows need two nonempty cells")
        name, source_minimum = cells
        if name in seen_names:
            raise LimitTableError(f"duplicate implementation limit: {name}")
        seen_names.add(name)
        match = MINIMUM.fullmatch(source_minimum)
        if match is None:
            raise LimitTableError(f"invalid minimum for implementation limit {name}")
        value = int(match["value"].replace(",", ""))
        unit = match["unit"]
        minimum = value * UNIT_MULTIPLIERS.get(unit, 1)
        limits.append(ResourceLimit(name, minimum, unit, source_minimum))

    if not limits:
        raise LimitTableError("implementation-limit table has no data rows")
    return tuple(limits)


def read_implementation_limits(path: Path) -> tuple[ResourceLimit, ...]:
    """Read and parse the specification table from a UTF-8 file."""
    try:
        markdown = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        raise LimitTableError(f"could not read implementation-limit document: {path}") from None
    limits = parse_implementation_limits(markdown)
    names = {limit.name for limit in limits}
    if names != EXPECTED_LIMIT_NAMES:
        missing = sorted(EXPECTED_LIMIT_NAMES - names)
        unexpected = sorted(names - EXPECTED_LIMIT_NAMES)
        details = []
        if missing:
            details.append(f"missing rows: {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected rows: {', '.join(unexpected)}")
        raise LimitTableError(
            "§1.5.4 implementation-limit table row set changed; "
            + "; ".join(details)
        )
    return limits


def source_file_minimum_bytes(limits: tuple[ResourceLimit, ...]) -> int:
    """Return the source-file minimum, requiring a byte-valued table entry."""
    matches = [limit for limit in limits if limit.name == "size of a source file"]
    if len(matches) != 1 or matches[0].unit not in UNIT_MULTIPLIERS:
        raise LimitTableError("§1.5.4 must define one byte-valued source-file minimum")
    return matches[0].minimum
