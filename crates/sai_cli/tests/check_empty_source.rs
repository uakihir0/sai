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
        "empty root module should be supported:\n{stdout}"
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
fn empty_root_source_emits_check_result_for_default_and_selected_target() {
    assert_success_result(run_check("", "", &[], &[]), "wasm32-core", "dev");
    assert_success_result(
        run_check("", "", &[], &["--target", "wasm32-core"]),
        "wasm32-core",
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
        "use missing;\npub const VALUE: I8 = 1\n",
        "unsafe module\npub const VALUE: I8 = 1\n",
        "property \"coverage\" () {}\npub const VALUE: I8 = 1\n",
        "@doc(hidden)\npub const ATTRIBUTED: I8 = 1\n",
        "pub const Max_size: I8 = 1\n",
        "pub const r#READY: Bool = true\n",
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
