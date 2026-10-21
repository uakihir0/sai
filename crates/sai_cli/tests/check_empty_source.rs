use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sai-cli-check-empty-source-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_check(
    root_source: &str,
    manifest_additions: &str,
    extra_files: &[(&str, &str)],
    options: &[&str],
) -> Output {
    let temporary = TestDirectory::new();
    let root = &temporary.0;
    fs::create_dir_all(root.join("src")).expect("create source directory");
    let default_targets = if manifest_additions.contains("targets {") {
        ""
    } else {
        "targets { wasm32-core, wasm32-wasi }\n"
    };
    let default_compiler = if manifest_additions.contains("compiler ") {
        ""
    } else {
        "compiler ^0.1\n"
    };
    fs::write(
        root.join("sai.package"),
        format!(
            "package acme:empty @ 0.1.0\n{default_compiler}source \"src\"\n{default_targets}{manifest_additions}"
        ),
    )
    .expect("write package manifest");
    fs::write(root.join("src/lib.sai"), root_source).expect("write root source");
    if options.contains(&"--locked") || options.contains(&"--frozen") {
        fs::write(
            root.join("sai.lock"),
            concat!(
                "lock 2\n",
                "\n",
                "root acme:empty @ 0.1.0\n",
                "    targets { wasm32-core, wasm32-wasi }\n"
            ),
        )
        .expect("write root-only lock file");
    }
    for (relative_path, contents) in extra_files {
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().expect("test file has a parent"))
            .expect("create test file parent directory");
        fs::write(path, contents).expect("write extra test file");
    }

    let mut arguments = vec!["--format=json", "check"];
    arguments.extend_from_slice(options);
    Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run sai check subprocess")
}

fn assert_check_stays_gated(output: &Output, context: &str) {
    let stdout = std::str::from_utf8(&output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "{context}: check must remain gated:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "{context}: incomplete input must not emit a successful result:\n{stdout}"
    );
}

fn assert_check_stays_gated_without_syntax_errors(output: &Output, context: &str) {
    assert_check_stays_gated(output, context);
    let stdout = std::str::from_utf8(&output.stdout).expect("CLI stdout is UTF-8");
    for code in [
        "EXPECTED_EXPRESSION",
        "EXPECTED_PATTERN",
        "EXPECTED_TOKEN",
        "EXPECTED_VALUE_FOUND_TYPE",
        "FILE_HAS_SYNTAX_ERRORS",
        "INVALID_CHAR_LITERAL",
        "INVALID_ESCAPE",
        "MALFORMED_MULTILINE_STRING",
        "MALFORMED_NUMBER",
        "MALFORMED_RAW_STRING",
        "NO_SEMICOLON",
        "UNEXPECTED_CHARACTER",
        "UNEXPECTED_NEWLINE",
        "UNTERMINATED_CHAR",
        "UNTERMINATED_COMMENT",
        "UNTERMINATED_STRING",
    ] {
        assert!(
            !stdout.contains(&format!("\"code\":\"{code}\"")),
            "{context}: gate fixture must not contain lexer or parser diagnostics ({code}):\n{stdout}"
        );
    }
}

fn assert_json_diagnostic_primary_span(
    stdout: &str,
    code: &str,
    message: &str,
    start: usize,
    end: usize,
) {
    let matching_records = stdout
        .lines()
        .filter(|record| {
            record.contains(&format!("\"code\":\"{code}\""))
                && record.contains(&format!("\"message\":\"{message}\""))
                && record.contains(&format!("\"start\":{{\"byte\":{start},"))
                && record.contains(&format!("\"end\":{{\"byte\":{end},"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching_records.len(),
        1,
        "expected exactly one {code} diagnostic with message {message:?}:\n{stdout}"
    );
    let diagnostic = matching_records[0];
    let primary = diagnostic
        .split_once("\"primary\":")
        .expect("diagnostic has a primary field")
        .1
        .split_once(",\"primary_label\":")
        .expect("primary span ends before the primary label")
        .0;
    assert!(
        primary.contains(&format!("\"start\":{{\"byte\":{start},")),
        "{code} diagnostic has the wrong primary start span:\n{diagnostic}"
    );
    assert!(
        primary.contains(&format!("\"end\":{{\"byte\":{end},")),
        "{code} diagnostic has the wrong primary end span:\n{diagnostic}"
    );
}

fn assert_success_result(output: Output, target: &str, profile: &str) {
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(0),
        "supported check input should succeed:\n{stdout}"
    );
    assert!(output.stderr.is_empty());
    let records = stdout.lines().collect::<Vec<_>>();
    assert_eq!(records.len(), 3, "{stdout}");
    assert!(records[0].contains("\"type\":\"header\""), "{}", records[0]);
    assert_eq!(
        records[1],
        format!(
            "{{\"type\":\"result\",\"kind\":\"check\",\"data\":{{\"target\":\"{target}\",\"profile\":\"{profile}\",\"tests\":false,\"packages\":1,\"fixes_applied\":0,\"changed_files\":[]}}}}"
        )
    );
    assert!(
        records[2].contains("\"type\":\"summary\"")
            && records[2].contains("\"errors\":0")
            && records[2].contains("\"exit_code\":0"),
        "{}",
        records[2]
    );
}

#[test]
fn public_direct_parameter_function_emits_check_result() {
    assert_success_result(
        run_check(
            "pub fn identity(value: I32) -> I32 { value }\n",
            "",
            &[],
            &[],
        ),
        "wasm32-core",
        "dev",
    );
}

#[test]
fn direct_parameter_return_type_mismatch_suppresses_check_result() {
    let output = run_check(
        "pub fn identity(value: I32) -> Bool { value }\n",
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(output.stderr.is_empty());
    assert!(
        stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && stdout.contains("expected `Bool`, found `I32`")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "type mismatch must be reported without a result or feature gate:\n{stdout}"
    );
}

#[test]
fn public_zero_parameter_direct_scalar_function_emits_check_result() {
    for source in [
        "pub fn answer() -> I32 { 42 }\n",
        "pub fn ready() -> Bool { true }\n",
        "pub fn letter() -> Char { 'z' }\n",
        "pub fn marker() -> Byte { b'z' }\n",
        "pub fn ratio() -> F32 { -1.25 }\n",
        "pub fn minimum() -> I8 { -128 }\n",
        "pub fn builtin_int_alias() -> Int { 42 }\n",
        "pub fn builtin_uint_alias() -> UInt { 42 }\n",
        "pub fn pointer_size() -> USize { 4294967295 }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }

    for source in [
        "pub fn signed_minimum() -> ISize { -9223372036854775808 }\n",
        "pub fn signed_maximum() -> ISize { 9223372036854775807 }\n",
        "pub fn above_wasm32() -> USize { 4294967296 }\n",
        "pub fn unsigned_maximum() -> USize { 18446744073709551615 }\n",
    ] {
        assert_success_result(
            run_check(
                source,
                "targets { wasm32-core, wasm64-core }\n",
                &[],
                &["--target", "wasm64-core"],
            ),
            "wasm64-core",
            "dev",
        );
    }
}

#[test]
fn public_function_with_annotated_primitive_locals_emits_check_result() {
    for source in [
        "pub fn answer() -> I32 {\n    let first: U8 = 7\n    let value: I32 = 42\n    value\n}\n",
        "pub fn first() -> U8 {\n    let first: U8 = 7\n    let value: I32 = 42\n    first\n}\n",
        "pub fn copy_chain() -> I32 {\n    let first: I32 = 21\n    let second: I32 = first\n    let third: I32 = second\n    third\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn annotated_primitive_local_reports_semantic_errors_without_a_feature_gate() {
    for (source, code, message) in [
        (
            "pub fn mismatch() -> I32 {\n    let value: I32 = true\n    value\n}\n",
            "TYPE_MISMATCH",
            "expected `I32`, found a Bool literal",
        ),
        (
            "pub fn overflow() -> I8 {\n    let value: I8 = 128\n    value\n}\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `I8`",
        ),
        (
            "pub fn mismatch() -> Bool {\n    let value: I32 = 42\n    value\n}\n",
            "TYPE_MISMATCH",
            "expected `Bool`, found `I32`",
        ),
        (
            "pub fn mismatch() -> Bool {\n    let first: I32 = true\n    let second: I8 = 128\n    first\n}\n",
            "TYPE_MISMATCH",
            "expected `Bool`, found `I32`",
        ),
        (
            "pub fn copy_mismatch() -> Bool {\n    let first: U8 = 7\n    let second: I32 = first\n    second\n}\n",
            "TYPE_MISMATCH",
            "expected `I32`, found `U8`",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains(&format!("\"code\":\"{code}\""))
                && stdout.contains(message)
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "a supported local type error must be reported without a gate:\n{stdout}"
        );
    }
}

#[test]
fn public_unannotated_copy_of_annotated_primitive_local_emits_check_result() {
    for source in [
        "pub fn copy() -> I32 {\n    let source: I32 = 42\n    let copied = source\n    copied\n}\n",
        "pub fn copy_source_tail() -> I32 {\n    let source: I32 = 42\n    let copied = source\n    source\n}\n",
        "pub fn byte_copy() -> U8 {\n    let source: U8 = 7\n    let copied = source\n    copied\n}\n",
        "pub fn bool_copy() -> Bool {\n    let source: Bool = true\n    let copied = source\n    copied\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unannotated_copy_of_annotated_primitive_local_reports_semantic_errors() {
    for (source, code, message, token) in [
        (
            "pub fn mismatch() -> I64 {\n    let source: I32 = 42\n    let copied = source\n    copied\n}\n",
            "TYPE_MISMATCH",
            "expected `I64`, found `I32`",
            "copied",
        ),
        (
            "pub fn mismatch() -> I32 {\n    let source: I32 = true\n    let copied = source\n    copied\n}\n",
            "TYPE_MISMATCH",
            "expected `I32`, found a Bool literal",
            "true",
        ),
        (
            "pub fn overflow() -> I8 {\n    let source: I8 = 128\n    let copied = source\n    copied\n}\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `I8`",
            "128",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let start = if code == "TYPE_MISMATCH" && token == "copied" {
            source.rfind(token)
        } else {
            source.find(token)
        }
        .expect("diagnostic token exists");
        let end = start + token.len();
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.matches(&format!("\"code\":\"{code}\"")).count() == 1
                && stdout.contains(message)
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\"")
                && stdout.contains(&format!("\"start\":{{\"byte\":{start},"))
                && stdout.contains(&format!("\"end\":{{\"byte\":{end},")),
            "supported copied-local errors must be reported once at their source span:\n{stdout}"
        );
    }
}

#[test]
fn unsupported_unannotated_copy_local_forms_remain_gated() {
    for source in [
        "pub fn copy() -> I32 {\n    let source: I32 = 42\n    let copied = source / (1 / 1)\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let source: I32 = 42\n    let copied = r#source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let source: I32 = 42\n    var copied = source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    var source: I32 = 42\n    let copied = source\n    copied\n}\n",
        "const SOURCE: I32 = 42\npub fn copy() -> I32 {\n    let copied = SOURCE\n    copied\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_parameter_local_copy_emits_check_result() {
    for source in [
        "pub fn copy_annotated(source: I32) -> I32 {\n    let copied: I32 = source\n    copied\n}\n",
        "pub fn copy_unannotated(source: I32) -> I32 {\n    let copied = source\n    copied\n}\n",
        "pub fn parameter_tail(source: I32) -> I32 {\n    let copied: I32 = source\n    source\n}\n",
        "pub fn parameter_tail_second(first: I32, second: Bool) -> Bool {\n    let copied: I32 = first\n    second\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn parameter_local_copy_reports_semantic_errors_without_a_feature_gate() {
    for (source, message, token) in [
        (
            "pub fn copy_mismatch(source: I32) -> F32 {\n    let copied: F32 = source\n    copied\n}\n",
            "expected `F32`, found `I32`",
            "source",
        ),
        (
            "pub fn return_mismatch(source: I32) -> Bool {\n    let copied: I32 = source\n    copied\n}\n",
            "expected `Bool`, found `I32`",
            "copied",
        ),
        (
            "pub fn parameter_tail_mismatch(source: I32) -> Bool {\n    let copied: I32 = source\n    source\n}\n",
            "expected `Bool`, found `I32`",
            "source",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let start = source.rfind(token).expect("diagnostic token exists");
        let end = start + token.len();
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "supported parameter-local errors must not be hidden by a feature gate:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(&stdout, "TYPE_MISMATCH", message, start, end);
    }
}

#[test]
fn shadowed_parameter_initializer_uses_parameter_and_tail_uses_local_binding() {
    let source =
        "pub fn shadow(source: I32) -> Bool {\n    let source: Bool = source\n    source\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source
        .find("= source\n")
        .map(|start| start + "= ".len())
        .expect("shadowing initializer exists");
    let initializer_end = initializer_start + "source".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "supported shadowing mismatch must not be hidden by a feature gate:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `I32`",
        initializer_start,
        initializer_end,
    );
}

#[test]
fn unsupported_parameter_local_copy_forms_remain_gated() {
    for source in [
        "pub fn copy(var source: I32) -> I32 {\n    let copied: I32 = source\n    copied\n}\n",
        "pub fn copy(self: I32) -> I32 {\n    let copied: I32 = self\n    copied\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_two_parameter_local_copies_emit_check_result() {
    for source in [
        "pub fn copy_pair(first: I32, second: Bool) -> I32 {\n    let first_copy: I32 = first\n    let second_copy: Bool = second\n    first_copy\n}\n",
        "pub fn copy_pair(first: I32, second: Bool) -> I32 {\n    let first_copy = first\n    let second_copy = second\n    first_copy\n}\n",
        "pub fn copy_triple(first: I32, second: Bool, third: U16) -> U16 {\n    let first_copy = first\n    let second_copy: Bool = second\n    let third_copy = third\n    third_copy\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn two_parameter_local_copy_type_mismatch_is_reported_at_initializer() {
    let source = "pub fn copy_pair(first: I32, second: Bool) -> F32 {\n    let first_copy: F32 = first\n    let second_copy: Bool = second\n    first_copy\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source
        .find("= first\n")
        .map(|start| start + "= ".len())
        .expect("first parameter initializer exists");
    let initializer_end = initializer_start + "first".len();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "supported parameter-local mismatch must not be hidden by a feature gate:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found `I32`",
        initializer_start,
        initializer_end,
    );
}

#[test]
fn unsupported_two_parameter_local_copy_forms_remain_gated() {
    for source in [
        "pub fn copy_pair(first: I32, var second: Bool) -> I32 {\n    let first_copy: I32 = first\n    let second_copy: Bool = second\n    first_copy\n}\n",
        "pub fn copy_pair(first: I32, second: Str) -> I32 {\n    let first_copy: I32 = first\n    let second_copy: Str = second\n    first_copy\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_unannotated_literal_copy_emits_check_result() {
    for source in [
        "pub fn integer_copy() -> I32 {\n    let source = 42\n    let copied = source\n    copied\n}\n",
        "pub fn float_copy() -> F32 {\n    let source = 1.25\n    let copied = source\n    copied\n}\n",
        "pub fn bool_copy() -> Bool {\n    let source = true\n    let copied = source\n    copied\n}\n",
        "pub fn byte_copy() -> Byte {\n    let source = b'z'\n    let copied = source\n    copied\n}\n",
        "pub fn char_copy() -> Char {\n    let source = 'z'\n    let copied = source\n    copied\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unannotated_literal_copy_reports_semantic_errors_at_the_source_and_tail() {
    let source = "pub fn integer_family_mismatch() -> F32 {\n    let source = 340282366920938463463374607431768211456\n    let copied = source\n    copied\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer = "340282366920938463463374607431768211456";
    let initializer_start = source
        .find(initializer)
        .expect("integer initializer exists");
    let tail_start = source.rfind("copied").expect("copied tail exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count() == 1
            && stdout.contains("\"message\":\"expected `F32`, found an integer literal\"")
            && stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"integer literal is out of range for `I64`\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "integer family mismatch and default-range errors must retain their source spans:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        tail_start,
        tail_start + "copied".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        initializer_start,
        initializer_start + initializer.len(),
    );

    let source = "pub fn float_family_mismatch() -> I32 {\n    let source = 1e309\n    let copied = source\n    copied\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer = "1e309";
    let initializer_start = source.find(initializer).expect("float initializer exists");
    let tail_start = source.rfind("copied").expect("copied tail exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count() == 1
            && stdout.contains("\"message\":\"expected `I32`, found a floating-point literal\"")
            && stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"floating-point literal is out of range for `F64`\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "float family mismatch and default-range errors must retain their source spans:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found a floating-point literal",
        tail_start,
        tail_start + "copied".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "floating-point literal is out of range for `F64`",
        initializer_start,
        initializer_start + initializer.len(),
    );

    let source = "pub fn integer_overflow() -> I8 {\n    let source = 128\n    let copied = source\n    copied\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source.find("128").expect("integer initializer exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"integer literal is out of range for `I8`\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{initializer_start},"))
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "same-family range errors must point to the source literal:\n{stdout}"
    );

    let source = "pub fn float_overflow() -> F32 {\n    let source = 3.4028236e38\n    let copied = source\n    copied\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer = "3.4028236e38";
    let initializer_start = source.find(initializer).expect("float initializer exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"floating-point literal is out of range for `F32`\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{initializer_start},"))
            && stdout.contains(&format!(
                "\"end\":{{\"byte\":{},",
                initializer_start + initializer.len()
            ))
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "same-family float rounding/range errors must point to the source literal:\n{stdout}"
    );

    let source = "pub fn pointer_sized_copy() -> USize {\n    let source = 4294967296\n    let copied = source\n    copied\n}\n";
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let output = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source.find("4294967296").expect("initializer exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"integer literal is out of range for `USize`\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{initializer_start},"))
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "32-bit USize overflow through a local copy must point to the source literal:\n{stdout}"
    );

    assert_success_result(
        run_check(source, manifest, &[], &["--target", "wasm64-core"]),
        "wasm64-core",
        "dev",
    );
}

#[test]
fn unsupported_unannotated_literal_copy_forms_remain_gated() {
    for source in [
        "pub fn copy() -> I32 {\n    let source = 42\n    let copied = r#source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let source = 42\n    let copied = source + 1\n    copied\n}\n",
        "const SOURCE: I32 = 42\npub fn copy() -> I32 {\n    let source = 7\n    let copied = SOURCE\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let source = 42\n    var copied = source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    var source = 42\n    let copied = source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let var source = 42\n    let copied = source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let source = 42\n    let var copied = source\n    copied\n}\n",
        "pub fn copy() -> I32 {\n    let (source, other) = (42, 7)\n    let copied = source\n    copied\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_flat_unannotated_primitive_local_sequences_emit_check_results() {
    for source in [
        "pub fn copy_chain() -> I8 {\n    let source = 127\n    let first = source\n    let second = first\n    let copied = second\n    copied\n}\n",
        "pub fn copy_source_tail() -> I32 {\n    let source = 42\n    let copied = source\n    source\n}\n",
        "pub fn earlier_tail() -> I8 {\n    let earlier = 42\n    let later = 7\n    earlier\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn flat_unannotated_copy_chain_checks_the_shared_literal_once() {
    let source = "pub fn copy_chain() -> I8 {\n    let source = 128\n    let first = source\n    let second = first\n    let copied = second\n    copied\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source.find("128").expect("integer initializer exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the shared literal must be range-checked once:\n{stdout}"
    );
    assert!(
        stdout.contains("\"message\":\"integer literal is out of range for `I8`\"")
            && !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the I8 tail should constrain the source literal through the copy chain:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        initializer_start,
        initializer_start + "128".len(),
    );
}

#[test]
fn flat_unannotated_primitive_locals_preserve_literal_origins_and_spans() {
    let source = "pub fn multiple_literals() -> I8 {\n    let returned = 127\n    let unused = 1e309\n    let copied = returned\n    copied\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer = "1e309";
    let initializer_start = source
        .find(initializer)
        .expect("unused float initializer exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"floating-point literal is out of range for `F64`\"")
            && !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "only the unused float's F64 default should fail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "floating-point literal is out of range for `F64`",
        initializer_start,
        initializer_start + initializer.len(),
    );

    let source = "pub fn annotation_constrains_literal() -> I8 {\n    let source = 128\n    let narrowed: I8 = source\n    narrowed\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source.find("128").expect("integer initializer exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"integer literal is out of range for `I8`\"")
            && !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the annotation should constrain the original integer literal:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        initializer_start,
        initializer_start + "128".len(),
    );

    let source = "pub fn incompatible_annotation() -> F32 {\n    let source = 18446744073709551616\n    let floating: F32 = source\n    floating\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let path_start = source
        .rfind("source")
        .expect("annotated source path exists");
    let initializer_start = source
        .find("18446744073709551616")
        .expect("integer initializer exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count() == 1
            && stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 1
            && stdout.contains("\"message\":\"expected `F32`, found an integer literal\"")
            && stdout.contains("\"message\":\"integer literal is out of range for `I64`\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "an incompatible annotation should recover its destination while defaulting the source literal:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        path_start,
        path_start + "source".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        initializer_start,
        initializer_start + "18446744073709551616".len(),
    );
}

#[test]
fn unsupported_flat_unannotated_primitive_local_forms_remain_gated() {
    for source in [
        "pub fn self_reference() -> I32 {\n    let source = source\n    source\n}\n",
        "pub fn forward_reference() -> I32 {\n    let copied = later\n    let later = 1\n    copied\n}\n",
        "pub fn computed() -> I32 {\n    let source = 1\n    let copied = source + 1\n    copied\n}\n",
        "pub fn mutable() -> I32 {\n    var source = 1\n    source\n}\n",
        "pub fn destructured() -> I32 {\n    let (source, other) = (1, 2)\n    source\n}\n",
        "pub fn raw_path() -> I32 {\n    let source = 1\n    let copied = r#source\n    copied\n}\n",
        "pub fn qualified_path() -> I32 {\n    let source = 1\n    let copied = values.VALUE\n    copied\n}\n",
        "pub fn generic_path() -> I32 {\n    let source = 1\n    let copied = source<I32>\n    copied\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    let source = concat!(
        "use acme:empty/values as values\n",
        "pub fn qualified_path() -> I32 {\n",
        "    let source = 1\n",
        "    let copied = values.VALUE\n",
        "    copied\n",
        "}\n",
    );
    assert_check_stays_gated(
        &run_check(
            source,
            "",
            &[("src/values.sai", "pub const VALUE: I32 = 42\n")],
            &[],
        ),
        source,
    );
}

#[test]
fn primitive_add_local_initializers_accept_matching_numeric_paths() {
    for source in [
        "pub fn signed(left: I32, right: I32) -> I32 {\n    let sum: I32 = left + right\n    sum\n}\n",
        "pub fn unsigned(left: U64, right: U64) -> U64 {\n    let sum = left + right\n    sum\n}\n",
        "pub fn float32(left: F32, right: F32) -> F32 {\n    let sum: F32 = left + right\n    sum\n}\n",
        "pub fn float64(left: F64, right: F64) -> F64 {\n    let sum = left + right\n    sum\n}\n",
        "pub fn preceding(left: I16, right: I16) -> I16 {\n    let first: I16 = left\n    let sum = first + right\n    sum\n}\n",
        "pub fn shadow(left: I32, right: U32) -> U32 {\n    let left: U32 = 2\n    let sum = left + right\n    sum\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_add_local_type_mismatch_points_right_and_recovers_as_left_type() {
    let source =
        "pub fn mismatch(left: I32, right: U32) -> I32 {\n    let sum = left + right\n    sum\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let right_start = source
        .find("+ right")
        .map(|start| start + "+ ".len())
        .expect("right addition operand exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the right operand should be the only mismatch; the local should recover as I32:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "a supported addition mismatch must not remain behind the body gate:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U32`",
        right_start,
        right_start + "right".len(),
    );
}

#[test]
fn primitive_add_annotation_mismatch_points_to_initializer_and_recovers_to_annotation() {
    let source = "pub fn mismatch(left: I32, right: I32) -> U32 {\n    let sum: U32 = left + right\n    sum\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source
        .find("left + right")
        .expect("addition initializer exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the annotation mismatch should be reported once, with the local recovered as U32:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "an annotated addition mismatch must continue through the local tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        initializer_start,
        initializer_start + "left + right".len(),
    );
}

#[test]
fn primitive_add_local_initializers_accept_a_right_numeric_literal() {
    for source in [
        "pub fn signed(value: I8) -> I8 {\n    let sum: I8 = value + 127\n    sum\n}\n",
        "pub fn signed_min(value: I8) -> I8 {\n    let sum = value + -128\n    sum\n}\n",
        "pub fn unsigned(value: U8) -> U8 {\n    let sum = value + 255\n    sum\n}\n",
        "pub fn float32(value: F32) -> F32 {\n    let sum: F32 = value + 1.25\n    sum\n}\n",
        "pub fn float64(value: F64) -> F64 {\n    let sum = value + 1e308\n    sum\n}\n",
        "pub fn preceding(value: U32) -> U32 {\n    let source: U32 = value\n    let sum = source + 42\n    sum\n}\n",
        "pub fn builtin_int_alias(value: Int) -> Int {\n    let sum = value + 42\n    sum\n}\n",
        "pub fn builtin_uint_alias(value: UInt) -> UInt {\n    let sum = value + 42\n    sum\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_add_local_literal_range_uses_the_selected_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    for source in [
        "pub fn unsigned_edge(value: USize) -> USize {\n    let sum = value + 4294967295\n    sum\n}\n",
        "pub fn signed_min(value: ISize) -> ISize {\n    let sum = value + -2147483648\n    sum\n}\n",
        "pub fn signed_max(value: ISize) -> ISize {\n    let sum = value + 2147483647\n    sum\n}\n",
    ] {
        let output = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "wasm32 boundary should fit:\n{source}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm32-core", "dev");
    }

    for source in [
        "pub fn unsigned_edge(value: USize) -> USize {\n    let sum = value + 18446744073709551615\n    sum\n}\n",
        "pub fn signed_min(value: ISize) -> ISize {\n    let sum = value + -9223372036854775808\n    sum\n}\n",
        "pub fn signed_max(value: ISize) -> ISize {\n    let sum = value + 9223372036854775807\n    sum\n}\n",
    ] {
        let output = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "wasm64 boundary should fit:\n{source}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm64-core", "dev");
    }
}

#[test]
fn primitive_add_local_literal_range_errors_use_literal_spans_and_recover() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    for (source, literal, message) in [
        (
            "pub fn out_of_range(value: USize) -> USize {\n    let sum = value + 4294967296\n    sum\n}\n",
            "4294967296",
            "integer literal is out of range for `USize`",
        ),
        (
            "pub fn out_of_range(value: ISize) -> ISize {\n    let sum = value + 2147483648\n    sum\n}\n",
            "2147483648",
            "integer literal is out of range for `ISize`",
        ),
    ] {
        let output = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let literal_start = source.find(literal).expect("out-of-range literal exists");

        assert_eq!(output.status.code(), Some(1), "{source}\n{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
            1,
            "the right literal should be range-checked at the wasm32 width:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the literal error should recover through the path result and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            literal_start,
            literal_start + literal.len(),
        );
    }

    for source in [
        "pub fn unsigned(value: USize) -> USize {\n    let sum = value + 4294967296\n    sum\n}\n",
        "pub fn signed(value: ISize) -> ISize {\n    let sum = value + 2147483648\n    sum\n}\n",
    ] {
        let output = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "wasm64 right literal should fit:\n{source}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm64-core", "dev");
    }
}

#[test]
fn primitive_add_local_literal_type_and_annotation_errors_have_exact_spans() {
    let source = concat!(
        "pub fn errors(narrow: I8, floating: F32, integer: I32) -> U32 {\n",
        "    let out_of_range = narrow + 128\n",
        "    let int_to_float = floating + 1\n",
        "    let float_out_of_range = floating + 1e39\n",
        "    let float_to_int = integer + 1.5\n",
        "    let annotation: U32 = integer + 1\n",
        "    annotation\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let out_of_range_literal = source.find("128").expect("I8 range literal exists");
    let float_out_of_range_literal = source
        .find("1e39")
        .expect("F32 out-of-range float literal exists");
    let int_to_float_literal = source.find("+ 1\n").map(|start| start + 2).unwrap();
    let float_to_int_literal = source
        .find("+ 1.5")
        .map(|start| start + 2)
        .expect("floating literal exists");
    let annotation_initializer = source
        .rfind("integer + 1")
        .expect("annotated addition exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        3,
        "literal-family and annotation errors should recover without a feature gate:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        2,
        "both numeric literals should be range-checked at the path type:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "literal errors should retain the synthesized left result type:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        out_of_range_literal,
        out_of_range_literal + "128".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "floating-point literal is out of range for `F32`",
        float_out_of_range_literal,
        float_out_of_range_literal + "1e39".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        int_to_float_literal,
        int_to_float_literal + "1".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found a floating-point literal",
        float_to_int_literal,
        float_to_int_literal + "1.5".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        annotation_initializer,
        annotation_initializer + "integer + 1".len(),
    );
}

#[test]
fn primitive_sub_mul_div_local_initializers_accept_right_numeric_literals() {
    for primitive in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize", "F32", "F64",
    ] {
        let literal = if primitive.starts_with('F') {
            "-1.25"
        } else {
            "1"
        };
        let signed_minimum = if primitive == "I8" {
            "    let signed_minimum = value / -128\n"
        } else {
            ""
        };
        let unsigned_negative_zero = if primitive == "U8" {
            "    let unsigned_negative_zero = value / -0\n"
        } else {
            ""
        };
        let source = format!(
            concat!(
                "pub fn arithmetic_{lower}(value: {primitive}) -> {primitive} {{\n",
                "    let sub_param = value - {literal}\n",
                "    let earlier: {primitive} = value\n",
                "    let sub_local: {primitive} = earlier - {literal}\n",
                "    let mul_param = value * {literal}\n",
                "    let mul_local: {primitive} = earlier * {literal}\n",
                "    let div_param = value / {literal}\n",
                "    let div_local: {primitive} = earlier / {literal}\n",
                "{signed_minimum}",
                "{unsigned_negative_zero}",
                "    sub_param\n",
                "}}\n",
            ),
            primitive = primitive,
            literal = literal,
            lower = primitive.to_ascii_lowercase(),
            signed_minimum = signed_minimum,
            unsigned_negative_zero = unsigned_negative_zero,
        );
        let output = run_check(&source, "", &[], &[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "subtraction and multiplication with a right {primitive} literal should accept parameter and preceding-local paths, with inferred and annotated results:\n{source}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_integer_division_by_zero_is_a_type_check_success() {
    let source =
        "pub fn divide(value: I32) -> I32 {\n    let quotient = value / 0\n    quotient\n}\n";
    let output = run_check(source, "", &[], &[]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "zero is a valid integer literal operand; division-by-zero behavior belongs to execution:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_success_result(output, "wasm32-core", "dev");
}

#[test]
fn primitive_integer_remainder_accepts_flat_local_forms_for_every_integer_type() {
    for primitive in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ] {
        let lower = primitive.to_ascii_lowercase();
        let literal = match primitive {
            "I8" => "-128",
            "U8" => "-0",
            _ => "1",
        };
        let forms = [
            (
                "paths",
                format!(
                    "pub fn remainder_{lower}_paths(left: {primitive}, right: {primitive}) -> {primitive} {{\n    let result = left % right\n    result\n}}\n"
                ),
            ),
            (
                "right literal",
                format!(
                    "pub fn remainder_{lower}_right_literal(left: {primitive}) -> {primitive} {{\n    let result = left % 0\n    result\n}}\n"
                ),
            ),
            (
                "left literal",
                format!(
                    "pub fn remainder_{lower}_left_literal(right: {primitive}) -> {primitive} {{\n    let result = {literal} % right\n    result\n}}\n"
                ),
            ),
        ];
        for (form, source) in forms {
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "integer remainder should accept matching {primitive} {form}, including zero divisors at type-check time:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }
}

#[test]
fn primitive_integer_remainder_checks_signed_unsigned_and_pointer_width_ranges() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let sign_source = concat!(
        "pub fn sign_ranges(signed: I8, unsigned: U8) -> I8 {\n",
        "    let signed_minimum = -128 % signed\n",
        "    let unsigned_negative_zero = -0 % unsigned\n",
        "    let signed_out_of_range = -129 % signed\n",
        "    let unsigned_out_of_range = -1 % unsigned\n",
        "    signed_minimum\n",
        "}\n",
    );
    let sign_output = run_check(sign_source, "", &[], &[]);
    let sign_stdout = String::from_utf8(sign_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(sign_output.status.code(), Some(1), "{sign_stdout}");
    assert_eq!(
        sign_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        2,
        "signed and unsigned boundaries should be checked:\n{sign_stdout}"
    );
    for (literal, message) in [
        ("-129", "integer literal is out of range for `I8`"),
        ("-1 % unsigned", "integer literal is out of range for `U8`"),
    ] {
        let start = sign_source
            .find(literal)
            .expect("out-of-range literal exists");
        let end = if literal == "-1 % unsigned" {
            start + "-1".len()
        } else {
            start + literal.len()
        };
        assert_json_diagnostic_primary_span(
            &sign_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            end,
        );
    }

    let pointer_source = concat!(
        "pub fn pointer_ranges(unsigned: USize, signed: ISize) -> ISize {\n",
        "    let unsigned_32 = 4294967295 % unsigned\n",
        "    let signed_minimum_32 = -2147483648 % signed\n",
        "    let unsigned_64 = 18446744073709551615 % unsigned\n",
        "    let signed_minimum_64 = -9223372036854775808 % signed\n",
        "    signed_minimum_32\n",
        "}\n",
    );
    let wasm32 = run_check(pointer_source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        2,
        "pointer-sized remainder literals must use wasm32 bounds:\n{wasm32_stdout}"
    );
    for (literal, message) in [
        (
            "18446744073709551615",
            "integer literal is out of range for `USize`",
        ),
        (
            "-9223372036854775808",
            "integer literal is out of range for `ISize`",
        ),
    ] {
        let start = pointer_source
            .find(literal)
            .expect("out-of-range pointer literal exists");
        assert_json_diagnostic_primary_span(
            &wasm32_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let wasm64 = run_check(pointer_source, manifest, &[], &["--target", "wasm64-core"]);
    assert_eq!(
        wasm64.status.code(),
        Some(0),
        "pointer-sized remainder literals should fit wasm64:\n{}",
        String::from_utf8_lossy(&wasm64.stdout)
    );
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn primitive_integer_remainder_operand_and_annotation_mismatches_recover() {
    let right_source = concat!(
        "pub fn right_mismatch(left: I32, right: U32) -> I32 {\n",
        "    let result = left % right\n",
        "    result\n",
        "}\n",
    );
    let right_output = run_check(right_source, "", &[], &[]);
    let right_stdout = String::from_utf8(right_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(right_output.status.code(), Some(1), "{right_stdout}");
    assert_eq!(
        right_stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "{right_stdout}"
    );
    assert!(
        !right_stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !right_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !right_stdout.contains("\"type\":\"result\""),
        "integer operand mismatches should recover without gating or float-operator errors:\n{right_stdout}"
    );
    let right = right_source
        .find("% right")
        .map(|start| start + "% ".len())
        .expect("right operand exists");
    assert_json_diagnostic_primary_span(
        &right_stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U32`",
        right,
        right + "right".len(),
    );

    let annotation_source = concat!(
        "pub fn annotation_mismatch(left: I32, right: I32) -> U32 {\n",
        "    let result: U32 = left % right\n",
        "    result\n",
        "}\n",
    );
    let annotation_output = run_check(annotation_source, "", &[], &[]);
    let annotation_stdout =
        String::from_utf8(annotation_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        annotation_output.status.code(),
        Some(1),
        "{annotation_stdout}"
    );
    assert_eq!(
        annotation_stdout
            .matches("\"code\":\"TYPE_MISMATCH\"")
            .count(),
        1,
        "{annotation_stdout}"
    );
    assert!(
        !annotation_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !annotation_stdout.contains("\"type\":\"result\""),
        "integer annotation mismatch should recover to U32 and reach the tail:\n{annotation_stdout}"
    );
    let annotation = annotation_source
        .find("left % right")
        .expect("annotated remainder exists");
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        annotation,
        annotation + "left % right".len(),
    );
}

#[test]
fn primitive_float_remainder_reports_operator_help_and_recovers() {
    let cases = [
        (
            concat!(
                "pub fn float_paths(left: F32, right: F32) -> F32 {\n",
                "    let result = left % right\n",
                "    result\n",
                "}\n",
            ),
            "left % right",
            "operator `%` is not defined for `F32`",
            None,
        ),
        (
            concat!(
                "pub fn float_right_literal(left: F32) -> F32 {\n",
                "    let result = left % 1\n",
                "    result\n",
                "}\n",
            ),
            "left % 1",
            "operator `%` is not defined for `F32`",
            Some(("expected `F32`, found an integer literal", 7, 1)),
        ),
        (
            concat!(
                "pub fn float_left_literal(right: F64) -> F64 {\n",
                "    let result = 1.5 % right\n",
                "    result\n",
                "}\n",
            ),
            "1.5 % right",
            "operator `%` is not defined for `F64`",
            None,
        ),
        (
            concat!(
                "pub fn float_path_mismatch(left: F32, right: F64) -> F32 {\n",
                "    let result = left % right\n",
                "    result\n",
                "}\n",
            ),
            "left % right",
            "operator `%` is not defined for `F32`",
            Some(("expected `F32`, found `F64`", 7, 5)),
        ),
        (
            concat!(
                "pub fn float_literal_path_mismatch(right: I32) -> F64 {\n",
                "    let result = 1.5 % right\n",
                "    result\n",
                "}\n",
            ),
            "1.5 % right",
            "operator `%` is not defined for floating-point values",
            Some(("expected `F64`, found `I32`", 6, 5)),
        ),
        (
            concat!(
                "pub fn float_literal_path_mismatch(right: I32) -> F32 {\n",
                "    let result: F32 = 1.5 % right\n",
                "    result\n",
                "}\n",
            ),
            "1.5 % right",
            "operator `%` is not defined for floating-point values",
            Some(("expected `F64`, found `I32`", 6, 5)),
        ),
    ];
    for (source, expression, operator_message, mismatch) in cases {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        let operator_records = stdout
            .lines()
            .filter(|record| record.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\""))
            .collect::<Vec<_>>();
        assert_eq!(operator_records.len(), 1, "{stdout}");
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "supported flat float forms should report the operator error instead of being gated:\n{stdout}"
        );
        let operator_record = operator_records[0];
        assert!(
            operator_record.contains("\"data\":{}")
                && operator_record.contains(&format!(
                    "\"message\":\"use the floating-point `rem` method (for example, `left.rem(right)`)"
                )),
            "{operator_record}"
        );
        let expression_start = source
            .find(expression)
            .expect("remainder expression exists");
        let expression_end = expression_start + expression.len();
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            operator_message,
            expression_start,
            expression_end,
        );
        if let Some((message, offset, length)) = mismatch {
            assert_json_diagnostic_primary_span(
                &stdout,
                "TYPE_MISMATCH",
                message,
                expression_start + offset,
                expression_start + offset + length,
            );
        }
        if source.contains("float_literal_path_mismatch") {
            assert!(
                !stdout.contains("expected `F32`, found `F64`"),
                "the annotation should resolve the literal without retroactively making the unsupported-operator diagnostic claim F64:\n{stdout}"
            );
        }
    }
}

#[test]
fn primitive_integer_wrapping_operators_accept_matching_path_pairs() {
    let primitives = [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ];
    let operators = [("add", "+%"), ("sub", "-%"), ("mul", "*%")];
    for (name, operator) in operators {
        for primitive in primitives {
            let lower = primitive.to_ascii_lowercase();
            let source = format!(
                concat!(
                    "pub fn wrapping_{name}_{lower}(left: {primitive}, right: {primitive}) -> {primitive} {{\n",
                    "    let result = left {operator} right\n",
                    "    result\n",
                    "}}\n",
                ),
                name = name,
                lower = lower,
                primitive = primitive,
                operator = operator,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{operator} should accept matching {primitive} paths:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }

        let shadow_source = format!(
            concat!(
                "pub fn wrapping_{name}_shadow(left: I32, right: I32) -> I32 {{\n",
                "    let left: I32 = right\n",
                "    let result = left {operator} right\n",
                "    result\n",
                "}}\n",
            ),
            name = name,
            operator = operator,
        );
        let shadow_output = run_check(&shadow_source, "", &[], &[]);
        assert_eq!(
            shadow_output.status.code(),
            Some(0),
            "shadowed path resolution should preserve binding identity for {operator}:\n{shadow_source}\n{}",
            String::from_utf8_lossy(&shadow_output.stdout)
        );
        assert_success_result(shadow_output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_wrapping_operators_recover_path_and_annotation_mismatches() {
    let path_source = concat!(
        "pub fn wrapping_path_mismatch(left: I32, right: U32) -> I32 {\n",
        "    let result = left +% right\n",
        "    result\n",
        "}\n",
    );
    let path_output = run_check(path_source, "", &[], &[]);
    let path_stdout = String::from_utf8(path_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(path_output.status.code(), Some(1), "{path_stdout}");
    assert_eq!(
        path_stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "{path_stdout}"
    );
    assert!(
        !path_stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !path_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !path_stdout.contains("\"type\":\"result\""),
        "an integer wrapping path mismatch should recover to the left type:\n{path_stdout}"
    );
    let right_start = path_source
        .find("+% right")
        .map(|start| start + "+% ".len())
        .expect("right path exists");
    assert_json_diagnostic_primary_span(
        &path_stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U32`",
        right_start,
        right_start + "right".len(),
    );

    let annotation_source = concat!(
        "pub fn wrapping_annotation_mismatch(left: I32, right: I32) -> U32 {\n",
        "    let result: U32 = left -% right\n",
        "    result\n",
        "}\n",
    );
    let annotation_output = run_check(annotation_source, "", &[], &[]);
    let annotation_stdout =
        String::from_utf8(annotation_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        annotation_output.status.code(),
        Some(1),
        "{annotation_stdout}"
    );
    assert_eq!(
        annotation_stdout
            .matches("\"code\":\"TYPE_MISMATCH\"")
            .count(),
        1,
        "{annotation_stdout}"
    );
    assert!(
        !annotation_stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !annotation_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !annotation_stdout.contains("\"type\":\"result\""),
        "an annotation mismatch should recover the wrapping local to its declaration:\n{annotation_stdout}"
    );
    let expression_start = annotation_source
        .find("left -% right")
        .expect("wrapping expression exists");
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        expression_start,
        expression_start + "left -% right".len(),
    );
}

#[test]
fn non_integer_primitive_wrapping_operators_report_operator_not_supported() {
    for (name, operator) in [("add", "+%"), ("sub", "-%"), ("mul", "*%")] {
        for primitive in ["F32", "F64", "Bool", "Byte", "Char"] {
            let source = format!(
                concat!(
                    "pub fn wrapping_{name}_{lower}(left: {primitive}, right: {primitive}) -> {primitive} {{\n",
                    "    let result = left {operator} right\n",
                    "    result\n",
                    "}}\n",
                ),
                name = name,
                operator = operator,
                primitive = primitive,
                lower = primitive.to_ascii_lowercase(),
            );
            let output = run_check(&source, "", &[], &[]);
            let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
            assert_eq!(output.status.code(), Some(1), "{stdout}");
            assert_eq!(
                stdout
                    .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                    .count(),
                1,
                "{stdout}"
            );
            assert!(
                !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                    && !stdout.contains("\"type\":\"result\""),
                "known non-integer primitive `{primitive}` should report the unsupported operator:\n{source}\n{stdout}"
            );
            let expression_start = source
                .find(&format!("left {operator} right"))
                .expect("wrapping expression exists");
            let expression_end = expression_start + format!("left {operator} right").len();
            assert_json_diagnostic_primary_span(
                &stdout,
                "OPERATOR_NOT_SUPPORTED",
                &format!("operator `{operator}` is not defined for `{primitive}`"),
                expression_start,
                expression_end,
            );
        }
    }

    let mismatch_source = concat!(
        "pub fn wrapping_float_mismatch(left: F32, right: F64) -> F32 {\n",
        "    let result = left *% right\n",
        "    result\n",
        "}\n",
    );
    let mismatch_output = run_check(mismatch_source, "", &[], &[]);
    let mismatch_stdout = String::from_utf8(mismatch_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(mismatch_output.status.code(), Some(1), "{mismatch_stdout}");
    assert_eq!(
        mismatch_stdout
            .matches("\"code\":\"TYPE_MISMATCH\"")
            .count(),
        1,
        "{mismatch_stdout}"
    );
    assert_eq!(
        mismatch_stdout
            .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            .count(),
        1,
        "{mismatch_stdout}"
    );
    let expression_start = mismatch_source
        .find("left *% right")
        .expect("wrapping expression exists");
    assert_json_diagnostic_primary_span(
        &mismatch_stdout,
        "OPERATOR_NOT_SUPPORTED",
        "operator `*%` is not defined for `F32`",
        expression_start,
        expression_start + "left *% right".len(),
    );
    let right_start = mismatch_source
        .find("*% right")
        .map(|start| start + "*% ".len())
        .expect("right path exists");
    assert_json_diagnostic_primary_span(
        &mismatch_stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found `F64`",
        right_start,
        right_start + "right".len(),
    );

    let char_mismatch_source = concat!(
        "pub fn wrapping_char_mismatch(left: Char, right: Bool) -> Char {\n",
        "    let result = left +% right\n",
        "    result\n",
        "}\n",
    );
    let char_mismatch_output = run_check(char_mismatch_source, "", &[], &[]);
    let char_mismatch_stdout =
        String::from_utf8(char_mismatch_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        char_mismatch_output.status.code(),
        Some(1),
        "{char_mismatch_stdout}"
    );
    assert_eq!(
        char_mismatch_stdout
            .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            .count(),
        1,
        "{char_mismatch_stdout}"
    );
    assert!(
        !char_mismatch_stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !char_mismatch_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "Char follows the missing-trait operator error without a synthesized right mismatch:\n{char_mismatch_stdout}"
    );
}

#[test]
fn primitive_integer_wrapping_operators_accept_right_integer_literals() {
    let primitives = [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ];
    let operators = [("add", "+%"), ("sub", "-%"), ("mul", "*%")];
    for (name, operator) in operators {
        for primitive in primitives {
            let lower = primitive.to_ascii_lowercase();
            let source = format!(
                concat!(
                    "pub fn wrapping_{name}_{lower}(left: {primitive}) -> {primitive} {{\n",
                    "    let result = left {operator} 1\n",
                    "    result\n",
                    "}}\n",
                ),
                name = name,
                lower = lower,
                primitive = primitive,
                operator = operator,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{operator} should accept a right integer literal for {primitive}:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }

        let shadow_source = format!(
            concat!(
                "pub fn wrapping_{name}_shadow(left: I32, right: I32) -> I32 {{\n",
                "    let left: I32 = right\n",
                "    let result = left {operator} -0\n",
                "    result\n",
                "}}\n",
            ),
            name = name,
            operator = operator,
        );
        let shadow_output = run_check(&shadow_source, "", &[], &[]);
        assert_eq!(
            shadow_output.status.code(),
            Some(0),
            "a shadowed local should remain eligible with a folded right literal for {operator}:\n{shadow_source}\n{}",
            String::from_utf8_lossy(&shadow_output.stdout)
        );
        assert_success_result(shadow_output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_integer_wrapping_operators_accept_left_integer_literals() {
    let primitives = [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ];
    let operators = [("add", "+%"), ("sub", "-%"), ("mul", "*%")];
    for (name, operator) in operators {
        for primitive in primitives {
            let literal = match primitive {
                "I8" => "-128",
                "U8" => "-0",
                _ => "42",
            };
            let lower = primitive.to_ascii_lowercase();
            let source = format!(
                concat!(
                    "pub fn wrapping_{name}_{lower}(value: {primitive}) -> {primitive} {{\n",
                    "    let result = {literal} {operator} value\n",
                    "    result\n",
                    "}}\n",
                ),
                name = name,
                lower = lower,
                primitive = primitive,
                literal = literal,
                operator = operator,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{operator} should accept a left integer literal for {primitive}:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }

        let shadow_source = format!(
            concat!(
                "pub fn wrapping_{name}_shadow(left: I32, right: I32) -> I32 {{\n",
                "    let right: I32 = left\n",
                "    let result = -0 {operator} right\n",
                "    result\n",
                "}}\n",
            ),
            name = name,
            operator = operator,
        );
        let shadow_output = run_check(&shadow_source, "", &[], &[]);
        assert_eq!(
            shadow_output.status.code(),
            Some(0),
            "a shadowed local should remain eligible with a folded left literal for {operator}:\n{shadow_source}\n{}",
            String::from_utf8_lossy(&shadow_output.stdout)
        );
        assert_success_result(shadow_output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_wrapping_left_integer_literals_check_right_operand_first() {
    let mut source = String::from(
        "pub fn wrapping_non_integer_rhs(f32_value: F32, f64_value: F64, byte_value: Byte, bool_value: Bool, char_value: Char) -> I64 {\n",
    );
    let operators = [("add", "+%"), ("sub", "-%"), ("mul", "*%")];
    let right_operands = [
        ("f32_value", "F32"),
        ("f64_value", "F64"),
        ("byte_value", "Byte"),
        ("bool_value", "Bool"),
        ("char_value", "Char"),
    ];
    for (name, operator) in operators {
        for (right, _) in right_operands {
            source.push_str(&format!(
                "    let mismatch_{name}_{right} = 1 {operator} {right}\n"
            ));
        }
    }
    source.push_str("    let result: I64 = 0\n    result\n}\n");

    let output = run_check(&source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        15,
        "each non-integer right path should be checked against the unresolved integer left literal:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "an integer literal left operand keeps integer operator selection after a right-family mismatch:\n{stdout}"
    );
    for (name, operator) in operators {
        for (right, primitive) in right_operands {
            let expression = format!("1 {operator} {right}");
            let start = source
                .find(&expression)
                .map(|start| start + format!("1 {operator} ").len())
                .expect("right path exists");
            assert_json_diagnostic_primary_span(
                &stdout,
                "TYPE_MISMATCH",
                &format!("expected `I64`, found `{primitive}`"),
                start,
                start + right.len(),
            );
            assert!(source.contains(&format!("mismatch_{name}_{right} = {expression}")));
        }
    }
}

#[test]
fn primitive_wrapping_integer_literals_check_ranges_and_recover_annotations() {
    let range_source = concat!(
        "pub fn wrapping_literal_ranges(signed: I8, unsigned: U8) -> I8 {\n",
        "    let signed_minimum = signed +% -128\n",
        "    let signed_maximum = signed -% 127\n",
        "    let unsigned_maximum = unsigned +% 255\n",
        "    let unsigned_negative_zero = unsigned -% -0\n",
        "    let signed_out_of_range = signed +% -129\n",
        "    let unsigned_negative = unsigned *% -1\n",
        "    signed_minimum\n",
        "}\n",
    );
    let range_output = run_check(range_source, "", &[], &[]);
    let range_stdout = String::from_utf8(range_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(range_output.status.code(), Some(1), "{range_stdout}");
    assert_eq!(
        range_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        2,
        "{range_stdout}"
    );
    for (literal, message) in [
        ("-129", "integer literal is out of range for `I8`"),
        ("-1", "integer literal is out of range for `U8`"),
    ] {
        let start = range_source
            .rfind(literal)
            .expect("out-of-range literal exists");
        assert_json_diagnostic_primary_span(
            &range_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let annotation_source = concat!(
        "pub fn wrapping_literal_annotation(left: I32) -> U32 {\n",
        "    let result: U32 = left +% 1\n",
        "    result\n",
        "}\n",
    );
    let annotation_output = run_check(annotation_source, "", &[], &[]);
    let annotation_stdout =
        String::from_utf8(annotation_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        annotation_output.status.code(),
        Some(1),
        "{annotation_stdout}"
    );
    assert_eq!(
        annotation_stdout
            .matches("\"code\":\"TYPE_MISMATCH\"")
            .count(),
        1,
        "{annotation_stdout}"
    );
    assert!(
        !annotation_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !annotation_stdout.contains("\"type\":\"result\""),
        "the initializer should recover to its supported annotation:\n{annotation_stdout}"
    );
    let expression_start = annotation_source
        .find("left +% 1")
        .expect("wrapping initializer exists");
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        expression_start,
        expression_start + "left +% 1".len(),
    );
}

#[test]
fn primitive_wrapping_integer_literals_use_target_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let source = concat!(
        "pub fn wrapping_pointer_ranges(unsigned: USize, signed: ISize) -> ISize {\n",
        "    let unsigned_32 = unsigned +% 4294967295\n",
        "    let signed_minimum_32 = signed -% -2147483648\n",
        "    let unsigned_64 = unsigned *% 18446744073709551615\n",
        "    let signed_minimum_64 = signed +% -9223372036854775808\n",
        "    let unsigned_32_out_of_range = unsigned +% 4294967296\n",
        "    let signed_32_out_of_range = signed -% -2147483649\n",
        "    signed_minimum_32\n",
        "}\n",
    );
    let wasm32 = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        4,
        "{wasm32_stdout}"
    );
    for (literal, message) in [
        (
            "18446744073709551615",
            "integer literal is out of range for `USize`",
        ),
        (
            "-9223372036854775808",
            "integer literal is out of range for `ISize`",
        ),
        ("4294967296", "integer literal is out of range for `USize`"),
        ("-2147483649", "integer literal is out of range for `ISize`"),
    ] {
        let start = source.find(literal).expect("pointer-sized literal exists");
        assert_json_diagnostic_primary_span(
            &wasm32_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let wasm64 = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn primitive_wrapping_left_integer_literals_check_ranges_and_recover_annotations() {
    let range_source = concat!(
        "pub fn wrapping_left_literal_ranges(signed: I8, unsigned: U8) -> I8 {\n",
        "    let signed_minimum = -128 +% signed\n",
        "    let signed_maximum = 127 -% signed\n",
        "    let unsigned_maximum = 255 +% unsigned\n",
        "    let unsigned_negative_zero = -0 -% unsigned\n",
        "    let signed_out_of_range = -129 +% signed\n",
        "    let unsigned_negative = -1 *% unsigned\n",
        "    signed_minimum\n",
        "}\n",
    );
    let range_output = run_check(range_source, "", &[], &[]);
    let range_stdout = String::from_utf8(range_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(range_output.status.code(), Some(1), "{range_stdout}");
    assert_eq!(
        range_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        2,
        "{range_stdout}"
    );
    for (literal, message) in [
        ("-129", "integer literal is out of range for `I8`"),
        ("-1", "integer literal is out of range for `U8`"),
    ] {
        let start = range_source
            .rfind(literal)
            .expect("out-of-range literal exists");
        assert_json_diagnostic_primary_span(
            &range_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let annotation_source = concat!(
        "pub fn wrapping_left_literal_annotation(float_value: F32, narrow: I8) -> I8 {\n",
        "    let annotation_mismatch: F32 = 1 +% float_value\n",
        "    let typed_range: I8 = 18446744073709551616 +% float_value\n",
        "    typed_range\n",
        "}\n",
    );
    let annotation_output = run_check(annotation_source, "", &[], &[]);
    let annotation_stdout =
        String::from_utf8(annotation_output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        annotation_output.status.code(),
        Some(1),
        "{annotation_stdout}"
    );
    assert_eq!(
        annotation_stdout
            .matches("\"code\":\"TYPE_MISMATCH\"")
            .count(),
        3,
        "{annotation_stdout}"
    );
    assert_eq!(
        annotation_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        1,
        "{annotation_stdout}"
    );
    assert!(
        !annotation_stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !annotation_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !annotation_stdout.contains("\"type\":\"result\""),
        "annotations should recover after the right operand and constrain the left literal:\n{annotation_stdout}"
    );
    let first_right = annotation_source
        .find("+% float_value")
        .map(|start| start + "+% ".len())
        .expect("first mismatched path exists");
    let second_operator = annotation_source
        .find("18446744073709551616 +% float_value")
        .expect("second expression exists");
    let second_right = second_operator + "18446744073709551616 +% ".len();
    let first_expression = annotation_source
        .find("1 +% float_value")
        .expect("annotated expression exists");
    let large_literal = annotation_source
        .find("18446744073709551616")
        .expect("large literal exists");
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        first_right,
        first_right + "float_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        first_expression,
        first_expression + "1 +% float_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        second_right,
        second_right + "float_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &annotation_stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        large_literal,
        large_literal + "18446744073709551616".len(),
    );
}

#[test]
fn primitive_wrapping_left_integer_literals_use_target_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let source = concat!(
        "pub fn wrapping_left_pointer_ranges(unsigned: USize, signed: ISize) -> ISize {\n",
        "    let unsigned_32 = 4294967295 +% unsigned\n",
        "    let signed_minimum_32 = -2147483648 -% signed\n",
        "    let unsigned_64 = 18446744073709551615 *% unsigned\n",
        "    let signed_minimum_64 = -9223372036854775808 +% signed\n",
        "    let unsigned_32_out_of_range = 4294967296 +% unsigned\n",
        "    let signed_32_out_of_range = -2147483649 -% signed\n",
        "    signed_minimum_32\n",
        "}\n",
    );
    let wasm32 = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        4,
        "{wasm32_stdout}"
    );
    for (literal, message) in [
        (
            "18446744073709551615",
            "integer literal is out of range for `USize`",
        ),
        (
            "-9223372036854775808",
            "integer literal is out of range for `ISize`",
        ),
        ("4294967296", "integer literal is out of range for `USize`"),
        ("-2147483649", "integer literal is out of range for `ISize`"),
    ] {
        let start = source.find(literal).expect("pointer-sized literal exists");
        assert_json_diagnostic_primary_span(
            &wasm32_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let wasm64 = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn primitive_wrapping_left_integer_literal_defaulting_checks_range_after_mismatch() {
    let source = concat!(
        "pub fn wrapping_left_literal_default(floating: F32, result: I64) -> I64 {\n",
        "    let too_large = 18446744073709551616 +% floating\n",
        "    result\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal = source
        .find("18446744073709551616")
        .expect("large literal exists");
    let path = source
        .find("+% floating")
        .map(|start| start + "+% ".len())
        .expect("mismatched float path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "an unconstrained left integer literal should default independently of the right-path mismatch:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        path,
        path + "floating".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        literal,
        literal + "18446744073709551616".len(),
    );
}

#[test]
fn primitive_arithmetic_local_initializers_accept_left_numeric_literals() {
    for (operator_name, operator) in [("add", "+"), ("sub", "-"), ("mul", "*"), ("div", "/")] {
        for primitive in [
            "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize", "F32", "F64",
        ] {
            let literal = match primitive {
                "I8" => "-128",
                "U8" => "-0",
                "F32" | "F64" => "-1.25",
                _ => "42",
            };
            let source = format!(
                concat!(
                    "pub fn {operator_name}_{lower}(value: {primitive}) -> {primitive} {{\n",
                    "    let from_parameter: {primitive} = {literal} {operator} value\n",
                    "    let earlier: {primitive} = value\n",
                    "    let from_local = {literal} {operator} earlier\n",
                    "    from_local\n",
                    "}}\n",
                ),
                operator_name = operator_name,
                lower = primitive.to_ascii_lowercase(),
                primitive = primitive,
                literal = literal,
                operator = operator,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{operator_name} with a left {primitive} literal should infer from parameter and preceding-local paths:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }
}

#[test]
fn primitive_arithmetic_left_literal_mismatches_keep_context_and_recover() {
    let source = concat!(
        "pub fn mismatches(f32_value: F32, f64_value: F64, integer: I32, narrow: I8) -> F32 {\n",
        "    let constrained = 7 + f32_value\n",
        "    let constrained_copy: I8 = constrained\n",
        "    let bad_annotation: F32 = 8 - f64_value\n",
        "    let float_mismatch = 1.5 * integer\n",
        "    let division_mismatch = 1.5 / narrow\n",
        "    bad_annotation\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let first_path = source
        .find("+ f32_value")
        .map(|start| start + "+ ".len())
        .expect("first mismatched path exists");
    let bad_annotation_path = source
        .find("- f64_value")
        .map(|start| start + "- ".len())
        .expect("annotated mismatched path exists");
    let bad_annotation = source
        .find("8 - f64_value")
        .expect("bad annotation initializer exists");
    let float_path = source
        .find("* integer")
        .map(|start| start + "* ".len())
        .expect("float literal family mismatch path exists");
    let division_path = source
        .find("/ narrow")
        .map(|start| start + "/ ".len())
        .expect("division literal family mismatch path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        5,
        "family and annotation mismatches should be reported while later context is preserved:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "mismatches should recover through later annotations and the function tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        first_path,
        first_path + "f32_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F64`",
        bad_annotation_path,
        bad_annotation_path + "f64_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        bad_annotation,
        bad_annotation + "8 - f64_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F64`, found `I32`",
        float_path,
        float_path + "integer".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F64`, found `I8`",
        division_path,
        division_path + "narrow".len(),
    );
}

#[test]
fn primitive_arithmetic_left_literal_defaulting_checks_range_after_family_mismatch() {
    let source = concat!(
        "pub fn default_on_failure(floating: F32, result: I64) -> I64 {\n",
        "    let too_large = 18446744073709551616 / floating\n",
        "    result\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal = source
        .find("18446744073709551616")
        .expect("large integer literal exists");
    let path = source
        .find("/ floating")
        .map(|start| start + "/ ".len())
        .expect("mismatched float path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the cross-family error should point to the path:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the unresolved integer literal should default and range-check at body end:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "failed family unification should retain a defaultable literal result:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        path,
        path + "floating".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        literal,
        literal + "18446744073709551616".len(),
    );
}

#[test]
fn primitive_arithmetic_left_literal_tail_constrains_after_family_mismatch() {
    let source = concat!(
        "pub fn tail_constraint(floating: F32) -> I8 {\n",
        "    let result = 7 / floating\n",
        "    result\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let path = source
        .find("/ floating")
        .map(|start| start + "/ ".len())
        .expect("mismatched path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the tail should constrain the unresolved left integer literal:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the failed operand match should retain a usable literal result type:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        path,
        path + "floating".len(),
    );
}

#[test]
fn primitive_arithmetic_left_negative_literals_report_signed_range_spans() {
    let source = concat!(
        "pub fn negative_ranges(signed: I8, wide: I64, unsigned: U8, result: I8) -> I8 {\n",
        "    let signed_range = -9223372036854775809 + wide\n",
        "    let unsigned_range = -1 * unsigned\n",
        "    let division_range = -129 / signed\n",
        "    result\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let signed = source
        .find("-9223372036854775809")
        .expect("out-of-range signed literal exists");
    let unsigned = source
        .find("-1 * unsigned")
        .expect("out-of-range unsigned literal exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        3,
        "both left negative literals should be range-checked:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "negative literal range errors should recover to each right path type:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        signed,
        signed + "-9223372036854775809".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `U8`",
        unsigned,
        unsigned + "-1".len(),
    );
    let division = source
        .find("-129 / signed")
        .expect("division literal exists");
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        division,
        division + "-129".len(),
    );
}

#[test]
fn primitive_arithmetic_left_literal_range_uses_the_selected_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let source = concat!(
        "pub fn bounds(unsigned: USize, signed: ISize) -> USize {\n",
        "    let unsigned_32 = 4294967295 + unsigned\n",
        "    let signed_min_32 = -2147483648 + signed\n",
        "    let div_unsigned_64 = 18446744073709551615 / unsigned\n",
        "    let div_signed_min_64 = -9223372036854775808 / signed\n",
        "    unsigned_32\n",
        "}\n",
    );
    let wasm32 = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        2,
        "out-of-range division literals should use wasm32 bounds:\n{wasm32_stdout}"
    );
    let div_unsigned = source
        .find("18446744073709551615 / unsigned")
        .expect("division USize literal exists");
    assert_json_diagnostic_primary_span(
        &wasm32_stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `USize`",
        div_unsigned,
        div_unsigned + "18446744073709551615".len(),
    );
    let div_signed = source
        .find("-9223372036854775808 / signed")
        .expect("division ISize literal exists");
    assert_json_diagnostic_primary_span(
        &wasm32_stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `ISize`",
        div_signed,
        div_signed + "-9223372036854775808".len(),
    );

    let wasm64 = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
    assert_eq!(
        wasm64.status.code(),
        Some(0),
        "all left literals should fit wasm64:\n{}",
        String::from_utf8_lossy(&wasm64.stdout)
    );
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn primitive_arithmetic_local_literal_errors_use_literal_spans_and_recover() {
    let source = concat!(
        "pub fn errors(narrow: I8, unsigned: U8, floating: F32, integer: I32) -> U32 {\n",
        "    let integer_range = narrow / -129\n",
        "    let unsigned_negative_range = unsigned / -1\n",
        "    let int_to_float = floating / 1\n",
        "    let float_range = floating / 1e39\n",
        "    let float_to_int = integer / 1.5\n",
        "    let annotation: U32 = integer / 1\n",
        "    let annotation_copy: U32 = annotation\n",
        "    annotation_copy\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let span_for = |token: &str| {
        let start = source.find(token).expect("diagnostic token exists");
        (start, start + token.len())
    };
    let int_range = span_for("-129");
    let unsigned_negative_range_start = source
        .find("unsigned / -1")
        .map(|start| start + "unsigned / ".len())
        .expect("negative U8 literal exists");
    let unsigned_negative_range = (
        unsigned_negative_range_start,
        unsigned_negative_range_start + 2,
    );
    let int_to_float_start = source
        .find("floating / 1")
        .map(|start| start + "floating / ".len())
        .expect("integer literal against F32 exists");
    let int_to_float = (int_to_float_start, int_to_float_start + "1".len());
    let float_range = span_for("1e39");
    let float_to_int = span_for("1.5");
    let annotation = source
        .find("annotation: U32 = integer / 1")
        .map(|start| start + "annotation: U32 = ".len())
        .expect("annotated division initializer exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        3,
        "literal-family and annotation diagnostics should be reported:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        3,
        "integer and float literal ranges should be checked:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "operator and annotation errors should recover through the local tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        int_range.0,
        int_range.1,
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `U8`",
        unsigned_negative_range.0,
        unsigned_negative_range.1,
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        int_to_float.0,
        int_to_float.1,
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "floating-point literal is out of range for `F32`",
        float_range.0,
        float_range.1,
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found a floating-point literal",
        float_to_int.0,
        float_to_int.1,
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        annotation,
        annotation + "integer / 1".len(),
    );
}

#[test]
fn primitive_arithmetic_local_literal_range_uses_the_selected_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let source = concat!(
        "pub fn bounds(unsigned: USize, signed: ISize) -> ISize {\n",
        "    let quotient_unsigned = unsigned / 4294967296\n",
        "    let quotient_signed = signed / 2147483648\n",
        "    quotient_signed\n",
        "}\n",
    );
    let wasm32 = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        2,
        "both division literals must use wasm32 pointer-width bounds:\n{wasm32_stdout}"
    );
    for (literal, message) in [
        ("4294967296", "integer literal is out of range for `USize`"),
        ("2147483648", "integer literal is out of range for `ISize`"),
    ] {
        let start = source.find(literal).expect("pointer-sized literal exists");
        assert_json_diagnostic_primary_span(
            &wasm32_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let wasm64 = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
    assert_eq!(
        wasm64.status.code(),
        Some(0),
        "both literals should fit wasm64:\n{}",
        String::from_utf8_lossy(&wasm64.stdout)
    );
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn unsupported_primitive_add_local_literal_shapes_remain_gated() {
    for source in [
        "pub fn fixed_literal(value: I32) -> I32 {\n    let result = value + true\n    result\n}\n",
        "pub fn fixed_literal_left(value: I32) -> I32 {\n    let result = true + value\n    result\n}\n",
        "pub fn byte_path(value: Byte) -> Byte {\n    let result = value + 1\n    result\n}\n",
        "pub fn unresolved_number(value: I32) -> I32 {\n    let number = 1\n    let result = value + number\n    result\n}\n",
        "pub fn nested(value: I32) -> I32 {\n    let result = value + (1 + 1)\n    result\n}\n",
        "pub fn computed_left(value: I32) -> I32 {\n    let result = (value + 1) + value\n    result\n}\n",
        "pub fn forward(value: I32) -> I32 {\n    let result = later + 1\n    let later = value\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn primitive_sub_mul_div_local_initializers_accept_integer_and_float_paths() {
    for (operator_name, operator) in [("sub", "-"), ("mul", "*"), ("div", "/")] {
        for primitive in [
            "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize", "F32", "F64",
        ] {
            let source = format!(
                "pub fn {operator_name}_{}(left: {primitive}, right: {primitive}) -> {primitive} {{\n    let earlier: {primitive} = left\n    let result: {primitive} = earlier {operator} right\n    result\n}}\n",
                primitive.to_ascii_lowercase()
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{operator_name} on {primitive} should accept parameter and earlier-local paths:\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }
    let source = "pub fn shadow(left: I32, right: U32) -> U32 {\n    let left: U32 = 2\n    let difference = left - right\n    let product = left * right\n    let quotient = left / right\n    quotient\n}\n";
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_sub_mul_div_right_mismatches_point_to_right_path_and_recover_to_left_type() {
    let source = concat!(
        "pub fn mismatch(left: I32, right: U32, f32_value: F32, f64_value: F64) -> I32 {\n",
        "    let difference = left - right\n",
        "    let product = f32_value * f64_value\n",
        "    let product_copy: F32 = product\n",
        "    let quotient = left / f32_value\n",
        "    difference\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let right_sub_start = source
        .find("- right")
        .map(|start| start + "- ".len())
        .expect("right subtraction operand exists");
    let right_mul_start = source
        .find("* f64_value")
        .map(|start| start + "* ".len())
        .expect("right multiplication operand exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        3,
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "both arithmetic operations should recover and reach the tail:\n{stdout}"
    );
    let sub_message = stdout
        .find("\"message\":\"expected `I32`, found `U32`\"")
        .expect("subtraction mismatch exists");
    let mul_message = stdout
        .find("\"message\":\"expected `F32`, found `F64`\"")
        .expect("multiplication mismatch exists");
    let div_message = stdout
        .find("\"message\":\"expected `I32`, found `F32`\"")
        .expect("division mismatch exists");
    assert!(
        sub_message < mul_message && mul_message < div_message,
        "diagnostics should follow source order:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U32`",
        right_sub_start,
        right_sub_start + "right".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found `F64`",
        right_mul_start,
        right_mul_start + "f64_value".len(),
    );
    let right_div_start = source
        .find("/ f32_value")
        .map(|start| start + "/ ".len())
        .expect("right division operand exists");
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `F32`",
        right_div_start,
        right_div_start + "f32_value".len(),
    );
}

#[test]
fn primitive_sub_mul_div_annotation_mismatches_point_to_initializer_and_recover_to_annotation() {
    let source = concat!(
        "pub fn mismatch(left: I32, right: I32, float: F32) -> U32 {\n",
        "    let difference: U32 = left - right\n",
        "    let product: F64 = float * float\n",
        "    let product_copy: F64 = product\n",
        "    let quotient: I8 = left / right\n",
        "    difference\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let sub_start = source
        .find("left - right")
        .expect("subtraction initializer exists");
    let mul_start = source
        .find("float * float")
        .expect("multiplication initializer exists");
    let div_start = source
        .find("left / right")
        .expect("division initializer exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        3,
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "invalid annotations should recover to their declared types and reach the tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        sub_start,
        sub_start + "left - right".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F64`, found `F32`",
        mul_start,
        mul_start + "float * float".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I8`, found `I32`",
        div_start,
        div_start + "left / right".len(),
    );
}

#[test]
fn unsupported_primitive_arithmetic_local_initializer_shapes_remain_gated() {
    for source in [
        "pub fn div_literal_left() -> I32 {\n    let result = 1 / 2\n    result\n}\n",
        "pub fn div_unresolved_local(left: I32) -> I32 {\n    let one = 1\n    let result = one / left\n    result\n}\n",
        "pub fn div_nested_right(left: I32, right: I32) -> I32 {\n    let result = left / (right / 1)\n    result\n}\n",
        "pub fn div_nested_left(left: I32, right: I32) -> I32 {\n    let result = (left / right) / left\n    result\n}\n",
        "type IntegerAlias = I32\npub fn div_alias(left: IntegerAlias, right: IntegerAlias) -> IntegerAlias {\n    let result = left / right\n    result\n}\n",
        "pub fn mul(left: I32, right: I32) -> I32 {\n    let result = 1 * 2\n    result\n}\n",
        "pub fn mul(left: I32, right: I32) -> I32 {\n    let one = 1\n    let result = one * left\n    result\n}\n",
        "pub fn nested(left: I32, right: I32) -> I32 {\n    let result = (left - right) * left\n    result\n}\n",
        "pub fn remainder_literal_literal() -> I32 {\n    let result = 1 % 2\n    result\n}\n",
        "pub fn remainder_unresolved_local(left: I32) -> I32 {\n    let one = 1\n    let result = one % left\n    result\n}\n",
        "pub fn remainder_nested_right(left: I32, right: I32) -> I32 {\n    let result = left % (right % 1)\n    result\n}\n",
        "pub fn remainder_nested_left(left: I32, right: I32) -> I32 {\n    let result = (left % right) % left\n    result\n}\n",
        "type IntegerAlias = I32\npub fn remainder_alias(left: IntegerAlias, right: IntegerAlias) -> IntegerAlias {\n    let result = left % right\n    result\n}\n",
        "pub fn remainder_byte(left: Byte, right: Byte) -> Byte {\n    let result = left % right\n    result\n}\n",
        "pub fn remainder_bool(left: Bool, right: Bool) -> Bool {\n    let result = left % right\n    result\n}\n",
        "pub fn remainder_char(left: Char, right: Char) -> Char {\n    let result = left % right\n    result\n}\n",
        "pub fn remainder_byte_rhs(left: I32, right: Byte) -> I32 {\n    let result = left % right\n    result\n}\n",
        "pub fn remainder_bool_rhs(left: I32, right: Bool) -> I32 {\n    let result = left % right\n    result\n}\n",
        "pub fn remainder_char_rhs(left: I32, right: Char) -> I32 {\n    let result = left % right\n    result\n}\n",
        "pub fn wrapping_literal_literal() -> I32 {\n    let result = 1 +% 2\n    result\n}\n",
        "pub fn wrapping_float_left_literal(right: I32) -> I32 {\n    let result = 1.0 +% right\n    result\n}\n",
        "pub fn wrapping_float_literal_right(left: I32) -> I32 {\n    let result = left +% 1.0\n    result\n}\n",
        "pub fn wrapping_integer_literal_non_integer_path(left: F32) -> F32 {\n    let result = left +% 1\n    result\n}\n",
        "pub fn wrapping_unresolved_local(left: I32) -> I32 {\n    let one = 1\n    let result = one +% left\n    result\n}\n",
        "pub fn wrapping_left_unresolved_local(right: I32) -> I32 {\n    let one = 1\n    let result = 1 +% one\n    result\n}\n",
        "pub fn wrapping_left_nested(right: I32) -> I32 {\n    let result = (1 +% right) +% right\n    result\n}\n",
        "pub fn wrapping_nested(left: I32, right: I32) -> I32 {\n    let result = left +% (right +% left)\n    result\n}\n",
        "type IntegerAlias = I32\npub fn wrapping_alias(left: IntegerAlias, right: IntegerAlias) -> IntegerAlias {\n    let result = left +% right\n    result\n}\n",
        "type IntegerAlias = I32\npub fn wrapping_left_alias(right: IntegerAlias) -> IntegerAlias {\n    let result = 1 +% right\n    result\n}\n",
        "pub fn byte(left: Byte, right: Byte) -> Byte {\n    let result = left - right\n    result\n}\n",
        "pub fn char(left: Char, right: Char) -> Char {\n    let result = left * right\n    result\n}\n",
        "pub fn bool(left: Bool, right: Bool) -> Bool {\n    let result = left - right\n    result\n}\n",
        "pub fn byte_div(left: Byte, right: Byte) -> Byte {\n    let result = left / right\n    result\n}\n",
        "pub fn bool_div(left: Bool, right: Bool) -> Bool {\n    let result = left / right\n    result\n}\n",
        "type IntegerAlias = I32\npub fn alias(left: IntegerAlias, right: IntegerAlias) -> IntegerAlias {\n    let result = left * right\n    result\n}\n",
        "pub fn forward(left: I32) -> I32 {\n    let result = later * left\n    let later: I32 = 1\n    result\n}\n",
        "pub fn self_reference(left: I32) -> I32 {\n    let result = result - left\n    result\n}\n",
        "pub fn mutable(left: I32, right: I32) -> I32 {\n    var result = left * right\n    result\n}\n",
        "pub fn destructured(left: I32, right: I32) -> I32 {\n    let (first, second) = (left, right)\n    let result = first - second\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    for (function, operator) in [("sub", "-"), ("mul", "*"), ("div", "/")] {
        let source = format!(
            "use acme:empty/values as values\npub fn {function}(right: I32) -> I32 {{\n    let result = values.VALUE {operator} right\n    result\n}}\n"
        );
        assert_check_stays_gated(
            &run_check(
                &source,
                "",
                &[("src/values.sai", "pub const VALUE: I32 = 42\n")],
                &[],
            ),
            &source,
        );
    }
}

#[test]
fn primitive_add_diagnostics_follow_source_order_after_shadowed_binding_resolution() {
    let source = concat!(
        "pub fn diagnostics(left: I32, right: U32, f32_value: F32, f64_value: F64) -> I32 {\n",
        "    let left: I32 = 1\n",
        "    let first = left + right\n",
        "    let second = f32_value + f64_value\n",
        "    first\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let first_right = source
        .find("+ right")
        .map(|start| start + "+ ".len())
        .expect("first right operand exists");
    let second_right = source
        .find("+ f64_value")
        .map(|start| start + "+ ".len())
        .expect("second right operand exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "both supported operand mismatches should be diagnosed:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "shadowed parameter names and multiple additions should remain supported:\n{stdout}"
    );
    let first_message = stdout
        .find("\"message\":\"expected `I32`, found `U32`\"")
        .expect("first mismatch exists");
    let second_message = stdout
        .find("\"message\":\"expected `F32`, found `F64`\"")
        .expect("second mismatch exists");
    assert!(
        first_message < second_message,
        "diagnostics should follow source order:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U32`",
        first_right,
        first_right + "right".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found `F64`",
        second_right,
        second_right + "f64_value".len(),
    );
}

#[test]
fn unsupported_primitive_add_local_initializer_shapes_remain_gated() {
    for source in [
        "pub fn add(left: I32, right: I32) -> I32 {\n    let sum = 1 + 2\n    sum\n}\n",
        "pub fn add(left: I32, right: I32) -> I32 {\n    let one = 1\n    let sum = one + right\n    sum\n}\n",
        "pub fn add(left: I32, right: I32) -> I32 {\n    let sum = (left + right) + left\n    sum\n}\n",
        "pub fn add(left: Byte, right: Byte) -> Byte {\n    let sum = left + right\n    sum\n}\n",
        "pub fn add(left: Char, right: Char) -> Char {\n    let sum = left + right\n    sum\n}\n",
        "pub fn add(left: Bool, right: Bool) -> Bool {\n    let sum = left + right\n    sum\n}\n",
        "type IntegerAlias = I32\npub fn add(left: IntegerAlias, right: IntegerAlias) -> IntegerAlias {\n    let sum = left + right\n    sum\n}\n",
        "pub fn forward(left: I32) -> I32 {\n    let sum = later + left\n    let later: I32 = 1\n    sum\n}\n",
        "pub fn self_reference(left: I32) -> I32 {\n    let sum = sum + left\n    sum\n}\n",
        "pub fn add(left: I32, right: I32) -> I32 {\n    var sum = left + right\n    sum\n}\n",
        "pub fn destructured(left: I32, right: I32) -> I32 {\n    let (first, second) = (left, right)\n    let sum = first + second\n    sum\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    let source = "use acme:empty/values as values\npub fn add(right: I32) -> I32 {\n    let sum = values.VALUE + right\n    sum\n}\n";
    assert_check_stays_gated(
        &run_check(
            source,
            "",
            &[("src/values.sai", "pub const VALUE: I32 = 42\n")],
            &[],
        ),
        source,
    );
}

#[test]
fn primitive_comparison_local_initializers_accept_all_six_operators() {
    for source in [
        "pub fn equal(left: I32, right: I32) -> Bool {\n    let result: Bool = left == right\n    result\n}\n",
        "pub fn not_equal(left: I32, right: I32) -> Bool {\n    let result: Bool = right != left\n    result\n}\n",
        "pub fn less(left: I32, right: I32) -> Bool {\n    let left_value: I32 = left\n    let right_value: I32 = right\n    let result: Bool = left_value < right_value\n    result\n}\n",
        "pub fn less_equal(left: I32, right: I32) -> Bool {\n    let left_value: I32 = left\n    let right_value: I32 = right\n    let result: Bool = right_value <= left_value\n    result\n}\n",
        "pub fn greater(left: I32, right: I32) -> Bool {\n    let result: Bool = left > right\n    result\n}\n",
        "pub fn greater_equal(left: I32, right: I32) -> Bool {\n    let left_value: I32 = left\n    let right_value: I32 = right\n    let result: Bool = left_value >= right_value\n    result\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_comparison_local_reports_operand_mismatch_on_the_right_path() {
    let source = "pub fn compare(left: I32, right: U32) -> Bool {\n    let result = left == right\n    result\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let right_start = source
        .find("== right")
        .map(|start| start + "== ".len())
        .expect("right comparison operand exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "a type-mismatched supported comparison must not be hidden by a feature gate:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U32`",
        right_start,
        right_start + "right".len(),
    );
}

#[test]
fn primitive_comparison_non_bool_annotation_reports_and_recovers_as_bool() {
    let source = "pub fn compare(left: I32, right: I32) -> Bool {\n    let result: I32 = left < right\n    result\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source.find("left < right").expect("comparison exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the invalid annotation should produce one diagnostic and the local should recover to Bool:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the comparison initializer must be checked without a feature gate:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `Bool`",
        initializer_start,
        initializer_start + "left < right".len(),
    );
}

#[test]
fn primitive_comparison_local_uses_shadowed_binding_and_source_order() {
    let source = "pub fn shadow(left: I32, right: Bool) -> Bool {\n    let left: Bool = true\n    let result = left == right\n    result\n}\n";
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");

    for gated in [
        "pub fn forward(left: I32, right: I32) -> Bool {\n    let result = later == right\n    let later: I32 = left\n    result\n}\n",
        "pub fn self_reference(right: I32) -> Bool {\n    let result = result == right\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(gated, "", &[], &[]), gated);
    }
}

#[test]
fn unsupported_primitive_comparison_local_initializer_shapes_remain_gated() {
    for source in [
        "pub fn compare(left: I32) -> Bool {\n    let inferred = 1\n    let result = inferred == left\n    result\n}\n",
        "pub fn compare(left: I32, right: I32) -> Bool {\n    let result = (left == right) and (left < right)\n    result\n}\n",
        "pub fn compare(left: Str, right: Str) -> Bool {\n    let result = left == right\n    result\n}\n",
        "pub fn compare(left: I32, right: I32) -> Bool {\n    let result = left as I64 == right as I64\n    result\n}\n",
        "type IntegerAlias = I32\npub fn compare(left: IntegerAlias, right: IntegerAlias) -> Bool {\n    let result = left == right\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    let source = "use acme:empty/values as values\npub fn compare(right: I32) -> Bool {\n    let result = values.VALUE == right\n    result\n}\n";
    assert_check_stays_gated(
        &run_check(
            source,
            "",
            &[("src/values.sai", "pub const VALUE: I32 = 42\n")],
            &[],
        ),
        source,
    );
}

#[test]
fn primitive_comparison_local_initializers_accept_one_path_and_one_scalar_literal() {
    for source in [
        "pub fn bool_right(value: Bool) -> Bool {\n    let result: Bool = value == true\n    result\n}\n",
        "pub fn bool_left(value: Bool) -> Bool {\n    let result = false < value\n    result\n}\n",
        "pub fn char_right(value: Char) -> Bool {\n    let result = value <= 'z'\n    result\n}\n",
        "pub fn char_left(value: Char) -> Bool {\n    let result = 'a' < value\n    result\n}\n",
        "pub fn byte_right(value: Byte) -> Bool {\n    let result = value == b'Z'\n    result\n}\n",
        "pub fn byte_left(value: Byte) -> Bool {\n    let result = b'A' < value\n    result\n}\n",
        "pub fn integer_right(value: I8) -> Bool {\n    let result = value < 10\n    result\n}\n",
        "pub fn integer_left(value: USize) -> Bool {\n    let result = 10 <= value\n    result\n}\n",
        "pub fn float_right(value: F64) -> Bool {\n    let result = value >= 1.5\n    result\n}\n",
        "pub fn float_left(value: F32) -> Bool {\n    let result = 1.5 < value\n    result\n}\n",
        "pub fn greater(value: I32) -> Bool {\n    let result = 1 > value\n    result\n}\n",
        "pub fn greater_equal(value: I32) -> Bool {\n    let result = 1 >= value\n    result\n}\n",
        "pub fn not_equal_left(value: I32) -> Bool {\n    let result = 1 != value\n    result\n}\n",
        "pub fn preceding_local(value: I16) -> Bool {\n    let source: I16 = value\n    let result = source != 0\n    result\n}\n",
        "pub fn shadowed(source: I32) -> Bool {\n    let source: Bool = true\n    let result = false <= source\n    result\n}\n",
        "pub fn character_path_literal(value: Char) -> Bool {\n    let result = value == 'x'\n    result\n}\n",
        "pub fn builtin_alias(value: Int) -> Bool {\n    let result = value < 10\n    result\n}\n",
        "pub fn builtin_unsigned_alias(value: UInt) -> Bool {\n    let result = 10 <= value\n    result\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_comparison_local_literal_range_uses_the_selected_pointer_width() {
    let source = "pub fn pointer_sized(value: USize) -> Bool {\n    let result = 4294967296 <= value\n    result\n}\n";
    let manifest = "targets { wasm32-core, wasm64-core }\n";

    let output = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source
        .find("4294967296")
        .expect("pointer-sized literal exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            && stdout.contains("\"message\":\"integer literal is out of range for `USize`\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "a 32-bit pointer-sized comparison literal should be out of range:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `USize`",
        literal_start,
        literal_start + "4294967296".len(),
    );

    assert_success_result(
        run_check(source, manifest, &[], &["--target", "wasm64-core"]),
        "wasm64-core",
        "dev",
    );
}

#[test]
fn primitive_comparison_left_literal_family_mismatch_still_checks_default_range() {
    let source = "pub fn compare(value: F32) -> Bool {\n    let result = 18446744073709551616 < value\n    result\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source
        .find("18446744073709551616")
        .expect("oversized integer literal exists");
    let path_start = source
        .find("value\n")
        .expect("right comparison path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "a mismatched literal family still reports the path type:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the integer literal must also be checked against its default I64 type:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "a literal-family mismatch and default range error should recover as Bool:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        literal_start,
        literal_start + "18446744073709551616".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        path_start,
        path_start + "value".len(),
    );
}

#[test]
fn primitive_comparison_local_literal_errors_use_operand_spans_and_recover_as_bool() {
    let source = concat!(
        "pub fn diagnostics(left: I32, float_value: F32, narrow: I8) -> Bool {\n",
        "    let first = left == true\n",
        "    let second = 10 < float_value\n",
        "    let right_family_mismatch = float_value < 10\n",
        "    let family_mismatch = 1.5 < narrow\n",
        "    let third: I8 = false == left\n",
        "    let fourth = narrow < 128\n",
        "    fourth\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        6,
        "literal/path and annotation errors should be reported without a feature gate:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the comparison result should recover to Bool through the tail:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "supported comparisons with diagnosed operands still recover to Bool:\n{stdout}"
    );

    let right_literal = source.find("true").expect("right Bool literal exists");
    let left_family_path = source
        .find("10 < float_value")
        .map(|start| start + "10 < ".len())
        .expect("integer-left comparison path exists");
    let float_family_path = source
        .find("1.5 < narrow")
        .map(|start| start + "1.5 < ".len())
        .expect("float-left comparison path exists");
    let right_family_literal = source
        .find("float_value < 10")
        .map(|start| start + "float_value < ".len())
        .expect("right-side literal mismatch exists");
    let fixed_family_path = source
        .find("false == left")
        .map(|start| start + "false == ".len())
        .expect("Bool-left comparison path exists");
    let invalid_annotation = source
        .find("false == left")
        .expect("comparison with a bad annotation exists");
    let out_of_range_literal = source.find("128").expect("out-of-range literal exists");

    let ordered_messages = [
        "\"message\":\"expected `I32`, found a Bool literal\"",
        "\"message\":\"expected `I64`, found `F32`\"",
        "\"message\":\"expected `F32`, found an integer literal\"",
        "\"message\":\"expected `F64`, found `I8`\"",
        "\"message\":\"expected `I8`, found `Bool`\"",
        "\"message\":\"expected `Bool`, found `I32`\"",
        "\"message\":\"integer literal is out of range for `I8`\"",
    ];
    let message_positions =
        ordered_messages.map(|message| stdout.find(message).expect("expected diagnostic exists"));
    assert!(
        message_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "diagnostics should follow their primary source spans:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found a Bool literal",
        right_literal,
        right_literal + "true".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        left_family_path,
        left_family_path + "float_value".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        right_family_literal,
        right_family_literal + "10".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F64`, found `I8`",
        float_family_path,
        float_family_path + "narrow".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `I32`",
        fixed_family_path,
        fixed_family_path + "left".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I8`, found `Bool`",
        invalid_annotation,
        invalid_annotation + "false == left".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        out_of_range_literal,
        out_of_range_literal + "128".len(),
    );
}

#[test]
fn unsupported_primitive_comparison_local_path_literal_shapes_remain_gated() {
    for source in [
        "pub fn compare(value: I32) -> Bool {\n    let result = 1 == 2\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = value < (1 + 2)\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = (value + 1) < 2\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = not (value < 1)\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = value < 1 and true\n    result\n}\n",
        "pub fn compare() -> Bool {\n    let inferred = 1\n    let result = inferred < 2\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = value < later\n    let later: I32 = 1\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = result < 1\n    result\n}\n",
        "pub fn compare(value: I32) -> Bool {\n    let result = value < missing\n    result\n}\n",
        "type IntegerAlias = I32\npub fn compare(value: IntegerAlias) -> Bool {\n    let result = value < 1\n    result\n}\n",
        "pub fn compare(value: Str) -> Bool {\n    let result = value == \"text\"\n    result\n}\n",
        "pub fn compare(value: ()) -> Bool {\n    let result = value == ()\n    result\n}\n",
        "const LIMIT: I32 = 10\npub fn compare(value: I32) -> Bool {\n    let result = value < LIMIT\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    let qualified = concat!(
        "use acme:empty/values as values\n",
        "pub fn compare(value: I32) -> Bool {\n",
        "    let result = values.LIMIT < value\n",
        "    result\n",
        "}\n",
    );
    assert_check_stays_gated(
        &run_check(
            qualified,
            "",
            &[("src/values.sai", "pub const LIMIT: I32 = 10\n")],
            &[],
        ),
        qualified,
    );
}

#[test]
fn primitive_logical_local_initializers_accept_parameter_and_local_paths() {
    for source in [
        "pub fn both(left: Bool, right: Bool) -> Bool {\n    let result: Bool = left and right\n    result\n}\n",
        "pub fn either(left: Bool, right: Bool) -> Bool {\n    let result: Bool = left or right\n    result\n}\n",
        "pub fn local_values() -> Bool {\n    let left: Bool = true\n    let right: Bool = false\n    let result = left and right\n    result\n}\n",
        "pub fn mixed(left: Bool, right: Bool) -> Bool {\n    let local: Bool = left\n    let result = local or right\n    result\n}\n",
        "pub fn repeated(flag: Bool) -> Bool {\n    let result = flag and flag\n    result\n}\n",
        "pub fn shadowed(source: I32, other: Bool) -> Bool {\n    let source: Bool = true\n    let result = source and other\n    result\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_logical_local_reports_each_non_bool_operand_in_source_order() {
    let source = "pub fn logical(left: I32, right: U32) -> Bool {\n    let result = left and right\n    result\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let left_start = source
        .find("left and")
        .expect("left logical operand exists");
    let right_start = source
        .find("and right")
        .map(|start| start + "and ".len())
        .expect("right logical operand exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "both invalid operands need their own diagnostic:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "a type-mismatched supported logical expression recovers as Bool:\n{stdout}"
    );
    let left_diagnostic = stdout
        .find("\"message\":\"expected `Bool`, found `I32`\"")
        .expect("left operand mismatch is reported");
    let right_diagnostic = stdout
        .find("\"message\":\"expected `Bool`, found `U32`\"")
        .expect("right operand mismatch is reported");
    assert!(
        left_diagnostic < right_diagnostic,
        "logical operands must be checked in written source order:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `I32`",
        left_start,
        left_start + "left".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `U32`",
        right_start,
        right_start + "right".len(),
    );
}

#[test]
fn primitive_logical_non_bool_annotation_reports_and_recovers_as_bool() {
    let source = "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result: I32 = left or right\n    result\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source
        .find("left or right")
        .expect("logical expression exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the bad annotation should recover to the logical expression's Bool type:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the logical initializer must remain supported and recover as Bool:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `Bool`",
        initializer_start,
        initializer_start + "left or right".len(),
    );
}

#[test]
fn unsupported_primitive_logical_local_initializer_shapes_remain_gated() {
    for source in [
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = (left and right) or right\n    result\n}\n",
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = not left and right\n    result\n}\n",
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = left and true\n    result\n}\n",
        "pub fn logical(flag: Bool) -> Bool {\n    let inferred = 1\n    let result = inferred and flag\n    result\n}\n",
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = left and (right == left)\n    result\n}\n",
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = left and right + true\n    result\n}\n",
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = left and later\n    let later: Bool = right\n    result\n}\n",
        "pub fn logical(right: Bool) -> Bool {\n    let result = result and right\n    result\n}\n",
        "pub fn logical(right: Bool) -> Bool {\n    let result = missing and right\n    result\n}\n",
        "pub fn logical(flag: Bool) -> Bool {\n    let result = values.FLAG and flag\n    result\n}\n",
        "const READY: Bool = true\npub fn logical(flag: Bool) -> Bool {\n    let result = READY and flag\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    let source = concat!(
        "use acme:empty/values as values\n",
        "pub fn logical(flag: Bool) -> Bool {\n",
        "    let result = values.FLAG and flag\n",
        "    result\n",
        "}\n",
    );
    assert_check_stays_gated(
        &run_check(
            source,
            "",
            &[("src/values.sai", "pub const FLAG: Bool = true\n")],
            &[],
        ),
        source,
    );
}

#[test]
fn primitive_local_not_accepts_bool_parameters_and_preceding_locals() {
    for source in [
        "pub fn parameter(flag: Bool) -> Bool {\n    let result: Bool = not flag\n    result\n}\n",
        "pub fn local(flag: Bool) -> Bool {\n    let source: Bool = flag\n    let result = not source\n    result\n}\n",
        "pub fn shadow(source: I32) -> Bool {\n    let source: Bool = true\n    let result = not source\n    result\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_local_not_reports_operand_mismatch_and_recovers_as_bool() {
    let source =
        "pub fn negated(value: I32) -> Bool {\n    let result = not value\n    result\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let operand_start = source
        .find("not value")
        .map(|start| start + "not ".len())
        .expect("not operand exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the invalid operand should recover to Bool for the tail:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "a supported non-Bool operand should produce a type diagnostic, not a gate:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `I32`",
        operand_start,
        operand_start + "value".len(),
    );
}

#[test]
fn primitive_local_not_diagnostics_follow_initializer_source_order() {
    let source = concat!(
        "pub fn ordered(non_bool: I32, bool_value: Bool) -> Bool {\n",
        "    let first = not non_bool\n",
        "    let second: I32 = not bool_value\n",
        "    second\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let operand_start = source
        .find("not non_bool")
        .map(|start| start + "not ".len())
        .expect("first not operand exists");
    let initializer_start = source
        .find("not bool_value")
        .expect("second not initializer exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "both the operand and annotation errors should be reported, with Bool recovery through the tail:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "diagnosed not initializers and their Bool recovery should remain supported:\n{stdout}"
    );
    let operand_diagnostic = stdout
        .find("\"message\":\"expected `Bool`, found `I32`\"")
        .expect("first initializer's operand error exists");
    let annotation_diagnostic = stdout
        .find("\"message\":\"expected `I32`, found `Bool`\"")
        .expect("second initializer's annotation error exists");
    assert!(
        operand_diagnostic < annotation_diagnostic,
        "initializer diagnostics should follow source order:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `I32`",
        operand_start,
        operand_start + "non_bool".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `Bool`",
        initializer_start,
        initializer_start + "not bool_value".len(),
    );
}

#[test]
fn unsupported_primitive_local_not_shapes_remain_gated() {
    for source in [
        "pub fn logical(flag: Bool) -> Bool {\n    let result = not not flag\n    result\n}\n",
        "pub fn logical(flag: Bool) -> Bool {\n    let result = not true\n    result\n}\n",
        "pub fn logical() -> Bool {\n    let inferred = 1\n    let result = not inferred\n    result\n}\n",
        "pub fn logical(left: I32, right: I32) -> Bool {\n    let result = not (left == right)\n    result\n}\n",
        "pub fn logical(left: Bool, right: Bool) -> Bool {\n    let result = not (left and right)\n    result\n}\n",
        "pub fn logical(left: I32, right: I32) -> Bool {\n    let result = not (left + right)\n    result\n}\n",
        "pub fn logical(flag: Bool) -> Bool {\n    let result = not missing\n    result\n}\n",
        "pub fn logical(flag: Bool) -> Bool {\n    let result = not later\n    let later: Bool = flag\n    result\n}\n",
        "pub fn logical(flag: Bool) -> Bool {\n    let result = not result\n    result\n}\n",
        "type Flag = Bool\npub fn logical(flag: Flag) -> Bool {\n    let result = not flag\n    result\n}\n",
        "const READY: Bool = true\npub fn logical(flag: Bool) -> Bool {\n    let result = not READY\n    result\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }

    let source = concat!(
        "use acme:empty/values as values\n",
        "pub fn logical(flag: Bool) -> Bool {\n",
        "    let result = not values.READY\n",
        "    result\n",
        "}\n",
    );
    assert_check_stays_gated(
        &run_check(
            source,
            "",
            &[("src/values.sai", "pub const READY: Bool = true\n")],
            &[],
        ),
        source,
    );
}

#[test]
fn public_unannotated_primitive_literal_local_emits_check_result() {
    for source in [
        "pub fn answer() -> I32 {\n    let value = 42\n    value\n}\n",
        "pub fn ratio() -> F32 {\n    let value = 1.25\n    value\n}\n",
        "pub fn ready() -> Bool {\n    let value = true\n    value\n}\n",
        "pub fn marker() -> Byte {\n    let value = b'z'\n    value\n}\n",
        "pub fn letter() -> Char {\n    let value = 'z'\n    value\n}\n",
        "pub fn pointer_max() -> USize {\n    let value = 4294967295\n    value\n}\n",
        "pub fn pointer_min() -> ISize {\n    let value = -2147483648\n    value\n}\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }

    assert_success_result(
        run_check(
            "pub fn pointer_size() -> USize {\n    let value = 4294967296\n    value\n}\n",
            "targets { wasm32-core, wasm64-core }\n",
            &[],
            &["--target", "wasm64-core"],
        ),
        "wasm64-core",
        "dev",
    );
}

#[test]
fn unannotated_primitive_literal_local_reports_semantic_errors_without_a_feature_gate() {
    for (source, code, message, token) in [
        (
            "pub fn overflow() -> I8 {\n    let value = 128\n    value\n}\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `I8`",
            "128",
        ),
        (
            "pub fn mismatch() -> Bool {\n    let value = 42\n    value\n}\n",
            "TYPE_MISMATCH",
            "expected `Bool`, found an integer literal",
            "value",
        ),
        (
            "pub fn mismatch() -> F32 {\n    let value = 42\n    value\n}\n",
            "TYPE_MISMATCH",
            "expected `F32`, found an integer literal",
            "value",
        ),
        (
            "pub fn mismatch() -> I32 {\n    let value = 1.25\n    value\n}\n",
            "TYPE_MISMATCH",
            "expected `I32`, found a floating-point literal",
            "value",
        ),
        (
            "pub fn mismatch() -> I32 {\n    let value = true\n    value\n}\n",
            "TYPE_MISMATCH",
            "expected `I32`, found `Bool`",
            "value",
        ),
        (
            "pub fn overflow() -> F32 {\n    let value = 3.4028236e38\n    value\n}\n",
            "LITERAL_OUT_OF_RANGE",
            "floating-point literal is out of range for `F32`",
            "3.4028236e38",
        ),
        (
            "pub fn overflow() -> USize {\n    let value = 4294967296\n    value\n}\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `USize`",
            "4294967296",
        ),
        (
            "pub fn overflow() -> ISize {\n    let value = -2147483649\n    value\n}\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `ISize`",
            "-2147483649",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let start = if code == "LITERAL_OUT_OF_RANGE" {
            source.find(token)
        } else {
            source.rfind(token)
        }
        .expect("diagnostic token exists");
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains(&format!("\"code\":\"{code}\""))
                && stdout.contains(message)
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\"")
                && stdout.contains(&format!("\"start\":{{\"byte\":{start},")),
            "supported unannotated-local error should use its specification span:\n{stdout}"
        );
    }

    let source = "pub fn mismatch_and_default_overflow() -> Bool {\n    let value = 340282366920938463463374607431768211456\n    value\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source
        .find("340282366920938463463374607431768211456")
        .expect("large initializer exists");
    let tail_start = source.rfind("value").expect("tail path exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"message\":\"expected `Bool`, found an integer literal\"")
            && stdout.contains("\"message\":\"integer literal is out of range for `I64`\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{initializer_start},"))
            && stdout.contains(&format!("\"start\":{{\"byte\":{tail_start},"))
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the tail mismatch and defaulting range error must retain their own spans:\n{stdout}"
    );

    let source = "pub fn mismatch_and_default_float_overflow() -> I32 {\n    let value = 1e309\n    value\n}\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer_start = source
        .find("1e309")
        .expect("large float initializer exists");
    let tail_start = source.rfind("value").expect("tail path exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            && stdout.contains("\"message\":\"expected `I32`, found a floating-point literal\"")
            && stdout.contains("\"message\":\"floating-point literal is out of range for `F64`\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{initializer_start},"))
            && stdout.contains(&format!("\"start\":{{\"byte\":{tail_start},"))
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the floating-point defaulting range error and tail mismatch must retain their spans:\n{stdout}"
    );
}

#[test]
fn public_zero_parameter_explicit_unit_function_emits_check_result() {
    for source in ["pub fn empty() -> () {}\n", "pub fn unit() -> () { () }\n"] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn public_zero_parameter_direct_literal_if_function_emits_check_result() {
    for source in [
        "pub fn choose() -> I32 { if true { 1 } else { 2 } }\n",
        "pub fn choose() -> I32 { if false { 1 } else { -2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn public_bool_parameter_literal_if_function_emits_check_result() {
    let source = "pub fn choose(flag: Bool) -> I32 { if flag { 1 } else { 2 } }\n";
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn unsupported_bool_parameter_if_shapes_remain_gated() {
    for source in [
        "pub fn choose(flag: I32) -> I32 { if flag { 1 } else { 2 } }\n",
        "pub fn choose(var flag: Bool) -> I32 { if flag { 1 } else { 2 } }\n",
        "pub fn choose(flag: Bool) -> I32 { if flag { 1 } }\n",
        "pub fn choose(flag: Bool, other: Bool) -> I32 { if flag { 1 } else { 2 } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_bool_parameter_not_if_function_emits_check_result() {
    let source = "pub fn choose(flag: Bool) -> I32 { if not flag { 1 } else { 2 } }\n";
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn unsupported_bool_parameter_negation_shapes_remain_gated() {
    for source in [
        "pub fn choose(flag: Bool) -> I32 { if not not flag { 1 } else { 2 } }\n",
        "pub fn choose(flag: Bool) -> I32 { if not flag { 1 } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_bool_parameters_and_if_function_emits_check_result() {
    for source in [
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left and right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left and right) { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unsupported_bool_parameter_and_shapes_remain_gated() {
    for source in [
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left and left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left and true { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: I32) -> I32 { if left and right { 1 } else { 2 } }\n",
        "pub fn choose(var left: Bool, right: Bool) -> I32 { if left and right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left and right { 1 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not not (left and right) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left and left) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left and right and left) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left == right and left) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 {\n    if left and right {\n        let value = 1\n        value\n    } else {\n        2\n    }\n}\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_bool_parameters_or_if_function_emits_check_result() {
    for source in [
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left or right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (right or left) { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unsupported_bool_parameter_or_shapes_remain_gated() {
    for source in [
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left or left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left or true { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left and right or left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left or right and left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left or right { 1 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not not (left or right) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left or right or left) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left == right or left) { 1 } else { 2 } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_bool_parameter_and_primitive_comparison_if_functions_emit_check_results() {
    for source in [
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled and value >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(value: I32, enabled: Bool) -> I32 { if value < 20 and enabled { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: U8) -> I32 { if enabled and 18 <= value { 1 } else { 2 } }\n",
        "pub fn choose(value: I32, enabled: Bool) -> I32 { if 10 < value and enabled { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn public_bool_parameter_or_primitive_comparison_if_functions_emit_check_results() {
    for source in [
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled or value >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(value: I32, enabled: Bool) -> I32 { if value < 20 or enabled { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: U8) -> I32 { if enabled or 18 <= value { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn public_negated_mixed_bool_comparison_conditions_emit_check_results() {
    for source in [
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled and value >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(value: I32, enabled: Bool) -> I32 { if not (value < 20 or enabled) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: U8) -> I32 { if not (enabled and 18 <= value) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled and not (value >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(value: U8, enabled: Bool) -> I32 { if not (18 <= value) and enabled { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled or not (value < 20) { 1 } else { 2 } }\n",
        "pub fn choose(value: I32, enabled: Bool) -> I32 { if not (value < 20) or enabled { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unsupported_mixed_bool_logical_comparison_conditions_remain_gated() {
    for source in [
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled and (value > 0 and value < 10) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled and enabled >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled or (value > 0 or value < 10) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: I32, value: I32) -> I32 { if enabled and value >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32, extra: I32) -> I32 { if enabled or value >= 18 { 1 } else { 2 } }\n",
        "type BoolAlias = Bool\npub fn choose(enabled: BoolAlias, value: I32) -> I32 { if enabled and value >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32, extra: I32) -> I32 { if enabled and value >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled and value >= 18 { 1 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled or value >= 18 { 1 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if enabled and not (not (value >= 18)) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not not (enabled and value >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not not (enabled or value >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled and not (value >= 18)) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (not enabled and value >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not enabled and value >= 18 { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled and (value > 0 and value < 10)) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled or (value > 0 or value < 10)) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled and enabled >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled or enabled >= 18) { 1 } else { 2 } }\n",
        "pub fn choose(enabled: Bool, value: I32) -> I32 { if not (enabled and value >= 18) { 1 } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_primitive_parameter_comparison_if_functions_emit_check_results() {
    for source in [
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left == right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if right != left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left < right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if right <= left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left > right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if right >= left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left < right) { 1 } else { 2 } }\n",
        "pub fn choose(left: F32, right: F32) -> I32 { if not (right <= left) { 1 } else { 2 } }\n",
        "pub fn choose(left: I32, right: I32) -> I32 { if left < right { 1 } else { 2 } }\n",
        "pub fn choose(left: U32, right: U32) -> I32 { if left <= right { 1 } else { 2 } }\n",
        "pub fn choose(left: F32, right: F32) -> I32 { if left > right { 1 } else { 2 } }\n",
        "pub fn choose(left: F64, right: F64) -> I32 { if left >= right { 1 } else { 2 } }\n",
        "pub fn choose(left: Char, right: Char) -> I32 { if left == right { 1 } else { 2 } }\n",
        "pub fn choose(left: Byte, right: Byte) -> I32 { if left != right { 1 } else { 2 } }\n",
        "pub fn choose(left: ISize, right: ISize) -> I32 { if left < right { 1 } else { 2 } }\n",
        "pub fn choose(left: USize, right: USize) -> I32 { if left >= right { 1 } else { 2 } }\n",
        "pub fn choose(left: Int, right: Int) -> I32 { if left == right { 1 } else { 2 } }\n",
        "pub fn choose(left: UInt, right: UInt) -> I32 { if left != right { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unsupported_primitive_parameter_comparison_shapes_remain_gated() {
    for source in [
        "pub fn choose(left: I32, right: U32) -> I32 { if left < right { 1 } else { 2 } }\n",
        "pub fn choose(left: I32, right: I32) -> I32 { if left and right { 1 } else { 2 } }\n",
        "pub fn choose(left: I32, right: I32) -> I32 { if left or right { 1 } else { 2 } }\n",
        "type IntegerAlias = I32\npub fn choose(left: IntegerAlias, right: IntegerAlias) -> I32 { if left < right { 1 } else { 2 } }\n",
        "pub fn choose(left: (), right: ()) -> I32 { if left == right { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left == left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left == true { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left == right and left { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if left == right { 1 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not not (left < right) { 1 } else { 2 } }\n",
        "pub fn choose(left: Bool, right: Bool) -> I32 { if not (left < right and left) { 1 } else { 2 } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn public_single_primitive_parameter_comparison_with_literal_emits_check_results() {
    for source in [
        "pub fn choose(value: Bool) -> I32 { if value == true { 1 } else { 2 } }\n",
        "pub fn choose(value: Bool) -> I32 { if false < value { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if value < -128 { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if 10 < value { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if not (value < 10) { 1 } else { 2 } }\n",
        "pub fn choose(value: F32) -> I32 { if not (1.5 < value) { 1 } else { 2 } }\n",
        "pub fn choose(value: U64) -> I32 { if value >= 10 { 1 } else { 2 } }\n",
        "pub fn choose(value: F32) -> I32 { if value > 1.5 { 1 } else { 2 } }\n",
        "pub fn choose(value: F32) -> I32 { if 1.5 < value { 1 } else { 2 } }\n",
        "pub fn choose(value: F64) -> I32 { if value <= 1.5 { 1 } else { 2 } }\n",
        "pub fn choose(value: Char) -> I32 { if value < 'z' { 1 } else { 2 } }\n",
        "pub fn choose(value: Char) -> I32 { if 'a' < value { 1 } else { 2 } }\n",
        "pub fn choose(value: Byte) -> I32 { if value == b'Z' { 1 } else { 2 } }\n",
        "pub fn choose(value: Byte) -> I32 { if b'A' < value { 1 } else { 2 } }\n",
        "pub fn choose(value: ISize) -> I32 { if value >= 0 { 1 } else { 2 } }\n",
        "pub fn choose(value: USize) -> I32 { if value != 0 { 1 } else { 2 } }\n",
        "pub fn choose(value: Int) -> I32 { if value < 10 { 1 } else { 2 } }\n",
        "pub fn choose(value: UInt) -> I32 { if value > 10 { 1 } else { 2 } }\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn unsupported_single_parameter_comparison_literal_shapes_remain_gated() {
    for source in [
        "const LIMIT: I8 = 10\npub fn choose(value: I8) -> I32 { if value < LIMIT { 1 } else { 2 } }\n",
        "const LIMIT: I8 = 10\npub fn choose(value: I8) -> I32 { if LIMIT < value { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if 10 < 11 { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if value < 1 + 2 { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if value < 1 and true { 1 } else { 2 } }\n",
        "pub fn choose(value: I8, other: I8) -> I32 { if value < 10 { 1 } else { 2 } }\n",
        "pub fn choose(value: I8, other: I8) -> I32 { if 10 < value { 1 } else { 2 } }\n",
        "pub fn choose(value: I8, other: I8) -> I32 { if not (10 < value) { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if 10 < other { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if not not (value < 10) { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if not (value < 1 and true) { 1 } else { 2 } }\n",
        "type I8Alias = I8\npub fn choose(value: I8Alias) -> I32 { if value < 10 { 1 } else { 2 } }\n",
        "pub fn choose(value: Str) -> I32 { if value == \"x\" { 1 } else { 2 } }\n",
        "pub fn choose(value: ()) -> I32 { if value == () { 1 } else { 2 } }\n",
        "pub fn choose(var value: I8) -> I32 { if value < 10 { 1 } else { 2 } }\n",
        "pub fn choose(value: I8) -> I32 { if value < 10 { 1 } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn comparison_literal_errors_report_condition_spans_before_branch_errors() {
    let source = "pub fn choose(value: I8) -> I32 { if true < value { 1 } else { 2 } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let condition_start = source.rfind("value").expect("right parameter path exists");
    let condition_end = condition_start + "value".len();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && stdout.contains("\"message\":\"expected `Bool`, found `I8`\"")
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{condition_start},"))
            && stdout.contains(&format!("\"end\":{{\"byte\":{condition_end},")),
        "condition type mismatch must be reported at the literal without a feature gate:\n{stdout}"
    );

    for (source, expected_message) in [
        (
            "pub fn choose(value: F32) -> I32 { if 10 < value { 1 } else { 2 } }\n",
            "\"message\":\"expected `I64`, found `F32`\"",
        ),
        (
            "pub fn choose(value: I8) -> I32 { if 1.5 < value { 1 } else { 2 } }\n",
            "\"message\":\"expected `F64`, found `I8`\"",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let condition_start = source.rfind("value").expect("right parameter path exists");
        let condition_end = condition_start + "value".len();
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && stdout.contains(expected_message)
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && stdout.contains(&format!("\"start\":{{\"byte\":{condition_start},"))
                && stdout.contains(&format!("\"end\":{{\"byte\":{condition_end},")),
            "a left numeric literal must check the right operand against its inferred type:\n{stdout}"
        );
    }

    let source = "pub fn choose(value: I8) -> I8 { if not (128 < value) { true } else { 128 } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let condition_start = source.find("128").expect("condition integer exists");
    let then_start = source.find("true").expect("then-branch literal exists");
    let else_start = source.rfind("128").expect("else-branch integer exists");
    let condition_position = stdout
        .find(&format!("\"start\":{{\"byte\":{condition_start},"))
        .expect("condition diagnostic span exists");
    let then_position = stdout
        .find(&format!("\"start\":{{\"byte\":{then_start},"))
        .expect("then-branch diagnostic span exists");
    let else_position = stdout
        .find(&format!("\"start\":{{\"byte\":{else_start},"))
        .expect("else-branch diagnostic span exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 2
            && stdout.matches("\"code\":\"TYPE_MISMATCH\"").count() == 1
            && condition_position < then_position
            && then_position < else_position
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "left-literal condition and then/else errors must retain source order:\n{stdout}"
    );

    let source = "pub fn choose(value: I8) -> I8 { if value < 128 { true } else { 128 } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let condition_start = source.find("128").expect("condition integer exists");
    let then_start = source.find("true").expect("then-branch literal exists");
    let else_start = source.rfind("128").expect("else-branch integer exists");
    let condition_position = stdout
        .find(&format!("\"start\":{{\"byte\":{condition_start},"))
        .expect("condition diagnostic span exists");
    let then_position = stdout
        .find(&format!("\"start\":{{\"byte\":{then_start},"))
        .expect("then-branch diagnostic span exists");
    let else_position = stdout
        .find(&format!("\"start\":{{\"byte\":{else_start},"))
        .expect("else-branch diagnostic span exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 2
            && stdout.matches("\"code\":\"TYPE_MISMATCH\"").count() == 1
            && condition_position < then_position
            && then_position < else_position
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "condition and then/else errors must retain source order without a feature gate:\n{stdout}"
    );

    let source = "pub fn choose(enabled: Bool, value: I8) -> I8 { if not (enabled and value < 128) { 128 } else { 129 } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let condition_start = source.find("128").expect("condition integer exists");
    let then_start = source[condition_start + "128".len()..]
        .find("128")
        .map(|offset| condition_start + "128".len() + offset)
        .expect("then-branch integer exists");
    let else_start = source.find("129").expect("else-branch integer exists");
    let condition_position = stdout
        .find(&format!("\"start\":{{\"byte\":{condition_start},"))
        .expect("condition diagnostic span exists");
    let then_position = stdout
        .find(&format!("\"start\":{{\"byte\":{then_start},"))
        .expect("then-branch diagnostic span exists");
    let else_position = stdout
        .find(&format!("\"start\":{{\"byte\":{else_start},"))
        .expect("else-branch diagnostic span exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 3
            && condition_position < then_position
            && then_position < else_position
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "mixed condition and then/else errors must retain source order without a feature gate:\n{stdout}"
    );
}

#[test]
fn negated_mixed_comparison_errors_are_checked_before_branch_errors() {
    let source = "pub fn choose(enabled: Bool, value: I8) -> I8 { if enabled and not (value < 128) { 128 } else { 129 } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let condition_start = source.find("128").expect("condition integer exists");
    let then_start = source[condition_start + "128".len()..]
        .find("128")
        .map(|offset| condition_start + "128".len() + offset)
        .expect("then-branch integer exists");
    let else_start = source.find("129").expect("else-branch integer exists");
    let condition_position = stdout
        .find(&format!("\"start\":{{\"byte\":{condition_start},"))
        .expect("condition diagnostic span exists");
    let then_position = stdout
        .find(&format!("\"start\":{{\"byte\":{then_start},"))
        .expect("then-branch diagnostic span exists");
    let else_position = stdout
        .find(&format!("\"start\":{{\"byte\":{else_start},"))
        .expect("else-branch diagnostic span exists");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count() == 3
            && condition_position < then_position
            && then_position < else_position
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "negated mixed comparison and then/else errors must retain source order:\n{stdout}"
    );

    let source = "pub fn choose(enabled: Bool, value: I8) -> I32 { if enabled and not (1.5 < value) { 1 } else { 2 } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let condition_start = source.rfind("value").expect("comparison parameter exists");
    let condition_end = condition_start + "value".len();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && stdout.contains("\"message\":\"expected `F64`, found `I8`\"")
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && stdout.contains(&format!("\"start\":{{\"byte\":{condition_start},"))
            && stdout.contains(&format!("\"end\":{{\"byte\":{condition_end},")),
        "negated comparison operands must retain literal type diagnostics at the parameter:\n{stdout}"
    );
}

#[test]
fn literal_if_checks_both_branches_and_reports_source_spans() {
    for (source, expected_code, literal) in [
        (
            "pub fn choose() -> I32 { if true { 1 } else { true } }\n",
            "TYPE_MISMATCH",
            "true",
        ),
        (
            "pub fn choose() -> I8 { if false { 128 } else { 1 } }\n",
            "LITERAL_OUT_OF_RANGE",
            "128",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains(&format!("\"code\":\"{expected_code}\""))
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: both branches must be checked without a result or feature gate:\n{stdout}"
        );
        let literal_start = source.rfind(literal).expect("unselected literal exists");
        let literal_end = literal_start + literal.len();
        assert!(
            stdout.contains(&format!("\"start\":{{\"byte\":{literal_start},"))
                && stdout.contains(&format!("\"end\":{{\"byte\":{literal_end},")),
            "{source:?}: the error in the unselected branch must retain its span:\n{stdout}"
        );
    }

    let source = "pub fn choose() -> I8 { if true { 128 } else { true } }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            && stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "both literal branches must be checked in source order:\n{stdout}"
    );
}

#[test]
fn unsupported_zero_parameter_if_shapes_remain_gated() {
    for source in [
        "pub fn choose() -> I32 { if 1 == 1 { 1 } else { 2 } }\n",
        "pub fn choose() -> I32 { if true { 1 } }\n",
        "pub fn choose() -> I32 { if true { let value = 1; value } else { 2 } }\n",
        "pub fn choose() -> I32 { if true { 1 + 2 } else { 2 } }\n",
        "pub fn choose() -> I32 { if true { 1 } else if false { 2 } else { 3 } }\n",
        "pub fn choose() -> I32 { if true { if false { 1 } else { 2 } } else { 3 } }\n",
        "pub fn choose() -> () { if true { () } else { () } }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn zero_parameter_direct_scalar_type_errors_suppress_check_result() {
    for (source, expected_code, expected_message, literal) in [
        (
            "pub fn answer() -> Bool { 42 }\n",
            "TYPE_MISMATCH",
            "expected `Bool`, found an integer literal",
            "42",
        ),
        (
            "pub fn answer() -> I8 { 128 }\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `I8`",
            "128",
        ),
        (
            "pub fn answer() -> USize { 4294967296 }\n",
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `USize`",
            "4294967296",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains(&format!("\"code\":\"{expected_code}\""))
                && stdout.contains(expected_message)
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected {expected_code} without a result or feature gate:\n{stdout}"
        );
        let literal_start = source.find(literal).expect("literal exists");
        let literal_end = literal_start + literal.len();
        assert!(
            stdout.contains(&format!("\"start\":{{\"byte\":{literal_start},"))
                && stdout.contains(&format!("\"end\":{{\"byte\":{literal_end},")),
            "{source:?}: the diagnostic must cover the direct literal:\n{stdout}"
        );
    }
}

#[test]
fn unsupported_zero_parameter_function_shapes_remain_gated() {
    for source in [
        "pub fn answer() { 42 }\n",
        "pub fn answer() -> Str { \"text\" }\n",
        "pub fn answer() -> Byte { b\"text\" }\n",
        "pub fn answer() -> I32 { 40 + 2 }\n",
        "pub fn answer() -> I32 { let value = 42; value }\n",
        "pub fn answer() -> () { 42 }\n",
        "pub fn answer() -> () { let value = (); }\n",
        "pub fn answer() {}\n",
        "pub type Number = I32\npub fn answer() -> Number { 42 }\n",
        "@inline\npub fn answer() -> I32 { 42 }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn non_ascii_public_function_name_reports_its_hard_diagnostic() {
    let source = "pub fn identité(value: I32) -> I32 { value }\n";
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
            && !stdout.contains("\"type\":\"result\""),
        "public non-ASCII names must produce their hard diagnostic:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"NON_ASCII_PUBLIC_NAME\"").count(),
        1,
        "a public name must be diagnosed exactly once:\n{stdout}"
    );
    let name_start = source.find("identité").expect("function name exists");
    let name_end = name_start + "identité".len();
    assert!(
        stdout.contains(&format!("\"byte\":{name_start}"))
            && stdout.contains(&format!("\"byte\":{name_end}")),
        "the diagnostic must point at the complete written public name:\n{stdout}"
    );

    let attributed = run_check(
        "@inline\npub fn identité(value: I32) -> I32 { value }\n",
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(attributed.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(attributed.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
            && !stdout.contains("\"type\":\"result\""),
        "a function attribute must not hide the public-name diagnostic:\n{stdout}"
    );

    for source in [
        concat!(
            "pub fn identité(value: I32) -> I32 { value }\n",
            "pub fn identity(value: I32) -> I32 { value }\n",
        ),
        "use acme:missing\npub fn identité(value: I32) -> I32 { value }\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
                && !stdout.contains("\"type\":\"result\""),
            "unsupported source shapes must not hide the public-name diagnostic:\n{stdout}"
        );
    }

    let output = run_check(
        "",
        "",
        &[(
            "src/extra.sai",
            "pub fn identité(value: I32) -> I32 { value }\n",
        )],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
            && !stdout.contains("\"type\":\"result\""),
        "a public name in an additional source module must be diagnosed:\n{stdout}"
    );
}

#[test]
fn exported_function_name_reports_its_hard_diagnostic() {
    let output = run_check(
        "export fn identité(value: I32) -> I32 { value }\n",
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
            && !stdout.contains("\"type\":\"result\""),
        "exported non-ASCII names must produce their hard diagnostic:\n{stdout}"
    );
}

#[test]
fn non_function_export_still_checks_its_public_name() {
    let output = run_check("export const CAFÉ: I32 = 1\n", "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"INVALID_EXPORT_TARGET\"")
            && stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
            && !stdout.contains("\"type\":\"result\""),
        "an invalid export modifier must not hide its non-ASCII public name:\n{stdout}"
    );
}

#[test]
fn non_function_export_trait_recovery_checks_trait_members() {
    let output = run_check(
        concat!("export trait Élement {\n", "fn identité() -> I32\n", "}\n",),
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"INVALID_EXPORT_TARGET\"")
            && stdout.matches("\"code\":\"NON_ASCII_PUBLIC_NAME\"").count() == 2,
        "export recovery must retain public visibility for the trait and its members:\n{stdout}"
    );
}

#[test]
fn public_trait_member_names_report_their_hard_diagnostics() {
    let output = run_check(
        concat!(
            "pub trait Names {\n",
            "type Élement\n",
            "fn identité(value: I32) -> I32\n",
            "}\n",
            "impl Names for I32 {\n",
            "pub fn identité(value: I32) -> I32 { value }\n",
            "}\n",
        ),
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"NON_ASCII_PUBLIC_NAME\"").count(),
        2,
        "each non-ASCII member of a public trait must be diagnosed once:\n{stdout}"
    );
    assert!(!stdout.contains("\"type\":\"result\""), "{stdout}");
}

#[test]
fn public_type_fields_and_enum_variants_report_their_hard_diagnostics() {
    let output = run_check(
        concat!(
            "pub type Café = I32\n",
            "pub type Record {\n",
            "pub façade: I32\n",
            "}\n",
            "type Hidden {\n",
            "pub caché: I32\n",
            "}\n",
            "pub enum Colors {\n",
            "Élément\n",
            "}\n",
            "pub const CAFÉ: I32 = 1\n",
            "pub distinct Élement = I32\n",
            "pub resource Rêsource {\n",
            "pub châmp: I32\n",
            "}\n",
        ),
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"NON_ASCII_PUBLIC_NAME\"").count(),
        8,
        "public types, fields, enum variants, constants, and resources must be diagnosed:\n{stdout}"
    );
    assert!(!stdout.contains("\"type\":\"result\""), "{stdout}");
}

#[test]
fn interface_and_world_names_report_their_hard_diagnostics() {
    let output = run_check(
        concat!(
            "interface api {\n",
            "fn identité(prénom: I32) -> I32\n",
            "}\n",
            "world café {\n",
            "import fn entrée(pâramètre: I32)\n",
            "export fn sortie(résultat: I32)\n",
            "import api\n",
            "}\n",
        ),
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"NON_ASCII_PUBLIC_NAME\"").count(),
        6,
        "interface, world, and WIT function parameter names must be diagnosed:\n{stdout}"
    );
    assert!(!stdout.contains("\"type\":\"result\""), "{stdout}");
}

#[test]
fn public_reexport_alias_reports_its_hard_diagnostic() {
    let output = run_check(
        "pub use acme:empty/child.{identity as identíté}\n",
        "",
        &[("src/child.sai", "pub fn identity() {}\n")],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\"")
            && !stdout.contains("\"type\":\"result\""),
        "a public re-export alias must be diagnosed:\n{stdout}"
    );
}

#[test]
fn target_filtered_public_name_does_not_report_a_name_error() {
    let output = run_check(
        concat!(
            "@target(\"wasm64-core\")\n",
            "pub fn identité(value: I32) -> I32 { value }\n",
        ),
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert!(
        !stdout.contains("\"code\":\"NON_ASCII_PUBLIC_NAME\""),
        "an item filtered out for the selected target must not be diagnosed:\n{stdout}"
    );
}

#[test]
fn function_body_forms_outside_direct_parameter_slice_remain_gated() {
    for source in [
        "pub fn add(value: I32) -> I32 { value + 1 }\n",
        "pub fn local(value: I32) -> I32 { let next = value; next }\n",
        "pub fn local() -> I32 {\nvar value: I32 = 42\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet var value: I32 = 42\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = 40 + 2\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = 42\n{ value }\n}\n",
        "pub fn local() -> I32 {\nlet first: I32 = 1\nvar second: I32 = 2\nfirst\n}\n",
        "pub fn local() -> I32 {\nlet first: I32 = 1\nlet second: I32 = 40 + 2\nfirst\n}\n",
        "pub fn local() -> I32 {\nlet first: I32 = 1\nlet second: I32 = first % (1 + 0)\nfirst\n}\n",
        "pub fn local() -> I32 {\nlet first: I32 = 1\nfirst\nfirst\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = 42\nlet copy: I32 = r#value\ncopy\n}\n",
        "pub fn local() -> I32 {\nlet first: I32 = second\nlet second: I32 = 2\nfirst\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = value\nvalue\n}\n",
        "const ONE: I32 = 1\npub fn local() -> I32 {\nlet copy: I32 = ONE\ncopy\n}\n",
        "pub fn local() -> I32 {\nlet valueName: I32 = 42\nvalueName\n}\n",
        "pub fn local() -> I32 {\nlet r#value: I32 = 42\nr#value\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = 42\nr#value\n}\n",
        "pub fn local() -> I32 {\nlet value = 40 + 2\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet valueName = 42\nvalueName\n}\n",
        "pub fn local() -> I32 {\nlet r#value = 42\nr#value\n}\n",
        "pub fn local() -> I32 {\nlet value = 42\nr#value\n}\n",
        "const ONE: I32 = 1\npub fn local() -> I32 {\nlet value = 42\nONE\n}\n",
        "pub fn local() -> Str {\nlet value = \"text\"\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet (first, second): (I32, I32) = (1, 2)\nfirst\n}\n",
        "pub fn local() -> I32 { let 0: I32 = 42 else { return 0 }\n1\n}\n",
        "pub fn local() -> I32 { let value: I32 = \"text\"\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet value: Missing = 42\nvalue\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = 42\nmissing\n}\n",
        "pub fn local() -> I32 {\nlet value: I32 = b\"text\"\nvalue\n}\n",
        "type Number = I32\npub fn local() -> I32 {\nlet value: Number = 42\nvalue\n}\n",
        "pub fn generic<T>(value: T) -> T { value }\n",
        "fn private(value: I32) -> I32 { value }\n",
        "pub fn identityValue(value: I32) -> I32 { value }\n",
        "pub fn identity(valueItem: I32) -> I32 { valueItem }\n",
        "pub fn non_ascii_param(valué: I32) -> I32 { valué }\n",
        "pub fn r#raw_function(value: I32) -> I32 { value }\n",
        "pub fn raw_parameter(r#value: I32) -> I32 { r#value }\n",
        "pub fn raw_tail(value: I32) -> I32 { r#value }\n",
        "pub fn raw_type(value: r#I32) -> r#I32 { value }\n",
        "pub fn identity(var value: I32) -> I32 { value }\n",
        "pub fn identity(value: Str) -> Str { value }\n",
        concat!(
            "pub fn first(value: I32) -> I32 { value }\n",
            "pub fn second(value: I32) -> I32 { value }\n",
        ),
        concat!(
            "@target(\"wasm64-core\")\n",
            "pub fn filtered(value: I32) -> I32 { value }\n",
            "pub fn identity(value: I32) -> I32 { value }\n",
        ),
    ] {
        assert_check_stays_gated(
            &run_check(source, "", &[], &[]),
            "function body outside the direct parameter slice",
        );
    }
}

#[test]
fn empty_root_source_emits_check_result_for_default_and_selected_target() {
    assert_success_result(run_check("", "", &[], &[]), "wasm32-core", "dev");
    assert_success_result(
        run_check("", "", &[], &["--target", "wasm32-core"]),
        "wasm32-core",
        "dev",
    );
    assert_success_result(
        run_check(
            "",
            "targets { wasm32-core, wasm64-core }\n",
            &[],
            &["--target", "wasm64-core"],
        ),
        "wasm64-core",
        "dev",
    );
}

#[test]
fn nonempty_or_extended_checks_remain_gated() {
    let cases: &[(&str, &str, &str, &[(&str, &str)], &[&str])] = &[
        ("declaration", "fn empty() {}\n", "", &[], &[]),
        ("use", "use missing;\n", "", &[], &[]),
        ("pub use", "pub use missing;\n", "", &[], &[]),
        ("malformed source", "fn {\n", "", &[], &[]),
        ("comment-only source", "// empty\n", "", &[], &[]),
        (
            "additional source module",
            "",
            "",
            &[("src/extra.sai", "")],
            &[],
        ),
        (
            "source dependency",
            "",
            "requires { acme:dep @ ^1 from path \"dep\" }\n",
            &[],
            &[],
        ),
        (
            "WIT package",
            "",
            "wit \"wit\"\n",
            &[("wit/api.wit", "package acme:empty@0.1.0;\n")],
            &[],
        ),
        (
            "artifact package",
            "",
            "artifact component { wasm32-wasi \"dist/app.wasm\" }\n",
            &[("dist/app.wasm", "artifact")],
            &[],
        ),
        ("test analysis", "", "", &[], &["--tests"]),
        ("fix mode", "", "", &[], &["--fix"]),
        ("non-core target", "", "", &[], &["--target", "wasm32-wasi"]),
        ("non-default profile", "", "", &[], &["--profile", "tiny"]),
        ("export world", "", "export world app\n", &[], &[]),
        ("locked mode", "", "", &[], &["--locked", "--offline"]),
        ("frozen mode", "", "", &[], &["--frozen"]),
    ];

    for (context, root_source, manifest_additions, extra_files, options) in cases {
        let output = run_check(root_source, manifest_additions, extra_files, options);
        assert_check_stays_gated(&output, context);
    }
}

#[test]
fn unlocked_checks_validate_compiler_and_declared_targets() {
    for source in ["", "fn empty() {}\n"] {
        let compiler = run_check(source, "compiler ^99.0\n", &[], &[]);
        assert_check_stays_gated(&compiler, "incompatible compiler");
        let compiler_stdout = String::from_utf8(compiler.stdout).expect("CLI stdout is UTF-8");
        assert!(compiler_stdout.contains("\"code\":\"COMPILER_VERSION_MISMATCH\""));

        let target = run_check(
            source,
            "targets { wasm32-core, imaginary-target }\n",
            &[],
            &[],
        );
        assert_check_stays_gated(&target, "unknown declared target");
        let target_stdout = String::from_utf8(target.stdout).expect("CLI stdout is UTF-8");
        assert!(target_stdout.contains("\"code\":\"UNKNOWN_TARGET\""));
    }
}

#[test]
fn public_primitive_const_literals_emit_check_results() {
    for source in [
        "pub const READY: Bool = true\n",
        "pub const LOW: I8 = -128\n",
        "pub const HIGH: I8 = 127\n",
        "pub const MAX: U8 = 255\n",
        "pub const ZERO: U8 = -0\n",
        "pub const MIN: Int = -9223372036854775808\n",
        "pub const MAX: UInt = 18446744073709551615\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn multiple_public_primitive_const_literals_emit_one_check_result() {
    assert_success_result(
        run_check(
            concat!(
                "pub const READY: Bool = true\n",
                "pub const I8_MIN: I8 = -128\n",
                "pub const U8_MAX: U8 = 255\n",
                "pub const INT_MAX: Int = 9223372036854775807\n",
                "pub const UINT_MAX: UInt = 18446744073709551615\n",
            ),
            "",
            &[],
            &[],
        ),
        "wasm32-core",
        "dev",
    );
}

#[test]
fn wasm64_wasi_remains_outside_the_narrow_success_slice() {
    let output = run_check(
        "",
        "targets { wasm64-wasi }\n",
        &[],
        &["--target", "wasm64-wasi"],
    );
    assert_check_stays_gated(
        &output,
        "wasm64-wasi is outside the core-target success slice",
    );
}

#[test]
fn pointer_sized_integer_const_literals_follow_the_selected_target_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    assert_success_result(
        run_check(
            concat!(
                "pub const ISIZE_MIN: ISize = -2147483648\n",
                "pub const ISIZE_MAX: ISize = 2147483647\n",
                "pub const USIZE_MAX: USize = 4294967295\n",
                "pub const USIZE_NEG_ZERO: USize = -0\n",
            ),
            manifest,
            &[],
            &["--target", "wasm32-core"],
        ),
        "wasm32-core",
        "dev",
    );

    for source in [
        "pub const TOO_SMALL: ISize = -2147483649\n",
        "pub const TOO_LARGE: ISize = 2147483648\n",
        "pub const TOO_LARGE: USize = 4294967296\n",
        "pub const NEGATIVE: USize = -1\n",
    ] {
        let output = run_check(source, manifest, &[], &["--target", "wasm32-core"]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: out-of-range pointer-sized const should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected a range error without a result or feature gate:\n{stdout}"
        );
    }

    assert_success_result(
        run_check(
            concat!(
                "pub const ISIZE_MIN: ISize = -9223372036854775808\n",
                "pub const ISIZE_MAX: ISize = 9223372036854775807\n",
                "pub const USIZE_MAX: USize = 18446744073709551615\n",
                "pub const ABOVE_WASM32: USize = 4294967296\n",
                "pub const USIZE_NEG_ZERO: USize = -0\n",
            ),
            manifest,
            &[],
            &["--target", "wasm64-core"],
        ),
        "wasm64-core",
        "dev",
    );

    for source in [
        "pub const TOO_SMALL: ISize = -9223372036854775809\n",
        "pub const TOO_LARGE: ISize = 9223372036854775808\n",
        "pub const TOO_LARGE: USize = 18446744073709551616\n",
        "pub const NEGATIVE: USize = -1\n",
    ] {
        let output = run_check(source, manifest, &[], &["--target", "wasm64-core"]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: out-of-range pointer-sized const should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected a range error without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn public_char_and_byte_const_literals_emit_check_results() {
    for source in [
        "pub const ASCII: Char = 'a'\n",
        "pub const UNICODE: Char = 'λ'\n",
        "pub const NULL: Char = '\\0'\n",
        "pub const MAX_SCALAR: Char = '\\u{10ffff}'\n",
        "pub const ASCII: Byte = b'z'\n",
        "pub const MIN: Byte = b'\\x00'\n",
        "pub const MAX: Byte = b'\\xff'\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn public_float_const_literals_emit_check_results() {
    for source in [
        "pub const POSITIVE: F32 = 1.25\n",
        "pub const NEGATIVE: F32 = -1.25\n",
        "pub const SEPARATORS: F32 = 1_234.5_0e+1_0\n",
        "pub const TIE_EVEN: F32 = 1.000000059604644775390625\n",
        "pub const UNDERFLOW: F32 = 1e-50\n",
        "pub const POSITIVE: F64 = 1.25\n",
        "pub const NEGATIVE: F64 = -1.25\n",
        "pub const TIE_EVEN: F64 = 1.00000000000000011102230246251565404236316680908203125\n",
        "pub const UNDERFLOW: F64 = 1e-400\n",
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn public_str_const_literals_emit_check_results() {
    let source = concat!(
        "pub const EMPTY: Str = \"\"\n",
        "pub const ESCAPED: Str = \"line\\nfeed\"\n",
        "pub const RAW: Str = r#\"raw { text }\"#\n",
        "pub const MULTILINE: Str = \"\"\"\n",
        "    first\n",
        "    second\n",
        "    \"\"\"\n",
        "pub const UNICODE: Str = \"こんにちは\"\n",
    );
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn public_str_const_type_errors_suppress_the_check_result() {
    for source in [
        "pub const BAD_BOOL: Str = true\n",
        "pub const BAD_BYTES: Str = b\"bytes\"\n",
        "pub const FIRST: Str = \"ok\"\npub const SECOND: Str = true\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: Str type mismatch should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected a supported Str type mismatch without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn str_const_forms_outside_the_literal_slice_remain_gated() {
    for source in [
        "pub const FORMATTED: Str = f\"text\"\n",
        "pub const BLOCK: Str = { \"text\" }\n",
        "pub const ALIAS: Text = \"text\"\n",
        "pub type Text = Str\npub const ALIAS: Text = \"text\"\n",
        "pub const RAW_TYPE: r#Str = \"text\"\n",
        "pub const BYTE_SLICE: Slice<Byte> = b\"bytes\"\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        assert_check_stays_gated(&output, source);
    }
}

#[test]
fn public_unit_const_literals_emit_check_results() {
    let source = "pub const UNIT: () = ()\npub const OTHER_UNIT: () = ()\n";
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn public_unit_const_type_errors_suppress_the_check_result() {
    for source in [
        "pub const BAD_BOOL: () = true\n",
        "pub const BAD_INTEGER: () = 0\n",
        "pub const BAD_STRING: () = \"text\"\n",
        "pub const FIRST: () = ()\npub const SECOND: () = 0\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: unit type mismatch should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected a supported unit type mismatch without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn unit_const_forms_outside_the_literal_slice_remain_gated() {
    for source in [
        "pub const NON_EMPTY_TUPLE: () = (1,)\n",
        "pub const BLOCK: () = { () }\n",
        "pub const ALIAS: Unit = ()\n",
        "pub type Unit = ()\npub const ALIAS: Unit = ()\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        assert_check_stays_gated(&output, source);
    }
}

#[test]
fn public_flat_tuple_const_literals_emit_check_results() {
    let source = concat!(
        "pub const PAIR: (I32, Bool) = (1, true)\n",
        "pub const SINGLE: (U8,) = (255,)\n",
        "pub const MIXED: (F32, Str, ()) = (1.25, \"text\", ())\n",
    );
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn public_flat_tuple_type_errors_suppress_the_check_result() {
    for (source, expected_code) in [
        (
            "pub const FIELD_MISMATCH: (I32, Bool) = (1, 2)\n",
            "TYPE_MISMATCH",
        ),
        (
            "pub const FIELD_RANGE: (I8,) = (128,)\n",
            "LITERAL_OUT_OF_RANGE",
        ),
        (
            "pub const ARITY_SHORT: (I32, Bool) = (1,)\n",
            "TYPE_MISMATCH",
        ),
        (
            "pub const ARITY_LONG: (I32,) = (1, true)\n",
            "TYPE_MISMATCH",
        ),
        ("pub const ARITY_ZERO: (I32,) = ()\n", "TYPE_MISMATCH"),
        ("pub const GROUPED_SCALAR: (I32,) = (1)\n", "TYPE_MISMATCH"),
        (
            "pub const SCALAR_MISMATCH: (I32, Bool) = false\n",
            "TYPE_MISMATCH",
        ),
        (
            "pub const UNIT_FIELD_MISMATCH: (I32, ()) = (1, true)\n",
            "TYPE_MISMATCH",
        ),
        (
            "pub const FIRST: (I32,) = (1,)\npub const SECOND: (I32,) = (true,)\n",
            "TYPE_MISMATCH",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: tuple const error should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!("\"code\":\"{expected_code}\""))
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected {expected_code} without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn tuple_const_forms_outside_the_flat_literal_slice_remain_gated() {
    for source in [
        "pub const NESTED: ((I32,),) = ((1,),)\n",
        "pub const ARRAY: Array<I32, 1> = [1]\n",
        "pub const BLOCK_FIELD: (I32,) = ({ 1 },)\n",
        "pub const EXPRESSION_FIELD: (I32,) = (1 + 2,)\n",
        "pub const UNIT_NONEMPTY_MISMATCH: () = (1,)\n",
        "pub const SCALAR_TUPLE_MISMATCH: I32 = (1,)\n",
        "pub type Number = I32\npub const ALIAS_FIELD: (Number,) = (1,)\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        assert_check_stays_gated(&output, source);
    }
}

#[test]
fn public_float_const_errors_and_invalid_later_const_suppress_results() {
    for (source, expected_code) in [
        (
            "pub const BAD: F32 = 3.4028236e38\n",
            "LITERAL_OUT_OF_RANGE",
        ),
        (
            "pub const BAD: F64 = 1.7976931348623159e308\n",
            "LITERAL_OUT_OF_RANGE",
        ),
        ("pub const BAD: F32 = true\n", "TYPE_MISMATCH"),
        ("pub const BAD: F64 = 1\n", "TYPE_MISMATCH"),
        (
            "pub const FIRST: F32 = 1.25\npub const SECOND: F32 = 3.4028236e38\n",
            "LITERAL_OUT_OF_RANGE",
        ),
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: float const error should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!("\"code\":\"{expected_code}\""))
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected supported float diagnostic `{expected_code}` without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn public_primitive_const_type_errors_do_not_emit_results() {
    let cases = [
        ("pub const BAD: Bool = 1\n", "TYPE_MISMATCH"),
        ("pub const BAD: I8 = true\n", "TYPE_MISMATCH"),
        ("pub const BAD: I8 = -129\n", "LITERAL_OUT_OF_RANGE"),
        ("pub const BAD: I8 = 128\n", "LITERAL_OUT_OF_RANGE"),
        ("pub const BAD: U8 = 256\n", "LITERAL_OUT_OF_RANGE"),
        (
            "pub const BAD: Int = 9223372036854775808\n",
            "LITERAL_OUT_OF_RANGE",
        ),
        (
            "pub const BAD: UInt = 18446744073709551616\n",
            "LITERAL_OUT_OF_RANGE",
        ),
    ];

    for (source, code) in cases {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: type-check failure should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!("\"code\":\"{code}\""))
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected supported {code} without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn char_and_byte_literal_type_mismatches_do_not_emit_results() {
    for source in [
        "pub const BAD_CHAR: Char = b'a'\n",
        "pub const BAD_BYTE: Byte = 'a'\n",
        "pub const BAD_BYTE: Byte = 255\n",
        "pub const BAD_CHAR: Char = \"a\"\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{source:?}: type mismatch should exit 1:\n{stdout}"
        );
        assert!(
            stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"type\":\"result\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "{source:?}: expected a supported type mismatch without a result or feature gate:\n{stdout}"
        );
    }
}

#[test]
fn escaped_bidi_char_literal_cannot_emit_a_check_result() {
    let output = run_check(r"pub const BIDI: Char = '\u{202e}'", "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "an escaped bidi control must fail the package check:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"FORBIDDEN_CHARACTER\"")
            && !stdout.contains("\"type\":\"result\""),
        "the escaped bidi control must be diagnosed and must not produce a result:\n{stdout}"
    );
}

#[test]
fn invalid_second_public_primitive_const_suppresses_the_check_result() {
    let output = run_check(
        "pub const FIRST: I8 = 1\npub const SECOND: U8 = 256\n",
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "an invalid later const should fail the package check:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "supported literal errors should suppress the result without gating the feature:\n{stdout}"
    );
}

#[test]
fn duplicate_public_primitive_const_names_report_a_duplicate_without_a_result() {
    let output = run_check(
        "pub const SAME: I8 = 1\npub const SAME: I8 = 2\n",
        "",
        &[],
        &[],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "duplicate const declarations should fail the package check:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"DUPLICATE_BINDING\"")
            && !stdout.contains("\"type\":\"result\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "duplicate names are a supported declaration error, not a feature gate:\n{stdout}"
    );
}

#[test]
fn const_forms_outside_literal_slice_remain_gated() {
    for source in [
        "const PRIVATE: I8 = 1\n",
        "pub const EXPRESSION: I8 = 1 + 2\n",
        "pub const ALIAS: MyInt = 1\n",
        "pub const BORROWED: &Bool = &true\n",
        "use missing;\npub const VALUE: I8 = 1\n",
        "unsafe module\npub const VALUE: I8 = 1\n",
        "property \"coverage\" () {}\npub const VALUE: I8 = 1\n",
        "@doc(hidden)\npub const ATTRIBUTED: I8 = 1\n",
        "pub const Max_size: I8 = 1\n",
        "pub const r#READY: Bool = true\n",
        "pub const RAW_TYPE: r#I32 = 1\n",
        "@target(\"wasm64-core\")\npub const OTHER: I8 = 1\npub const VALUE: I8 = 2\n",
        "test \"unselected\" {}\npub const VALUE: I8 = 2\n",
    ] {
        let output = run_check(source, "", &[], &[]);
        assert_check_stays_gated(&output, source);
    }
}

#[test]
fn primitive_bitwise_path_pairs_accept_matching_integer_and_byte_types() {
    let integer_types = [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ];
    for primitive in integer_types {
        let source = format!(
            concat!(
                "pub fn bitwise_{}(left: {primitive}, right: {primitive}) -> {primitive} {{\n",
                "    let and_value = left & right\n",
                "    let or_value = left | right\n",
                "    let xor_value = and_value ^ or_value\n",
                "    xor_value\n",
                "}}\n",
            ),
            primitive.to_ascii_lowercase(),
            primitive = primitive,
        );
        assert_success_result(run_check(&source, "", &[], &[]), "wasm32-core", "dev");
    }
    let byte_source = concat!(
        "pub fn byte_bitwise(left: Byte, right: Byte) -> Byte {\n",
        "    let and_value = left & right\n",
        "    let or_value = left | right\n",
        "    let xor_value = and_value ^ or_value\n",
        "    xor_value\n",
        "}\n",
    );
    assert_success_result(run_check(byte_source, "", &[], &[]), "wasm32-core", "dev");

    for source in [
        concat!(
            "pub fn preceding_locals(left: I32, right: I32) -> I32 {\n",
            "    let first = left\n",
            "    let second = right\n",
            "    let value = first ^ second\n",
            "    value\n",
            "}\n",
        ),
        concat!(
            "pub fn shadow(left: U32, right: I32) -> I32 {\n",
            "    let left: I32 = right\n",
            "    let value = left | right\n",
            "    value\n",
            "}\n",
        ),
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_bitwise_path_rhs_mismatches_recover_to_the_left_type() {
    for (name, left_type, right_type, operator) in
        [("integer", "I32", "U8", "&"), ("byte", "Byte", "Char", "|")]
    {
        let source = format!(
            concat!(
                "pub fn {name}_mismatch(left: {left_type}, right: {right_type}) -> {left_type} {{\n",
                "    let value = left {operator} right\n",
                "    let recovered: {left_type} = value\n",
                "    recovered\n",
                "}}\n",
            ),
            name = name,
            left_type = left_type,
            right_type = right_type,
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = source
            .find(&format!("left {operator} right"))
            .expect("mismatch expression exists");
        let right = expression + format!("left {operator} ").len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "mismatched right path should report one type mismatch:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "integer and Byte right-path mismatches are supported errors and recover to the left type:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &format!("expected `{left_type}`, found `{right_type}`"),
            right,
            right + "right".len(),
        );
    }
}

#[test]
fn primitive_shift_path_pairs_accept_integer_and_byte_left_types() {
    for primitive in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize", "Byte",
    ] {
        let source = format!(
            concat!(
                "pub fn shift_{}(left: {primitive}, amount: U32) -> {primitive} {{\n",
                "    let from_parameters = left << amount\n",
                "    let local_left: {primitive} = from_parameters\n",
                "    let local_amount: U32 = amount\n",
                "    let from_locals = local_left >> local_amount\n",
                "    from_locals\n",
                "}}\n",
            ),
            primitive.to_ascii_lowercase(),
            primitive = primitive,
        );
        let output = run_check(&source, "", &[], &[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "path shifts should work for {primitive}:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_shift_right_integer_literals_accept_integer_and_byte_left_types() {
    for primitive in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize", "Byte",
    ] {
        for (operator, suffix) in [("<<", "left"), (">>", "right")] {
            let source = format!(
                concat!(
                    "pub fn shift_{suffix}(left: {primitive}) -> {primitive} {{\n",
                    "    let from_literal = left {operator} 1\n",
                    "    let recovered: {primitive} = from_literal\n",
                    "    recovered\n",
                    "}}\n",
                ),
                primitive = primitive,
                suffix = suffix,
                operator = operator,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "right integer literals should work for {primitive} {operator}:\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }
}

#[test]
fn primitive_shift_left_integer_literals_infer_from_annotations_and_function_tails() {
    for primitive in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ] {
        for (operator, suffix) in [("<<", "left"), (">>", "right")] {
            let source = format!(
                concat!(
                    "pub fn shift_{fn_suffix}_{suffix}(amount: U32) -> {primitive} {{\n",
                    "    let shifted: {primitive} = 1 {operator} amount\n",
                    "    shifted\n",
                    "}}\n",
                ),
                fn_suffix = primitive.to_ascii_lowercase(),
                suffix = suffix,
                operator = operator,
                primitive = primitive,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "left integer literals should infer as {primitive} for {operator}:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }

    for primitive in ["I8", "I16", "I32", "I64", "ISize"] {
        let source = format!(
            concat!(
                "pub fn tail_{fn_suffix}(raw_amount: U32) -> {primitive} {{\n",
                "    let amount: U32 = raw_amount\n",
                "    let shifted = -1 >> amount\n",
                "    shifted\n",
                "}}\n",
            ),
            fn_suffix = primitive.to_ascii_lowercase(),
            primitive = primitive,
        );
        let output = run_check(&source, "", &[], &[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "the function tail should infer a signed left integer literal as {primitive}:\n{source}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_shift_byte_literal_left_accepts_u32_paths_and_integer_literals() {
    for operator in ["<<", ">>"] {
        for (suffix, right, annotation) in [
            ("parameter", "raw_amount", ""),
            ("local", "amount", ": Byte"),
            ("zero", "0", ""),
            ("maximum", "4294967295", ": Byte"),
        ] {
            let source = format!(
                concat!(
                    "pub fn byte_shift_{suffix}(raw_amount: U32) -> Byte {{\n",
                    "    let amount: U32 = raw_amount\n",
                    "    let shifted{annotation} = b'a' {operator} {right}\n",
                    "    let recovered: Byte = shifted\n",
                    "    recovered\n",
                    "}}\n",
                ),
                suffix = suffix,
                annotation = annotation,
                operator = operator,
                right = right,
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "Byte literal left shifts should accept {right} with {operator}:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }
}

#[test]
fn primitive_shift_byte_literal_left_rhs_path_mismatches_recover_to_byte() {
    for right_type in ["I32", "Byte", "Bool", "Char", "F32"] {
        let source = format!(
            concat!(
                "pub fn mismatch(amount: {right_type}) -> Byte {{\n",
                "    let shifted = b'a' << amount\n",
                "    let recovered: Byte = shifted\n",
                "    recovered\n",
                "}}\n",
            ),
            right_type = right_type,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let right_start = source
            .find("<< amount")
            .map(|start| start + "<< ".len())
            .expect("shift count path exists");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains("\"code\":\"TYPE_MISMATCH\""),
            "non-U32 path {right_type} should report a type mismatch:\n{stdout}"
        );
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "the non-U32 path should produce one type mismatch:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the invalid count should recover through a Byte local and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &format!("expected `U32`, found `{right_type}`"),
            right_start,
            right_start + "amount".len(),
        );
    }
}

#[test]
fn primitive_shift_byte_literal_left_count_ranges_use_u32_and_recover_to_byte() {
    for operator in ["<<", ">>"] {
        for literal in ["-1", "4294967296"] {
            let source = format!(
                concat!(
                    "pub fn out_of_range() -> Byte {{\n",
                    "    let shifted = b'a' {operator} {literal}\n",
                    "    let recovered: Byte = shifted\n",
                    "    recovered\n",
                    "}}\n",
                ),
                operator = operator,
                literal = literal,
            );
            let output = run_check(&source, "", &[], &[]);
            let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
            let literal_start = source.find(literal).expect("shift count literal exists");

            assert_eq!(output.status.code(), Some(1), "{stdout}");
            assert_eq!(
                stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
                1,
                "each invalid U32 count should produce one range diagnostic:\n{stdout}"
            );
            assert!(
                !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                    && !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                    && !stdout.contains("\"type\":\"result\""),
                "the invalid count should recover to Byte through the later local and tail:\n{stdout}"
            );
            assert_json_diagnostic_primary_span(
                &stdout,
                "LITERAL_OUT_OF_RANGE",
                "integer literal is out of range for `U32`",
                literal_start,
                literal_start + literal.len(),
            );
        }
    }
}

#[test]
fn primitive_shift_byte_literal_left_annotation_mismatch_recovers_to_annotation() {
    let source = concat!(
        "pub fn annotation_recovery(amount: U32) -> I8 {\n",
        "    let shifted: I8 = b'a' << amount\n",
        "    let recovered: I8 = shifted\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression_start = source
        .find("b'a' << amount")
        .expect("annotated Byte shift expression exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the incompatible annotation should produce one mismatch:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to I8 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I8`, found `Byte`",
        expression_start,
        expression_start + "b'a' << amount".len(),
    );
}

#[test]
fn primitive_shift_left_integer_literal_right_path_mismatch_recovers() {
    let source = concat!(
        "pub fn mismatch(amount: I16) -> I32 {\n",
        "    let shifted: I32 = -1 << amount\n",
        "    let recovered: I32 = shifted\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let right = source
        .find("<< amount")
        .map(|start| start + "<< ".len())
        .expect("shift-count path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "a non-U32 right path should produce one mismatch:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the annotated left literal should recover through the later local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I16`",
        right,
        right + "amount".len(),
    );
}

#[test]
fn primitive_shift_left_integer_literal_noninteger_annotation_reports_and_recovers() {
    let source = concat!(
        "pub fn annotation_mismatch(amount: U32) -> Bool {\n",
        "    let shifted: Bool = 1 << amount\n",
        "    let recovered: Bool = shifted\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let initializer = source
        .find("1 << amount")
        .expect("shift initializer exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "an integer literal cannot satisfy a non-integer annotation:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the annotation should remain the recovered local type:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found an integer literal",
        initializer,
        initializer + "1 << amount".len(),
    );
}

#[test]
fn primitive_shift_left_unconstrained_integer_literal_defaults_to_i64() {
    let source = concat!(
        "pub fn default(amount: U32) -> Bool {\n",
        "    let unused = 9223372036854775808 << amount\n",
        "    let result: Bool = true\n",
        "    result\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source
        .find("9223372036854775808")
        .expect("defaulted integer literal exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "an unconstrained integer left literal should default to I64:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the defaulted literal error should not gate the function:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        literal_start,
        literal_start + "9223372036854775808".len(),
    );
}

#[test]
fn primitive_shift_left_integer_literal_ranges_use_annotation_target_width_and_full_span() {
    for (target, primitive, literal, expected_error) in [
        ("wasm32-core", "ISize", "-2147483648", false),
        ("wasm32-core", "ISize", "-2147483649", true),
        ("wasm64-core", "ISize", "-9223372036854775808", false),
        ("wasm32-core", "USize", "4294967295", false),
        ("wasm32-core", "USize", "4294967296", true),
        ("wasm64-core", "USize", "4294967296", false),
        ("wasm32-core", "I8", "-129", true),
    ] {
        let source = format!(
            concat!(
                "pub fn shift(amount: U32) -> {primitive} {{\n",
                "    let shifted: {primitive} = {literal} << amount\n",
                "    let recovered: {primitive} = shifted\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
            literal = literal,
        );
        let output = run_check(
            &source,
            "targets { wasm32-core, wasm64-core }\n",
            &[],
            &["--target", target],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if expected_error {
            let literal_start = source.find(literal).expect("shift-left literal exists");
            assert_eq!(output.status.code(), Some(1), "{stdout}");
            assert_eq!(
                stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
                1,
                "out-of-range left literals should report one range error:\n{stdout}"
            );
            assert!(
                !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                    && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                    && !stdout.contains("\"type\":\"result\""),
                "range errors should recover through the following local and tail:\n{stdout}"
            );
            assert_json_diagnostic_primary_span(
                &stdout,
                "LITERAL_OUT_OF_RANGE",
                &format!("integer literal is out of range for `{primitive}`"),
                literal_start,
                literal_start + literal.len(),
            );
        } else {
            assert_eq!(
                output.status.code(),
                Some(0),
                "the left literal {literal} should fit {primitive} on {target}:\n{source}\n{stdout}"
            );
            assert_success_result(output, target, "dev");
        }
    }

    let source = concat!(
        "pub fn inferred(amount: U32) -> ISize {\n",
        "    let shifted = -2147483649 >> amount\n",
        "    shifted\n",
        "}\n",
    );
    let output = run_check(
        source,
        "targets { wasm32-core, wasm64-core }\n",
        &[],
        &["--target", "wasm32-core"],
    );
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source
        .find("-2147483649")
        .expect("tail-inferred pointer-sized literal exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the function tail should resolve the pointer-sized left literal:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the tail-inferred range error should not prevent recovery:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `ISize`",
        literal_start,
        literal_start + "-2147483649".len(),
    );
}

#[test]
fn primitive_shift_right_integer_literal_range_uses_u32_and_recovers() {
    for (operator, suffix) in [("<<", "left"), (">>", "right")] {
        for literal in ["-1", "4294967296"] {
            let source = format!(
                concat!(
                    "pub fn out_of_range_{suffix}(left: I32) -> I32 {{\n",
                    "    let value = left {operator} {literal}\n",
                    "    let recovered: I32 = value\n",
                    "    recovered\n",
                    "}}\n",
                ),
                suffix = suffix,
                operator = operator,
                literal = literal,
            );
            let output = run_check(&source, "", &[], &[]);
            let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
            let literal_start = source.find(literal).expect("shift-count literal exists");

            assert_eq!(output.status.code(), Some(1), "{stdout}");
            assert_eq!(
                stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
                1,
                "each invalid U32 literal should produce one range diagnostic:\n{stdout}"
            );
            assert!(
                !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                    && !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                    && !stdout.contains("\"type\":\"result\""),
                "the invalid count should recover to I32 through the later local and tail:\n{stdout}"
            );
            assert_json_diagnostic_primary_span(
                &stdout,
                "LITERAL_OUT_OF_RANGE",
                "integer literal is out of range for `U32`",
                literal_start,
                literal_start + literal.len(),
            );
        }
    }
}

#[test]
fn primitive_shift_right_integer_literal_u32_boundaries_and_left_widths_are_type_valid() {
    for (operator, suffix, literal) in [
        ("<<", "zero_left", "0"),
        (">>", "zero_right", "0"),
        ("<<", "max_left", "4294967295"),
        (">>", "max_right", "4294967295"),
        ("<<", "i32_width", "32"),
        (">>", "i32_width_right", "32"),
        ("<<", "byte_width", "8"),
        (">>", "byte_width_right", "8"),
    ] {
        let primitive = if suffix.starts_with("byte_") {
            "Byte"
        } else {
            "I32"
        };
        let source = format!(
            concat!(
                "pub fn boundary_{suffix}(left: {primitive}) -> {primitive} {{\n",
                "    let value = left {operator} {literal}\n",
                "    value\n",
                "}}\n",
            ),
            suffix = suffix,
            primitive = primitive,
            operator = operator,
            literal = literal,
        );
        let output = run_check(&source, "", &[], &[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "in-range U32 count {literal} should type-check for {primitive} {operator}:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_success_result(output, "wasm32-core", "dev");
    }
}

#[test]
fn primitive_shift_right_path_mismatches_report_the_operand_and_recover_to_the_left_type() {
    let right_types = [
        "I8", "I16", "I32", "I64", "U8", "U16", "U64", "ISize", "USize", "Byte", "Bool", "Char",
        "F32", "F64",
    ];
    for (index, right_type) in right_types.into_iter().enumerate() {
        for (operator, suffix) in [("<<", "left"), (">>", "right")] {
            let function = format!(
                concat!(
                    "pub fn mismatch_{index}_{suffix}(left: I32, amount: {right_type}) -> I32 {{\n",
                    "    let value = left {operator} amount\n",
                    "    let recovered: I32 = value\n",
                    "    recovered\n",
                    "}}\n",
                ),
                index = index,
                suffix = suffix,
                right_type = right_type,
                operator = operator,
            );
            let expression = format!("left {operator} amount");
            let expression_start = function
                .find(&expression)
                .expect("shift expression exists in generated function");
            let right_start = expression_start + format!("left {operator} ").len();
            let output = run_check(&function, "", &[], &[]);
            let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");

            assert_eq!(
                output.status.code(),
                Some(1),
                "I32 << or >> {right_type} should produce a supported type error:\n{stdout}"
            );
            assert_eq!(
                stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
                1,
                "each non-U32 right path should produce one mismatch:\n{stdout}"
            );
            assert!(
                !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                    && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                    && !stdout.contains("\"type\":\"result\""),
                "integer left operands should recover to I32 after right-path mismatches:\n{stdout}"
            );
            assert_json_diagnostic_primary_span(
                &stdout,
                "TYPE_MISMATCH",
                &format!("expected `U32`, found `{right_type}`"),
                right_start,
                right_start + "amount".len(),
            );
        }
    }
}

#[test]
fn primitive_shift_rhs_and_annotation_mismatches_recover_through_following_local() {
    let source = concat!(
        "pub fn annotation_recovery(left: I32, amount: I8) -> U32 {\n",
        "    let shifted: U32 = left << amount\n",
        "    let recovered: U32 = shifted\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression = "left << amount";
    let expression_start = source.find(expression).expect("shift expression exists");
    let right_start = expression_start + "left << ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "the RHS and initializer annotation should both mismatch:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the annotated local should recover to U32 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        expression_start,
        expression_start + expression.len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I8`",
        right_start,
        right_start + "amount".len(),
    );
    let annotation_position = stdout
        .find("\"message\":\"expected `U32`, found `I32`\"")
        .expect("annotation mismatch exists");
    let right_position = stdout
        .find("\"message\":\"expected `U32`, found `I8`\"")
        .expect("shift-count mismatch exists");
    assert!(
        annotation_position < right_position,
        "CLI output should sort the full initializer span before the nested RHS mismatch:\n{stdout}"
    );
}

#[test]
fn primitive_shift_right_literal_range_and_annotation_errors_recover_through_following_local() {
    let source = concat!(
        "pub fn annotation_recovery(left: I32) -> U32 {\n",
        "    let shifted: U32 = left << -1\n",
        "    let recovered: U32 = shifted\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression = "left << -1";
    let expression_start = source.find(expression).expect("shift expression exists");
    let literal_start = expression_start + "left << ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the negative U32 shift count should report one range error:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the recovered I32 expression should mismatch its U32 annotation:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the annotation should recover the later local and function tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `U32`",
        literal_start,
        literal_start + "-1".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        expression_start,
        expression_start + expression.len(),
    );
}

#[test]
fn primitive_shift_noninteger_left_paths_report_full_expression_diagnostics_and_recover() {
    for (primitive, operator, suffix) in [
        ("Bool", "<<", "left"),
        ("Bool", ">>", "right"),
        ("F32", "<<", "left"),
        ("F32", ">>", "right"),
        ("F64", "<<", "left"),
        ("F64", ">>", "right"),
        ("Char", "<<", "left"),
        ("Char", ">>", "right"),
    ] {
        let source = format!(
            concat!(
                "pub fn unsupported_{suffix}(left: {primitive}, amount: U32) -> {primitive} {{\n",
                "    let value = left {operator} amount\n",
                "    let recovered: {primitive} = value\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
            operator = operator,
            suffix = suffix,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("left {operator} amount");
        let expression_start = source.find(&expression).expect("shift expression exists");
        let message = format!("operator `{operator}` is not defined for `{primitive}`");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "{primitive} should report one unsupported shift diagnostic:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "a U32 RHS should leave only the unsupported-left diagnostic and recover:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &message,
            expression_start,
            expression_start + expression.len(),
        );
    }
}

#[test]
fn primitive_shift_right_integer_literal_on_noninteger_left_reports_unsupported_operator() {
    for (primitive, operator, suffix) in [
        ("Bool", "<<", "bool_left"),
        ("Bool", ">>", "bool_right"),
        ("F32", "<<", "f32_left"),
        ("F32", ">>", "f32_right"),
        ("F64", "<<", "f64_left"),
        ("F64", ">>", "f64_right"),
        ("Char", "<<", "char_left"),
        ("Char", ">>", "char_right"),
    ] {
        let source = format!(
            concat!(
                "pub fn unsupported_{suffix}(left: {primitive}) -> {primitive} {{\n",
                "    let value = left {operator} 1\n",
                "    let recovered: {primitive} = value\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
            operator = operator,
            suffix = suffix,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("left {operator} 1");
        let expression_start = source.find(&expression).expect("shift expression exists");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "{primitive} left operand should keep its unsupported-operator diagnostic:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"type\":\"result\""),
            "a valid U32 literal count should recover to the left type:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &format!("operator `{operator}` is not defined for `{primitive}`"),
            expression_start,
            expression_start + expression.len(),
        );
    }
}

#[test]
fn primitive_shift_out_of_range_literal_keeps_unsupported_left_diagnostic() {
    for primitive in ["Bool", "F32", "F64", "Char"] {
        let source = format!(
            concat!(
                "pub fn unsupported(left: {primitive}) -> {primitive} {{\n",
                "    let value = left << -1\n",
                "    let recovered: {primitive} = value\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = "left << -1";
        let expression_start = source.find(expression).expect("shift expression exists");
        let literal_start = expression_start + "left << ".len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
            1,
            "the U32 literal must still be range-checked for `{primitive}`:\n{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "the unsupported left operator diagnostic must remain:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "both independent errors should preserve left-type recovery:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "LITERAL_OUT_OF_RANGE",
            "integer literal is out of range for `U32`",
            literal_start,
            literal_start + "-1".len(),
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &format!("operator `<<` is not defined for `{primitive}`"),
            expression_start,
            expression_start + expression.len(),
        );
    }
}

#[test]
fn primitive_shift_noninteger_left_and_non_u32_right_report_both_diagnostics_in_span_order() {
    for (primitive, operator, suffix) in [
        ("Bool", "<<", "left"),
        ("Bool", ">>", "right"),
        ("F32", "<<", "left"),
        ("F32", ">>", "right"),
        ("F64", "<<", "left"),
        ("F64", ">>", "right"),
        ("Char", "<<", "left"),
        ("Char", ">>", "right"),
    ] {
        let source = format!(
            concat!(
                "pub fn invalid_{suffix}(left: {primitive}, amount: I32) -> {primitive} {{\n",
                "    let value = left {operator} amount\n",
                "    let recovered: {primitive} = value\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
            operator = operator,
            suffix = suffix,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("left {operator} amount");
        let expression_start = source.find(&expression).expect("shift expression exists");
        let expression_end = expression_start + expression.len();
        let right_start = expression_start + format!("left {operator} ").len();
        let operator_message = format!("operator `{operator}` is not defined for `{primitive}`");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "a non-U32 RHS should report one shift-count mismatch:\n{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "{primitive} should also report one unsupported shift diagnostic:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "both diagnostics should recover through the local and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            "expected `U32`, found `I32`",
            right_start,
            right_start + "amount".len(),
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &operator_message,
            expression_start,
            expression_end,
        );
        let operator_position = stdout
            .find(&format!("\"message\":\"{operator_message}\""))
            .expect("unsupported shift diagnostic exists");
        let mismatch_position = stdout
            .find("\"message\":\"expected `U32`, found `I32`\"")
            .expect("shift-count mismatch exists");
        assert!(
            operator_position < mismatch_position,
            "CLI output should sort the full-expression diagnostic before the RHS mismatch:\n{stdout}"
        );
    }
}

#[test]
fn primitive_bitwise_bool_float_and_char_diagnostics_have_exact_spans_and_order() {
    let cases = [
        (
            "bool_mismatch",
            "Bool",
            "I32",
            "^",
            "Bool",
            "operator `^` is not defined for `Bool`",
        ),
        (
            "bool_same",
            "Bool",
            "Bool",
            "&",
            "Bool",
            "operator `&` is not defined for `Bool`",
        ),
        (
            "float_mismatch",
            "F64",
            "F32",
            "|",
            "F64",
            "operator `|` is not defined for `F64`",
        ),
        (
            "float_same",
            "F32",
            "F32",
            "^",
            "F32",
            "operator `^` is not defined for `F32`",
        ),
        (
            "char_and",
            "Char",
            "U8",
            "&",
            "Char",
            "operator `&` has no `BitAnd<U8>` implementation for `Char`",
        ),
        (
            "char_or",
            "Char",
            "Char",
            "|",
            "Char",
            "operator `|` has no `BitOr<Char>` implementation for `Char`",
        ),
        (
            "char_xor",
            "Char",
            "I32",
            "^",
            "Char",
            "operator `^` has no `BitXor<I32>` implementation for `Char`",
        ),
    ];

    for (name, left_type, right_type, operator, return_type, operator_message) in cases {
        let source = format!(
            concat!(
                "pub fn {name}(left: {left_type}, right: {right_type}) -> {return_type} {{\n",
                "    let value = left {operator} right\n",
                "    value\n",
                "}}\n",
            ),
            name = name,
            left_type = left_type,
            right_type = right_type,
            return_type = return_type,
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = source
            .find(&format!("left {operator} right"))
            .expect("operator expression exists");
        let expression_end = expression + format!("left {operator} right").len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "{name} should report one unsupported-operator diagnostic:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "these primitive operand combinations have supported diagnostics:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            operator_message,
            expression,
            expression_end,
        );
        let mismatch_position = stdout.find("\"code\":\"TYPE_MISMATCH\"");
        if left_type == "Bool" && right_type != left_type
            || matches!(left_type, "F32" | "F64") && right_type != left_type
        {
            assert!(mismatch_position.is_some(), "{stdout}");
            assert_eq!(
                stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
                1,
                "{stdout}"
            );
            assert!(
                stdout
                    .find(&format!("\"message\":\"{operator_message}\""))
                    .expect("operator diagnostic precedes its mismatch")
                    < mismatch_position.unwrap(),
                "CLI output should sort the whole-expression error before the RHS mismatch:\n{stdout}"
            );
            let right = expression + format!("left {operator} ").len();
            assert_json_diagnostic_primary_span(
                &stdout,
                "TYPE_MISMATCH",
                &format!("expected `{left_type}`, found `{right_type}`"),
                right,
                right + "right".len(),
            );
        } else {
            assert!(mismatch_position.is_none(), "{stdout}");
        }
    }
}

#[test]
fn primitive_bitwise_integer_annotation_mismatch_recovers_after_operand_mismatch() {
    let source = concat!(
        "pub fn annotation_recovery(left: I32, right: U8) -> Bool {\n",
        "    let value: Bool = left & right\n",
        "    let recovered: Bool = value\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression = source
        .find("left & right")
        .expect("annotated initializer expression exists");
    let right = expression + "left & ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "the operand mismatch and annotation mismatch should both be reported:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the initializer should recover to its Bool annotation and reach the following local:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found `I32`",
        expression,
        expression + "left & right".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I32`, found `U8`",
        right,
        right + "right".len(),
    );

    let annotation_position = stdout
        .find("\"message\":\"expected `Bool`, found `I32`\"")
        .expect("annotation mismatch exists");
    let operand_position = stdout
        .find("\"message\":\"expected `I32`, found `U8`\"")
        .expect("right operand mismatch exists");
    assert!(
        annotation_position < operand_position,
        "CLI output should sort the full initializer span before its nested RHS span:\n{stdout}"
    );
}

#[test]
fn unsupported_primitive_bitwise_shapes_remain_gated() {
    let fixture = |signature: &str, statements: &str| {
        format!("{signature} {{\n    {statements}\n    result\n}}\n")
    };
    for source in [
        fixture("pub fn literal_pair() -> I32", "let result = 1 & 2"),
        fixture(
            "pub fn integer_byte_literal_pair() -> I32",
            "let result = 1 & b'a'",
        ),
        fixture(
            "pub fn integer_float_literal_pair() -> F32",
            "let result = 1.0 & 2.0",
        ),
        fixture(
            "pub fn bool_literal_pair() -> Bool",
            "let result = true & false",
        ),
        fixture(
            "pub fn char_literal_pair() -> Char",
            "let result = 'a' ^ 'b'",
        ),
        fixture(
            "pub fn bool_byte_literal_pair() -> Bool",
            "let result = true & b'a'",
        ),
        fixture(
            "pub fn char_byte_literal_pair() -> Char",
            "let result = 'a' ^ b'b'",
        ),
        fixture(
            "pub fn float_byte_literal_pair() -> F32",
            "let result = 1.0 & b'a'",
        ),
        fixture(
            "pub fn byte_float_literal_rhs() -> Byte",
            "let result = b'a' | 1.0",
        ),
        fixture(
            "pub fn byte_bool_literal_rhs() -> Byte",
            "let result = b'a' & true",
        ),
        fixture(
            "pub fn byte_char_literal_rhs() -> Byte",
            "let result = b'a' ^ 'b'",
        ),
        fixture(
            "pub fn byte_string_left(right: Byte) -> Byte",
            "let result = b\"text\" & right",
        ),
        format!(
            "type ByteAlias = Byte\n{}",
            fixture(
                "pub fn byte_alias_rhs(right: ByteAlias) -> Byte",
                "let result = b'a' & right"
            )
        ),
        fixture(
            "pub fn byte_unresolved_rhs() -> Byte",
            "let result = b'a' & missing",
        ),
        fixture(
            "pub fn byte_unresolved_integer_rhs() -> Byte",
            "let integer = 1\n    let result = b'a' & integer",
        ),
        fixture(
            "pub fn byte_self_reference() -> Byte",
            "let result = b'a' & result",
        ),
        fixture(
            "pub fn byte_forward_reference() -> Byte",
            "let result = b'a' & later\n    let later: Byte = b'b'",
        ),
        fixture(
            "pub fn byte_nested_rhs(left: Byte, right: Byte) -> Byte",
            "let result = b'a' & (left & right)",
        ),
        fixture(
            "pub fn byte_nested_left(right: Byte, right2: Byte) -> Byte",
            "let result = (b'a' & right) ^ right2",
        ),
        fixture(
            "pub fn byte_computed_left(right: Byte) -> Byte",
            "let result = (~b'a') & right",
        ),
        fixture(
            "pub fn byte_computed_rhs() -> Byte",
            "let result = b'a' & (1 + 0)",
        ),
        fixture(
            "pub fn nested(left: I32, right: I32, extra: I32) -> I32",
            "let result = left & (right ^ extra)",
        ),
        fixture(
            "pub fn computed(left: I32, right: I32) -> I32",
            "let result = (left + right) & right",
        ),
        fixture(
            "pub fn unresolved(left: I32) -> I32",
            "let bits = 1\n    let result = bits & left",
        ),
        format!(
            "type Word = I32\n{}",
            fixture(
                "pub fn alias(left: Word, right: Word) -> Word",
                "let result = left & right"
            )
        ),
        fixture(
            "pub fn self_reference(left: I32) -> I32",
            "let result = result & left",
        ),
        fixture(
            "pub fn forward_reference(left: I32) -> I32",
            "let result = later & left\n    let later: I32 = 1",
        ),
        fixture(
            "pub fn generic<T>(unused: I32) -> Byte",
            "let result = b'a' & b'b'",
        ),
        fixture(
            "pub fn generic_integer_rhs<T>(unused: I32) -> Byte",
            "let result = b'a' & 1",
        ),
        concat!(
            "type BitOwner { marker: Byte }\n",
            "impl BitOwner {\n",
            "    fn method(self: borrow Self) -> Byte {\n",
            "        let result = b'a' & b'b'\n",
            "        result\n",
            "    }\n",
            "}\n",
        )
        .to_owned(),
        concat!(
            "type IntegerBitOwner { marker: Byte }\n",
            "impl IntegerBitOwner {\n",
            "    fn integer_method(self: borrow Self) -> Byte {\n",
            "        let result = b'a' & 1\n",
            "        result\n",
            "    }\n",
            "}\n",
        )
        .to_owned(),
    ] {
        assert_check_stays_gated_without_syntax_errors(&run_check(&source, "", &[], &[]), &source);
    }
}

#[test]
fn unsupported_primitive_shift_shapes_and_owners_remain_gated() {
    for source in [
        "pub fn byte_literal_right(left: Byte) -> Byte { let result = left >> b'a'; result }\n",
        "pub fn byte_literal_pair(amount: U32) -> Byte { let result = b'a' << b'b'; result }\n",
        "pub fn byte_literal_bool_right(amount: U32) -> Byte { let result = b'a' << true; result }\n",
        "pub fn byte_literal_char_right(amount: U32) -> Byte { let result = b'a' >> 'a'; result }\n",
        "pub fn byte_literal_float_right(amount: U32) -> Byte { let result = b'a' << 1.0; result }\n",
        "pub fn byte_string_left(amount: U32) -> Byte { let result = b\"text\" << amount; result }\n",
        "type ShiftCount = U32\npub fn aliased_byte_literal_left(amount: ShiftCount) -> Byte { let result = b'a' << amount; result }\n",
        "pub fn unresolved_byte_literal_left() -> Byte { let result = b'a' << missing; result }\n",
        "pub fn self_reference_byte_literal_left() -> Byte { let result = b'a' << result; result }\n",
        "pub fn forward_reference_byte_literal_left() -> Byte { let result = b'a' << later; let later: U32 = 1; result }\n",
        "pub fn nested_byte_literal_left(amount: U32) -> Byte { let result = (b'a' << amount) >> amount; result }\n",
        "pub fn nested_byte_literal_right(amount: U32, extra: U32) -> Byte { let result = b'a' << (amount >> extra); result }\n",
        "pub fn computed_byte_literal_left(amount: U32) -> Byte { let result = (~b'a') << amount; result }\n",
        "pub fn bool_literal_right(left: I32) -> I32 { let result = left << true; result }\n",
        "pub fn char_literal_right(left: I32) -> I32 { let result = left >> 'a'; result }\n",
        "pub fn float_literal_right(left: I32) -> I32 { let result = left << 1.0; result }\n",
        "pub fn literal_pair() -> I32 { let result = 1 << 2; result }\n",
        "type ShiftCount = U32\npub fn aliased_literal_left(amount: ShiftCount) -> I32 { let result = 1 << amount; result }\n",
        "pub fn unresolved_numeric_literal_left() -> I32 { let amount = 1; let result = 1 << amount; result }\n",
        "pub fn unresolved_path_literal_left() -> I32 { let result = 1 << missing; result }\n",
        "pub fn self_reference_literal_left() -> I32 { let result = 1 << result; result }\n",
        "pub fn forward_reference_literal_left() -> I32 { let result = 1 << later; let later: U32 = 1; result }\n",
        "type Word = I32\ntype ShiftCount = U32\npub fn aliased_left(left: Word, amount: U32) -> Word { let result = left << amount; result }\n",
        "type ShiftCount = U32\npub fn aliased_right(left: I32, amount: ShiftCount) -> I32 { let result = left >> amount; result }\n",
        "pub fn unresolved_numeric_right(left: I32) -> I32 { let amount = 1; let result = left << amount; result }\n",
        "pub fn unresolved(left: I32) -> I32 { let result = left << missing; result }\n",
        "pub fn self_reference(left: I32, amount: U32) -> I32 { let result = result << amount; result }\n",
        "pub fn forward_reference(left: I32, amount: U32) -> I32 { let result = later << amount; let later: I32 = left; result }\n",
        "pub fn computed_left(left: I32, amount: U32) -> I32 { let result = (left + left) << amount; result }\n",
        "pub fn nested_left(left: I32, amount: U32) -> I32 { let result = (left << amount) >> amount; result }\n",
        "pub fn nested_right(left: I32, amount: U32, extra: U32) -> I32 { let result = left << (amount >> extra); result }\n",
        "pub fn generic<T>(left: I32, amount: U32) -> I32 { let result = left << amount; result }\n",
        concat!(
            "type ShiftOwner { marker: I32 }\n",
            "impl ShiftOwner {\n",
            "    fn method(self: borrow Self, left: I32, amount: U32) -> I32 {\n",
            "        let result = left << amount\n",
            "        result\n",
            "    }\n",
            "}\n",
        ),
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn primitive_bitwise_accepts_right_integer_literals_for_every_integer_type_and_operator() {
    for primitive in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "ISize", "USize",
    ] {
        for (operator, name) in [("&", "and"), ("|", "or"), ("^", "xor")] {
            let source = format!(
                "pub fn {name}_{}(value: {primitive}) -> {primitive} {{\n    let result = value {operator} 1\n    result\n}}\n",
                primitive.to_ascii_lowercase()
            );
            let output = run_check(&source, "", &[], &[]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{operator} with a right integer literal should accept {primitive}:\n{source}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_success_result(output, "wasm32-core", "dev");
        }
    }
}

#[test]
fn primitive_bitwise_right_integer_literal_resolves_preceding_and_shadowed_paths() {
    for source in [
        concat!(
            "pub fn preceding(value: I32) -> I32 {\n",
            "    let earlier: I32 = value\n",
            "    let result = earlier ^ 7\n",
            "    result\n",
            "}\n",
        ),
        concat!(
            "pub fn shadow(value: U32, right: I32) -> I32 {\n",
            "    let value: I32 = right\n",
            "    let result = value | 11\n",
            "    result\n",
            "}\n",
        ),
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_bitwise_right_integer_literals_accept_fixed_width_boundaries() {
    let source = concat!(
        "pub fn boundaries(\n",
        "    signed8: I8, signed16: I16, signed32: I32, signed64: I64,\n",
        "    unsigned8: U8, unsigned16: U16, unsigned32: U32, unsigned64: U64\n",
        ") -> I64 {\n",
        "    let signed8_min = signed8 & -128\n",
        "    let signed8_max = signed8 | 127\n",
        "    let signed16_min = signed16 & -32768\n",
        "    let signed16_max = signed16 ^ 32767\n",
        "    let signed32_min = signed32 & -2147483648\n",
        "    let signed32_max = signed32 | 2147483647\n",
        "    let signed64_min = signed64 & -9223372036854775808\n",
        "    let signed64_max = signed64 ^ 9223372036854775807\n",
        "    let unsigned8_max = unsigned8 ^ 255\n",
        "    let unsigned8_negative_zero = unsigned8 & -0\n",
        "    let unsigned16_max = unsigned16 | 65535\n",
        "    let unsigned32_max = unsigned32 & 4294967295\n",
        "    let unsigned64_max = unsigned64 ^ 18446744073709551615\n",
        "    signed64_min\n",
        "}\n",
    );
    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_right_integer_literal_range_errors_cover_signed_spans_and_recover() {
    let source = concat!(
        "pub fn out_of_range(signed: I8, unsigned: U8) -> I8 {\n",
        "    let signed_low = signed & -129\n",
        "    let signed_high = signed | 128\n",
        "    let unsigned_high = unsigned ^ 256\n",
        "    let unsigned_negative = unsigned & -1\n",
        "    let recovered: I8 = signed_low\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_spans = [
        (
            source
                .find("signed & -129")
                .map(|start| start + "signed & ".len())
                .expect("negative signed boundary exists"),
            "-129",
            "integer literal is out of range for `I8`",
        ),
        (
            source
                .find("signed | 128")
                .map(|start| start + "signed | ".len())
                .expect("positive signed boundary exists"),
            "128",
            "integer literal is out of range for `I8`",
        ),
        (
            source
                .find("unsigned ^ 256")
                .map(|start| start + "unsigned ^ ".len())
                .expect("positive unsigned boundary exists"),
            "256",
            "integer literal is out of range for `U8`",
        ),
        (
            source
                .find("unsigned & -1")
                .map(|start| start + "unsigned & ".len())
                .expect("negative unsigned boundary exists"),
            "-1",
            "integer literal is out of range for `U8`",
        ),
    ];

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        literal_spans.len(),
        "each invalid right literal should be range-checked against the left path:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "range errors should recover the expression and continue through the annotated local:\n{stdout}"
    );
    for (start, literal, message) in literal_spans {
        assert_json_diagnostic_primary_span(
            &stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }
}

#[test]
fn primitive_bitwise_right_integer_literal_uses_wasm_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let source = concat!(
        "pub fn pointer_boundaries(signed: ISize, unsigned: USize) -> ISize {\n",
        "    let signed_min_32 = signed & -2147483648\n",
        "    let signed_max_32 = signed | 2147483647\n",
        "    let unsigned_max_32 = unsigned ^ 4294967295\n",
        "    let signed_min_64 = signed & -9223372036854775808\n",
        "    let signed_max_64 = signed | 9223372036854775807\n",
        "    let unsigned_max_64 = unsigned ^ 18446744073709551615\n",
        "    signed_min_32\n",
        "}\n",
    );

    let wasm32 = run_check(&source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        3,
        "wasm32 must reject the 64-bit ISize and USize bounds:\n{wasm32_stdout}"
    );
    assert!(
        !wasm32_stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !wasm32_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !wasm32_stdout.contains("\"type\":\"result\""),
        "pointer-width range failures should recover to the left path type:\n{wasm32_stdout}"
    );
    for (expression, literal, message) in [
        (
            "signed & -9223372036854775808",
            "-9223372036854775808",
            "integer literal is out of range for `ISize`",
        ),
        (
            "signed | 9223372036854775807",
            "9223372036854775807",
            "integer literal is out of range for `ISize`",
        ),
        (
            "unsigned ^ 18446744073709551615",
            "18446744073709551615",
            "integer literal is out of range for `USize`",
        ),
    ] {
        let start = source
            .find(expression)
            .map(|start| start + expression.find(literal).expect("literal is in expression"))
            .expect("pointer-bound expression exists");
        assert_json_diagnostic_primary_span(
            &wasm32_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let wasm64 = run_check(&source, manifest, &[], &["--target", "wasm64-core"]);
    assert_eq!(
        wasm64.status.code(),
        Some(0),
        "the pointer-sized 64-bit bounds should fit wasm64:\n{}",
        String::from_utf8_lossy(&wasm64.stdout)
    );
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn primitive_bitwise_byte_right_integer_literal_reports_mismatch_without_unsupported_operator() {
    let source = concat!(
        "pub fn byte_operand(value: Byte) -> Byte {\n",
        "    let result = value & 1\n",
        "    let recovered: Byte = result\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source
        .find("value & 1")
        .map(|start| start + "value & ".len())
        .expect("right integer literal exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "Byte has no integer-literal type, so report a literal mismatch:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the Byte bitwise operator exists; only its integer literal operand mismatches:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Byte`, found an integer literal",
        literal_start,
        literal_start + "1".len(),
    );
}

#[test]
fn primitive_bitwise_bool_integer_literal_reports_operand_and_operator_diagnostics() {
    let source = concat!(
        "pub fn bool_operand(value: Bool) -> Bool {\n",
        "    let result = value ^ 1\n",
        "    let recovered: Bool = result\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression_start = source
        .find("value ^ 1")
        .expect("right integer literal expression exists");
    let literal_start = expression_start + "value ^ ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the integer literal should be checked against Bool:\n{stdout}"
    );
    assert_eq!(
        stdout
            .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            .count(),
        1,
        "bitwise XOR remains unsupported for Bool:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the two supported diagnostics should recover through the Bool local:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Bool`, found an integer literal",
        literal_start,
        literal_start + "1".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "OPERATOR_NOT_SUPPORTED",
        "operator `^` is not defined for `Bool`",
        expression_start,
        expression_start + "value ^ 1".len(),
    );
    let operator_position = stdout
        .find("\"message\":\"operator `^` is not defined for `Bool`\"")
        .expect("Bool operator diagnostic exists");
    let operand_position = stdout
        .find("\"message\":\"expected `Bool`, found an integer literal\"")
        .expect("Bool literal mismatch exists");
    assert!(
        operator_position < operand_position,
        "CLI output sorts the whole-expression span before the nested literal span:\n{stdout}"
    );
}

#[test]
fn primitive_bitwise_float_integer_literal_reports_operand_and_operator_diagnostics() {
    for (primitive, operator, name) in [("F32", "|", "or"), ("F64", "&", "and")] {
        let source = format!(
            "pub fn {name}_float_operand(value: {primitive}) -> {primitive} {{\n    let result = value {operator} 1\n    let recovered: {primitive} = result\n    recovered\n}}\n"
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("value {operator} 1");
        let expression_start = source
            .find(&expression)
            .expect("right integer literal expression exists");
        let literal_start = expression_start + format!("value {operator} ").len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "the integer literal should be checked against {primitive}:\n{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "bitwise {operator} remains unsupported for {primitive}:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the two supported diagnostics should recover through the {primitive} local:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &format!("expected `{primitive}`, found an integer literal"),
            literal_start,
            literal_start + "1".len(),
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &format!("operator `{operator}` is not defined for `{primitive}`"),
            expression_start,
            expression_start + expression.len(),
        );
        let operator_position = stdout
            .find(&format!(
                "\"message\":\"operator `{operator}` is not defined for `{primitive}`\""
            ))
            .expect("float operator diagnostic exists");
        let operand_position = stdout
            .find(&format!(
                "\"message\":\"expected `{primitive}`, found an integer literal\""
            ))
            .expect("float literal mismatch exists");
        assert!(
            operator_position < operand_position,
            "CLI output sorts the whole-expression span before the nested literal span:\n{stdout}"
        );
    }
}

#[test]
fn primitive_bitwise_right_byte_literals_accept_byte_paths_and_tail_expressions() {
    let source = concat!(
        "pub fn byte_literals(left: Byte) -> Byte {\n",
        "    let prior: Byte = left\n",
        "    let and_value = prior & b'a'\n",
        "    let or_value = and_value | b'b'\n",
        "    let xor_value = or_value ^ b'c'\n",
        "    xor_value\n",
        "}\n",
    );

    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_byte_literal_left_accepts_byte_parameter_and_local_paths() {
    let source = concat!(
        "pub fn byte_literal_lefts(right: Byte) -> Byte {\n",
        "    let and_parameter = b'a' & right\n",
        "    let and_local = b'b' & and_parameter\n",
        "    let or_parameter = b'c' | right\n",
        "    let or_local = b'd' | or_parameter\n",
        "    let xor_parameter = b'e' ^ right\n",
        "    let xor_local = b'f' ^ xor_parameter\n",
        "    let annotated: Byte = b'g' & xor_local\n",
        "    annotated\n",
        "}\n",
    );

    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_byte_literal_left_rhs_mismatches_recover_to_byte() {
    for (operator, right_type) in [
        ("&", "I16"),
        ("|", "Bool"),
        ("^", "Char"),
        ("&", "F32"),
        ("|", "F64"),
    ] {
        let source = format!(
            concat!(
                "pub fn byte_mismatch(right: {right_type}) -> Byte {{\n",
                "    let value = b'a' {operator} right\n",
                "    let recovered: Byte = value\n",
                "    recovered\n",
                "}}\n",
            ),
            operator = operator,
            right_type = right_type,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("b'a' {operator} right");
        let expression_start = source
            .find(&expression)
            .expect("Byte literal left expression exists");
        let right_start = expression_start + format!("b'a' {operator} ").len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "each non-Byte path should produce one mismatch:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the Byte operator mismatch should recover through the later local and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &format!("expected `Byte`, found `{right_type}`"),
            right_start,
            right_start + "right".len(),
        );
    }
}

#[test]
fn primitive_bitwise_byte_literal_left_annotation_mismatch_recovers_to_annotation() {
    let source = concat!(
        "pub fn annotation_recovery(right: Byte) -> U8 {\n",
        "    let value: U8 = b'a' ^ right\n",
        "    let recovered: U8 = value\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression = "b'a' ^ right";
    let expression_start = source
        .find(expression)
        .expect("annotated Byte literal-left expression exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the incompatible annotation should produce one mismatch:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to U8 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U8`, found `Byte`",
        expression_start,
        expression_start + expression.len(),
    );
}

#[test]
fn primitive_bitwise_byte_literal_pairs_accept_all_operators_and_byte_annotations() {
    let source = concat!(
        "pub fn byte_pairs() -> Byte {\n",
        "    let and_inferred = b'a' & b'b'\n",
        "    let and_annotated: Byte = b'c' & b'd'\n",
        "    let or_inferred = b'e' | b'f'\n",
        "    let or_annotated: Byte = b'g' | b'h'\n",
        "    let xor_inferred = b'i' ^ b'j'\n",
        "    let xor_annotated: Byte = b'k' ^ b'l'\n",
        "    xor_annotated\n",
        "}\n",
    );

    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_byte_literal_pair_annotation_mismatch_recovers_to_annotation() {
    let source = concat!(
        "pub fn annotation_recovery() -> U8 {\n",
        "    let value: U8 = b'a' ^ b'b'\n",
        "    let recovered: U8 = value\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression = "b'a' ^ b'b'";
    let expression_start = source
        .find(expression)
        .expect("annotated Byte literal pair exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the incompatible annotation should produce one mismatch:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to U8 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U8`, found `Byte`",
        expression_start,
        expression_start + expression.len(),
    );
}

#[test]
fn primitive_bitwise_byte_literal_left_integer_rhs_mismatches_and_recovers_to_byte() {
    for operator in ["&", "|", "^"] {
        let source = format!(
            concat!(
                "pub fn byte_integer_rhs() -> Byte {{\n",
                "    let value = b'a' {operator} -1\n",
                "    let recovered: Byte = value\n",
                "    recovered\n",
                "}}\n",
            ),
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("b'a' {operator} -1");
        let expression_start = source
            .find(&expression)
            .expect("Byte literal-left integer expression exists");
        let literal_start = expression_start + format!("b'a' {operator} ").len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "an integer RHS should produce one Byte literal mismatch:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"LITERAL_OUT_OF_RANGE\"")
                && !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the expression should recover as Byte through the following local and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            "expected `Byte`, found an integer literal",
            literal_start,
            literal_start + "-1".len(),
        );
    }
}

#[test]
fn primitive_bitwise_byte_literal_left_integer_rhs_recovers_to_different_annotation() {
    let source = concat!(
        "pub fn annotation_recovery() -> U8 {\n",
        "    let value: U8 = b'a' ^ 1\n",
        "    let recovered: U8 = value\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression = "b'a' ^ 1";
    let expression_start = source
        .find(expression)
        .expect("annotated Byte literal-left integer expression exists");
    let literal_start = expression_start + "b'a' ^ ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "the RHS mismatch and annotation mismatch should both be reported:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to U8 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U8`, found `Byte`",
        expression_start,
        expression_start + expression.len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `Byte`, found an integer literal",
        literal_start,
        literal_start + "1".len(),
    );
}

#[test]
fn primitive_bitwise_right_byte_literal_integer_mismatches_recover_to_the_left_type() {
    for operator in ["&", "|", "^"] {
        let source = format!(
            concat!(
                "pub fn integer_mismatch(left: I32) -> I32 {{\n",
                "    let value = left {operator} b'a'\n",
                "    let recovered: I32 = value\n",
                "    recovered\n",
                "}}\n",
            ),
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("left {operator} b'a'");
        let expression_start = source
            .find(&expression)
            .expect("right Byte literal expression exists");
        let literal_start = expression_start + format!("left {operator} ").len();

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "the Byte literal should produce one mismatch for I32:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the supported operator mismatch should recover through the local and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            "expected `I32`, found a byte literal",
            literal_start,
            literal_start + "b'a'".len(),
        );
    }
}

#[test]
fn primitive_bitwise_right_byte_literals_report_noninteger_diagnostics_in_span_order() {
    for (primitive, operator) in [
        ("Bool", "&"),
        ("Bool", "|"),
        ("Bool", "^"),
        ("F32", "&"),
        ("F32", "|"),
        ("F32", "^"),
        ("F64", "&"),
        ("F64", "|"),
        ("F64", "^"),
    ] {
        let source = format!(
            concat!(
                "pub fn noninteger_operand(left: {primitive}) -> {primitive} {{\n",
                "    let value = left {operator} b'a'\n",
                "    let recovered: {primitive} = value\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("left {operator} b'a'");
        let expression_start = source
            .find(&expression)
            .expect("right Byte literal expression exists");
        let literal_start = expression_start + format!("left {operator} ").len();
        let operator_message = format!("operator `{operator}` is not defined for `{primitive}`");
        let literal_message = format!("expected `{primitive}`, found a byte literal");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "the Byte literal should produce one mismatch for {primitive}:\n{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "bitwise {operator} should remain unsupported for {primitive}:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the diagnostics should recover through the {primitive} local and tail:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &literal_message,
            literal_start,
            literal_start + "b'a'".len(),
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &operator_message,
            expression_start,
            expression_start + expression.len(),
        );
        let operator_position = stdout
            .find(&format!("\"message\":\"{operator_message}\""))
            .expect("operator diagnostic exists");
        let literal_position = stdout
            .find(&format!("\"message\":\"{literal_message}\""))
            .expect("Byte literal mismatch exists");
        assert!(
            operator_position < literal_position,
            "CLI output should sort the whole-expression error before the RHS literal mismatch:\n{stdout}"
        );
    }
}

#[test]
fn primitive_bitwise_right_byte_literals_report_char_trait_failures_without_type_mismatches() {
    for (operator, trait_name) in [("&", "BitAnd"), ("|", "BitOr"), ("^", "BitXor")] {
        let source = format!(
            concat!(
                "pub fn char_operand(left: Char) -> Char {{\n",
                "    let value = left {operator} b'a'\n",
                "    let recovered: Char = value\n",
                "    recovered\n",
                "}}\n",
            ),
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression = format!("left {operator} b'a'");
        let expression_start = source
            .find(&expression)
            .expect("Char and Byte operator expression exists");
        let operator_message =
            format!("operator `{operator}` has no `{trait_name}<Byte>` implementation for `Char`");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "Char should report one concrete trait failure:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the Char trait failure should not synthesize a literal mismatch or block recovery:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &operator_message,
            expression_start,
            expression_start + expression.len(),
        );
    }
}

#[test]
fn primitive_bitwise_right_integer_literal_annotation_mismatch_recovers() {
    let source = concat!(
        "pub fn annotation_recovery(value: I32) -> U32 {\n",
        "    let result: U32 = value & 1\n",
        "    let recovered: U32 = result\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression_start = source
        .find("value & 1")
        .expect("annotated bitwise expression exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the initializer annotation mismatch should be reported once:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the initializer mismatch should recover to U32 for the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U32`, found `I32`",
        expression_start,
        expression_start + "value & 1".len(),
    );
}

#[test]
fn primitive_bitwise_left_integer_literals_accept_all_integer_types_and_operators() {
    let integer_types = [
        ("I8", "i8_value"),
        ("I16", "i16_value"),
        ("I32", "i32_value"),
        ("I64", "i64_value"),
        ("U8", "u8_value"),
        ("U16", "u16_value"),
        ("U32", "u32_value"),
        ("U64", "u64_value"),
        ("ISize", "isize_value"),
        ("USize", "usize_value"),
    ];
    let mut source = String::from("pub fn bitwise_left_literals(");
    for (index, (primitive, parameter)) in integer_types.iter().enumerate() {
        if index > 0 {
            source.push_str(", ");
        }
        source.push_str(&format!("{parameter}: {primitive}"));
    }
    source.push_str(") -> I8 {\n");

    for (_, parameter) in integer_types {
        for (operator, name) in [("&", "and"), ("|", "or"), ("^", "xor")] {
            source.push_str(&format!(
                "    let {name}_{parameter} = 1 {operator} {parameter}\n"
            ));
        }
    }
    source.push_str("    and_i8_value\n}\n");

    assert_success_result(run_check(&source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_left_integer_literals_resolve_preceding_and_shadowed_paths() {
    for source in [
        concat!(
            "pub fn preceding(value: I32) -> I32 {\n",
            "    let earlier: I32 = value\n",
            "    let result = 7 ^ earlier\n",
            "    result\n",
            "}\n",
        ),
        concat!(
            "pub fn shadow(value: U32, right: I32) -> I32 {\n",
            "    let value: I32 = right\n",
            "    let result = 11 | value\n",
            "    result\n",
            "}\n",
        ),
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_bitwise_left_integer_literals_check_boundaries_and_signed_spans() {
    let source = concat!(
        "pub fn left_literal_boundaries(signed: I8, unsigned: U8) -> I8 {\n",
        "    let signed_min = -128 & signed\n",
        "    let signed_max = 127 | signed\n",
        "    let unsigned_max = 255 ^ unsigned\n",
        "    let unsigned_negative_zero = -0 & unsigned\n",
        "    let signed_low = -129 & signed\n",
        "    let signed_high = 128 | signed\n",
        "    let unsigned_high = 256 ^ unsigned\n",
        "    let unsigned_negative = -1 & unsigned\n",
        "    signed_min\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_spans = [
        (
            source
                .find("-129 & signed")
                .expect("negative signed boundary exists"),
            "-129",
            "integer literal is out of range for `I8`",
        ),
        (
            source
                .find("128 | signed")
                .expect("positive signed boundary exists"),
            "128",
            "integer literal is out of range for `I8`",
        ),
        (
            source
                .find("256 ^ unsigned")
                .expect("positive unsigned boundary exists"),
            "256",
            "integer literal is out of range for `U8`",
        ),
        (
            source
                .find("-1 & unsigned")
                .expect("negative unsigned boundary exists"),
            "-1",
            "integer literal is out of range for `U8`",
        ),
    ];

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        literal_spans.len(),
        "left literals should be range-checked against the right path:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "range errors should recover through the later local and tail:\n{stdout}"
    );
    for (start, literal, message) in literal_spans {
        assert_json_diagnostic_primary_span(
            &stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }
}

#[test]
fn primitive_bitwise_left_integer_literals_use_wasm_pointer_width() {
    let manifest = "targets { wasm32-core, wasm64-core }\n";
    let source = concat!(
        "pub fn left_pointer_boundaries(signed: ISize, unsigned: USize) -> ISize {\n",
        "    let signed_min_32 = -2147483648 & signed\n",
        "    let signed_max_32 = 2147483647 | signed\n",
        "    let unsigned_max_32 = 4294967295 ^ unsigned\n",
        "    let signed_min_64 = -9223372036854775808 & signed\n",
        "    let signed_max_64 = 9223372036854775807 | signed\n",
        "    let unsigned_max_64 = 18446744073709551615 ^ unsigned\n",
        "    signed_min_32\n",
        "}\n",
    );

    let wasm32 = run_check(&source, manifest, &[], &["--target", "wasm32-core"]);
    let wasm32_stdout = String::from_utf8(wasm32.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(wasm32.status.code(), Some(1), "{wasm32_stdout}");
    assert_eq!(
        wasm32_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        3,
        "wasm32 must reject the 64-bit ISize and USize bounds:\n{wasm32_stdout}"
    );
    assert!(
        !wasm32_stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !wasm32_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !wasm32_stdout.contains("\"type\":\"result\""),
        "pointer-width range failures should recover to the right path type:\n{wasm32_stdout}"
    );
    for (literal, message) in [
        (
            "-9223372036854775808",
            "integer literal is out of range for `ISize`",
        ),
        (
            "9223372036854775807",
            "integer literal is out of range for `ISize`",
        ),
        (
            "18446744073709551615",
            "integer literal is out of range for `USize`",
        ),
    ] {
        let start = source.find(literal).expect("pointer-sized literal exists");
        assert_json_diagnostic_primary_span(
            &wasm32_stdout,
            "LITERAL_OUT_OF_RANGE",
            message,
            start,
            start + literal.len(),
        );
    }

    let wasm64 = run_check(&source, manifest, &[], &["--target", "wasm64-core"]);
    assert_success_result(wasm64, "wasm64-core", "dev");
}

#[test]
fn primitive_bitwise_left_integer_literal_mismatches_recover_without_operator_errors() {
    for (right_type, operator) in [
        ("Byte", "&"),
        ("Bool", "|"),
        ("F32", "^"),
        ("F64", "&"),
        ("Char", "|"),
    ] {
        let source = format!(
            concat!(
                "pub fn mismatch(right: {right_type}) -> I8 {{\n",
                "    let result: I8 = 7 {operator} right\n",
                "    let recovered: I8 = result\n",
                "    recovered\n",
                "}}\n",
            ),
            right_type = right_type,
            operator = operator,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let right_start = source
            .find(&format!("{operator} right"))
            .map(|start| start + operator.len() + 1)
            .expect("mismatched right path exists");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "a non-integer right path should produce one mismatch:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the integer literal mismatch should preserve recovery through the annotation:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &format!("expected `I64`, found `{right_type}`"),
            right_start,
            right_start + "right".len(),
        );
    }
}

#[test]
fn primitive_bitwise_left_integer_literal_annotation_constrains_after_rhs_mismatch() {
    let source = concat!(
        "pub fn annotation_recovery(right: F32) -> I8 {\n",
        "    let result: I8 = 128 & right\n",
        "    let recovered: I8 = result\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source.find("128 & right").expect("left literal exists");
    let right_start = literal_start + "128 & ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the incompatible right path should be diagnosed:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "the I8 annotation should constrain and range-check the left literal:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to its I8 annotation and continue:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        right_start,
        right_start + "right".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I8`",
        literal_start,
        literal_start + "128".len(),
    );
}

#[test]
fn primitive_bitwise_left_integer_literal_tail_constrains_after_rhs_mismatch() {
    let source = concat!(
        "pub fn tail_recovery(right: F32) -> I8 {\n",
        "    let result = 7 ^ right\n",
        "    result\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let right_start = source
        .find("^ right")
        .map(|start| start + "^ ".len())
        .expect("mismatched right path exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the right path mismatch should be reported once:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the I8 tail should constrain the retained integer literal:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        right_start,
        right_start + "right".len(),
    );
}

#[test]
fn primitive_bitwise_left_integer_literal_annotation_mismatch_recovers() {
    let source = concat!(
        "pub fn annotation_mismatch(right: F32) -> F32 {\n",
        "    let result: F32 = 7 & right\n",
        "    let recovered: F32 = result\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression_start = source
        .find("7 & right")
        .expect("annotated bitwise expression exists");
    let right_start = expression_start + "7 & ".len();

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        2,
        "the annotation and right-path mismatches should both be diagnosed:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to F32 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `F32`, found an integer literal",
        expression_start,
        expression_start + "7 & right".len(),
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I64`, found `F32`",
        right_start,
        right_start + "right".len(),
    );
    let annotation_position = stdout
        .find("\"message\":\"expected `F32`, found an integer literal\"")
        .expect("annotation mismatch exists");
    let operand_position = stdout
        .find("\"message\":\"expected `I64`, found `F32`\"")
        .expect("right-path mismatch exists");
    assert!(
        annotation_position < operand_position,
        "the full initializer mismatch should precede its nested right-path mismatch:\n{stdout}"
    );
}

#[test]
fn unsupported_primitive_bitwise_right_integer_literal_forms_remain_gated() {
    for source in [
        "pub fn literal_pair() -> I32 { let result = 1 ^ 2; result }\n",
        "pub fn unresolved(value: I32) -> I32 { let bits = 1; let result = value & bits; result }\n",
        "type Word = I32\npub fn alias(left: Word, right: Word) -> Word { let result = left & right; result }\n",
        "pub fn nested(left: I32, right: I32, extra: I32) -> I32 { let result = left & (right ^ extra); result }\n",
        "pub fn computed(left: I32, right: I32) -> I32 { let result = (left ^ right) & right; result }\n",
        "pub fn float_left_literal(value: I32) -> I32 { let result = 1.0 & value; result }\n",
        "pub fn float_literal(value: I32) -> I32 { let result = value & 1.0; result }\n",
        "pub fn bool_literal(value: I32) -> I32 { let result = value | true; result }\n",
        "pub fn char_literal(value: I32) -> I32 { let result = value ^ 'a'; result }\n",
        "pub fn self_reference(value: I32) -> I32 { let result = result & value; result }\n",
        "pub fn forward_reference(value: I32) -> I32 { let result = later & value; let later: I32 = 1; result }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn unsupported_primitive_bitwise_right_byte_literal_shapes_remain_gated() {
    for source in [
        "pub fn byte_string(value: Byte) -> Byte { let result = value & b\"text\"; result }\n",
        "pub fn computed_left(value: Byte) -> Byte { let result = (value & value) | b'a'; result }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn primitive_bitwise_not_accepts_all_integer_and_byte_paths() {
    let supported_types = [
        ("I8", "i8_value"),
        ("I16", "i16_value"),
        ("I32", "i32_value"),
        ("I64", "i64_value"),
        ("U8", "u8_value"),
        ("U16", "u16_value"),
        ("U32", "u32_value"),
        ("U64", "u64_value"),
        ("ISize", "isize_value"),
        ("USize", "usize_value"),
        ("Byte", "byte_value"),
    ];
    let mut source = String::from("pub fn bitwise_not(");
    for (index, (primitive, parameter)) in supported_types.iter().enumerate() {
        if index > 0 {
            source.push_str(", ");
        }
        source.push_str(&format!("{parameter}: {primitive}"));
    }
    source.push_str(") -> I8 {\n");
    for (_, parameter) in supported_types {
        source.push_str(&format!("    let inverted_{parameter} = ~{parameter}\n"));
    }
    source.push_str("    inverted_i8_value\n}\n");

    assert_success_result(run_check(&source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_not_resolves_preceding_and_shadowed_paths() {
    for source in [
        concat!(
            "pub fn preceding(value: I16) -> I16 {\n",
            "    let earlier: I16 = value\n",
            "    let inverted = ~earlier\n",
            "    inverted\n",
            "}\n",
        ),
        concat!(
            "pub fn shadow(value: U32, right: I32) -> I32 {\n",
            "    let value: I32 = right\n",
            "    let inverted: I32 = ~value\n",
            "    inverted\n",
            "}\n",
        ),
    ] {
        assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
    }
}

#[test]
fn primitive_bitwise_not_reports_unsupported_types_and_recovers_the_local() {
    for primitive in ["Bool", "Char", "F32", "F64"] {
        let source = format!(
            concat!(
                "pub fn unsupported(value: {primitive}) -> {primitive} {{\n",
                "    let inverted: {primitive} = ~value\n",
                "    let recovered: {primitive} = inverted\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let expression_start = source
            .find("~value")
            .expect("unsupported unary expression exists");

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout
                .matches("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                .count(),
            1,
            "unsupported `~` should produce one operator diagnostic:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the unsupported unary expression should recover to its operand type:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "OPERATOR_NOT_SUPPORTED",
            &format!("operator `~` has no `BitNot` implementation for `{primitive}`"),
            expression_start,
            expression_start + "~value".len(),
        );
    }
}

#[test]
fn primitive_bitwise_not_annotation_mismatch_recovers_to_the_annotation() {
    let source = concat!(
        "pub fn annotation_recovery(value: I8) -> U8 {\n",
        "    let inverted: U8 = ~value\n",
        "    let recovered: U8 = inverted\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression_start = source
        .find("~value")
        .expect("annotated unary expression exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the incompatible local annotation should be reported once:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to U8 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `U8`, found `I8`",
        expression_start,
        expression_start + "~value".len(),
    );
}

#[test]
fn unsupported_primitive_bitwise_not_noninteger_literal_and_computed_operands_remain_gated() {
    for source in [
        "pub fn float_literal() -> I32 { let result = ~1.0; result }\n",
        "pub fn bool_literal() -> Bool { let result = ~true; result }\n",
        "pub fn char_literal() -> Char { let result = ~'a'; result }\n",
        "pub fn byte_string() -> Byte { let result = ~b\"text\"; result }\n",
        "pub fn nested(left: I32, right: I32) -> I32 { let result = ~(left ^ right); result }\n",
        "pub fn computed(left: I32, right: I32) -> I32 { let result = ~(left + right); result }\n",
        "pub fn unresolved() -> I64 { let bits = 1; let result = ~bits; result }\n",
        "type Word = I32\npub fn alias(value: Word) -> Word { let result = ~value; result }\n",
        "pub fn self_reference(value: I32) -> I32 { let result = ~result; result }\n",
        "pub fn forward_reference(value: I32) -> I32 { let result = ~later; let later: I32 = value; result }\n",
    ] {
        assert_check_stays_gated(&run_check(source, "", &[], &[]), source);
    }
}

#[test]
fn primitive_bitwise_not_accepts_direct_byte_literals_and_preserves_byte_type() {
    let source = concat!(
        "pub fn byte_literals() -> Byte {\n",
        "    let inferred = ~b'a'\n",
        "    let inferred_recovered: Byte = inferred\n",
        "    let annotated: Byte = ~b'z'\n",
        "    let annotated_recovered: Byte = annotated\n",
        "    annotated_recovered\n",
        "}\n",
    );

    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_not_byte_literal_annotation_mismatch_recovers_to_annotation() {
    let source = concat!(
        "pub fn annotation_recovery() -> I8 {\n",
        "    let invalid: I8 = ~b'a'\n",
        "    let recovered: I8 = invalid\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expression_start = source
        .find("~b'a'")
        .expect("annotated byte literal expression exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
        1,
        "the incompatible annotation should be reported once:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the local should recover to I8 and reach the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "TYPE_MISMATCH",
        "expected `I8`, found `Byte`",
        expression_start,
        expression_start + "~b'a'".len(),
    );
}

#[test]
fn primitive_bitwise_not_accepts_direct_integer_literals_with_annotation_and_tail_inference() {
    let source = concat!(
        "pub fn literal_inference() -> I16 {\n",
        "    let annotated: I8 = ~-128\n",
        "    let inferred = ~42\n",
        "    inferred\n",
        "}\n",
    );

    assert_success_result(run_check(source, "", &[], &[]), "wasm32-core", "dev");
}

#[test]
fn primitive_bitwise_not_defaults_unannotated_integer_literals_to_i64() {
    let source = concat!(
        "pub fn defaulting() -> I64 {\n",
        "    let defaulted = ~9223372036854775808\n",
        "    let tail: I64 = 0\n",
        "    tail\n",
        "}\n",
    );

    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_start = source
        .find("9223372036854775808")
        .expect("I64 out-of-range literal exists");

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        1,
        "an unconstrained unary integer literal should default to I64 and be range-checked:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "I64 defaulting should recover through the following local and tail:\n{stdout}"
    );
    assert_json_diagnostic_primary_span(
        &stdout,
        "LITERAL_OUT_OF_RANGE",
        "integer literal is out of range for `I64`",
        literal_start,
        literal_start + "9223372036854775808".len(),
    );
}

#[test]
fn primitive_bitwise_not_rejects_integer_literals_for_noninteger_annotations_and_recovers() {
    for primitive in ["F64", "Byte"] {
        let source = format!(
            concat!(
                "pub fn annotation_mismatch() -> {primitive} {{\n",
                "    let invalid: {primitive} = ~1\n",
                "    let recovered: {primitive} = invalid\n",
                "    recovered\n",
                "}}\n",
            ),
            primitive = primitive,
        );
        let output = run_check(&source, "", &[], &[]);
        let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
        let literal_start = source.find("~1").expect("integer literal operand exists") + 1;

        assert_eq!(output.status.code(), Some(1), "{stdout}");
        assert_eq!(
            stdout.matches("\"code\":\"TYPE_MISMATCH\"").count(),
            1,
            "integer literal family mismatch should be reported once:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"OPERATOR_NOT_SUPPORTED\"")
                && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
                && !stdout.contains("\"type\":\"result\""),
            "the annotation mismatch should recover to `{primitive}`:\n{stdout}"
        );
        assert_json_diagnostic_primary_span(
            &stdout,
            "TYPE_MISMATCH",
            &format!("expected `{primitive}`, found an integer literal"),
            literal_start,
            literal_start + 1,
        );
    }
}

#[test]
fn primitive_bitwise_not_integer_literal_range_errors_use_full_span_and_recover() {
    let source = concat!(
        "pub fn literal_ranges() -> I8 {\n",
        "    let positive_out_of_range: I8 = ~128\n",
        "    let negative_out_of_range: I8 = ~-129\n",
        "    let positive_pointer_out_of_range: USize = ~4294967296\n",
        "    let negative_pointer_out_of_range: ISize = ~-2147483649\n",
        "    let recovered: I8 = ~127\n",
        "    recovered\n",
        "}\n",
    );
    let output = run_check(source, "", &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let literal_spans = [
        (
            source
                .find("128")
                .expect("positive out-of-range literal exists"),
            "128",
            "I8",
        ),
        (
            source
                .find("-129")
                .expect("negative out-of-range literal exists"),
            "-129",
            "I8",
        ),
        (
            source
                .find("4294967296")
                .expect("positive pointer-width out-of-range literal exists"),
            "4294967296",
            "USize",
        ),
        (
            source
                .find("-2147483649")
                .expect("negative pointer-width out-of-range literal exists"),
            "-2147483649",
            "ISize",
        ),
    ];

    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert_eq!(
        stdout.matches("\"code\":\"LITERAL_OUT_OF_RANGE\"").count(),
        literal_spans.len(),
        "all annotated integer literals should be checked against their exact target type:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"TYPE_MISMATCH\"")
            && !stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "range errors should recover to I8 and reach the following local and tail:\n{stdout}"
    );
    for (start, literal, primitive) in literal_spans {
        assert_json_diagnostic_primary_span(
            &stdout,
            "LITERAL_OUT_OF_RANGE",
            &format!("integer literal is out of range for `{primitive}`"),
            start,
            start + literal.len(),
        );
    }
}

#[test]
fn primitive_const_success_stays_limited_to_the_default_unlocked_check() {
    let source = "pub const VALUE: I8 = 1\n";
    let cases: &[(&str, &[&str])] = &[
        ("non-core target", &["--target", "wasm32-wasi"]),
        ("non-default profile", &["--profile", "tiny"]),
        ("test analysis", &["--tests"]),
        ("fix mode", &["--fix"]),
        ("locked mode", &["--locked", "--offline"]),
        ("frozen mode", &["--frozen"]),
        ("allow lint override", &["--allow", "MISSING_DOCS"]),
        ("custom lint levels", &["--warn", "MISSING_DOCS"]),
        ("deny lint override", &["--deny", "MISSING_DOCS"]),
        ("equals-form allow override", &["--allow=MISSING_DOCS"]),
        ("equals-form lint override", &["--warn=MISSING_DOCS"]),
        ("equals-form deny override", &["--deny=MISSING_DOCS"]),
        ("deny warnings", &["--deny-warnings"]),
    ];

    for (context, options) in cases {
        let output = run_check(source, "", &[], options);
        assert_check_stays_gated(&output, context);
    }
}
