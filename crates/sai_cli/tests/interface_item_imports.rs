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

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
#[test]
fn check_reports_non_interface_impl_targets_but_accepts_wit_view_interfaces() {
    let directory = TestDirectory::new();
    let package = directory.path().join("package");
    let dependency = package.join("dep");
    let source_directory = package.join("src");
    let wit_directory = dependency.join("wit");
    fs::create_dir_all(&source_directory).expect("create package source directory");
    fs::create_dir_all(&wit_directory).expect("create dependency WIT directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");
    fs::write(
        package.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:codec @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write package manifest");
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
    .expect("write WIT dependency manifest");
    fs::write(
        wit_directory.join("api.wit"),
        "package acme:codec@1.0.0;\ninterface api { run: func(); }\nworld host { export api; }\n",
    )
    .expect("write WIT view interface");
    fs::write(dependency.join("dist/codec.wasm"), b"component")
        .expect("write dependency component artifact");
    fs::write(
        package.join("sai.lock"),
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
    let source = concat!(
        "use acme:codec/api as api\n",
        "interface local_api {\n",
        "  fn run()\n",
        "  fn stop()\n",
        "}\n",
        "fn not_interface() {}\n",
        "impl api { fn run() {} }\n",
        "impl api {}\n",
        "impl api.run {}\n",
        "impl local_api { fn run() {} fn run() {} fn extra() {} }\n",
        "impl not_interface {}\n",
        "impl missing {}\n",
    );
    fs::write(source_directory.join("lib.sai"), source)
        .expect("write interface implementation targets");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args([
            "--format=json",
            "check",
            "--target",
            "wasm32-wasi",
            "--locked",
            "--offline",
        ])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"IMPL_TARGET_NOT_INTERFACE\""))
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{stdout}");
    for target in ["api.run", "not_interface"] {
        let start = source
            .find(&format!("impl {target}"))
            .expect("non-interface implementation target exists")
            + "impl ".len();
        let end = start + target.len();
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.contains(&format!("\"start\":{{\"byte\":{start},"))
                    && diagnostic.contains(&format!("\"end\":{{\"byte\":{end},"))
            }),
            "expected a full-path diagnostic for {target} in {stdout}"
        );
    }
    assert!(
        !stdout.contains("\"code\":\"WRONG_NAME_KIND\""),
        "the target-kind diagnostic replaces WRONG_NAME_KIND instead of duplicating it: {stdout}"
    );
    let repeated_and_extra = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"INTERFACE_IMPL_EXTRA_ITEM\""))
        .collect::<Vec<_>>();
    assert_eq!(repeated_and_extra.len(), 2, "{stdout}");
    let repeated_run_start = source.match_indices("run").nth(3).unwrap().0;
    assert!(
        repeated_and_extra
            .iter()
            .any(|line| line.contains(&format!("\"start\":{{\"byte\":{repeated_run_start},"))),
        "repeated item reports its later name: {stdout}"
    );
    let extra_start = source.find("extra").expect("local extra item exists");
    assert!(
        repeated_and_extra
            .iter()
            .any(|line| line.contains(&format!("\"start\":{{\"byte\":{extra_start},"))),
        "extra item reports its name: {stdout}"
    );
    let missing = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"INTERFACE_IMPL_MISSING_ITEM\""))
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 1, "{stdout}");
    let local_target_start = source.find("impl local_api").unwrap() + "impl ".len();
    assert!(
        missing[0].contains(&format!("\"start\":{{\"byte\":{local_target_start},")),
        "missing item points at its implementation target: {}",
        missing[0]
    );
    let duplicate = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"DUPLICATE_INTERFACE_IMPL\""))
        .collect::<Vec<_>>();
    assert_eq!(duplicate.len(), 1, "{stdout}");
    let second_api_target = source.match_indices("impl api").nth(1).unwrap().0 + "impl ".len();
    assert!(
        duplicate[0].contains(&format!("\"start\":{{\"byte\":{second_api_target},")),
        "duplicate target points at the later WIT-view target: {}",
        duplicate[0]
    );
    assert!(
        diagnostics[0].contains("\"path\":\"src/lib.sai\"")
            && diagnostics[0].contains("\"package\":\"acme:check@0.1.0\""),
        "{}",
        diagnostics[0]
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\"")
            && stdout.contains("\"type\":\"summary\""),
        "{stdout}"
    );
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
#[test]
fn check_rejects_a_wit_resource_used_by_a_visible_record() {
    let directory = TestDirectory::new();
    let package = directory.path().join("package");
    let dependency = package.join("dep");
    let source_directory = package.join("src");
    let wit_directory = dependency.join("wit");
    fs::create_dir_all(&source_directory).expect("create package source directory");
    fs::create_dir_all(&wit_directory).expect("create dependency WIT directory");
    fs::create_dir_all(dependency.join("dist")).expect("create dependency artifact directory");
    fs::write(
        package.join("sai.package"),
        concat!(
            "package acme:check @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n",
            "targets { wasm32-wasi }\n",
            "requires { acme:codec @ ^1 from path \"dep\" }\n"
        ),
    )
    .expect("write package manifest");
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
    .expect("write WIT dependency manifest");
    fs::write(
        wit_directory.join("api.wit"),
        concat!(
            "package acme:codec@1.0.0;\n",
            "interface api {\n",
            "  resource buffer {}\n",
            "  record envelope { value: own<buffer> }\n",
            "}\n",
            "world host { export api; }\n"
        ),
    )
    .expect("write WIT resource and record");
    fs::write(dependency.join("dist/codec.wasm"), b"component")
        .expect("write dependency component artifact");
    fs::write(
        package.join("sai.lock"),
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
    let source = concat!(
        "use acme:codec/api as api\n",
        "resource Concrete {}\n",
        "impl api { type Buffer = Concrete }\n",
    );
    fs::write(source_directory.join("lib.sai"), source).expect("write concrete resource binding");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args([
            "--format=json",
            "check",
            "--target",
            "wasm32-wasi",
            "--locked",
            "--offline",
        ])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let binding_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"INVALID_RESOURCE_BINDING\""))
        .collect::<Vec<_>>();
    assert_eq!(binding_diagnostics.len(), 1, "{stdout}");
    let binding = binding_diagnostics[0];
    let binding_start = source.rfind("Concrete").expect("binding target is present");
    let binding_end = binding_start + "Concrete".len();
    assert!(
        binding.contains(&format!(
            "\"primary\":{{\"file\":{{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null}},\"start\":{{\"byte\":{binding_start},"
        )),
        "{binding}"
    );
    assert!(
        binding.contains(&format!("\"end\":{{\"byte\":{binding_end},")),
        "{binding}"
    );
    assert!(
        binding.contains("\"kind\":\"note\"")
            && binding.contains("envelope")
            && binding.contains("record field `value`")
            && binding.contains("acme:codec@1.0.0"),
        "the note identifies the first WIT occurrence:\n{binding}"
    );
    assert!(
        !stdout.contains("\"code\":\"INTERFACE_IMPL_MISSING_ITEM\""),
        "a rejected binding still matched the interface item:\n{stdout}"
    );
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
fn check_reports_interface_wit_name_collisions_only_within_each_scope() {
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
    let source = concat!(
        "pub interface api {\n",
        "    type FetchUser = U32\n",
        "    type Fetch__User = U32\n",
        "    fn fetch_user()\n",
        "    type FirstRecord { value: U32 }\n",
        "    type SecondRecord { value: U32 }\n",
        "    enum FirstState { Ready }\n",
        "    enum SecondState { Ready }\n",
        "    fn first(value: U32)\n",
        "    fn second(value: U32)\n",
        "    resource FirstResource {\n",
        "        fn get(self: borrow Self)\n",
        "    }\n",
        "    fn first_resource()\n",
        "    resource SecondResource {\n",
        "        fn get(self: borrow Self)\n",
        "    }\n",
        "}\n",
    );
    fs::write(source_directory.join("lib.sai"), source)
        .expect("write interface with scoped WIT names");

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
    assert_eq!(collisions.len(), 2, "{stdout}");

    let earlier_start = source
        .find("FetchUser")
        .expect("the earlier type name is present");
    let earlier_end = earlier_start + "FetchUser".len();
    let later_start = source
        .find("fetch_user")
        .expect("the later function name is present");
    let later_end = later_start + "fetch_user".len();
    assert!(
        collisions[0].contains(&format!(
            "\"primary\":{{\"file\":{{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null}},\"start\":{{\"byte\":{later_start},"
        )),
        "the collision primary should point to the later function name: {}",
        collisions[0]
    );
    assert!(
        collisions[0].contains(&format!("\"end\":{{\"byte\":{later_end},")),
        "the collision primary should span the complete function name: {}",
        collisions[0]
    );
    assert!(
        collisions[0].contains("\"what\":\"earlier declaration\""),
        "{}",
        collisions[0]
    );
    let related = collisions[0]
        .split("\"related\":[")
        .nth(1)
        .expect("the collision has a related earlier declaration");
    assert!(
        related.starts_with(
            "{\"kind\":\"declared_here\",\"location\":{\"file\":{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null},"
        ),
        "{related}"
    );
    assert!(
        related.contains(&format!("\"start\":{{\"byte\":{earlier_start},")),
        "the related location should start at the earlier type name: {related}"
    );
    assert!(
        related.contains(&format!("\"end\":{{\"byte\":{earlier_end},")),
        "the related location should span the complete type name: {related}"
    );
    let resource_primary_start = source
        .find("first_resource")
        .expect("the later function is present");
    let resource_primary_end = resource_primary_start + "first_resource".len();
    assert!(
        collisions[1].contains(&format!(
            "\"primary\":{{\"file\":{{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null}},\"start\":{{\"byte\":{resource_primary_start},"
        )),
        "the resource/function collision should point to the later function: {}",
        collisions[1]
    );
    assert!(
        collisions[1].contains(&format!("\"end\":{{\"byte\":{resource_primary_end},")),
        "the resource/function collision should span the full function name: {}",
        collisions[1]
    );
    let resource_related = collisions[1]
        .split("\"related\":[")
        .nth(1)
        .expect("the resource/function collision has an earlier location");
    let resource_earlier_start = source
        .find("FirstResource")
        .expect("the earlier resource is present");
    let resource_earlier_end = resource_earlier_start + "FirstResource".len();
    assert!(
        resource_related.starts_with(
            "{\"kind\":\"declared_here\",\"location\":{\"file\":{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null},"
        ),
        "{resource_related}"
    );
    assert!(
        resource_related.contains(&format!("\"start\":{{\"byte\":{resource_earlier_start},")),
        "the related location should start at the resource name: {resource_related}"
    );
    assert!(
        resource_related.contains(&format!("\"end\":{{\"byte\":{resource_earlier_end},")),
        "the related location should span the full resource name: {resource_related}"
    );

    let unrepresentable = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_NAME_NOT_REPRESENTABLE\""))
        .collect::<Vec<_>>();
    assert_eq!(unrepresentable.len(), 1, "{stdout}");
    let unrepresentable_start = source
        .find("Fetch__User")
        .expect("the unrepresentable type name is present");
    let unrepresentable_end = unrepresentable_start + "Fetch__User".len();
    assert!(
        unrepresentable[0].contains(&format!("\"start\":{{\"byte\":{unrepresentable_start},")),
        "the unrepresentable name diagnostic should point to Fetch__User: {}",
        unrepresentable[0]
    );
    assert!(
        unrepresentable[0].contains(&format!("\"end\":{{\"byte\":{unrepresentable_end},")),
        "the unrepresentable name diagnostic should span Fetch__User: {}",
        unrepresentable[0]
    );

    for code in [
        "DUPLICATE_BINDING",
        "EXPECTED_TOKEN",
        "INTERNAL_COMPILER_ERROR",
    ] {
        assert!(
            !stdout.contains(&format!("\"code\":\"{code}\"")),
            "{code} should not be reported:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":4,"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
}

#[test]
fn check_reports_unrepresentable_interface_names_and_accepts_raw_keyword_names() {
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
    let source = concat!(
        "pub interface api {\n",
        "  type User_Id = U32\n",
        "  type Record { bad__field: U32 }\n",
        "  enum Kind { Bad_Case }\n",
        "  fn run__task(value__name: U32, _: U32)\n",
        "  resource Handle {\n",
        "    fn get__value(self: borrow Self, input__value: U32)\n",
        "  }\n",
        "  fn r#type()\n",
        "}\n",
    );
    fs::write(source_directory.join("lib.sai"), source)
        .expect("write interface with boundary names");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let name_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_NAME_NOT_REPRESENTABLE\""))
        .collect::<Vec<_>>();
    assert_eq!(name_diagnostics.len(), 8, "{stdout}");

    for (name, suggestion) in [
        ("User_Id", "UserId"),
        ("bad__field", "badfield"),
        ("Bad_Case", "BadCase"),
        ("run__task", "runtask"),
        ("value__name", "valuename"),
        ("get__value", "getvalue"),
        ("input__value", "inputvalue"),
    ] {
        let diagnostic = name_diagnostics
            .iter()
            .find(|line| line.contains(&format!("\"suggestion\":\"{suggestion}\"")))
            .unwrap_or_else(|| panic!("missing suggestion for {name}:\n{stdout}"));
        let start = source
            .find(name)
            .unwrap_or_else(|| panic!("fixture name {name} is present"));
        let end = start + name.len();
        assert!(
            diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
            "diagnostic for {name} should start at byte {start}: {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
            "diagnostic for {name} should end at byte {end}: {diagnostic}"
        );
        assert!(
            diagnostic.contains("\"path\":\"src/lib.sai\""),
            "diagnostic for {name} should point to the package source: {diagnostic}"
        );
        assert!(
            diagnostic.contains("\"package\":\"acme:check@0.1.0\""),
            "diagnostic for {name} should carry its package: {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!(
                "\"kind\":\"help\",\"message\":\"try `{suggestion}`\""
            )),
            "diagnostic for {name} should include its help suggestion: {diagnostic}"
        );
    }

    let unnamed_parameter = "_: U32";
    let unnamed_start = source
        .find(unnamed_parameter)
        .expect("the unnamed parameter is present");
    let unnamed_end = unnamed_start + unnamed_parameter.len();
    let unnamed_diagnostic = name_diagnostics
        .iter()
        .find(|line| {
            line.contains(&format!("\"start\":{{\"byte\":{unnamed_start},"))
                && line.contains(&format!("\"end\":{{\"byte\":{unnamed_end},"))
        })
        .expect("the unnamed parameter diagnostic spans the whole parameter");
    assert!(
        unnamed_diagnostic.contains("\"children\":[]"),
        "an unnamed parameter has no name suggestion: {unnamed_diagnostic}"
    );

    let self_start = source
        .find("self: borrow Self")
        .expect("the resource receiver is present");
    assert!(
        name_diagnostics
            .iter()
            .all(|line| { !line.contains(&format!("\"start\":{{\"byte\":{self_start},")) }),
        "the resource receiver `self` must not get a name diagnostic: {name_diagnostics:?}"
    );
    let raw_keyword_start = source.find("r#type").expect("raw keyword name is present");
    assert!(
        name_diagnostics
            .iter()
            .all(|line| { !line.contains(&format!("\"start\":{{\"byte\":{raw_keyword_start},")) }),
        "the raw keyword `r#type` has a representable WIT name: {name_diagnostics:?}"
    );
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":9,"),
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
    let source = concat!(
        "pub interface api {\n",
        "    type EmptyRecord {}\n",
        "    enum EmptyEnum {}\n",
        "    type NonEmptyRecord { value: U32 }\n",
        "    enum NonEmptyEnum { Ready }\n",
        "}\n"
    );
    fs::write(source_directory.join("lib.sai"), source)
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

#[test]
fn check_reports_generic_interface_restrictions_and_type_parameter_boundaries() {
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
    let source = concat!(
        "pub interface api {\n",
        "  type Box<T> where T: Copy { value: T }\n",
        "  enum Choice<T> where T: Copy { Ready(T) }\n",
        "  type Refined<T> = U32 where true\n",
        "  fn invoke<T>() where T: Copy\n",
        "  resource Handle {\n",
        "    fn method<T>() where T: Copy\n",
        "  }\n",
        "  type Ordinary { value: U32 }\n",
        "  fn plain()\n",
        "}\n",
    );
    fs::write(source_directory.join("lib.sai"), source).expect("write generic interface items");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let generic_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"GENERIC_INTERFACE_ITEM\""))
        .collect::<Vec<_>>();
    assert_eq!(generic_diagnostics.len(), 9, "{stdout}");
    assert!(
        generic_diagnostics.iter().all(|line| line.contains(
            "\"message\":\"WIT interface items cannot have generic parameters or a `where` clause\""
        )),
        "{generic_diagnostics:?}"
    );

    let expected_spans = [
        "<T>",
        "where T: Copy",
        "<T>",
        "where T: Copy",
        "<T>",
        "<T>",
        "where T: Copy",
        "<T>",
        "where T: Copy",
    ];
    let mut search_from = 0;
    for (diagnostic, text) in generic_diagnostics.iter().zip(expected_spans) {
        let start = source[search_from..]
            .find(text)
            .map(|relative| search_from + relative)
            .expect("expected generic syntax appears after the previous span");
        let end = start + text.len();
        assert!(
            diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
            "diagnostic should start at {text:?} ({start}): {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
            "diagnostic should end at {text:?} ({end}): {diagnostic}"
        );
        search_from = end;
    }
    let incompatible_diagnostics = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_INCOMPATIBLE_TYPE\""))
        .collect::<Vec<_>>();
    assert_eq!(incompatible_diagnostics.len(), 3, "{stdout}");
    let expected_incompatible_spans = [
        ("value: T", 7, 1),
        ("Ready(T)", 6, 1),
        ("where true", 0, "where true".len()),
    ];
    let mut source_search_from = 0;
    for (diagnostic, (anchor, span_offset, span_length)) in incompatible_diagnostics
        .iter()
        .zip(expected_incompatible_spans)
    {
        let anchor_start = source[source_search_from..]
            .find(anchor)
            .map(|relative| source_search_from + relative)
            .expect("expected incompatible type occurs after the previous span");
        let start = anchor_start + span_offset;
        let end = start + span_length;
        assert!(
            diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
            "diagnostic should start at {anchor:?} ({start}): {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
            "diagnostic should end at {anchor:?} ({end}): {diagnostic}"
        );
        source_search_from = anchor_start + anchor.len();
    }
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":13,"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
}

#[test]
fn check_reports_wit_discriminants_and_payload_shapes_at_full_spans() {
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
    let source = concat!(
        "pub interface api {\n",
        "  enum Mixed {\n",
        "    Pair(U32, Bool)\n",
        "    Record(code: U32)\n",
        "    Plain = 7\n",
        "    Huge = 340282366920938463463374607431768211456\n",
        "    Single(U32)\n",
        "    Nested((U32, Bool))\n",
        "  }\n",
        "  @flags\n",
        "  enum Flags {\n",
        "    First = 1\n",
        "    Second = 2\n",
        "  }\n",
        "}\n",
    );
    fs::write(source_directory.join("lib.sai"), source).expect("write interface enum restrictions");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let diagnostics_for = |code: &str| {
        stdout
            .lines()
            .filter(|line| line.contains(&format!("\"code\":\"{code}\"")))
            .collect::<Vec<_>>()
    };
    let explicit_discriminants = diagnostics_for("WIT_EXPLICIT_DISCRIMINANT");
    assert_eq!(explicit_discriminants.len(), 4, "{stdout}");
    let incompatible_payloads = diagnostics_for("WIT_INCOMPATIBLE_TYPE");
    assert_eq!(incompatible_payloads.len(), 2, "{stdout}");
    assert_eq!(
        diagnostics_for("EXPLICIT_DISCRIMINANT_ON_DATA_ENUM").len(),
        1,
        "{stdout}"
    );

    let span_texts = [
        (incompatible_payloads[0], "(U32, Bool)"),
        (incompatible_payloads[1], "(code: U32)"),
        (explicit_discriminants[0], "Plain = 7"),
        (
            explicit_discriminants[1],
            "Huge = 340282366920938463463374607431768211456",
        ),
        (explicit_discriminants[2], "First = 1"),
        (explicit_discriminants[3], "Second = 2"),
    ];
    for (diagnostic, text) in span_texts {
        let start = source
            .find(text)
            .expect("expected enum restriction occurs in source");
        let end = start + text.len();
        assert!(
            diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
            "diagnostic should start at {text:?} ({start}): {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
            "diagnostic should end at {text:?} ({end}): {diagnostic}"
        );
    }
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":9,"),
        "{stdout}"
    );
}

#[test]
fn check_reports_incompatible_interface_boundary_types_without_payload_cascades() {
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
    let source = concat!(
        "pub interface api {\n",
        "  type Option { value: U32 }\n",
        "  fn use_option(value: Option) -> Option\n",
        "  type Record { field: USize }\n",
        "  type Alias = USize\n",
        "  enum Payload {\n",
        "    Single(USize)\n",
        "    Pair(USize, USize)\n",
        "    Fields(value: USize)\n",
        "  }\n",
        "  fn submit(input: USize) -> USize\n",
        "  resource Handle {\n",
        "    fn adjust(self: borrow Self, amount: USize) -> USize\n",
        "  }\n",
        "  fn generic<T: USize>(input: U32)\n",
        "}\n",
    );
    fs::write(source_directory.join("lib.sai"), source)
        .expect("write interface with incompatible boundary types");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let incompatible = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_INCOMPATIBLE_TYPE\""))
        .collect::<Vec<_>>();
    assert_eq!(incompatible.len(), 9, "{stdout}");
    let expected_spans = [
        "USize",
        "USize",
        "USize",
        "(USize, USize)",
        "(value: USize)",
        "USize",
        "USize",
        "USize",
        "USize",
    ];
    let mut source_search_from = 0;
    for (diagnostic, text) in incompatible.iter().zip(expected_spans) {
        let relative_start = source[source_search_from..]
            .find(text)
            .unwrap_or_else(|| panic!("expected boundary span {text:?} in source"));
        let start = source_search_from + relative_start;
        let end = start + text.len();
        assert!(
            diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
            "diagnostic should start at {text:?} ({start}): {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
            "diagnostic should end at {text:?} ({end}): {diagnostic}"
        );
        source_search_from = end;
    }
    assert!(
        stdout.contains("\"wit_incompatibility\":\"pointer_sized_integer\""),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"wit_position\":\"nested type\""),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"wit_position\":\"parameter\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"wit_position\":\"result\""), "{stdout}");
    assert!(
        stdout.contains("\"stage\":0,\"feature\":\"sai check\""),
        "{stdout}"
    );
}

#[test]
fn check_expands_local_module_aliases_at_the_interface_use_span() {
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
        source_directory.join("types.sai"),
        "pub type ModuleSize = USize\n",
    )
    .expect("write external module alias");
    let source = concat!(
        "use acme:check/types.{ModuleSize}\n",
        "pub interface api {\n",
        "  fn send(value: ModuleSize)\n",
        "}\n"
    );
    fs::write(source_directory.join("lib.sai"), source)
        .expect("write interface using module alias");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let incompatible = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_INCOMPATIBLE_TYPE\""))
        .collect::<Vec<_>>();
    assert_eq!(incompatible.len(), 1, "{stdout}");
    let diagnostic = incompatible[0];
    let start = source
        .rfind("ModuleSize")
        .expect("the interface parameter uses the alias");
    let end = start + "ModuleSize".len();
    assert!(
        diagnostic.contains("\"path\":\"src/lib.sai\"")
            && diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("\"wit_incompatibility\":\"pointer_sized_integer\"")
            && diagnostic.contains("\"wit_position\":\"parameter\""),
        "{diagnostic}"
    );
}

#[test]
fn check_reports_interface_repr_at_the_full_attribute_span_once() {
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
    let source = concat!(
        "pub interface api {\n",
        "  @repr(C)\n",
        "  type Record { value: U32 }\n",
        "  @repr(C)\n",
        "  enum Kind { Ready }\n",
        "}\n",
    );
    fs::write(source_directory.join("lib.sai"), source).expect("write interface types");

    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(package)
        .args(["--format=json", "check"])
        .output()
        .expect("run sai check");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
    let incompatible = stdout
        .lines()
        .filter(|line| line.contains("\"code\":\"WIT_INCOMPATIBLE_TYPE\""))
        .collect::<Vec<_>>();
    assert_eq!(incompatible.len(), 2, "{stdout}");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""))
            .count(),
        1,
        "{stdout}"
    );
    let repr_spans = source.match_indices("@repr(C)").collect::<Vec<_>>();
    assert_eq!(repr_spans.len(), 2);
    for ((diagnostic, (start, text)), expected) in incompatible
        .iter()
        .zip(repr_spans)
        .zip(["@repr(C)", "@repr(C)"])
    {
        assert_eq!(text, expected);
        let end = start + text.len();
        assert!(
            diagnostic.contains(&format!("\"start\":{{\"byte\":{start},")),
            "diagnostic should start at the full {text:?} attribute: {diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("\"end\":{{\"byte\":{end},")),
            "diagnostic should end at the full {text:?} attribute: {diagnostic}"
        );
    }
    assert!(
        stdout.contains("\"type\":\"summary\",\"errors\":3,"),
        "{stdout}"
    );
}
