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
