import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from diagnostic_matcher import compare
from expectation_extractor import (
    extract,
    extract_bytes,
    extract_bytes_with_directives,
    extract_with_directives,
)


def expected(line, severity, code, file="src/lib.sai"):
    return {"file": file, "line": line, "severity": severity, "code": code}


class ExpectationExtractorTests(unittest.TestCase):
    def test_complete_directive_metadata_is_normalized(self):
        source = r"""//@ kind: run-pass
//@ stage: 1
//@ spec: spec/06 §6.3, impl/15 §15.2.3, targets/targets-and-profiles §1.4
//@ targets: wasm32-core wasm64-core
//@ profiles: dev release
//@ options: --tests --allow UNUSED_VARIABLE --wasm-feature tail-call --wasm-feature simd128
//@ component:
//@ call: add(i32:1, f32:0x3f800000) -> i32:2
//@ call: acme:image/codec#encode("a,b", (true, 2)) -> ("x", "y")
//@ args: "hello world" "" "escaped\nline"
//@ seed: 42
//@ diag: error EXPECTED_TOKEN
//@ diag: warning UNUSED_VARIABLE at sub/pkg.sai:3
//@ fuel: 1234
//@ memory-limit: 64
//@ stderr-fails: first-write
//@ reference-only: relies on a reference implementation limit
fn main() {}
"""
        result = extract_with_directives(source, "test.sai")
        self.assertEqual(result["expected"], [
            expected(None, "error", "EXPECTED_TOKEN", None),
            expected(3, "warning", "UNUSED_VARIABLE", "sub/pkg.sai"),
        ])
        self.assertEqual(result["directives"], {
            "specified": [
                "args", "call", "component", "diag", "fuel", "kind", "memory-limit",
                "options", "profiles", "reference-only", "seed", "spec", "stage",
                "stderr-fails", "targets",
            ],
            "kind": "run-pass",
            "stage": 1,
            "spec": [
                {"document": "spec/06", "section": "6.3"},
                {"document": "impl/15", "section": "15.2.3"},
                {"document": "targets/targets-and-profiles", "section": "1.4"},
            ],
            "targets": ["wasm32-core", "wasm64-core"],
            "profiles": ["dev", "release"],
            "options": [
                "--tests", "--allow", "UNUSED_VARIABLE",
                "--wasm-feature", "tail-call", "--wasm-feature", "simd128",
            ],
            "component": True,
            "call": [
                {"export": "add", "arguments": ["i32:1", "f32:0x3f800000"], "result": "i32:2"},
                {"export": "acme:image/codec#encode",
                 "arguments": ['"a,b"', "(true, 2)"], "result": '("x", "y")'},
            ],
            "args": ["hello world", "", "escaped\nline"],
            "seed": 42,
            "diag": [
                expected(None, "error", "EXPECTED_TOKEN", None),
                expected(3, "warning", "UNUSED_VARIABLE", "sub/pkg.sai"),
            ],
            "fuel": 1234,
            "memory_limit": 64,
            "stderr_fails": "first-write",
            "reference_only": "relies on a reference implementation limit",
        })

    def test_complete_directive_metadata_supplies_documented_defaults(self):
        source = (
            "//@ kind: check-pass\n"
            "//@ stage: 0\n"
            "//@ spec: spec/03 §3.1\n"
            "package example:test @ 0.1.0\n"
        )
        result = extract_with_directives(source, "sai.package")
        self.assertEqual(result, {
            "expected": [],
            "directives": {
                "specified": ["kind", "spec", "stage"],
                "kind": "check-pass",
                "stage": 0,
                "spec": [{"document": "spec/03", "section": "3.1"}],
                "targets": ["wasm32-core", "wasm32-wasi", "wasm64-core", "wasm64-wasi"],
                "profiles": ["dev", "release", "tiny"],
                "options": [],
                "component": False,
                "call": [],
                "args": [],
                "seed": None,
                "diag": [],
                "fuel": 10_000_000_000,
                "memory_limit": 4096,
                "stderr_fails": None,
                "reference_only": None,
            },
        })
        self.assertEqual(
            extract_bytes_with_directives(source.encode(), "sai.package"),
            result,
        )

    def test_complete_directive_metadata_rejects_missing_or_repeated_required_fields(self):
        required = [
            "//@ kind: check-pass\n",
            "//@ stage: 0\n",
            "//@ spec: spec/03 §3.1\n",
        ]
        for missing in range(len(required)):
            source = "".join(value for index, value in enumerate(required) if index != missing)
            with self.subTest(missing=missing), self.assertRaisesRegex(ValueError, "missing required"):
                extract_with_directives(source, "test.sai")

        for duplicate in required:
            source = "".join(required) + duplicate
            with self.subTest(duplicate=duplicate), self.assertRaisesRegex(ValueError, "repeated"):
                extract_with_directives(source, "test.sai")

    def test_complete_directive_metadata_rejects_repeated_optional_nonrepeatable_fields(self):
        base = (
            "//@ kind: check-pass\n"
            "//@ stage: 0\n"
            "//@ spec: spec/03 §3.1\n"
        )
        for directive in (
            "//@ targets: wasm32-core\n",
            "//@ profiles: dev\n",
            "//@ options: --tests\n",
            "//@ component:\n",
            "//@ args: \"one\"\n",
            "//@ seed: 1\n",
            "//@ fuel: 1\n",
            "//@ memory-limit: 1\n",
            "//@ stderr-fails: first-write\n",
            "//@ reference-only: reason\n",
        ):
            with self.subTest(directive=directive), self.assertRaisesRegex(ValueError, "repeated"):
                extract_with_directives(base + directive + directive, "test.sai")

    def test_complete_directive_metadata_validates_values(self):
        base = (
            "//@ kind: check-pass\n"
            "//@ stage: 0\n"
            "//@ spec: spec/03 §3.1\n"
        )
        invalid = [
            "//@ unknown: x\n",
            "//@ kind: unknown\n",
            "//@ stage: 8\n",
            "//@ stage: -1\n",
            "//@ spec: spec/03\n",
            "//@ spec: spec/03 §3.1,\n",
            "//@ targets: wasm32-core wasi\n",
            "//@ targets: wasm32-core wasm32-core\n",
            "//@ profiles: debug\n",
            "//@ profiles: dev tiny dev\n",
            "//@ options: --jobs 2\n",
            "//@ options: --allow\n",
            "//@ options: --allow not_a_code\n",
            "//@ options: --wasm-feature reference-types\n",
            "//@ component: yes\n",
            "//@ call: add(i32:1, ) -> i32:2\n",
            "//@ call: add (i32:1) -> i32:2\n",
            "//@ call: add(i32:1) i32:2\n",
            "//@ call: add(i32:1) ->\n",
            "//@ call: add(i32:1) -> (i32:2\n",
            "//@ args: \n",
            "//@ args: 1\n",
            "//@ args: \"one\"\"two\"\n",
            "//@ seed: -1\n",
            "//@ fuel: many\n",
            "//@ memory-limit: 1.5\n",
            "//@ stderr-fails: always\n",
            "//@ reference-only:\n",
            "//@ diag: error BAD_CODE at ../bad.sai:2\n",
        ]
        for directive in invalid:
            with self.subTest(directive=directive), self.assertRaises(ValueError):
                extract_with_directives(base + directive, "test.sai")

    def test_complete_directive_metadata_rejects_directives_after_source_token(self):
        source = (
            "//@ kind: check-pass\n"
            "//@ stage: 0\n"
            "//@ spec: spec/03 §3.1\n"
            "fn main() {}\n"
            "//@ targets: wasm32-core\n"
        )
        with self.assertRaisesRegex(ValueError, "directive outside"):
            extract_with_directives(source, "test.sai")

    def test_annotations_carets_multisets_and_line_endings(self):
        source = (
            "\ufefffn one() {\r\n"
            "  value // more\r"
            "  //~^ error TYPE_MISMATCH, warning UNUSED_VARIABLE\n"
            "  value // rejected: OTHER_ERROR (example explanation)\n"
            "  value // lint: UNUSED_VARIABLE; explanation\n"
            "  value //~ error TYPE_MISMATCH, error TYPE_MISMATCH\n"
            "}\n"
        )
        self.assertEqual(extract(source, "src/lib.sai", lint_levels={"UNUSED_VARIABLE": "warn"}), [
            expected(2, "error", "TYPE_MISMATCH"),
            expected(2, "warning", "UNUSED_VARIABLE"),
            expected(4, "error", "OTHER_ERROR"),
            expected(5, "warning", "UNUSED_VARIABLE"),
            expected(6, "error", "TYPE_MISMATCH"),
            expected(6, "error", "TYPE_MISMATCH"),
        ])

    def test_strings_comments_and_labels_do_not_create_annotations(self):
        source = '''\
let a = "//~ error FALSE_ERROR"
let b = "escaped \\" // rejected: FALSE_ERROR"
let c = r##"raw "# // lint: FALSE_ERROR
//@ diag: error FALSE_ERROR
"##
let d = br#"//~ error FALSE_ERROR"#
let e = """
    //~ error FALSE_ERROR
    /* nested */
    """
/* // rejected: FALSE_ERROR
   /* //~ error FALSE_ERROR */
*/
let quote = '\\''
let label = 'outer
let slash = '/'
let byte = b'/'
let good = 1 //~ error TRUE_ERROR
'''
        self.assertEqual(extract(source, "src/lib.sai"), [expected(18, "error", "TRUE_ERROR")])

    def test_raw_identifiers_do_not_start_literals(self):
        source = 'r#r"//~ error FALSE_ERROR"\nr#br"// rejected: FALSE_ERROR"\nx //~ error TRUE_ERROR\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(3, "error", "TRUE_ERROR")])

    def test_numeric_suffixes_do_not_start_prefixed_literals(self):
        for number in ("123r", "123br", "123b", "123f", "0xffr", "0b2r", "0Xr",
                       "1.2r", "1e+2r", "1e-2f", "t.0.1r", "1_e2r"):
            source = number + '"escaped \\\" //~ error FALSE_ERROR" //~ error MALFORMED_NUMBER\n'
            with self.subTest(number=number):
                self.assertEqual(extract(source, "src/lib.sai"), [expected(1, "error", "MALFORMED_NUMBER")])
        source = '123r#"escaped \\\" //~ error FALSE_ERROR"# //~ error MALFORMED_NUMBER\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(1, "error", "MALFORMED_NUMBER")])

    def test_numeric_suffixes_end_at_token_boundaries(self):
        source = '123r "//~ error FALSE_ERROR"\n123 r#"raw " //~ error FALSE_ERROR\n"#\nx //~ error TRUE_ERROR\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(4, "error", "TRUE_ERROR")])
        for source in ('123 f"text"\n', '1e+f"text"\n'):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "format strings"):
                extract(source, "src/lib.sai")
        with self.assertRaises(ValueError):
            extract('123r"text"\n', "sai.package")

    def test_package_path_candidates_are_rejected_before_ambiguous_literals(self):
        for introducer in ("use", "export", "import", "include"):
            for separator in (" ", " /* comment */ "):
                for path in ("acme:r", "acme:f", "acme:br", "acme:b", "foo-bar:api/sub-r",
                             "acme:Bad--r", "acme:", "f:pkg"):
                    source = introducer + separator + path + '"escaped \\\" //~ error FALSE_ERROR"\n'
                    with self.subTest(introducer=introducer, separator=separator, path=path):
                        with self.assertRaisesRegex(ValueError, "package-path candidates"):
                            extract(source, "src/lib.sai")

    def test_complete_package_paths_with_unambiguous_boundaries(self):
        for introducer in ("use", "export", "import", "include"):
            for path in ("acme:json", "foo-bar:api/sub_foo-v2", "f:pkg/r"):
                for suffix in (" ", "\t", ".{Thing} ", ", ", "{} "):
                    source = introducer + " /* comment */ " + path + suffix + "//~ error SOME_ERROR\n"
                    with self.subTest(source=source):
                        self.assertEqual(extract(source, "src/lib.sai"), [expected(1, "error", "SOME_ERROR")])
                with self.subTest(introducer=introducer, path=path):
                    self.assertEqual(extract(introducer + " " + path, "src/lib.sai"), [])
                    source = "(" + introducer + "\n" + path + "\n)\nx //~ error SOME_ERROR\n"
                    self.assertEqual(extract(source, "src/lib.sai"), [expected(4, "error", "SOME_ERROR")])
        source = 'use acme:json\nlet text = "//~ error FALSE_ERROR"\nx //~ error TRUE_ERROR\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(3, "error", "TRUE_ERROR")])

    def test_ambiguous_package_path_boundaries_remain_unsupported(self):
        for path in ("acme:", "acme:Bad", "acme:foo-", "acme:foo--bar", "acme:foo_",
                     "acme:foo/", "acme:foo/Bad", "acme:foo/r#\"text\"", "acme:foo//~ error FALSE_ERROR"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "package-path candidates"):
                extract("use " + path + "\n", "src/lib.sai")
        for source in ("use\nname:import acme:r\"text\"", ") use\nacme:foo"):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "package-path candidates"):
                extract(source, "src/lib.sai")

    def test_type_annotations_do_not_activate_package_paths(self):
        for prefix in ('let name:Type = 1\n', 'name: Type\n', 'let name: pkg.Type = 1\n',
                       'let value = "import"; name:Type\n', 'r#import name:Type\n',
                       'import other name:Type\n', 'import /// doc\nname:Type\n'):
            with self.subTest(source=prefix):
                self.assertEqual(extract(prefix + 'x //~ error TRUE_ERROR\n', "src/lib.sai"), [
                    expected(prefix.count('\n') + 1, "error", "TRUE_ERROR"),
                ])
        source = 'let name : Type = "acme:r"\n/* acme:f */\nx //~ error TRUE_ERROR\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(3, "error", "TRUE_ERROR")])
        self.assertEqual(extract('use r#"acme:r"#\nx //~ error TRUE_ERROR\n', "src/lib.sai"), [
            expected(2, "error", "TRUE_ERROR"),
        ])
        self.assertEqual(extract('package acme:api @ 1.0.0\n', "sai.package", directives=True), [])

    def test_package_path_newline_and_bracket_context(self):
        for introducer in ("use", "export", "import", "include"):
            for separator in ("\n", " /*\n*/ ", " // comment\n"):
                for open_bracket in ("(", "["):
                    source = open_bracket + introducer + separator + 'acme:r"text"\n'
                    with self.subTest(source=source), self.assertRaisesRegex(ValueError, "package-path candidates"):
                        extract(source, "src/lib.sai")
                if introducer in ("use", "export"):
                    source = introducer + separator + 'acme:r"text"\n'
                    with self.subTest(source=source), self.assertRaisesRegex(ValueError, "package-path candidates"):
                        extract(source, "src/lib.sai")
                else:
                    for prefix in ("", "{"):
                        source = prefix + introducer + separator + 'name:Type\nx //~ error TRUE_ERROR\n'
                        with self.subTest(source=source):
                            self.assertEqual(extract(source, "src/lib.sai"), [
                                expected(source.count('\n'), "error", "TRUE_ERROR"),
                            ])
        for source in (') import\nacme:r"text"', '([} import\nacme:r"text"'):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "package-path candidates"):
                extract(source, "src/lib.sai")
        with self.assertRaisesRegex(ValueError, "package-path candidates"):
            extract('({[import\nacme:r"text"', "src/lib.sai")
        self.assertEqual(extract('([{import\nname:Type\nx //~ error TRUE_ERROR\n', "src/lib.sai"), [
            expected(3, "error", "TRUE_ERROR"),
        ])

    def test_non_ascii_code_is_rejected_independent_of_unicode_version(self):
        for source in ('let α = 1\n', 'r#\u1c89"//~ error FALSE_ERROR"\n',
                       '\U00010d40r"//~ error FALSE_ERROR"\n', "'α\n"):
            with self.subTest(source=source), self.assertRaises(ValueError):
                extract(source, "src/lib.sai")
        source = 'let x = "日本語 //~ error FALSE_ERROR"\n/* 日本語 */ x //~ error TRUE_ERROR\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(2, "error", "TRUE_ERROR")])

    def test_shebang_is_not_a_comment_annotation(self):
        source = '#!/usr/bin/env sai //~ error FALSE_ERROR\n//@ diag: error SOME_ERROR\nx\n'
        self.assertEqual(extract(source, "src/lib.sai", directives=True), [expected(None, "error", "SOME_ERROR", None)])

    def test_comma_separated_rejected_and_lint_explanations(self):
        source = 'x // rejected: EXPECTED_TOKEN, explanation\ny // lint: SOME_LINT, explanation\n'
        self.assertEqual(extract(source, "src/lib.sai", lint_levels={"SOME_LINT": "warn"}), [
            expected(1, "error", "EXPECTED_TOKEN"), expected(2, "warning", "SOME_LINT"),
        ])

    def test_invalid_utf8_is_supported_only_when_boundaries_are_known(self):
        self.assertEqual(extract_bytes(b'\xff //~ error INVALID_UTF8\n', "src/lib.sai"), [
            expected(1, "error", "INVALID_UTF8"),
        ])
        self.assertEqual(extract_bytes(b'//@ diag: error INVALID_UTF8 at src/lib.sai:2\n\xe2\x82\n',
                                       "src/lib.sai", directives=True), [
            expected(2, "error", "INVALID_UTF8"),
        ])
        for source in (b'r#\xffr"//~ error FALSE_ERROR"', b'let x = "\xff"',
                       b'let x = b"\\\xff"', b'/* \xff */', b'// \xff', b'#!\xff\n'):
            with self.subTest(source=source), self.assertRaises(ValueError):
                extract_bytes(source, "src/lib.sai")

    def test_directives_locationless_located_and_repeated(self):
        source = (
            "//@ kind: check-fail\n"
            "//@ stage: 0\n"
            "//@ diag: error EXPECTED_TOKEN\n"
            "//@ diag: warning UNUSED_VARIABLE at sub/pkg.sai:3\n"
            "//@ diag: error EXPECTED_TOKEN\n"
            "package example:test @ 0.1.0\n"
        )
        self.assertEqual(extract(source, "sai.package", directives=True), [
            expected(None, "error", "EXPECTED_TOKEN", None),
            expected(3, "warning", "UNUSED_VARIABLE", "sub/pkg.sai"),
            expected(None, "error", "EXPECTED_TOKEN", None),
        ])

    def test_carets_and_escaped_newlines_keep_original_line_numbers(self):
        source = 'let text = "first\\\n  //~ error FALSE_ERROR"\nnext\n//~^^ warning SOME_WARNING\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(2, "warning", "SOME_WARNING")])

    def test_directive_placement_and_bad_directives_fail(self):
        for source, directives in (
            ("//@ diag: error SOME_ERROR\n", False),
            ("let value = 1\n//@ diag: error SOME_ERROR\n", True),
            ("//@ unknown: 1\n", True),
            ("//@ stage: 0\n//@ stage: 0\n", True),
            ("//@ diag: error SOME_ERROR at ../file:2\n", True),
            ("//@ diag: error SOME_ERROR at file:0\n", True),
            ("//@ diag: error SOME_ERROR at C:/file:2\n", True),
        ):
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    extract(source, "sai.package", directives=directives)

    def test_doc_tokens_prevent_following_directives(self):
        for doc in ("/// documentation", "//! documentation", "///", "//!"):
            for directive in ("//@ kind: check-fail", "//@ diag: error SOME_ERROR"):
                source = doc + "\n" + directive + "\n"
                with self.subTest(source=source), self.assertRaisesRegex(ValueError, "directive outside"):
                    extract(source, "src/lib.sai", directives=True)
        for source in ("//// ordinary comment\n//@ diag: error SOME_ERROR\n",
                       "//@ diag: error SOME_ERROR\n/// documentation\n"):
            with self.subTest(source=source):
                self.assertEqual(extract(source, "src/lib.sai", directives=True), [
                    expected(None, "error", "SOME_ERROR", None),
                ])
        self.assertEqual(extract('/// manifest comment\n//@ diag: error SOME_ERROR\n',
                                 "sai.package", directives=True), [expected(None, "error", "SOME_ERROR", None)])

    def test_invalid_escapes_are_rejected_before_interpreting_comments(self):
        source = r'let x:Type = "\u{\" import acme:r"foo" // rejected: EXPECTED_TOKEN " //~ error INVALID_ESCAPE'
        with self.assertRaisesRegex(ValueError, "invalid or unsupported escape"):
            extract(source, "src/lib.sai")
        for escape in (r'\q', r'\u', r'\u{}', r'\u{12', r'\u{12z}', r'\u{1234567}',
                       r'\u{d800}', r'\u{110000}', r'\x41', r'\x', r'\x4'):
            for wrap in ('"{}"', "'{}'", '"""\n{}\n"""'):
                with self.subTest(escape=escape, wrapper=wrap), self.assertRaises(ValueError):
                    extract('let value = ' + wrap.format(escape) + '\n', "src/lib.sai")
        for escape in (r'\q', r'\u{41}', r'\x', r'\x4', r'\xgg'):
            for wrap in ('b"{}"', "b'{}'"):
                with self.subTest(escape=escape, wrapper=wrap), self.assertRaises(ValueError):
                    extract('let value = ' + wrap.format(escape) + '\n', "src/lib.sai")
        for source in ('source "\\u{\\" // rejected: EXPECTED_TOKEN"\n', 'source "\\\ntext"\n'):
            with self.subTest(source=source), self.assertRaises(ValueError):
                extract(source, "sai.package", directives=True)
        for source in ('let x = "bad\\\rnot a continuation"\n',
                       'let x = r"raw\\\rtext"\n', '// comment\\\r'):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "lone CR"):
                extract(source, "src/lib.sai")

    def test_invalid_escape_is_supported_only_in_a_fmt_refuse_fixture(self):
        source = r"""//@ kind: fmt-refuse
//@ stage: 0
//@ spec: spec/02 §2.6.4, impl/14 §14.2.2
//@ diag: error INVALID_ESCAPE at src/lib.sai:5
pub fn invalid_escape() -> Str { "\q" }
"""
        result = extract_with_directives(source, "src/lib.sai")
        self.assertEqual(result["expected"], [
            expected(5, "error", "INVALID_ESCAPE"),
        ])
        self.assertEqual(result["directives"]["kind"], "fmt-refuse")
        for kind in ("fmt", "check-fail"):
            with self.subTest(kind=kind), self.assertRaisesRegex(
                ValueError, "invalid or unsupported escape"
            ):
                extract_with_directives(
                    source.replace("kind: fmt-refuse", f"kind: {kind}"),
                    "src/lib.sai",
                )

    def test_valid_escapes_and_raw_strings_are_preserved(self):
        source = (r'''let a = "\u{41}\n\" // not a comment"''' + '\n' +
                  r'''let b = b"\x41\n\" // not a comment"''' + '\n' +
                  r'''let c = '\u{41}' ''' + '\n' +
                  r'''let d = b'\x41' ''' + '\n' +
                  r'''let e = r"\u{broken // not a comment"''' + '\n' +
                  'let multi = """\n' + r'\u{41}\n' + '\n"""\n' +
                  'x //~ error TRUE_ERROR\n')
        self.assertEqual(extract(source, "src/lib.sai"), [expected(9, "error", "TRUE_ERROR")])
        self.assertEqual(extract('let x = "continued\\\r\n text"\r\nx //~ error TRUE_ERROR\r\n',
                                 "src/lib.sai"), [expected(3, "error", "TRUE_ERROR")])
        self.assertEqual(extract(r'source "\u{41}"' + '\n', "sai.package", directives=True), [])

    def test_malformed_and_unsupported_annotations_fail(self):
        for source in (
            "//~ error SOME_ERROR\n",
            "//~^ error SOME_ERROR\n",
            "x //~ missing CODE_NAME\n",
            "x //~ error SOME_ERROR,\n",
            "x //~ panic\n",
            "x //~ test-failed: panic\n",
            "x // rejected: nope\n",
            "// lint: SOME_LINT\n",
        ):
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    extract(source, "src/lib.sai")

    def test_lint_levels_require_explicit_emitting_default(self):
        source = "x // lint: SOME_LINT\n"
        for level, severity in (("info", "info"), ("warn", "warning"), ("deny", "error")):
            self.assertEqual(extract(source, "src/lib.sai", lint_levels={"SOME_LINT": level}), [
                expected(1, severity, "SOME_LINT"),
            ])
        for levels in ({}, {"SOME_LINT": "allow"}, {"SOME_LINT": "warning"}, {"SOME_LINT": []}):
            with self.subTest(levels=levels):
                with self.assertRaises(ValueError):
                    extract(source, "src/lib.sai", lint_levels=levels)

    def test_unsafe_lexical_boundaries_are_rejected(self):
        for source in (
            'let x = f"{a} //~ error BAD_ERROR"\n',
            'let x = "unterminated\n//~^ error BAD_ERROR\n',
            'let x = r#"unterminated\n//~^ error BAD_ERROR\n',
            'let x = """oops\n//~^ error BAD_ERROR\n',
            'let x = """\n//~ error BAD_ERROR\n',
            "/* unterminated //~ error BAD_ERROR\n",
            'let x = b"""\n//~^ error BAD_ERROR\n',
            "\u00a0//~ error SOME_ERROR\n",
        ):
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    extract(source, "src/lib.sai")

    def test_multiline_closer_must_be_a_line_of_its_own(self):
        source = 'let s = """\n    """ //~ error FALSE_ERROR\n    """\nreal //~ error TRUE_ERROR\n'
        self.assertEqual(extract(source, "src/lib.sai"), [expected(4, "error", "TRUE_ERROR")])

    def test_manifest_rejects_ambiguous_source_literal_forms(self):
        for source in ('source r"text"\n', 'source b"text"\n', 'source """\ntext\n"""\n', "source 'text'\n"):
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    extract(source, "sai.package", directives=True)
        self.assertEqual(extract('source "//~ error FALSE_ERROR"\n', "sai.package", directives=True), [])

    def test_code_before_annotation_must_not_be_only_trivia(self):
        for source in ("/* trivia */ //~ error SOME_ERROR\n", "/* trivia\n */ //~ error SOME_ERROR\n"):
            with self.subTest(source=source):
                with self.assertRaises(ValueError):
                    extract(source, "src/lib.sai")

    def test_result_integrates_with_matcher(self):
        result = extract("bad //~ error SOME_ERROR\n", "src/lib.sai")
        response = compare({"kind": "check-fail", "implementation": "reference", "exit_code": 1,
                            "expected": result, "actual": result})
        self.assertEqual(response["outcome"], "pass")

    def test_command_line_and_invalid_utf8(self):
        script = Path(__file__).with_name("expectation_extractor.py")
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "test.sai"
            source.write_bytes(b"x //~ error SOME_ERROR\n")
            process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai"],
                                     capture_output=True, check=False)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(json.loads(process.stdout)["expected"], [expected(1, "error", "SOME_ERROR")])
            source.write_text(
                "//@ kind: check-pass\n//@ stage: 0\n//@ spec: spec/03 §3.1\nfn main() {}\n",
                encoding="utf-8",
            )
            process = subprocess.run(
                [sys.executable, "-B", str(script), str(source), "--path", "test.sai",
                 "--validate-test-directives"],
                capture_output=True, check=False,
            )
            self.assertEqual(process.returncode, 0)
            self.assertEqual(json.loads(process.stdout)["directives"]["kind"], "check-pass")
            source.write_text("//@ kind: check-pass\n", encoding="utf-8")
            process = subprocess.run(
                [sys.executable, "-B", str(script), str(source), "--path", "test.sai",
                 "--validate-test-directives"],
                capture_output=True, check=False,
            )
            self.assertEqual(process.returncode, 2)
            self.assertEqual(process.stdout, b"")
            source.write_bytes(b"use foo-bar:api/sub_name.{Thing} //~ error UNKNOWN_PACKAGE\n")
            process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai"],
                                     capture_output=True, check=False)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(json.loads(process.stdout)["expected"], [expected(1, "error", "UNKNOWN_PACKAGE")])
            source.write_bytes(b'123r"escaped \\\" //~ error FALSE_ERROR" //~ error MALFORMED_NUMBER\n')
            process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai"],
                                     capture_output=True, check=False)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(json.loads(process.stdout)["expected"], [expected(1, "error", "MALFORMED_NUMBER")])
            source.write_bytes(b'use acme:r"escaped \\\" //~ error FALSE_ERROR"\n')
            process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai"],
                                     capture_output=True, check=False)
            self.assertEqual(process.returncode, 2)
            self.assertEqual(process.stdout, b"")
            self.assertIn(b"package-path candidates", process.stderr)
            for contents in (b'let x = "\\u{\\" import acme:r"foo" // rejected: EXPECTED_TOKEN " //~ error INVALID_ESCAPE',
                             b'/// documentation\n//@ diag: error SOME_ERROR\n'):
                source.write_bytes(contents)
                process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai",
                                          "--directives"], capture_output=True, check=False)
                self.assertEqual(process.returncode, 2)
                self.assertEqual(process.stdout, b"")
                self.assertIn(b"invalid input:", process.stderr)
            source.write_bytes(b"\xff //~ error INVALID_UTF8\n")
            process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai"],
                                     capture_output=True, check=False)
            self.assertEqual(process.returncode, 0)
            self.assertEqual(json.loads(process.stdout)["expected"], [expected(1, "error", "INVALID_UTF8")])
            source.write_bytes(b'"\xff"')
            process = subprocess.run([sys.executable, "-B", str(script), str(source), "--path", "src/lib.sai"],
                                     capture_output=True, check=False)
            self.assertEqual(process.returncode, 2)
            self.assertEqual(process.stdout, b"")
            self.assertIn(b"invalid input:", process.stderr)

    def test_command_line_lint_levels_requires_json_object(self):
        script = Path(__file__).with_name("expectation_extractor.py")
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "test.sai"
            source.write_text("x\n", encoding="utf-8")
            for value in ("null", "[]", "true"):
                with self.subTest(value=value):
                    process = subprocess.run([sys.executable, "-B", str(script), str(source),
                                              "--path", "src/lib.sai", "--lint-levels", value],
                                             capture_output=True, check=False)
                    self.assertEqual(process.returncode, 2)
                    self.assertEqual(process.stdout, b"")
                    self.assertIn(b"invalid input:", process.stderr)


if __name__ == "__main__":
    unittest.main()
