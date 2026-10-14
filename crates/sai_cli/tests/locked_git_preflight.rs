use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use gix::objs::tree::{Entry, EntryKind};
use sai_package::git_tree::GitTreeReader;
use sai_package::package_cache::PackageCache;

static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sai-cli-locked-git-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}

fn remove_tree(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        make_writable(path);
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                remove_tree(&entry.path());
            }
        }
        let _ = fs::remove_dir(path);
    } else {
        make_writable(path);
        let _ = fs::remove_file(path);
    }
}

fn make_writable(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}

struct LockedGitFixture {
    _temporary: TestDirectory,
    root: PathBuf,
    cache: PathBuf,
    cached_manifest: PathBuf,
    commit: String,
}

fn write_tree(
    repository: &gix::Repository,
    entries: impl IntoIterator<Item = (&'static [u8], EntryKind, gix::ObjectId)>,
) -> gix::ObjectId {
    let mut entries = entries
        .into_iter()
        .map(|(name, kind, oid)| Entry {
            mode: kind.into(),
            filename: name.to_vec().into(),
            oid,
        })
        .collect::<Vec<_>>();
    entries.sort();
    repository
        .write_object(gix::objs::Tree { entries })
        .expect("write Git fixture tree")
        .detach()
}

fn git_source_tree(
    repository_path: &Path,
    manifest: &[u8],
    source: &[u8],
) -> (gix::ObjectId, sai_package::git_tree::GitPackageSourceTree) {
    fs::create_dir_all(repository_path).expect("create bare Git fixture directory");
    let repository: gix::Repository = gix::ThreadSafeRepository::init(
        repository_path,
        gix::create::Kind::Bare,
        Default::default(),
    )
    .expect("initialize bare Git fixture")
    .into();
    let manifest = repository
        .write_blob(manifest)
        .expect("write package manifest blob")
        .detach();
    let source = repository
        .write_blob(source)
        .expect("write source blob")
        .detach();
    let source_tree = write_tree(
        &repository,
        [(b"lib.sai".as_slice(), EntryKind::Blob, source)],
    );
    let root_tree = write_tree(
        &repository,
        [
            (b"sai.package".as_slice(), EntryKind::Blob, manifest),
            (b"src".as_slice(), EntryKind::Tree, source_tree),
        ],
    );
    let commit = repository
        .write_object(gix::objs::Commit {
            tree: root_tree,
            parents: Default::default(),
            author: gix::actor::Signature::default(),
            committer: gix::actor::Signature::default(),
            encoding: None,
            message: "locked preflight fixture".into(),
            extra_headers: Vec::new(),
        })
        .expect("write Git fixture commit")
        .detach();
    let source_tree =
        GitTreeReader::open(repository_path.join("objects"), repository.object_hash())
            .expect("open Git fixture object database")
            .read_commit_package_source_tree(&commit, &[])
            .expect("read Git fixture package source tree");
    (commit, source_tree)
}

fn locked_git_fixture(seed_cache: bool, corrupt_cache: bool) -> LockedGitFixture {
    let temporary = TestDirectory::new();
    let root = temporary.0.join("project");
    let repository = temporary.0.join("dependency-repository.git");
    let cache = temporary.0.join("cache");
    fs::create_dir_all(root.join("src")).expect("create root source directory");
    fs::write(
        root.join("sai.package"),
        "package acme:check @ 0.1.0\nsource \"src\"\ntargets { wasm32-wasi }\nrequires { acme:dep @ ^1 from git \"https://example.com/acme/dep\" }\n",
    )
    .expect("write root manifest");
    fs::write(root.join("src/lib.sai"), "fn {\n").expect("write malformed root source");
    let (commit, source) = git_source_tree(
        &repository,
        b"package acme:dep @ 1.0.0\ncompiler ^0.2\nsource \"src\"\ntargets { wasm32-wasi }\nrequires { acme:extra @ ^1 from git \"https://example.com/acme/extra\" }\n",
        b"pub fn dep() {}\n",
    );
    let commit = commit.to_hex().to_string();
    let checksum = source.source_tree().checksum.clone();
    let cached_manifest = if seed_cache {
        let cache_store = PackageCache::open(&cache).expect("open package cache");
        let cached = cache_store
            .materialize_source(source)
            .expect("materialize verified Git source");
        let manifest = cached.path().join("sai.package");
        if corrupt_cache {
            let mut permissions = fs::metadata(&manifest)
                .expect("inspect cached dependency manifest")
                .permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&manifest, permissions)
                .expect("make cached dependency manifest writable");
            fs::write(&manifest, b"corrupt cache contents\n")
                .expect("corrupt cached dependency manifest");
        }
        manifest
    } else {
        root.join("missing-cache-entry")
    };
    fs::write(
        root.join("sai.lock"),
        format!(
            "lock 2\n\nroot acme:check @ 0.1.0\n    targets {{ wasm32-wasi }}\n    requires acme:dep @ ^1 -> 1.0.0 from git \"https://example.com/acme/dep\"\n\npackage acme:dep @ 1.0.0\n    source git \"https://example.com/acme/dep\"\n    commit \"{commit}\"\n    checksum \"{checksum}\"\n    capabilities {{ }}\n"
        ),
    )
    .expect("write complete format-2 Git lock");

    LockedGitFixture {
        _temporary: temporary,
        root,
        cache,
        cached_manifest,
        commit,
    }
}

fn run_locked_check(fixture: &LockedGitFixture) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_sai"))
        .current_dir(&fixture.root)
        .env("SAI_CACHE_DIR", &fixture.cache)
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
    (
        output.status.code().unwrap_or(4),
        String::from_utf8(output.stdout).expect("CLI stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("CLI stderr is UTF-8"),
    )
}

#[test]
fn locked_check_compares_verified_git_contract_before_compiler_and_root_source() {
    let fixture = locked_git_fixture(true, false);
    assert!(fixture.cached_manifest.is_file());

    let (exit_code, stdout, stderr) = run_locked_check(&fixture);

    assert_eq!(exit_code, 1);
    assert!(stderr.is_empty());
    assert!(stdout.contains("\"code\":\"LOCK_CORRUPT\""), "{stdout}");
    assert!(
        stdout
            .contains("current active package contract for acme:dep @ 1.0.0 differs from the lock"),
        "{stdout}"
    );
    assert!(!stdout.contains("\"code\":\"COMPILER_VERSION_MISMATCH\""));
    assert!(!stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
    assert!(!stdout.contains("\"code\":\"OFFLINE_FETCH_REQUIRED\""));
}

#[test]
fn locked_check_offline_git_cache_miss_reports_the_locked_commit() {
    let fixture = locked_git_fixture(false, false);

    let (exit_code, stdout, stderr) = run_locked_check(&fixture);

    assert_eq!(exit_code, 1);
    assert!(stderr.is_empty());
    assert!(
        stdout.contains("\"code\":\"OFFLINE_FETCH_REQUIRED\""),
        "{stdout}"
    );
    assert!(stdout.contains("https://example.com/acme/dep"), "{stdout}");
    assert!(stdout.contains(&fixture.commit), "{stdout}");
    assert!(!stdout.contains("\"code\":\"CACHE_CORRUPT\""));
    assert!(!stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
}

#[test]
fn locked_check_offline_corrupt_git_cache_reports_warning_and_fetch_requirement() {
    let fixture = locked_git_fixture(true, true);

    let (exit_code, stdout, stderr) = run_locked_check(&fixture);

    assert_eq!(exit_code, 1);
    assert!(stderr.is_empty());
    assert!(stdout.contains("\"code\":\"CACHE_CORRUPT\""), "{stdout}");
    assert!(
        stdout.contains("\"code\":\"OFFLINE_FETCH_REQUIRED\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"warnings\":1"), "{stdout}");
    assert!(!fixture.cached_manifest.exists());
    assert!(!stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
}
