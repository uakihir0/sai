use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sai-cli-interface-imports-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary working directory");
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
fn check_resolves_local_interface_member_imports_but_keeps_check_incomplete() {
    let directory = TestDirectory::new();
    let package = directory.path().join("package");
    let source_directory = package.join("src");
    fs::create_dir_all(&source_directory).expect("create package source directory");
    fs::write(
        package.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n\n",
            "targets { wasm32-wasi }\n"
        ),
    )
    .expect("write package manifest");
    fs::write(
        source_directory.join("lib.sai"),
        concat!(
            "pub interface codec {\n",
            "    type Request {}\n",
            "    fn send()\n",
            "}\n",
            "use acme:check/codec.{Request, send}\n",
            "pub use acme:check/codec.{Request as ExportedRequest, send as exported_send}\n"
        ),
    )
    .expect("write interface and local member imports");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""))
            .count(),
        1,
        "{stdout}"
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"exit_code\":1"), "{stdout}");
    assert!(!stdout.contains("\"type\":\"result\""), "{stdout}");

    for code in [
        "EXPECTED_TOKEN",
        "UNKNOWN_MODULE",
        "UNRESOLVED_IMPORT",
        "PRIVATE_ITEM",
        "PRIVATE_REEXPORT",
        "INTERNAL_COMPILER_ERROR",
    ] {
        assert!(
            !stdout.contains(&format!("\"code\":\"{code}\"")),
            "{code} should not be reported:\n{stdout}"
        );
    }
}
