use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sai-cli-lexical-diagnostics-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary test directory");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn check_reports_no_semicolon_without_parser_cascades() {
    let temporary = TestDirectory::new();
    fs::create_dir(temporary.path().join("src")).expect("create source directory");
    fs::write(
        temporary.path().join("sai.package"),
        concat!(
            "package acme:lexical @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-core }\n"
        ),
    )
    .expect("write package manifest");
    fs::write(
        temporary.path().join("src/lib.sai"),
        concat!(
            "pub fn value() -> I32 {\n",
            "    let first: I32 = 1;; let second: I32 = 2\n",
            "    second\n",
            "}\n"
        ),
    )
    .expect("write source file");

    let cache = temporary.path().join("cache");
    let home = temporary.path().join("home");
    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(temporary.path())
        .env("NO_COLOR", "1")
        .env("SAI_CACHE_DIR", cache)
        .env("HOME", home)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check subprocess");

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("CLI stderr is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "check should reject the lexical error:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.is_empty(),
        "JSON output must stay on stdout:\n{stderr}"
    );
    assert_eq!(
        stdout.lines().count(),
        5,
        "unexpected NDJSON stream:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"type\":\"diagnostic\"").count(),
        3,
        "the semicolon must not trigger parser cascades:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("\"code\":\"NO_SEMICOLON\"").count(),
        2,
        "both lexical errors must be reported once:\n{stdout}"
    );
    assert!(stdout.contains("\"severity\":\"error\""), "{stdout}");
    assert!(stdout.contains("\"path\":\"src/lib.sai\""), "{stdout}");
    assert!(stdout.contains("\"line\":2"), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "the check feature gate is expected for this valid source shape:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"EXPECTED_TOKEN\"")
            && !stdout.contains("\"code\":\"EXPECTED_EXPRESSION\"")
            && !stdout.contains("\"type\":\"result\""),
        "unexpected cascade or successful result:\n{stdout}"
    );
    assert!(stdout.contains("\"exit_code\":1"), "{stdout}");
}
