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

#[test]
fn check_reports_tracked_interface_use_cycle_diagnostics() {
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
            "pub interface first {\n",
            "    type Request = second.Response\n",
            "}\n",
            "pub interface second {\n",
            "    type Response = first.Request\n",
            "}\n"
        ),
    )
    .expect("write cyclic local interfaces");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let cycle_records = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"INTERFACE_USE_CYCLE\""))
        .collect::<Vec<_>>();
    assert_eq!(cycle_records.len(), 1, "{stdout}");
    assert!(
        cycle_records[0].contains("\"primary\":{\"file\":{\"path\":\"src/lib.sai\""),
        "{}",
        cycle_records[0]
    );
    assert!(
        cycle_records[0].contains("\"package\":\"acme:check@0.1.0\""),
        "{}",
        cycle_records[0]
    );
    assert!(
        cycle_records[0].contains("\"children\":["),
        "{}",
        cycle_records[0]
    );
    assert!(
        cycle_records[0].contains(
            "\"kind\":\"note\",\"message\":\"this reference continues the interface-use cycle\",\"location\":{\"file\":{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null}"
        ),
        "{}",
        cycle_records[0]
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":2,"),
        "{stdout}"
    );
    assert!(stdout.contains("\"exit_code\":1"), "{stdout}");
}

#[test]
fn check_reports_package_wit_name_collisions_and_counts_them() {
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
    fs::write(source_directory.join("lib.sai"), b"").expect("write root module");
    fs::write(source_directory.join("a.sai"), b"pub interface codec {}\n")
        .expect("write earlier interface");
    fs::write(source_directory.join("b.sai"), b"pub interface codec {}\n")
        .expect("write colliding interface");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let collisions = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_NAME_COLLISION\""))
        .collect::<Vec<_>>();
    assert_eq!(collisions.len(), 1, "{stdout}");
    assert!(
        collisions[0].contains("\"primary\":{\"file\":{\"path\":\"src/b.sai\""),
        "{}",
        collisions[0]
    );
    assert!(
        collisions[0].contains("\"what\":\"earlier declaration\""),
        "{}",
        collisions[0]
    );
    assert!(
        collisions[0]
            .contains("\"related\":[{\"kind\":\"declared_here\",\"location\":{\"file\":{\"path\":\"src/a.sai\""),
        "{}",
        collisions[0]
    );
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":2,"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
}

#[test]
fn check_reports_empty_interface_records_and_enums() {
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
            "pub interface api {\n",
            "    type EmptyRecord {}\n",
            "    enum EmptyEnum {}\n",
            "    type NonEmptyRecord { value: U32 }\n",
            "    enum NonEmptyEnum { Ready }\n",
            "}\n"
        ),
    )
    .expect("write empty and nonempty interface types");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let empty_types = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_EMPTY_TYPE\""))
        .collect::<Vec<_>>();
    assert_eq!(empty_types.len(), 2, "{stdout}");
    assert!(
        empty_types[0].contains("\"primary_label\":\"empty WIT interface type\""),
        "{}",
        empty_types[0]
    );
    assert!(
        empty_types[0].contains("\"primary\":{\"file\":{\"path\":\"src/lib.sai\""),
        "{}",
        empty_types[0]
    );
    assert!(
        empty_types[0].contains("\"start\":{\"byte\":29,\"line\":2,"),
        "{}",
        empty_types[0]
    );
    assert!(
        empty_types[1].contains("\"primary\":{\"file\":{\"path\":\"src/lib.sai\""),
        "{}",
        empty_types[1]
    );
    assert!(
        empty_types[1].contains("\"start\":{\"byte\":53,\"line\":3,"),
        "{}",
        empty_types[1]
    );
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":3,"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
}
