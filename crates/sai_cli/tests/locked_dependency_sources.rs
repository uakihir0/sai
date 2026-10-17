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

fn locked_artifact_root_with_external_world(dependency_wit: &str) -> (TestDirectory, PathBuf) {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");
    fs::create_dir_all(dependency.join("wit")).expect("create dependency WIT directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:binary @ 1.0.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world acme:dep/api\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n",
            "artifact component { wasm32-wasi \"dist/binary.wasm\" }\n"
        ),
    )
    .expect("write artifact root manifest");
    fs::write(
        root.join("wit/root.wit"),
        "package acme:binary@1.0.0;\nworld local {}\n",
    )
    .expect("write root WIT");
    fs::write(root.join("dist/binary.wasm"), b"component").expect("write root component");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "wit \"wit\"\n",
            "export world api\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/dep.wasm\" }\n"
        ),
    )
    .expect("write artifact dependency manifest");
    fs::write(dependency.join("wit/api.wit"), dependency_wit).expect("write dependency WIT");
    fs::write(dependency.join("dist/dep.wasm"), b"component")
        .expect("write dependency component artifact");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:binary @ 1.0.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:dep @ ^1 -> 1.0.0 from path \"dep\"\n",
            "\n",
            "package acme:dep @ 1.0.0\n",
            "    source path \"dep\"\n"
        ),
    )
    .expect("write complete format-2 path lock");

    (temporary, root)
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
    fs::write(dependency.join("src/a.sai"), "pub interface codec {}\n")
        .expect("write earlier dependency interface");
    fs::write(dependency.join("src/b.sai"), "pub interface codec {}\n")
        .expect("write colliding dependency interface");
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
    let wit_collision = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"WIT_NAME_COLLISION\""))
        .expect("dependency WIT-name collision should appear in JSON output");
    assert!(
        wit_collision.contains("\"package\":\"acme:dep@1.0.0\""),
        "{wit_collision}"
    );
    assert!(
        wit_collision.contains("\"path\":\"dep:acme:dep@1.0.0/src/b.sai\""),
        "{wit_collision}"
    );
    assert!(
        wit_collision.contains("\"path\":\"dep:acme:dep@1.0.0/src/a.sai\""),
        "{wit_collision}"
    );
}

#[test]
fn locked_check_reports_external_export_implementation_at_manifest_value() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("wit")).expect("create dependency WIT directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");

    let manifest = concat!(
        "package acme:check @ 0.1.0\n",
        "compiler ^0.1\n",
        "source \"src\"\n",
        "targets { wasm32-wasi }\n",
        "requires { acme:codec @ ^1 from path \"dep\" }\n",
        "export world acme:codec/host\n"
    );
    fs::write(root.join("sai.package"), manifest).expect("write root package manifest");
    fs::write(root.join("src/lib.sai"), "pub fn root() {}\n").expect("write root source");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:codec @ 1.0.0\n",
            "wit \"wit\"\n",
            "export world host\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/codec.wasm\" }\n"
        ),
    )
    .expect("write artifact dependency manifest");
    fs::write(
        dependency.join("wit/api.wit"),
        "package acme:codec@1.0.0;\ninterface api {}\nworld host { export api; }\n",
    )
    .expect("write dependency WIT");
    fs::write(dependency.join("dist/codec.wasm"), b"component")
        .expect("write dependency component artifact");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:codec @ ^1 -> 1.0.0 from path \"dep\"\n",
            "\n",
            "package acme:codec @ 1.0.0\n",
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
    let diagnostic = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"WORLD_EXPORT_UNIMPLEMENTED\""))
        .unwrap_or_else(|| {
            panic!("the external exported interface needs an implementation:\n{stdout}")
        });
    let value_start = manifest
        .find("acme:codec/host")
        .expect("manifest export value exists");
    let value_end = value_start + "acme:codec/host".len();
    assert!(
        diagnostic.contains("\"package\":\"acme:check@0.1.0\"")
            && diagnostic.contains("\"path\":\"sai.package\"")
            && diagnostic.contains(&format!("\"start\":{{\"byte\":{value_start},"))
            && diagnostic.contains(&format!("\"end\":{{\"byte\":{value_end},")),
        "{diagnostic}"
    );
}

#[test]
fn locked_check_resolves_artifact_root_export_world_without_source_inputs() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");

    let manifest = concat!(
        "package acme:binary @ 1.0.0\n",
        "compiler ^0.1\n",
        "wit \"wit\"\n",
        "export world absent\n",
        "targets { wasm32-wasi }\n",
        "artifact component { wasm32-wasi \"dist/binary.wasm\" }\n"
    );
    fs::write(root.join("sai.package"), manifest).expect("write artifact root manifest");
    fs::write(
        root.join("wit/api.wit"),
        "package acme:binary@1.0.0;\nworld present {}\n",
    )
    .expect("write root WIT");
    fs::write(root.join("dist/binary.wasm"), b"component").expect("write root component");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:binary @ 1.0.0\n",
            "    targets { wasm32-wasi }\n"
        ),
    )
    .expect("write complete format-2 root lock");

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
    let diagnostic = stdout
        .lines()
        .find(|line| line.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\""))
        .unwrap_or_else(|| panic!("missing artifact export world should be diagnosed:\n{stdout}"));
    let value_start = manifest.find("absent").expect("manifest value exists");
    let value_end = value_start + "absent".len();
    assert!(
        diagnostic.contains("\"package\":\"acme:binary@1.0.0\"")
            && diagnostic.contains("\"path\":\"sai.package\"")
            && diagnostic.contains(&format!("\"start\":{{\"byte\":{value_start},"))
            && diagnostic.contains(&format!("\"end\":{{\"byte\":{value_end},")),
        "{diagnostic}"
    );
    assert!(
        !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "artifact world selection should be a user diagnostic:\n{stdout}"
    );
}

#[test]
fn locked_check_skips_artifact_export_world_query_when_root_wit_directory_is_missing() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:binary @ 1.0.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world app\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/binary.wasm\" }\n"
        ),
    )
    .expect("write artifact root manifest");
    fs::write(root.join("dist/binary.wasm"), b"component").expect("write root component");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:binary @ 1.0.0\n",
            "    targets { wasm32-wasi }\n"
        ),
    )
    .expect("write complete format-2 root lock");

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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "missing root WIT should fail the check with its input diagnostic:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"WIT_DIR_NOT_FOUND\"")
            && !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "unavailable root WIT input must not trigger export-world resolution:\n{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn locked_check_does_not_resolve_artifact_world_from_a_partial_wit_inventory() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:binary @ 1.0.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world hidden\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/binary.wasm\" }\n"
        ),
    )
    .expect("write artifact root manifest");
    fs::write(
        root.join("wit/present.wit"),
        "package acme:binary@1.0.0;\nworld present {}\n",
    )
    .expect("write captured root WIT");
    fs::write(
        root.join("hidden.wit"),
        "package acme:binary@1.0.0;\nworld hidden {}\n",
    )
    .expect("write WIT behind the symlink");
    std::os::unix::fs::symlink(root.join("hidden.wit"), root.join("wit/hidden.wit"))
        .expect("create WIT symlink");
    fs::write(root.join("dist/binary.wasm"), b"component").expect("write root component");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:binary @ 1.0.0\n",
            "    targets { wasm32-wasi }\n"
        ),
    )
    .expect("write complete format-2 root lock");

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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "the unsafe WIT entry should fail the check:\n{stdout}"
    );
    assert!(
        stdout.contains("not a regular file")
            && !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "selection must not use an incomplete captured WIT inventory:\n{stdout}"
    );
}

#[test]
fn locked_check_resolves_external_export_world_for_artifact_root() {
    let (_temporary, root) =
        locked_artifact_root_with_external_world("package acme:dep@1.0.0;\nworld api {}\n");

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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "artifact root should resolve its direct dependency's WIT world:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\"")
            && stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
        "external artifact export-world selection should complete cleanly:\n{stdout}"
    );
}

#[test]
fn locked_check_keeps_artifact_root_world_unresolved_when_dependency_wit_directory_is_missing() {
    let (_temporary, root) =
        locked_artifact_root_with_external_world("package acme:dep@1.0.0;\nworld api {}\n");
    fs::remove_dir_all(root.join("dep/wit")).expect("remove captured dependency WIT directory");

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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "the WIT capture diagnostic should own the missing dependency directory:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"WIT_DIR_NOT_FOUND\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\"")
            && !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\""),
        "an unavailable dependency inventory must not cascade into selection:\n{stdout}"
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"WIT_DIR_NOT_FOUND\""))
            .count(),
        1,
        "the dependency capture failure should be reported once:\n{stdout}"
    );
}

#[test]
fn locked_check_keeps_source_root_world_unresolved_when_dependency_wit_directory_is_missing() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:app @ 1.0.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "export world acme:dep/api\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write source root manifest");
    fs::write(root.join("src/lib.sai"), "pub fn main() {}\n").expect("write root source");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "wit \"wit\"\n",
            "export world api\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/dep.wasm\" }\n"
        ),
    )
    .expect("write artifact dependency manifest");
    fs::write(dependency.join("dist/dep.wasm"), b"component")
        .expect("write dependency component artifact");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:app @ 1.0.0\n",
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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "the WIT capture diagnostic should own the missing dependency directory:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"WIT_DIR_NOT_FOUND\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\"")
            && !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\""),
        "an unavailable dependency inventory must not cascade into selection:\n{stdout}"
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"WIT_DIR_NOT_FOUND\""))
            .count(),
        1,
        "the dependency capture failure should be reported once:\n{stdout}"
    );
}

#[test]
fn locked_check_still_resolves_local_source_worlds_when_an_unrelated_dependency_wit_is_missing() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:app @ 1.0.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "export world absent\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write source root manifest");
    fs::write(root.join("src/lib.sai"), "pub fn main() {}\n").expect("write root source");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "wit \"wit\"\n",
            "export world api\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/dep.wasm\" }\n"
        ),
    )
    .expect("write artifact dependency manifest");
    fs::write(dependency.join("dist/dep.wasm"), b"component")
        .expect("write dependency component artifact");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:app @ 1.0.0\n",
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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "both the incomplete dependency input and local miss are errors:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"WIT_DIR_NOT_FOUND\"")
            && stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "an unrelated dependency WIT capture failure must preserve local world lookup:\n{stdout}"
    );
}

#[test]
fn locked_check_does_not_report_missing_source_wit_generation_as_internal_error() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");
    fs::create_dir_all(dependency.join("src")).expect("create source dependency directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:binary @ 1.0.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world acme:dep/api\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n",
            "artifact component { wasm32-wasi \"dist/binary.wasm\" }\n"
        ),
    )
    .expect("write artifact root manifest");
    fs::write(
        root.join("wit/root.wit"),
        "package acme:binary@1.0.0;\nworld local {}\n",
    )
    .expect("write root WIT");
    fs::write(root.join("dist/binary.wasm"), b"component").expect("write root component");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n"
        ),
    )
    .expect("write source dependency manifest");
    fs::write(
        dependency.join("src/lib.sai"),
        "pub interface types {}\npub world api {}\n",
    )
    .expect("write source dependency with a world the generator cannot emit yet");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:binary @ 1.0.0\n",
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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "source dependency WIT generation remains an incomplete check stage:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && stdout.contains("generated WIT for source dependencies")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\"")
            && !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\""),
        "missing generated WIT must not be misreported as an internal failure or an unknown world:\n{stdout}"
    );
}

#[test]
fn locked_check_keeps_source_root_world_unresolved_when_dependency_wit_is_not_generated() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("src")).expect("create source dependency directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:app @ 1.0.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "export world acme:dep/api\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write source root manifest");
    fs::write(root.join("src/lib.sai"), "pub fn main() {}\n").expect("write root source");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n"
        ),
    )
    .expect("write source dependency manifest");
    fs::write(
        dependency.join("src/lib.sai"),
        "pub interface api {}\npub world api {}\n",
    )
    .expect("write valid source dependency");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:app @ 1.0.0\n",
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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "source-root export-world checks remain incomplete without generated dependency WIT:\n{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && stdout.contains("generated WIT for source dependencies")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\"")
            && !stdout.contains("\"code\":\"UNKNOWN_EXPORT_WORLD\""),
        "unavailable generated WIT must not become an internal error or a false unknown world:\n{stdout}"
    );
}

#[test]
fn locked_check_reports_artifact_external_world_load_failure_without_internal_error() {
    let malformed_wit = "package acme:dep@1.0.0;\ninterface api { # }\n";
    let (_temporary, root) = locked_artifact_root_with_external_world(malformed_wit);

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

    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "malformed dependency WIT should fail the check:\n{stdout}"
    );
    assert!(output.stderr.is_empty());
    let wit_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_PARSE_ERROR\""))
        .collect::<Vec<_>>();
    assert_eq!(
        wit_diagnostics.len(),
        1,
        "the WIT loader diagnostic should be emitted once by its owner:\n{stdout}"
    );
    assert!(
        wit_diagnostics[0].contains("\"package\":\"acme:dep@1.0.0\"")
            && wit_diagnostics[0].contains("\"path\":\"dep:acme:dep@1.0.0/wit/api.wit\"")
            && !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "artifact external selection must not turn an owned WIT error into an internal error:\n{stdout}"
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
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\"")
            && !stdout.contains("\"type\":\"result\""),
        "partial source-scope diagnostics keep check incomplete:\n{stdout}"
    );
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
fn locked_check_reports_generated_wit_typos_in_function_and_const_owners() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dependency");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("src")).expect("create dependency source directory");
    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:dependency @ ^1 from path \"dependency\" }\n",
        ),
    )
    .expect("write root manifest");
    fs::write(
        dependency.join("sai.package"),
        "package acme:dependency @ 1.0.0\nsource \"src\"\ntargets { wasm32-wasi }\n",
    )
    .expect("write dependency manifest");
    fs::write(
        dependency.join("src/lib.sai"),
        "pub interface api {\n  type PublicType = U32\n  type Wrapped = PublicType\n}\n",
    )
    .expect("write partial generated interface");
    let source = concat!(
        "use acme:dependency/api as api\n",
        "fn valid(value: api.PublicType) {}\n",
        "const GOOD: api.PublicType = 0\n",
        "fn omitted(value: api.Wrapped) {}\n",
        "const OMITTED: api.Wrapped = 0\n",
        "fn bad(value: api.PublicTypo) {\n",
        "  let body_missing: api.PublicTypo = 0\n",
        "}\n",
        "const BAD: api.PublicTypo = {\n",
        "  let inner_missing: api.PublicTypo = 0\n",
        "  0\n",
        "}\n",
    );
    fs::write(root.join("src/lib.sai"), source).expect("write root owners");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n",
            "    requires acme:dependency @ ^1 -> 1.0.0 from path \"dependency\"\n",
            "\n",
            "package acme:dependency @ 1.0.0\n",
            "    source path \"dependency\"\n",
        ),
    )
    .expect("write complete dependency lock");
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
        .expect("run locked check");
    let stdout = String::from_utf8(output.stdout).expect("diagnostics are UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert_eq!(output.status.code(), Some(1), "{stdout}\n{stderr}");
    let unresolved = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"UNRESOLVED_NAME\""))
        .collect::<Vec<_>>();
    assert_eq!(unresolved.len(), 4, "{stdout}\n{stderr}");
    for (start, _) in source.match_indices("PublicTypo") {
        let diagnostic = unresolved
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{start},")))
            .unwrap_or_else(|| panic!("missing owner diagnostic at byte {start}: {stdout}"));
        assert!(
            diagnostic.contains("\"candidates\":[\"PublicType\"]"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("\"module\":\"acme:dependency@1.0.0/api\""),
            "{diagnostic}"
        );
    }
    assert!(
        !unresolved
            .iter()
            .any(|line| line.contains("\"name\":\"Wrapped\"")),
        "source-present omitted declarations stay suppressed: {stdout}"
    );
    assert!(
        !stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""),
        "{stdout}\n{stderr}"
    );
}

#[test]
fn locked_check_reports_private_type_members_through_transitive_source_scopes() {
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
        "use acme:middle as middle\n",
        "world root_world {\n",
        "  import fn root_world_import(missing: middle.api.PublicTypo) -> middle.api.PublicType\n",
        "  export fn root_world_export(missing: middle.api.PublicTypo) -> middle.api.PublicType\n",
        "}\n",
        "interface root_api {\n",
        "  type RootAlias = middle.api.PublicTypo\n",
        "  fn root_interface_member(missing: middle.api.PublicTypo)\n",
        "  resource RootRemote {\n",
        "    fn root_resource_member(missing: middle.api.PublicTypo)\n",
        "  }\n",
        "}\n",
        "type Record {\n",
        "  public: middle.api.PublicType\n",
        "}\n",
        "type RootTypo = middle.api.PublicTypo\n",
        "invariant RootInvariant { {\n",
        "  let missing: middle.api.PublicTypo = 0\n",
        "  true\n",
        "} }\n",
        "type RootRefined = U32 where {\n",
        "  let missing: middle.api.PublicTypo = 0\n",
        "  true\n",
        "}\n",
        "unsafe extern \"wasm\" module \"env\" {\n",
        "  fn transitive_extern(missing: middle.api.PublicTypo)\n",
        "}\n",
        "trait RootHeader<Missing: middle.api.PublicTypo> {\n",
        "  type RootAssociated: middle.api.PublicType\n",
        "  type RootMissingAssociated: middle.api.PublicTypo\n",
        "  fn root_trait_member(missing: middle.api.PublicTypo)\n",
        "}\n",
        "impl middle.api.PublicType for middle.api.PublicType where Self: middle.api.PublicTypo {\n",
        "  type RootImplAssociated = middle.api.PublicType\n",
        "  type RootMissingImplAssociated = middle.api.PublicTypo\n",
        "  fn root_impl_member(missing: middle.api.PublicTypo) {}\n",
        "}\n",
    );
    fs::write(root.join("src/lib.sai"), root_source).expect("write root source");
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
    let middle_source = concat!(
        "use acme:leaf/model as leaf_model\n",
        "pub use acme:leaf.{api}\n",
        "interface middle_api {\n",
        "  fn middle_interface_member(private: leaf_model.PrivateType, missing: leaf_model.PublicTypo) -> leaf_model.PublicType\n",
        "  resource MiddleRemote {\n",
        "    fn middle_resource_member(private: leaf_model.PrivateType, missing: leaf_model.PublicTypo) -> leaf_model.PublicType\n",
        "  }\n",
        "}\n",
        "type MiddleRecord {\n",
        "  value: leaf_model.PrivateType\n",
        "  missing: leaf_model.PublicTypo\n",
        "  hidden_missing: leaf_model.PrivatType\n",
        "}\n",
        "const MIDDLE_CONST: leaf_model.PrivateType = {\n",
        "  let nested: leaf_model.PublicTypo = 0\n",
        "  0\n",
        "}\n",
        "fn middle_signature(private: leaf_model.PrivateType, missing: leaf_model.PublicTypo) {\n",
        "  let body_private: leaf_model.PrivateType = 0\n",
        "  let body_missing: leaf_model.PublicTypo = 0\n",
        "}\n",
        "invariant U32 { {\n",
        "  let private: leaf_model.PrivateType = 0\n",
        "  let missing: leaf_model.PublicTypo = 0\n",
        "  true\n",
        "} }\n",
        "type MiddleRefined = U32 where {\n",
        "  let private: leaf_model.PrivateType = 0\n",
        "  let missing: leaf_model.PublicTypo = 0\n",
        "  true\n",
        "}\n",
        "distinct MiddleDistinct = U32 where {\n",
        "  let private: leaf_model.PrivateType = 0\n",
        "  let missing: leaf_model.PublicTypo = 0\n",
        "  true\n",
        "}\n",
    );
    fs::write(middle.join("src/lib.sai"), middle_source)
        .expect("write middle package scopes and type owner");
    fs::write(
        leaf.join("sai.package"),
        "package acme:leaf @ 1.0.0\nsource \"src\"\ntargets { wasm32-wasi }\n",
    )
    .expect("write leaf package manifest");
    fs::write(
        leaf.join("src/lib.sai"),
        concat!(
            "pub interface api {\n",
            "  type PublicTppe = U32\n",
            "  type PublicType = U32\n",
            "  type PrivateType = U32\n",
            "  type PublicTypoC = U32\n",
            "  type PublicTypoB = U32\n",
            "  type PublicTypoA = U32\n",
            "  type PublicTypoE = U32\n",
            "}\n",
        ),
    )
    .expect("write leaf package root");
    fs::write(
        leaf.join("src/model.sai"),
        "pub type PublicType = U32\ntype PrivateType = U32\n",
    )
    .expect("write leaf module with public and private types");
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
    .expect("write complete format-2 transitive path lock");

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
    let private_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"PRIVATE_ITEM\""))
        .collect::<Vec<_>>();
    assert_eq!(private_diagnostics.len(), 9, "{stdout}");
    let reference_start = middle_source
        .find("PrivateType")
        .expect("private reference exists");
    let leaf_model_source = "pub type PublicType = U32\ntype PrivateType = U32\n";
    let declaration_start = leaf_model_source
        .find("PrivateType")
        .expect("private declaration exists");
    let const_start = middle_source
        .find("const MIDDLE_CONST")
        .expect("const declaration exists");
    let const_end = middle_source[const_start..]
        .find("\n}\n")
        .expect("const declaration block has a closing brace")
        + const_start
        + 2;
    let const_source = &middle_source[const_start..const_end];
    let const_private_start = const_start
        + const_source
            .find("PrivateType")
            .expect("const declared private type exists");
    let const_missing_start = const_start
        + const_source
            .rfind("PublicTypo")
            .expect("const initializer missing type exists");
    let private = private_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{reference_start},"))
        })
        .expect("the middle package private member diagnostic is present");
    assert!(
        private.contains("\"package\":\"acme:middle@1.0.0\"")
            && private.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
            && private.contains(&format!("\"start\":{{\"byte\":{reference_start},")),
        "{private}"
    );
    assert!(
        private.contains("\"what\":\"private member declared here\"")
            && private.contains(
                "\"path\":\"dep:acme:leaf@1.0.0/src/model.sai\",\"package\":\"acme:leaf@1.0.0\""
            )
            && private.contains(&format!(
                "\"start\":{{\"byte\":{declaration_start},\"line\":2"
            )),
        "the foreign declaration is attached at its exact source location: {private}"
    );
    let function_start = middle_source
        .find("fn middle_signature")
        .expect("function signature exists");
    let function_private_start = function_start
        + middle_source[function_start..]
            .find("private: leaf_model.PrivateType")
            .expect("function signature private reference exists")
        + "private: leaf_model.".len();
    let function_body_private_start = function_start
        + middle_source[function_start..]
            .find("body_private: leaf_model.PrivateType")
            .expect("function body private reference exists")
        + "body_private: leaf_model.".len();
    assert_eq!(
        private_diagnostics
            .iter()
            .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{function_private_start},")))
            .count(),
        1,
        "the function private member at byte {function_private_start} is reported exactly once: {stdout}"
    );
    let function_private = private_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{function_private_start},"))
        })
        .expect("the middle function private member diagnostic is present");
    assert!(
        function_private.contains("\"package\":\"acme:middle@1.0.0\"")
            && function_private.contains("\"what\":\"private member declared here\"")
            && function_private.contains(
                "\"path\":\"dep:acme:leaf@1.0.0/src/model.sai\",\"package\":\"acme:leaf@1.0.0\""
            ),
        "{function_private}"
    );
    assert_eq!(
        private_diagnostics
            .iter()
            .filter(|line| {
                line.contains(&format!(
                    "\"start\":{{\"byte\":{function_body_private_start},"
                ))
            })
            .count(),
        1,
        "the function body private member is reported exactly once: {stdout}"
    );
    let function_body_private = private_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!(
                    "\"start\":{{\"byte\":{function_body_private_start},"
                ))
        })
        .expect("the middle function body private member diagnostic is present");
    assert!(
        function_body_private.contains("\"package\":\"acme:middle@1.0.0\"")
            && function_body_private.contains("\"what\":\"private member declared here\"")
            && function_body_private.contains(
                "\"path\":\"dep:acme:leaf@1.0.0/src/model.sai\",\"package\":\"acme:leaf@1.0.0\""
            ),
        "{function_body_private}"
    );
    assert_eq!(
        private_diagnostics
            .iter()
            .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{const_private_start},")))
            .count(),
        1,
        "the const declared private type is reported exactly once: {stdout}"
    );
    let const_private = private_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{const_private_start},"))
        })
        .expect("the const declared private type diagnostic is present");
    assert!(
        const_private.contains("\"package\":\"acme:middle@1.0.0\"")
            && const_private.contains("\"what\":\"private member declared here\"")
            && const_private.contains(
                "\"path\":\"dep:acme:leaf@1.0.0/src/model.sai\",\"package\":\"acme:leaf@1.0.0\""
            ),
        "{const_private}"
    );
    let unresolved_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"UNRESOLVED_NAME\""))
        .collect::<Vec<_>>();
    assert_eq!(unresolved_diagnostics.len(), 25, "{stdout}");
    for (owner_name, owner_source) in [
        ("interface member", "fn middle_interface_member"),
        ("resource method", "fn middle_resource_member"),
    ] {
        let owner_start = middle_source
            .find(owner_source)
            .unwrap_or_else(|| panic!("{owner_name} source exists"));
        let owner_end = middle_source[owner_start..]
            .find('\n')
            .map_or(middle_source.len(), |index| owner_start + index);
        let owner_line = &middle_source[owner_start..owner_end];
        let private_start = owner_start
            + owner_line
                .find("PrivateType")
                .unwrap_or_else(|| panic!("{owner_name} has a private member path"));
        let missing_start = owner_start
            + owner_line
                .find("PublicTypo")
                .unwrap_or_else(|| panic!("{owner_name} has a missing member path"));
        let private = private_diagnostics
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{private_start},")))
            .unwrap_or_else(|| panic!("{owner_name} private path is reported: {stdout}"));
        assert!(
            private.contains("\"package\":\"acme:middle@1.0.0\"")
                && private.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && private.contains("\"what\":\"private member declared here\"")
                && private.contains(
                    "\"path\":\"dep:acme:leaf@1.0.0/src/model.sai\",\"package\":\"acme:leaf@1.0.0\""
                )
                && private.contains(&format!(
                    "\"start\":{{\"byte\":{declaration_start},\"line\":2"
                )),
            "{private}"
        );
        assert_eq!(
            private_diagnostics
                .iter()
                .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{private_start},")))
                .count(),
            1,
            "{owner_name} private path is reported exactly once: {stdout}"
        );
        let missing = unresolved_diagnostics
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{missing_start},")))
            .unwrap_or_else(|| panic!("{owner_name} missing path is reported: {stdout}"));
        assert!(
            missing.contains("\"package\":\"acme:middle@1.0.0\"")
                && missing.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && missing.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/model`")
                && missing.contains("\"candidates\":[\"PublicType\"]"),
            "{missing}"
        );
        assert_eq!(
            unresolved_diagnostics
                .iter()
                .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{missing_start},")))
                .count(),
            1,
            "{owner_name} missing path is reported exactly once: {stdout}"
        );
    }
    for (owner_name, owner_source) in [
        ("invariant", "invariant U32"),
        ("refined type", "type MiddleRefined"),
        ("refinement-bearing distinct", "distinct MiddleDistinct"),
    ] {
        let owner_start = middle_source
            .find(owner_source)
            .unwrap_or_else(|| panic!("{owner_name} source exists"));
        let owner_source = &middle_source[owner_start..];
        let private_start = owner_start
            + owner_source
                .find("PrivateType")
                .unwrap_or_else(|| panic!("{owner_name} has a private type path"));
        let missing_start = owner_start
            + owner_source
                .find("PublicTypo")
                .unwrap_or_else(|| panic!("{owner_name} has a missing type path"));
        assert_eq!(
            private_diagnostics
                .iter()
                .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{private_start},")))
                .count(),
            1,
            "{owner_name} private path is reported exactly once: {stdout}"
        );
        let private = private_diagnostics
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{private_start},")))
            .unwrap_or_else(|| panic!("{owner_name} private type diagnostic is present"));
        assert!(
            private.contains("\"package\":\"acme:middle@1.0.0\"")
                && private.contains("\"what\":\"private member declared here\"")
                && private.contains(
                    "\"path\":\"dep:acme:leaf@1.0.0/src/model.sai\",\"package\":\"acme:leaf@1.0.0\""
                ),
            "{private}"
        );
        assert_eq!(
            unresolved_diagnostics
                .iter()
                .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{missing_start},")))
                .count(),
            1,
            "{owner_name} missing path is reported exactly once: {stdout}"
        );
        let missing = unresolved_diagnostics
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{missing_start},")))
            .unwrap_or_else(|| panic!("{owner_name} missing type diagnostic is present"));
        assert!(
            missing.contains("\"package\":\"acme:middle@1.0.0\"")
                && missing.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/model`")
                && missing.contains("\"candidates\":[\"PublicType\"]"),
            "{missing}"
        );
    }
    let missing_start = middle_source
        .find("PublicTypo")
        .expect("missing reference exists");
    let missing = unresolved_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{missing_start},"))
        })
        .expect("missing member diagnostic is present");
    assert!(
        missing.contains("\"package\":\"acme:middle@1.0.0\"")
            && missing.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
            && missing.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/model`")
            && missing.contains("\"candidates\":[\"PublicType\"]")
            && missing.contains(&format!("\"start\":{{\"byte\":{missing_start},")),
        "{missing}"
    );
    let hidden = unresolved_diagnostics
        .iter()
        .find(|line| line.contains("PrivatType"))
        .expect("private lookalike miss is diagnosed");
    assert!(
        hidden.contains("\"candidates\":[]"),
        "private members of a dependency are not suggested: {hidden}"
    );
    let root_missing_start = root_source
        .find("PublicTypo")
        .expect("root missing reference exists");
    let root_missing = unresolved_diagnostics
        .iter()
        .find(|line| line.contains("\"path\":\"src/lib.sai\"") && line.contains("PublicTypo"))
        .expect("root transitive missing member diagnostic is present");
    assert!(
        root_missing.contains("\"package\":\"acme:check@0.1.0\"")
            && root_missing.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/api`")
            && root_missing
                .contains("\"candidates\":[\"PublicType\",\"PublicTypoC\",\"PublicTypoB\"]")
            && root_missing.contains(&format!("\"start\":{{\"byte\":{root_missing_start},")),
        "{root_missing}"
    );
    assert!(
        root_missing.contains("similar names: `PublicType`, `PublicTypoC`, `PublicTypoB`"),
        "the help child preserves the same ordered, capped candidates: {root_missing}"
    );
    for (owner_name, owner_source) in [
        ("invariant", "invariant RootInvariant"),
        ("refined type", "type RootRefined"),
        ("extern function", "transitive_extern"),
        ("trait header", "trait RootHeader"),
        (
            "impl header",
            "impl middle.api.PublicType for middle.api.PublicType",
        ),
        ("trait member", "fn root_trait_member"),
        ("impl member", "fn root_impl_member"),
        ("trait associated type", "type RootMissingAssociated"),
        ("impl associated type", "type RootMissingImplAssociated"),
        ("interface member", "fn root_interface_member"),
        ("interface resource method", "fn root_resource_member"),
        ("interface alias", "type RootAlias"),
        ("world import", "fn root_world_import"),
        ("world export", "fn root_world_export"),
    ] {
        let owner_start = root_source
            .find(owner_source)
            .unwrap_or_else(|| panic!("{owner_name} source exists"));
        let path_start = owner_start
            + root_source[owner_start..]
                .find("middle.api.PublicTypo")
                .unwrap_or_else(|| panic!("{owner_name} has a transitive missing path"))
            + "middle.api.".len();
        assert_eq!(
            unresolved_diagnostics
                .iter()
                .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{path_start},")))
                .count(),
            1,
            "{owner_name} transitive path is reported exactly once: {stdout}"
        );
        let diagnostic = unresolved_diagnostics
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{path_start},")))
            .unwrap_or_else(|| panic!("{owner_name} transitive path diagnostic is present"));
        assert!(
            diagnostic.contains("\"package\":\"acme:check@0.1.0\"")
                && diagnostic.contains("\"path\":\"src/lib.sai\"")
                && diagnostic.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/api`")
                && diagnostic
                    .contains("\"candidates\":[\"PublicType\",\"PublicTypoC\",\"PublicTypoB\"]"),
            "{diagnostic}"
        );
    }
    let function_missing_start = function_start
        + middle_source[function_start..]
            .find("missing: leaf_model.PublicTypo")
            .expect("function signature missing reference exists")
        + "missing: leaf_model.".len();
    assert_eq!(
        unresolved_diagnostics
            .iter()
            .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{function_missing_start},")))
            .count(),
        1,
        "the function missing member is reported exactly once: {stdout}"
    );
    let function_missing = unresolved_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{function_missing_start},"))
        })
        .expect("the middle function missing member diagnostic is present exactly once");
    assert!(
        function_missing.contains("\"package\":\"acme:middle@1.0.0\"")
            && function_missing.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/model`")
            && function_missing.contains("\"candidates\":[\"PublicType\"]"),
        "{function_missing}"
    );
    let function_body_missing_start = function_start
        + middle_source[function_start..]
            .find("body_missing: leaf_model.PublicTypo")
            .expect("function body missing reference exists")
        + "body_missing: leaf_model.".len();
    assert_eq!(
        unresolved_diagnostics
            .iter()
            .filter(|line| {
                line.contains(&format!(
                    "\"start\":{{\"byte\":{function_body_missing_start},"
                ))
            })
            .count(),
        1,
        "the function body missing member is reported exactly once: {stdout}"
    );
    let function_body_missing = unresolved_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!(
                    "\"start\":{{\"byte\":{function_body_missing_start},"
                ))
        })
        .expect("the middle function body missing member diagnostic is present");
    assert!(
        function_body_missing.contains("\"package\":\"acme:middle@1.0.0\"")
            && function_body_missing
                .contains("no item `PublicTypo` in module `acme:leaf@1.0.0/model`")
            && function_body_missing.contains("\"candidates\":[\"PublicType\"]"),
        "{function_body_missing}"
    );
    assert_eq!(
        unresolved_diagnostics
            .iter()
            .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{const_missing_start},")))
            .count(),
        1,
        "the const initializer missing type is reported exactly once: {stdout}"
    );
    let const_missing = unresolved_diagnostics
        .iter()
        .find(|line| {
            line.contains("\"path\":\"dep:acme:middle@1.0.0/src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{const_missing_start},"))
        })
        .expect("the const initializer missing type diagnostic is present");
    assert!(
        const_missing.contains("\"package\":\"acme:middle@1.0.0\"")
            && const_missing.contains("no item `PublicTypo` in module `acme:leaf@1.0.0/model`")
            && const_missing.contains("\"candidates\":[\"PublicType\"]"),
        "{const_missing}"
    );
    assert!(
        !stdout.contains("\"stage\":3,\"feature\":\"dependencies\""),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "the existing incomplete-analysis outcome remains in effect: {stdout}"
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

#[test]
fn locked_check_reports_one_root_wit_loader_parse_diagnostic_with_source_range() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world check\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/check.wasm\" }\n"
        ),
    )
    .expect("write artifact-only root manifest");
    fs::write(root.join("dist/check.wasm"), b"component").expect("write root component artifact");
    let malformed_wit = "package acme:check@0.1.0;\nworld check { # }\n";
    fs::write(root.join("wit/bad.wit"), malformed_wit).expect("write malformed root WIT source");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 0.1.0\n",
            "    targets { wasm32-wasi }\n"
        ),
    )
    .expect("write root-only format-2 lock");

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

    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI stdout is UTF-8");
    let wit_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_PARSE_ERROR\""))
        .collect::<Vec<_>>();
    assert_eq!(
        wit_diagnostics.len(),
        1,
        "the root WIT loader error should be emitted once: {stdout}"
    );
    let diagnostic = wit_diagnostics[0];
    let error_byte = malformed_wit
        .find('#')
        .expect("the malformed WIT token exists");
    assert!(
        diagnostic.contains("\"package\":\"acme:check@0.1.0\"")
            && diagnostic.contains("\"path\":\"wit/bad.wit\"")
            && diagnostic.contains(&format!("\"start\":{{\"byte\":{error_byte},"))
            && diagnostic.contains(&format!("\"end\":{{\"byte\":{},", error_byte + 1)),
        "{diagnostic}"
    );
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "successful check remains unavailable: {stdout}"
    );
}

#[test]
fn locked_check_reports_locationless_root_wit_resolve_failure_once() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 1.0.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world check\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/check.wasm\" }\n"
        ),
    )
    .expect("write artifact-only root manifest");
    fs::write(root.join("dist/check.wasm"), b"component").expect("write root component artifact");
    fs::write(
        root.join("wit/semantic.wit"),
        "package acme:check@1.0.0;\n@since(version = 2.0.0) interface api {}\n",
    )
    .expect("write WIT with a locationless Resolve::push failure");
    fs::write(
        root.join("sai.lock"),
        concat!(
            "lock 2\n",
            "\n",
            "root acme:check @ 1.0.0\n",
            "    targets { wasm32-wasi }\n"
        ),
    )
    .expect("write root-only format-2 lock");

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
    let wit_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_PARSE_ERROR\""))
        .collect::<Vec<_>>();
    assert_eq!(
        wit_diagnostics.len(),
        1,
        "the root Resolve::push failure should be emitted once: {stdout}"
    );
    assert!(
        wit_diagnostics[0].contains("\"package\":\"acme:check@1.0.0\"")
            && wit_diagnostics[0].contains("\"primary\":null"),
        "the locationless semantic failure keeps root ownership without a fabricated span: {}",
        wit_diagnostics[0]
    );
    assert!(
        stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\"")
            && !stdout.contains("\"type\":\"result\""),
        "semantic diagnostics do not imply full check success: {stdout}"
    );
}

#[test]
fn locked_check_reports_referenced_artifact_wit_failure_once_for_its_owner() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("wit")).expect("create root WIT directory");
    fs::create_dir_all(root.join("dist")).expect("create root artifact directory");
    fs::create_dir_all(dependency.join("wit")).expect("create dependency WIT directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");

    fs::write(
        root.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "wit \"wit\"\n",
            "export world app\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/check.wasm\" }\n",
            "requires { acme:dep @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write artifact-only root manifest");
    fs::write(
        root.join("wit/root.wit"),
        "package acme:check@0.1.0;\nworld app { import acme:dep/api; }\n",
    )
    .expect("write root WIT reference");
    fs::write(root.join("dist/check.wasm"), b"component").expect("write root component artifact");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "wit \"wit\"\n",
            "export world api\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/dep.wasm\" }\n"
        ),
    )
    .expect("write artifact dependency manifest");
    let malformed_wit = "package acme:dep@1.0.0;\ninterface api { # }\n";
    fs::write(dependency.join("wit/bad.wit"), malformed_wit)
        .expect("write malformed dependency WIT");
    fs::write(dependency.join("dist/dep.wasm"), b"component")
        .expect("write dependency component artifact");
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
    let wit_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_PARSE_ERROR\""))
        .collect::<Vec<_>>();
    assert_eq!(
        wit_diagnostics.len(),
        1,
        "the dependency failure propagated through root loading must be emitted once: {stdout}"
    );
    let diagnostic = wit_diagnostics[0];
    let error_byte = malformed_wit
        .find('#')
        .expect("the malformed WIT token exists");
    assert!(
        diagnostic.contains("\"package\":\"acme:dep@1.0.0\"")
            && diagnostic.contains("\"path\":\"dep:acme:dep@1.0.0/wit/bad.wit\"")
            && diagnostic.contains(&format!("\"start\":{{\"byte\":{error_byte},"))
            && diagnostic.contains(&format!("\"end\":{{\"byte\":{},", error_byte + 1))
            && !diagnostic.contains("\"package\":\"acme:check@0.1.0\""),
        "{diagnostic}"
    );
}

#[test]
fn locked_check_reports_wit_failure_from_source_selected_hybrid_dependency() {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let dependency = root.join("dep");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::create_dir_all(dependency.join("src")).expect("create hybrid source directory");
    fs::create_dir_all(dependency.join("wit")).expect("create hybrid WIT directory");
    fs::create_dir_all(dependency.join("dist")).expect("create hybrid artifact directory");

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
    .expect("write root source manifest");
    fs::write(root.join("src/lib.sai"), "pub fn root() {}\n").expect("write root source");
    fs::write(
        dependency.join("sai.package"),
        concat!(
            "package acme:dep @ 1.0.0\n",
            "source \"src\"\n",
            "wit \"wit\"\n",
            "export world api\n",
            "targets { wasm32-wasi }\n",
            "artifact component { wasm32-wasi \"dist/dep.wasm\" }\n"
        ),
    )
    .expect("write source-form hybrid manifest");
    fs::write(dependency.join("src/lib.sai"), "pub fn dep() {}\n").expect("write hybrid source");
    let malformed_wit = "package acme:dep@1.0.0;\ninterface api { # }\n";
    fs::write(dependency.join("wit/bad.wit"), malformed_wit).expect("write malformed hybrid WIT");
    fs::write(dependency.join("dist/dep.wasm"), b"component").expect("write hybrid artifact");
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
    let wit_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_PARSE_ERROR\""))
        .collect::<Vec<_>>();
    assert_eq!(
        wit_diagnostics.len(),
        1,
        "the source-selected hybrid WIT failure should be emitted once: {stdout}"
    );
    let diagnostic = wit_diagnostics[0];
    let error_byte = malformed_wit
        .find('#')
        .expect("the malformed WIT token exists");
    assert!(
        diagnostic.contains("\"package\":\"acme:dep@1.0.0\"")
            && diagnostic.contains("\"path\":\"dep:acme:dep@1.0.0/wit/bad.wit\"")
            && diagnostic.contains(&format!("\"start\":{{\"byte\":{error_byte},"))
            && diagnostic.contains(&format!("\"end\":{{\"byte\":{},", error_byte + 1)),
        "{diagnostic}"
    );
    assert!(
        !stdout.contains("\"type\":\"result\""),
        "incomplete check does not produce a success record: {stdout}"
    );
}
