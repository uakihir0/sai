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
    fs::write(
        middle.join("src/lib.sai"),
        concat!(
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
            "const MIDDLE_CONST: leaf_model.PrivateType = { let nested: leaf_model.PublicTypo = 0; 0 }\n",
            "fn middle_signature(private: leaf_model.PrivateType, missing: leaf_model.PublicTypo) { let body_private: leaf_model.PrivateType = 0; let body_missing: leaf_model.PublicTypo = 0; }\n",
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
        ),
    )
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
        "const MIDDLE_CONST: leaf_model.PrivateType = { let nested: leaf_model.PublicTypo = 0; 0 }\n",
        "fn middle_signature(private: leaf_model.PrivateType, missing: leaf_model.PublicTypo) { let body_private: leaf_model.PrivateType = 0; let body_missing: leaf_model.PublicTypo = 0; }\n",
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
        .find('\n')
        .expect("const declaration has a line ending")
        + const_start;
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
    let function_end = middle_source[function_start..]
        .find('\n')
        .expect("function line has a line ending")
        + function_start;
    let function_line = &middle_source[function_start..function_end];
    let body_start_in_function = function_line
        .find('{')
        .expect("function body starts on the same line");
    let function_signature = &function_line[..body_start_in_function];
    let function_body = &function_line[body_start_in_function..];
    let function_private_start = function_start
        + function_signature
            .rfind("PrivateType")
            .expect("function signature private reference exists");
    let function_body_private_start = function_start
        + body_start_in_function
        + function_body
            .rfind("PrivateType")
            .expect("function body private reference exists");
    assert_eq!(
        private_diagnostics
            .iter()
            .filter(|line| line.contains(&format!("\"start\":{{\"byte\":{function_private_start},")))
            .count(),
        1,
        "the function private member is reported exactly once: {stdout}"
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
        + function_signature
            .rfind("PublicTypo")
            .expect("function signature missing reference exists");
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
        + body_start_in_function
        + function_body
            .rfind("PublicTypo")
            .expect("function body missing reference exists");
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
