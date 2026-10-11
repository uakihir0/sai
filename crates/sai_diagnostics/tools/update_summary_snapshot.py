#!/usr/bin/env python3
"""Regenerate diagnostic_code_summaries.txt from the normative Appendix A."""

from pathlib import Path


def read_rows(appendix: Path) -> list[tuple[str, str, int, str]]:
    rows = []
    for line in appendix.read_text(encoding="utf-8").splitlines():
        if not line.startswith("| `"):
            continue
        code, remainder = line[3:].split("` |", 1)
        severity, remainder = remainder.strip().split("|", 1)
        _, summary = remainder.split("|", 1)
        summary = summary.strip().removesuffix("|").strip()

        if severity.strip() in {"warning (lint)", "info (lint)"}:
            category = "lint"
        elif severity.strip() in {"error", "warning", "info"}:
            category = "hard"
        else:
            raise ValueError(f"unknown severity for {code}: {severity}")
        if not summary or "\n" in summary:
            raise ValueError(f"missing or multiline summary for {code}")
        rows.append((code, category, len(rows), summary))

    rows.sort(key=lambda row: row[0].encode("ascii"))
    if len({row[0] for row in rows}) != len(rows):
        raise ValueError("Appendix A repeats a diagnostic code")
    return rows


def main() -> None:
    repository = Path(__file__).resolve().parents[3]
    appendix = repository / "docs/spec/appendix-a-diagnostic-codes.md"
    output = repository / "crates/sai_diagnostics/src/diagnostic_code_summaries.txt"
    rows = read_rows(appendix)
    output.write_text(
        "".join(
            f"{code}|{category}|{order}|{summary}\n"
            for code, category, order, summary in rows
        ),
        encoding="utf-8",
        newline="\n",
    )
    print(f"wrote {len(rows)} diagnostic summaries to {output}")


if __name__ == "__main__":
    main()
