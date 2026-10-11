use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sai-cli-integration-{}-{unique}",
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
fn binary_creates_a_package_in_the_current_directory() {
    let directory = TestDirectory::new();
    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(directory.path())
        .args(["new", ".", "--name", "acme:dot"])
        .output()
        .expect("run sai binary");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("CLI output is UTF-8");
    let lines = stderr.lines().collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            "wrote sai.package (77 bytes)",
            "wrote .gitignore (13 bytes)",
            "wrote src/lib.sai (107 bytes)"
        ]
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("sai.package")).unwrap(),
        concat!(
            "package acme:dot @ 0.1.0\n",
            "compiler ^0.1\n",
            "source \"src\"\n\n",
            "targets { wasm32-wasi }\n"
        )
    );

    let root_entries = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(root_entries.len(), 3);
    assert!(
        root_entries
            .iter()
            .all(|entry| { !entry.to_string_lossy().starts_with(".sai-new-") })
    );
}
