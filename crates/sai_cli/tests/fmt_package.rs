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
            "sai-cli-fmt-package-{}-{unique}",
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
fn fmt_check_distinguishes_empty_and_whitespace_only_packages() {
    let directory = TestDirectory::new();
    let manifest = directory.0.join("sai.package");
    for (source, expected_exit, expected_changed) in [
        (b"".as_slice(), 0, false),
        (b" \t\r\n\n ", 1, true),
        (b"\xef\xbb\xbf", 1, true),
        (b"\xef\xbb\xbf\t\r\n", 1, true),
    ] {
        fs::write(&manifest, source).expect("write package manifest");
        let output = Command::new(env!("CARGO_BIN_EXE_sai"))
            .current_dir(&directory.0)
            .args(["--format=json", "fmt", "--check", "sai.package"])
            .output()
            .expect("run sai fmt --check");

        assert_eq!(output.status.code(), Some(expected_exit), "{source:?}");
        assert!(output.stderr.is_empty(), "{source:?}");
        assert_eq!(fs::read(&manifest).unwrap(), source);
        let stdout = String::from_utf8(output.stdout).expect("CLI output is UTF-8");
        assert_eq!(
            stdout.contains("\"code\":\"UNFORMATTED_FILE\""),
            expected_changed
        );
        assert!(
            stdout.contains(if expected_changed {
                "\"files\":1,\"changed\":[\"sai.package\"],\"refused\":[]"
            } else {
                "\"files\":1,\"changed\":[],\"refused\":[]"
            }),
            "{stdout}"
        );
    }
}
