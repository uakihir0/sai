use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

const TYPE_MISMATCH_MARKDOWN: &str =
    include_str!("../../sai_diagnostics/src/explanations/type_mismatch.md");
const TYPE_MISMATCH_SUMMARY: &str = "Two types that must be equal, or a coercion that does not apply; names the expected and found types and the origin of the expectation";

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

fn run_check(source: &str) -> Output {
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
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check subprocess")
}

fn source_block(markdown: &str, section: &str) -> String {
    let heading = format!("## {section}\n");
    let section = markdown
        .split_once(&heading)
        .unwrap_or_else(|| panic!("missing {heading:?} section"))
        .1;
    let fence = section
        .strip_prefix("\n```sai\n")
        .or_else(|| section.strip_prefix("```sai\n"))
        .expect("section starts with a SAI code fence");
    let source = fence
        .split_once("\n```")
        .expect("SAI code fence is closed")
        .0;
    format!("{source}\n")
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
        .args(["fmt", "--check", "--stdin-kind", "source", "-"])
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
fn explanation_example_and_fix_compile_with_the_documented_result() {
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
    assert!(list_stdout.contains("\"code\":\"TYPE_MISMATCH\",\"class\":\"hard\""));
    assert_eq!(list_stdout.matches("\"code\":").count(), 493);
}
