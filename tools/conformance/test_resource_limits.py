import tempfile
import unittest
from pathlib import Path

from resource_limits import (
    LimitTableError,
    byte_minimum_for,
    parse_implementation_limits,
    read_implementation_limits,
    source_file_minimum_bytes,
)


class ResourceLimitTests(unittest.TestCase):
    def test_repository_limit_table_has_all_twelve_numeric_rows(self):
        path = Path(__file__).resolve().parents[2] / "docs/spec/01-introduction.md"
        limits = read_implementation_limits(path)

        self.assertEqual(
            [limit.name for limit in limits],
            [
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
            ],
        )
        self.assertEqual(
            [limit.minimum for limit in limits],
            [
                16 * 1024 * 1024,
                1024,
                16 * 1024 * 1024,
                256,
                255,
                64,
                4096,
                64,
                65535,
                65535,
                128,
                4096,
            ],
        )
        self.assertEqual(source_file_minimum_bytes(limits), 16 * 1024 * 1024)

    def test_parser_rejects_duplicate_names_and_malformed_rows(self):
        prefix = "### 1.5.4 Implementation limits\n\n| Limit | Minimum |\n|---|---|\n"
        with self.subTest("duplicate"):
            with self.assertRaisesRegex(LimitTableError, "duplicate"):
                parse_implementation_limits(
                    prefix + "| size of a source file | 16 MiB |\n"
                    "| size of a source file | 32 MiB |\n"
                )
        with self.subTest("invalid number"):
            with self.assertRaisesRegex(LimitTableError, "invalid minimum"):
                parse_implementation_limits(prefix + "| size of a source file | sixteen MiB |\n")
        with self.subTest("column count"):
            with self.assertRaisesRegex(LimitTableError, "two nonempty cells"):
                parse_implementation_limits(prefix + "| size of a source file | 16 MiB | extra |\n")

    def test_parser_requires_one_section_table_and_source_size_row(self):
        with self.assertRaisesRegex(LimitTableError, "expected one"):
            parse_implementation_limits("### 1.5.3 Other limits\n| Limit | Minimum |\n|---|---|\n")
        limits = parse_implementation_limits(
            "### 1.5.4 Implementation limits\n"
            "| Limit | Minimum |\n|---|---|\n"
            "| nesting | 1,024 |\n"
        )
        with self.assertRaisesRegex(LimitTableError, "byte-valued minimum"):
            source_file_minimum_bytes(limits)

    def test_byte_minimum_lookup_requires_one_matching_byte_valued_row(self):
        limits = parse_implementation_limits(
            "### 1.5.4 Implementation limits\n"
            "| Limit | Minimum |\n|---|---|\n"
            "| length of an identifier | 1,024 bytes |\n"
        )

        self.assertEqual(
            byte_minimum_for(limits, "length of an identifier"),
            1024,
        )
        with self.assertRaisesRegex(LimitTableError, "byte-valued minimum"):
            byte_minimum_for(limits, "not a limit")
        with self.assertRaisesRegex(LimitTableError, "byte-valued minimum"):
            byte_minimum_for(
                parse_implementation_limits(
                    "### 1.5.4 Implementation limits\n"
                    "| Limit | Minimum |\n|---|---|\n"
                    "| length of an identifier | 1,024 |\n"
                ),
                "length of an identifier",
            )

    def test_file_reader_rejects_a_truncated_implementation_limit_table(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "01-introduction.md"
            path.write_text(
                "### 1.5.4 Implementation limits\n\n"
                "| Limit | Minimum |\n|---|---|\n"
                "| size of a source file | 16 MiB |\n",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(LimitTableError, "row set changed"):
                read_implementation_limits(path)


if __name__ == "__main__":
    unittest.main()
