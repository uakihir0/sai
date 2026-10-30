use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

const TYPE_MISMATCH_MARKDOWN: &str =
    include_str!("../../sai_diagnostics/src/explanations/type_mismatch.md");
const TYPE_MISMATCH_SUMMARY: &str = "Two types that must be equal, or a coercion that does not apply; names the expected and found types and the origin of the expectation";
const DUPLICATE_BINDING_MARKDOWN: &str =
    include_str!("../../sai_diagnostics/src/explanations/duplicate_binding.md");
const DUPLICATE_BINDING_SUMMARY: &str = "Two bindings of one name in one scope: module scope, block, parameter list, generic parameter list, pattern, interface (§4.4.3, D-MOD-03, N-A-8)";
const INVALID_ESCAPE_MARKDOWN: &str =
    include_str!("../../sai_diagnostics/src/explanations/invalid_escape.md");
const INVALID_ESCAPE_SUMMARY: &str = "Invalid escape sequence";
const LITERAL_OUT_OF_RANGE_MARKDOWN: &str =
    include_str!("../../sai_diagnostics/src/explanations/literal_out_of_range.md");
const LITERAL_OUT_OF_RANGE_SUMMARY: &str = "Literal out of range for its type";
const MALFORMED_NUMBER_MARKDOWN: &str =
    include_str!("../../sai_diagnostics/src/explanations/malformed_number.md");
const MALFORMED_NUMBER_SUMMARY: &str = "Malformed numeric literal";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("sai-cli-explain-{}-{unique}", std::process::id()));
        fs::create_dir(&path).expect("create temporary test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_explain(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sai"))
        .args(arguments)
        .output()
        .expect("run sai explain subprocess")
}

fn run_package_command(source: &str, arguments: &[&str]) -> Output {
    let temporary = TestDirectory::new();
    let root = &temporary.0;
    fs::create_dir_all(root.join("src")).expect("create source directory");
    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:explain @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-core, wasm32-wasi }\n"
        ),
    )
    .expect("write package manifest");
    fs::write(root.join("src/lib.sai"), source).expect("write root source");

    Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run SAI package command subprocess")
}

fn run_check(source: &str) -> Output {
    run_package_command(source, &["--format=json", "check"])
}

fn run_build(source: &str) -> Output {
    run_package_command(
        source,
        &["--format=json", "build", "--target", "wasm32-core"],
    )
}

fn source_block(markdown: &str, section: &str) -> String {
    let (source, info) = source_block_with_info(markdown, section);
    assert_eq!(info, "sai", "{section} must use a canonical SAI fence");
    source
}

fn source_block_with_info(markdown: &str, section: &str) -> (String, String) {
    let markdown = markdown.replace("\r\n", "\n");
    let heading = format!("## {section}\n");
    let section = markdown
        .split_once(&heading)
        .unwrap_or_else(|| panic!("missing {heading:?} section"))
        .1;
    let section = section.strip_prefix('\n').unwrap_or(section);
    let (fence_info, fence) = section
        .split_once('\n')
        .expect("section starts with a fenced code block");
    let info = fence_info
        .strip_prefix("```")
        .expect("section starts with a fenced code block");
    assert!(
        matches!(info, "sai" | "sai noncanonical"),
        "unsupported SAI fence info string {info:?}"
    );
    let source = fence
        .split_once("\n```")
        .expect("SAI code fence is closed")
        .0;
    (format!("{source}\n"), info.to_owned())
}

#[test]
fn source_block_normalizes_windows_line_endings() {
    let markdown = "## Example\r\n\r\n```sai\r\npub fn main() {\r\n    42\r\n}\r\n```\r\n";

    assert_eq!(
        source_block(markdown, "Example"),
        "pub fn main() {\n    42\n}\n"
    );
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character <= '\u{1f}' => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing to a String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn run_fmt_check(source: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sai"))
        .args([
            "--format=json",
            "fmt",
            "--check",
            "--stdin-kind",
            "source",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start sai fmt subprocess");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(source.as_bytes())
        .expect("write SAI source to formatter");
    child
        .wait_with_output()
        .expect("wait for sai fmt subprocess")
}

fn run_fmt(source: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sai"))
        .args(["--format=json", "fmt", "--stdin-kind", "source", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start sai fmt subprocess");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(source.as_bytes())
        .expect("write SAI source to formatter");
    child
        .wait_with_output()
        .expect("wait for sai fmt subprocess")
}

#[test]
fn explain_type_mismatch_prints_the_exact_markdown_record() {
    let output = run_explain(&["explain", "TYPE_MISMATCH"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, TYPE_MISMATCH_MARKDOWN);
}

#[test]
fn explain_type_mismatch_serializes_the_exact_json_record() {
    let output = run_explain(&["--format=json", "explain", "TYPE_MISMATCH"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expected_result = format!(
        concat!(
            "{{\"type\":\"result\",\"kind\":\"explain\",\"data\":{{",
            "\"code\":\"TYPE_MISMATCH\",\"class\":\"hard\",\"severity\":\"error\",",
            "\"default_level\":null,\"stage\":1,\"owner\":\"spec/05-types.md\",",
            "\"since\":\"0.1.0\",\"status\":\"active\",\"superseded_by\":null,",
            "\"summary\":{},\"markdown\":{}",
            "}}}}"
        ),
        json_string(TYPE_MISMATCH_SUMMARY),
        json_string(TYPE_MISMATCH_MARKDOWN),
    );
    let expected = format!(
        concat!(
            "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,",
            "\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},",
            "\"command\":\"explain\",\"root\":null}}\n",
            "{}\n",
            "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,",
            "\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,",
            "\"dependency\":0}},\"exit_code\":0}}\n"
        ),
        env!("CARGO_PKG_VERSION"),
        expected_result,
    );

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, expected);
}

#[test]
fn explain_duplicate_binding_prints_the_exact_markdown_record() {
    let output = run_explain(&["explain", "DUPLICATE_BINDING"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, DUPLICATE_BINDING_MARKDOWN);
}

#[test]
fn explain_duplicate_binding_serializes_the_exact_json_record() {
    let output = run_explain(&["--format=json", "explain", "DUPLICATE_BINDING"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expected_result = format!(
        concat!(
            "{{\"type\":\"result\",\"kind\":\"explain\",\"data\":{{",
            "\"code\":\"DUPLICATE_BINDING\",\"class\":\"hard\",\"severity\":\"error\",",
            "\"default_level\":null,\"stage\":0,\"owner\":\"spec/04-modules-and-names.md\",",
            "\"since\":\"0.1.0\",\"status\":\"active\",\"superseded_by\":null,",
            "\"summary\":{},\"markdown\":{}",
            "}}}}"
        ),
        json_string(DUPLICATE_BINDING_SUMMARY),
        json_string(DUPLICATE_BINDING_MARKDOWN),
    );
    let expected = format!(
        concat!(
            "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,",
            "\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},",
            "\"command\":\"explain\",\"root\":null}}\n",
            "{}\n",
            "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,",
            "\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,",
            "\"dependency\":0}},\"exit_code\":0}}\n"
        ),
        env!("CARGO_PKG_VERSION"),
        expected_result,
    );

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, expected);
}

#[test]
fn explain_invalid_escape_prints_the_exact_markdown_record() {
    let output = run_explain(&["explain", "INVALID_ESCAPE"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, INVALID_ESCAPE_MARKDOWN);
}

#[test]
fn explain_invalid_escape_serializes_the_exact_json_record() {
    let output = run_explain(&["--format=json", "explain", "INVALID_ESCAPE"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expected_result = format!(
        concat!(
            "{{\"type\":\"result\",\"kind\":\"explain\",\"data\":{{",
            "\"code\":\"INVALID_ESCAPE\",\"class\":\"hard\",\"severity\":\"error\",",
            "\"default_level\":null,\"stage\":0,\"owner\":\"spec/02-lexical-structure.md\",",
            "\"since\":\"0.1.0\",\"status\":\"active\",\"superseded_by\":null,",
            "\"summary\":{},\"markdown\":{}",
            "}}}}"
        ),
        json_string(INVALID_ESCAPE_SUMMARY),
        json_string(INVALID_ESCAPE_MARKDOWN),
    );
    let expected = format!(
        concat!(
            "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,",
            "\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},",
            "\"command\":\"explain\",\"root\":null}}\n",
            "{}\n",
            "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,",
            "\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,",
            "\"dependency\":0}},\"exit_code\":0}}\n"
        ),
        env!("CARGO_PKG_VERSION"),
        expected_result,
    );

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, expected);
}

#[test]
fn explain_literal_out_of_range_prints_the_exact_markdown_record() {
    let output = run_explain(&["explain", "LITERAL_OUT_OF_RANGE"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, LITERAL_OUT_OF_RANGE_MARKDOWN);
}

#[test]
fn explain_literal_out_of_range_serializes_the_exact_json_record() {
    let output = run_explain(&["--format=json", "explain", "LITERAL_OUT_OF_RANGE"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expected_result = format!(
        concat!(
            "{{\"type\":\"result\",\"kind\":\"explain\",\"data\":{{",
            "\"code\":\"LITERAL_OUT_OF_RANGE\",\"class\":\"hard\",\"severity\":\"error\",",
            "\"default_level\":null,\"stage\":0,\"owner\":\"spec/02-lexical-structure.md\",",
            "\"since\":\"0.1.0\",\"status\":\"active\",\"superseded_by\":null,",
            "\"summary\":{},\"markdown\":{}",
            "}}}}"
        ),
        json_string(LITERAL_OUT_OF_RANGE_SUMMARY),
        json_string(LITERAL_OUT_OF_RANGE_MARKDOWN),
    );
    let expected = format!(
        concat!(
            "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,",
            "\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},",
            "\"command\":\"explain\",\"root\":null}}\n",
            "{}\n",
            "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,",
            "\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,",
            "\"dependency\":0}},\"exit_code\":0}}\n"
        ),
        env!("CARGO_PKG_VERSION"),
        expected_result,
    );

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, expected);
}

#[test]
fn explanation_example_is_rejected_and_fix_is_semantically_checked() {
    let example = source_block(TYPE_MISMATCH_MARKDOWN, "Example");
    let fix = source_block(TYPE_MISMATCH_MARKDOWN, "Fix");

    let rejected = run_check(&example);
    let rejected_stdout = String::from_utf8(rejected.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(rejected.status.code(), Some(1), "{rejected_stdout}");
    assert!(rejected.stderr.is_empty());
    assert_eq!(
        rejected_stdout
            .matches("\"code\":\"TYPE_MISMATCH\"")
            .count(),
        1,
        "{rejected_stdout}"
    );
    assert!(!rejected_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
    assert!(!rejected_stdout.contains("\"type\":\"result\""));

    let accepted = run_check(&fix);
    let accepted_stdout = String::from_utf8(accepted.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(accepted.status.code(), Some(0), "{accepted_stdout}");
    assert!(accepted.stderr.is_empty());
    assert!(
        accepted_stdout.contains("\"kind\":\"check\""),
        "{accepted_stdout}"
    );
    assert!(!accepted_stdout.contains("\"type\":\"diagnostic\""));

    // Stage 1 code generation is not implemented yet. Keep this explicit so
    // the check result is not mistaken for a successful target build.
    let build = run_build(&fix);
    let build_stdout = String::from_utf8(build.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(build.status.code(), Some(1), "{build_stdout}");
    assert!(build.stderr.is_empty());
    assert!(
        build_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "{build_stdout}"
    );
    assert!(build_stdout.contains("\"stage\":1"), "{build_stdout}");
    assert!(
        build_stdout.contains("\"feature\":\"sai build\""),
        "{build_stdout}"
    );

    for (label, source) in [("Example", example), ("Fix", fix)] {
        let formatted = run_fmt_check(&source);
        let stdout = String::from_utf8(formatted.stdout).expect("formatter stdout is UTF-8");
        assert_eq!(
            formatted.status.code(),
            Some(0),
            "{label} source is not formatter-canonical:\n{stdout}"
        );
        assert!(
            !stdout.contains("\"code\":\"UNFORMATTED_FILE\""),
            "{label} source is not formatter-canonical:\n{stdout}"
        );
    }
}

#[test]
fn duplicate_binding_explanation_example_is_rejected_and_fix_is_clean() {
    let example = source_block(DUPLICATE_BINDING_MARKDOWN, "Example");
    let fix = source_block(DUPLICATE_BINDING_MARKDOWN, "Fix");

    let rejected = run_check(&example);
    let rejected_stdout = String::from_utf8(rejected.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(rejected.status.code(), Some(1), "{rejected_stdout}");
    assert!(rejected.stderr.is_empty());
    assert_eq!(
        rejected_stdout.matches("\"type\":\"diagnostic\"").count(),
        1,
        "{rejected_stdout}"
    );
    assert_eq!(
        rejected_stdout
            .matches("\"code\":\"DUPLICATE_BINDING\"")
            .count(),
        1,
        "{rejected_stdout}"
    );
    assert!(!rejected_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));

    let accepted = run_check(&fix);
    let accepted_stdout = String::from_utf8(accepted.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(accepted.status.code(), Some(0), "{accepted_stdout}");
    assert!(accepted.stderr.is_empty());
    assert!(
        accepted_stdout.contains("\"kind\":\"check\""),
        "{accepted_stdout}"
    );
    assert!(!accepted_stdout.contains("\"type\":\"diagnostic\""));

    for (label, source) in [("Example", example), ("Fix", fix)] {
        let formatted = run_fmt_check(&source);
        let stdout = String::from_utf8(formatted.stdout).expect("formatter stdout is UTF-8");
        assert_eq!(
            formatted.status.code(),
            Some(0),
            "{label} source is not formatter-canonical:\n{stdout}"
        );
        assert!(formatted.stderr.is_empty());
        assert!(!stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
        assert!(
            stdout.contains("\"refused\":[]"),
            "{label} source was unexpectedly refused:\n{stdout}"
        );
    }
}

#[test]
fn invalid_escape_explanation_example_refuses_and_fix_is_clean_and_canonical() {
    let (example, info) = source_block_with_info(INVALID_ESCAPE_MARKDOWN, "Example");
    assert_eq!(info, "sai noncanonical");
    let fix = source_block(INVALID_ESCAPE_MARKDOWN, "Fix");

    let rejected = run_check(&example);
    let rejected_stdout = String::from_utf8(rejected.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(rejected.status.code(), Some(1), "{rejected_stdout}");
    assert!(rejected.stderr.is_empty());
    assert_eq!(
        rejected_stdout
            .matches("\"code\":\"INVALID_ESCAPE\"")
            .count(),
        1,
        "{rejected_stdout}"
    );
    let check_diagnostic = rejected_stdout
        .lines()
        .find(|line| line.contains("\"code\":\"INVALID_ESCAPE\""))
        .expect("check reports the expected code");
    assert!(check_diagnostic.contains("\"severity\":\"error\""));
    assert!(check_diagnostic.contains("\"path\":\"src/lib.sai\""));
    assert!(check_diagnostic.contains("\"line\":1"));
    for diagnostic in rejected_stdout
        .lines()
        .filter(|line| line.contains("\"type\":\"diagnostic\""))
    {
        assert!(
            diagnostic.contains("\"code\":\"INVALID_ESCAPE\"")
                || diagnostic.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "unexpected check diagnostic:\n{diagnostic}"
        );
    }

    let refused = run_fmt(&example);
    let refused_stdout = String::from_utf8(refused.stdout).expect("formatter stdout is UTF-8");
    assert_eq!(refused.status.code(), Some(1), "{refused_stdout}");
    assert!(refused.stderr.is_empty());
    assert_eq!(refused_stdout.matches("\"type\":\"result\"").count(), 1);
    assert!(
        refused_stdout.contains("\"check\":false"),
        "{refused_stdout}"
    );
    assert!(refused_stdout.contains("\"files\":1"), "{refused_stdout}");
    assert!(
        refused_stdout.contains("\"changed\":[]"),
        "{refused_stdout}"
    );
    assert_eq!(
        refused_stdout.matches("\"type\":\"diagnostic\"").count(),
        1,
        "{refused_stdout}"
    );
    assert_eq!(
        refused_stdout
            .matches("\"code\":\"INVALID_ESCAPE\"")
            .count(),
        1,
        "{refused_stdout}"
    );
    let formatter_diagnostic = refused_stdout
        .lines()
        .find(|line| line.contains("\"code\":\"INVALID_ESCAPE\""))
        .expect("formatter reports the expected refusal code");
    assert!(formatter_diagnostic.contains("\"severity\":\"error\""));
    assert!(formatter_diagnostic.contains("\"path\":\"<stdin>\""));
    assert!(formatter_diagnostic.contains("\"line\":1"));
    assert!(!refused_stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
    assert!(
        refused_stdout.contains("\"refused\":[\"<stdin>\"]"),
        "{refused_stdout}"
    );
    assert!(
        refused_stdout.contains("\"output\":null"),
        "{refused_stdout}"
    );
    let summary = refused_stdout
        .lines()
        .find(|line| line.contains("\"type\":\"summary\""))
        .expect("formatter emits one summary record");
    assert!(summary.contains("\"exit_code\":1"), "{summary}");
    assert!(
        !refused_stdout.contains("pub const VALUE"),
        "refused formatter input must not emit formatted source:\n{refused_stdout}"
    );

    let accepted = run_check(&fix);
    let accepted_stdout = String::from_utf8(accepted.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(accepted.status.code(), Some(0), "{accepted_stdout}");
    assert!(accepted.stderr.is_empty());
    assert!(
        accepted_stdout.contains("\"kind\":\"check\""),
        "{accepted_stdout}"
    );
    assert!(!accepted_stdout.contains("\"type\":\"diagnostic\""));

    let formatted = run_fmt_check(&fix);
    let formatted_stdout = String::from_utf8(formatted.stdout).expect("formatter stdout is UTF-8");
    assert_eq!(formatted.status.code(), Some(0), "{formatted_stdout}");
    assert!(formatted.stderr.is_empty());
    assert!(
        formatted_stdout.contains("\"refused\":[]"),
        "{formatted_stdout}"
    );
    assert!(!formatted_stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
}

#[test]
fn literal_out_of_range_explanation_example_is_exact_and_fix_is_clean_and_canonical() {
    let example = source_block(LITERAL_OUT_OF_RANGE_MARKDOWN, "Example");
    let fix = source_block(LITERAL_OUT_OF_RANGE_MARKDOWN, "Fix");

    let rejected = run_check(&example);
    let rejected_stdout = String::from_utf8(rejected.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(rejected.status.code(), Some(1), "{rejected_stdout}");
    assert!(rejected.stderr.is_empty());
    assert_eq!(
        rejected_stdout.matches("\"type\":\"diagnostic\"").count(),
        1,
        "{rejected_stdout}"
    );
    assert_eq!(
        rejected_stdout
            .matches("\"code\":\"LITERAL_OUT_OF_RANGE\"")
            .count(),
        1,
        "{rejected_stdout}"
    );
    let diagnostic = rejected_stdout
        .lines()
        .find(|line| line.contains("\"code\":\"LITERAL_OUT_OF_RANGE\""))
        .expect("check reports the expected code");
    assert!(
        diagnostic.contains("\"severity\":\"error\""),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("\"path\":\"src/lib.sai\""),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("\"line\":1"), "{diagnostic}");
    assert!(
        diagnostic.contains("\"message\":\"integer literal is out of range for `U8`\""),
        "{diagnostic}"
    );
    assert!(!rejected_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
    assert!(!rejected_stdout.contains("\"type\":\"result\""));

    let accepted = run_check(&fix);
    let accepted_stdout = String::from_utf8(accepted.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(accepted.status.code(), Some(0), "{accepted_stdout}");
    assert!(accepted.stderr.is_empty());
    assert!(
        accepted_stdout.contains("\"kind\":\"check\""),
        "{accepted_stdout}"
    );
    assert!(!accepted_stdout.contains("\"type\":\"diagnostic\""));

    for (label, source) in [("Example", example), ("Fix", fix)] {
        let formatted = run_fmt_check(&source);
        let stdout = String::from_utf8(formatted.stdout).expect("formatter stdout is UTF-8");
        assert_eq!(
            formatted.status.code(),
            Some(0),
            "{label} source is not formatter-canonical:\n{stdout}"
        );
        assert!(formatted.stderr.is_empty());
        assert!(!stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
        assert!(
            stdout.contains("\"refused\":[]"),
            "{label} source was refused:\n{stdout}"
        );
    }
}

#[test]
fn explain_malformed_number_prints_the_exact_marked_record() {
    let output = run_explain(&["explain", "MALFORMED_NUMBER"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, MALFORMED_NUMBER_MARKDOWN);
}

#[test]
fn explain_malformed_number_serializes_the_exact_json_record() {
    let output = run_explain(&["--format=json", "explain", "MALFORMED_NUMBER"]);
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let expected_result = format!(
        concat!(
            "{{\"type\":\"result\",\"kind\":\"explain\",\"data\":{{",
            "\"code\":\"MALFORMED_NUMBER\",\"class\":\"hard\",\"severity\":\"error\",",
            "\"default_level\":null,\"stage\":0,\"owner\":\"spec/02-lexical-structure.md\",",
            "\"since\":\"0.1.0\",\"status\":\"active\",\"superseded_by\":null,",
            "\"summary\":{},\"markdown\":{}",
            "}}}}"
        ),
        json_string(MALFORMED_NUMBER_SUMMARY),
        json_string(MALFORMED_NUMBER_MARKDOWN),
    );
    let expected = format!(
        concat!(
            "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,",
            "\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},",
            "\"command\":\"explain\",\"root\":null}}\n",
            "{}\n",
            "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,",
            "\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,",
            "\"dependency\":0}},\"exit_code\":0}}\n"
        ),
        env!("CARGO_PKG_VERSION"),
        expected_result,
    );

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout, expected);
}

#[test]
fn malformed_number_explanation_refuses_and_fix_is_clean_and_canonical() {
    let (example, info) = source_block_with_info(MALFORMED_NUMBER_MARKDOWN, "Example");
    assert_eq!(info, "sai noncanonical");
    let fix = source_block(MALFORMED_NUMBER_MARKDOWN, "Fix");

    let rejected = run_check(&example);
    let rejected_stdout = String::from_utf8(rejected.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(rejected.status.code(), Some(1), "{rejected_stdout}");
    assert!(rejected.stderr.is_empty());
    assert_eq!(
        rejected_stdout
            .matches("\"code\":\"MALFORMED_NUMBER\"")
            .count(),
        1,
        "{rejected_stdout}"
    );
    let check_diagnostic = rejected_stdout
        .lines()
        .find(|line| line.contains("\"code\":\"MALFORMED_NUMBER\""))
        .expect("check reports the expected code");
    assert!(check_diagnostic.contains("\"severity\":\"error\""));
    assert!(check_diagnostic.contains("\"path\":\"src/lib.sai\""));
    assert!(check_diagnostic.contains("\"line\":1"));
    assert!(check_diagnostic.contains("\"message\":\"malformed numeric literal\""));
    for diagnostic in rejected_stdout
        .lines()
        .filter(|line| line.contains("\"type\":\"diagnostic\""))
    {
        assert!(
            diagnostic.contains("\"code\":\"MALFORMED_NUMBER\"")
                || diagnostic.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "unexpected check diagnostic:\n{diagnostic}"
        );
    }

    let refused = run_fmt(&example);
    let refused_stdout = String::from_utf8(refused.stdout).expect("formatter stdout is UTF-8");
    assert_eq!(refused.status.code(), Some(1), "{refused_stdout}");
    assert!(refused.stderr.is_empty());
    assert_eq!(refused_stdout.matches("\"type\":\"result\"").count(), 1);
    assert!(
        refused_stdout.contains("\"check\":false"),
        "{refused_stdout}"
    );
    assert!(refused_stdout.contains("\"files\":1"), "{refused_stdout}");
    assert!(
        refused_stdout.contains("\"changed\":[]"),
        "{refused_stdout}"
    );
    assert!(
        refused_stdout.contains("\"refused\":[\"<stdin>\"]"),
        "{refused_stdout}"
    );
    assert!(
        refused_stdout.contains("\"output\":null"),
        "{refused_stdout}"
    );
    assert_eq!(
        refused_stdout.matches("\"type\":\"diagnostic\"").count(),
        1,
        "{refused_stdout}"
    );
    let formatter_diagnostic = refused_stdout
        .lines()
        .find(|line| line.contains("\"code\":\"MALFORMED_NUMBER\""))
        .expect("formatter reports the expected refusal code");
    assert!(formatter_diagnostic.contains("\"severity\":\"error\""));
    assert!(formatter_diagnostic.contains("\"path\":\"<stdin>\""));
    assert!(formatter_diagnostic.contains("\"line\":1"));
    assert!(!refused_stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
    assert!(!refused_stdout.contains("pub const VALUE"));
    let summary = refused_stdout
        .lines()
        .find(|line| line.contains("\"type\":\"summary\""))
        .expect("formatter emits one summary record");
    assert!(summary.contains("\"exit_code\":1"), "{summary}");

    let accepted = run_check(&fix);
    let accepted_stdout = String::from_utf8(accepted.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(accepted.status.code(), Some(0), "{accepted_stdout}");
    assert!(accepted.stderr.is_empty());
    assert!(
        accepted_stdout.contains("\"kind\":\"check\""),
        "{accepted_stdout}"
    );
    assert!(!accepted_stdout.contains("\"type\":\"diagnostic\""));

    let formatted = run_fmt_check(&fix);
    let formatted_stdout = String::from_utf8(formatted.stdout).expect("formatter stdout is UTF-8");
    assert_eq!(formatted.status.code(), Some(0), "{formatted_stdout}");
    assert!(formatted.stderr.is_empty());
    assert!(
        formatted_stdout.contains("\"refused\":[]"),
        "{formatted_stdout}"
    );
    assert!(!formatted_stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
}

#[test]
fn explain_keeps_other_individual_codes_gated_and_preserves_list() {
    let gated = run_explain(&["--format=json", "explain", "UNKNOWN_FIELD"]);
    let gated_stdout = String::from_utf8(gated.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(gated.status.code(), Some(1), "{gated_stdout}");
    assert!(gated.stderr.is_empty());
    assert!(gated_stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
    assert!(gated_stdout.contains("\"feature\":\"sai explain\""));
    assert!(!gated_stdout.contains("\"kind\":\"explain\""));

    let list = run_explain(&["--format=json", "explain", "--list"]);
    let list_stdout = String::from_utf8(list.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(list.status.code(), Some(0), "{list_stdout}");
    assert!(list.stderr.is_empty());
    assert!(list_stdout.contains("\"kind\":\"explain.list\""));
    assert!(list_stdout.contains("\"code\":\"DUPLICATE_BINDING\",\"class\":\"hard\""));
    let invalid_escape_list_entry =
        "{\"code\":\"INVALID_ESCAPE\",\"class\":\"hard\",\"summary\":\"Invalid escape sequence\"}";
    let previous_code = list_stdout
        .find("\"code\":\"INVALID_ENTRY_POINT\"")
        .expect("list contains the preceding code");
    let invalid_escape = list_stdout
        .find(invalid_escape_list_entry)
        .expect("list contains the exact INVALID_ESCAPE entry");
    let next_code = list_stdout
        .find("\"code\":\"INVALID_EXPORT_TARGET\"")
        .expect("list contains the following code");
    assert!(previous_code < invalid_escape && invalid_escape < next_code);
    let literal_out_of_range_list_entry = "{\"code\":\"LITERAL_OUT_OF_RANGE\",\"class\":\"hard\","
        .to_owned()
        + "\"summary\":\"Literal out of range for its type\"}";
    let previous_literal_code = list_stdout
        .find("\"code\":\"LET_ELSE_NOT_DIVERGING\"")
        .expect("list contains the preceding literal-range code");
    let literal_out_of_range = list_stdout
        .find(&literal_out_of_range_list_entry)
        .expect("list contains the exact LITERAL_OUT_OF_RANGE entry");
    let next_literal_code = list_stdout
        .find("\"code\":\"LOCK_CORRUPT\"")
        .expect("list contains the following literal-range code");
    assert!(
        previous_literal_code < literal_out_of_range && literal_out_of_range < next_literal_code
    );
    let preceding_malformed_number = list_stdout
        .find("\"code\":\"MALFORMED_MULTILINE_STRING\"")
        .expect("list contains the preceding malformed-number code");
    let malformed_number_list_entry = "{\"code\":\"MALFORMED_NUMBER\",\"class\":\"hard\",\"summary\":\"Malformed numeric literal\"}";
    let malformed_number = list_stdout
        .find(malformed_number_list_entry)
        .expect("list contains the exact MALFORMED_NUMBER entry");
    let next_malformed_number = list_stdout
        .find("\"code\":\"MALFORMED_PACKAGE_PATH\"")
        .expect("list contains the following malformed-number code");
    assert!(
        preceding_malformed_number < malformed_number && malformed_number < next_malformed_number
    );
    assert!(list_stdout.contains("\"code\":\"TYPE_MISMATCH\",\"class\":\"hard\""));
    assert_eq!(list_stdout.matches("\"code\":").count(), 494);
}
