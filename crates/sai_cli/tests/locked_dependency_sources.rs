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
fn locked_check_resolves_foreign_imports_after_dependency_reexports() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let middle = root.join("middle");
    let leaf = middle.join("leaf");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(middle.join("src")).expect("create middle source directory");
    fs::create_dir_all(leaf.join("src")).expect("create leaf source directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:middle @ ^1 from path \"middle\" }\n"
        ),
    )
    .expect("write root package manifest");
    let root_source = concat!(
        "use acme:middle\n",
        "use acme:middle.{exported_fn}\n",
        "use acme:middle.{Missing}\n",
        "use acme:middle/not_a_module as unavailable\n",
    );
    fs::write(root.join("src/lib.sai"), root_source).expect("write root imports");
    fs::write(
        middle.join("sai.package"),
        concat!(
            "package acme:middle @ 1.0.0\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:leaf @ ^1 from path \"leaf\" }\n"
        ),
    )
    .expect("write middle package manifest");
    fs::write(
        middle.join("src/lib.sai"),
        "pub use acme:leaf/model.{public_fn as exported_fn}\n\
         pub use acme:leaf/model.{private_fn as unavailable_reexport}\n",
    )
    .expect("write middle package re-exports");
    fs::write(
        leaf.join("sai.package"),
        "package acme:leaf @ 1.0.0\nsource \"src\"\ntargets { wasm32-wasi }\n",
    )
    .expect("write leaf package manifest");
    fs::write(leaf.join("src/lib.sai"), "pub fn leaf_root() {}\n")
        .expect("write leaf package root");
    fs::write(
        leaf.join("src/model.sai"),
        "pub fn public_fn() {}\nfn private_fn() {}\n",
    )
    .expect("write leaf module");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:middle @ ^1 -> 1.0.0 from path \"middle\"\n",
            "\n",
            "package acme:leaf @ 1.0.0\n",
            "    source path \"middle/leaf\"\n",
            "\n",
            "package acme:middle @ 1.0.0\n",
            "    source path \"middle\"\n",
            "    requires acme:leaf @ ^1 -> 1.0.0 from path \"leaf\"\n"
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
    let unresolved = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"UNRESOLVED_IMPORT\""))
        .expect("a missing foreign member should be diagnosed");
    assert!(
        unresolved.contains("\"package\":\"acme:check@0.1.0\"")
            && unresolved.contains("\"path\":\"src/lib.sai\""),
        "{unresolved}"
    );
    assert!(
        stdout.contains("\"code\":\"PRIVATE_REEXPORT\""),
        "middle must not re-export a private leaf item:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"UNKNOWN_MODULE\""),
        "the active dependency module tree should diagnose unknown suffixes:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"stage\":3,\"feature\":\"dependencies\""),
        "active source package imports should not remain behind the Stage 3 gate:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"PRIVATE_ITEM\""),
        "the root does not directly import a private leaf item:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "{stdout}"
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
