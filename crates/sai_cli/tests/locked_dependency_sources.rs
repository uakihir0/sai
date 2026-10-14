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
            "sai-cli-locked-dependency-sources-{}-{unique}",
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

#[test]
fn locked_check_reports_syntax_errors_from_active_path_dependencies() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("src")).expect("create dependency source directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write root package manifest");
    fs::write(root.join("src/lib.sai"), "pub fn root() {}\n").expect("write valid root source");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n"
        ),
    )
    .expect("write dependency package manifest");
    fs::write(dependency.join("src/lib.sai"), "pub fn {\n")
        .expect("write malformed dependency source");
    fs::write(
        dependency.join("src/other.sai"),
        "fn repeated() {}\nfn repeated() {}\n",
    )
    .expect("write dependency with duplicate declarations");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:dep @ ^1 -> 1.0.0 from path \"dep\"\n",
            "\n",
            "package acme:dep @ 1.0.0\n",
            "    source path \"dep\"\n"
        ),
    )
    .expect("write complete format-2 path lock");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(&root)
        .args([
            "--format=json",
            "check",
            "--target",
            "wasm32-wasi",
            "--locked",
            "--offline",
        ])
        .output()
        .expect("run sai check subprocess");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let syntax_diagnostic = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"EXPECTED_TOKEN\""))
        .expect("dependency syntax diagnostic should appear in JSON output");
    assert!(
        syntax_diagnostic.contains("\"package\":\"acme:dep@1.0.0\""),
        "{syntax_diagnostic}"
    );
    assert!(
        syntax_diagnostic.contains("\"path\":\"dep:acme:dep@1.0.0/src/lib.sai\""),
        "{syntax_diagnostic}"
    );
    let phase1_diagnostic = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"DUPLICATE_BINDING\""))
        .expect("dependency Phase 1 diagnostic should appear in JSON output");
    assert!(
        phase1_diagnostic.contains("\"package\":\"acme:dep@1.0.0\""),
        "{phase1_diagnostic}"
    );
    assert!(
        phase1_diagnostic.contains("\"path\":\"dep:acme:dep@1.0.0/src/other.sai\""),
        "{phase1_diagnostic}"
    );
}

#[test]
fn locked_check_still_checks_active_dependency_sources_when_root_source_is_missing() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(&root).expect("create root package directory");
    fs::create_dir_all(dependency.join("src")).expect("create dependency source directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write root package manifest");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n"
        ),
    )
    .expect("write dependency package manifest");
    fs::write(dependency.join("src/lib.sai"), "pub fn {\n")
        .expect("write malformed dependency source");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:dep @ ^1 -> 1.0.0 from path \"dep\"\n",
            "\n",
            "package acme:dep @ 1.0.0\n",
            "    source path \"dep\"\n"
        ),
    )
    .expect("write complete format-2 path lock");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(&root)
        .args([
            "--format=json",
            "check",
            "--target",
            "wasm32-wasi",
            "--locked",
            "--offline",
        ])
        .output()
        .expect("run sai check subprocess");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert!(
        stdout
            .lines()
            .any(|line| line.contains("\"code\":\"MISSING_ROOT_MODULE\"")),
        "{stdout}"
    );
    let syntax_diagnostic = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"EXPECTED_TOKEN\""))
        .expect("dependency syntax diagnostic should appear even with a missing root source");
    assert!(
        syntax_diagnostic.contains("\"package\":\"acme:dep@1.0.0\"")
            && syntax_diagnostic.contains("\"path\":\"dep:acme:dep@1.0.0/src/lib.sai\""),
        "{syntax_diagnostic}"
    );
}

#[test]
fn locked_check_checks_active_dependency_sources_for_an_artifact_only_root() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");
    fs::create_dir_all(dependency.join("src")).expect("create dependency source directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world check\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/check.wasm\" }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write artifact-only root manifest");
    fs::write(
        root.join("wit/check.wit"),
        "package acme:check; world check {}\n",
    )
    .expect("write root WIT file");
    fs::write(root.join("dist/check.wasm"), b"component").expect("write root component artifact");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n"
        ),
    )
    .expect("write dependency package manifest");
    fs::write(dependency.join("src/lib.sai"), "pub fn {\n")
        .expect("write malformed dependency source");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:dep @ ^1 -> 1.0.0 from path \"dep\"\n",
            "\n",
            "package acme:dep @ 1.0.0\n",
            "    source path \"dep\"\n"
        ),
    )
    .expect("write complete format-2 path lock");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(&root)
        .args([
            "--format=json",
            "check",
            "--target",
            "wasm32-wasi",
            "--locked",
            "--offline",
        ])
        .output()
        .expect("run sai check subprocess");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let syntax_diagnostic = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"EXPECTED_TOKEN\""))
        .expect("dependency syntax diagnostic should appear for an artifact-only root");
    assert!(
        syntax_diagnostic.contains("\"package\":\"acme:dep@1.0.0\"")
            && syntax_diagnostic.contains("\"path\":\"dep:acme:dep@1.0.0/src/lib.sai\""),
        "{syntax_diagnostic}"
    );
}
