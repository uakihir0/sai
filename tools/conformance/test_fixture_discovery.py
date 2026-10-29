import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fixture_discovery import DiscoveryError, _check_anchor, _is_reparse_point, _name_key, discover


class FixtureDiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve() / "conformance"
        self.root.mkdir()

    def file(self, name, data=b""):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def directory(self, name):
        path = self.root / name
        path.mkdir(parents=True, exist_ok=True)
        return path

    def test_discovery_order_and_test_boundaries(self):
        self.file("spec-03/z.sai")
        self.file("spec-02/nested/z.sai")
        self.file("spec-02/nested/A.sai")
        self.file("spec-02/a.t/sai.package")
        self.file("spec-02/a.t/src/lib.sai")
        self.file("spec-02/a.t/dependency.t/sai.package")
        self.file("spec-02/a.t/expected/fixed/src/lib.sai")
        self.file("spec-03/z.stdout")
        self.file("spec-03/z.wasm32-core.dev.stderr")
        self.file("spec-03/z.wit/golden.wit")
        self.file("spec-03/z.wasm64-wasi.release.mir/phase.mir")
        self.assertEqual(discover(self.root), [
            {"path": "conformance/spec-02/a.t", "kind": "directory",
             "root_file": "conformance/spec-02/a.t/sai.package"},
            {"path": "conformance/spec-02/nested/A.sai", "kind": "single-file",
             "root_file": "conformance/spec-02/nested/A.sai"},
            {"path": "conformance/spec-02/nested/z.sai", "kind": "single-file",
             "root_file": "conformance/spec-02/nested/z.sai"},
            {"path": "conformance/spec-03/z.sai", "kind": "single-file",
             "root_file": "conformance/spec-03/z.sai"},
        ])

    def test_sidecar_specialization_and_ambiguous_owners(self):
        self.file("chapter/case.sai")
        for sidecar in ("case.dev.stdout", "case.wasm32-core.stderr",
                        "case.wasm32-core.release.color-human"):
            self.file("chapter/" + sidecar)
        self.assertEqual(len(discover(self.root)), 1)
        self.file("chapter/case.dev.sai")
        with self.assertRaisesRegex(DiscoveryError, "ambiguous sidecar"):
            discover(self.root)

    def test_source_sidecars_are_not_silently_classified(self):
        self.file("chapter/case.sai", b"//@ kind: check-pass\n")
        self.file("chapter/case.expected.sai", b"//@ kind: check-pass\n")
        with self.assertRaisesRegex(DiscoveryError, "ambiguous test or source sidecar"):
            discover(self.root)
        (self.root / "chapter/case.expected.sai").unlink()
        self.file("chapter/case.wasm32-core.dev.fixed.sai")
        with self.assertRaisesRegex(DiscoveryError, "ambiguous test or source sidecar"):
            discover(self.root)

    def test_sidecar_shaped_directories_do_not_hide_nested_tests(self):
        self.file("chapter/case.sai")
        nested = self.file("chapter/case.wit/sub/nested.sai", b"//@ kind: check-pass\n")
        with self.assertRaisesRegex(DiscoveryError, "possible nested test"):
            discover(self.root)
        nested.unlink()
        self.directory("chapter/case.wit/sub/nested.t")
        with self.assertRaisesRegex(DiscoveryError, "possible nested test"):
            discover(self.root)

    def test_sidecar_wrong_types_and_orphans(self):
        self.file("chapter/case.sai")
        self.directory("chapter/case.stdout")
        with self.assertRaisesRegex(DiscoveryError, "sidecar is not a file"):
            discover(self.root)
        (self.root / "chapter/case.stdout").rmdir()
        self.file("chapter/missing.stderr")
        with self.assertRaisesRegex(DiscoveryError, "unowned or ambiguous"):
            discover(self.root)

    def test_all_file_sidecar_types_and_specializations(self):
        self.file("chapter/case.sai")
        for suffix in ("stdin", "stdout", "stderr", "panic", "json", "human", "color-human"):
            for specialization in ("", ".dev", ".wasm32-core", ".wasm64-wasi.release"):
                self.file("chapter/case" + specialization + "." + suffix)
        self.assertEqual([fixture["path"] for fixture in discover(self.root)],
                         ["conformance/chapter/case.sai"])

    def test_sidecar_directory_owner_ambiguity_and_wrong_type(self):
        self.file("chapter/case.sai")
        self.file("chapter/case.dev.sai")
        self.directory("chapter/case.dev.wit")
        with self.assertRaisesRegex(DiscoveryError, "ambiguous sidecar"):
            discover(self.root)
        (self.root / "chapter/case.dev.wit").rmdir()
        self.file("chapter/case.mir")
        with self.assertRaisesRegex(DiscoveryError, "not a directory"):
            discover(self.root)

    def test_error_choice_does_not_depend_on_hash_seed(self):
        self.file("chapter/z.sai")
        self.file("chapter/z.expected.sai")
        self.file("chapter/a.sai")
        self.file("chapter/a.fixed.sai")
        script = Path(__file__).with_name("fixture_discovery.py")
        errors = []
        for seed in ("1", "2", "99"):
            environment = dict(os.environ, PYTHONHASHSEED=seed)
            result = subprocess.run([sys.executable, "-B", str(script), str(self.root)],
                                    env=environment, capture_output=True, check=False)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, b"")
            errors.append(result.stderr)
        self.assertEqual(len(set(errors)), 1)
        self.assertIn(b"a.fixed.sai", errors[0])

    def test_missing_manifest_and_wrong_node_types(self):
        self.directory("chapter/missing.t")
        with self.assertRaisesRegex(DiscoveryError, "missing regular sai.package"):
            discover(self.root)
        self.directory("chapter/missing.t/sai.package")
        with self.assertRaisesRegex(DiscoveryError, "missing regular sai.package"):
            discover(self.root)

    def test_unowned_sidecar_directory_rejected(self):
        self.directory("chapter/unknown.wit")
        with self.assertRaisesRegex(DiscoveryError, "unowned or ambiguous"):
            discover(self.root)

    def test_symlinks_rejected_inside_and_outside_fixture(self):
        self.file("chapter/case.t/sai.package")
        link = self.root / "chapter/case.t/expected/link"
        link.parent.mkdir()
        try:
            link.symlink_to(self.root / "chapter/case.t/sai.package")
        except (NotImplementedError, OSError) as error:
            self.skipTest(str(error))
        with self.assertRaisesRegex(DiscoveryError, "symlink or special file|reparse point"):
            discover(self.root)
        link.unlink()
        (self.root / "chapter/link.sai").symlink_to(self.root / "chapter/case.t/sai.package")
        with self.assertRaisesRegex(DiscoveryError, "symlink or special file|reparse point"):
            discover(self.root)
        with self.assertRaisesRegex(DiscoveryError, "real directories"):
            discover(self.root / "chapter/link.sai")
        directory_link = Path(self.temporary.name).resolve() / "linked-conformance"
        directory_link.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(DiscoveryError, "real directories"):
            discover(directory_link)
        with self.assertRaisesRegex(DiscoveryError, "real directories"):
            discover(directory_link / "chapter")
        with self.assertRaisesRegex(DiscoveryError, "real directories"):
            discover(directory_link / ".." / "conformance")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO not supported")
    def test_special_file_rejected_without_opening_it(self):
        self.directory("chapter")
        os.mkfifo(self.root / "chapter" / "pipe")
        with self.assertRaisesRegex(DiscoveryError, "symlink or special file"):
            discover(self.root)

    def test_nonportable_names_and_case_collision(self):
        self.file("chapter/UPPER.sai")
        lower = self.root / "chapter/upper.sai"
        if lower.exists():
            self.skipTest("filesystem is case-insensitive")
        lower.write_bytes(b"")
        with self.assertRaisesRegex(DiscoveryError, "collision"):
            discover(self.root)

    def test_portable_name_keys_and_controls(self):
        self.assertEqual(_name_key("UPPER.sai"), _name_key("upper.sai"))
        for name in ("a\\b.sai", "a:b.sai", "trailing.", "LPT1.sai", "bad\u202ename",
                     "COM¹.sai", "NUL .txt", "conin$", "CONOUT$.txt", "COM0.sai",
                     "é.sai", "e\u0301.sai", "ſ.sai", "CON.sai"):
            with self.subTest(name=name), self.assertRaises(DiscoveryError):
                _name_key(name)
        with self.assertRaisesRegex(DiscoveryError, "non-UTF-8"):
            _name_key("bad\udcff.sai")

    def test_non_ascii_names_rejected_in_fixture_tree_and_root(self):
        self.file("chapter/case.t/sai.package")
        try:
            path = self.file("chapter/case.t/é.sai")
        except OSError as error:
            self.skipTest(f"filesystem cannot create non-ASCII names: {error}")
        with self.assertRaisesRegex(DiscoveryError, "nonportable name"):
            discover(self.root)
        path.unlink()
        non_ascii_root = self.root / "é"
        non_ascii_root.mkdir()
        with self.assertRaisesRegex(DiscoveryError, "nonportable name"):
            discover(non_ascii_root)

    def test_simulated_reparse_points_on_root_and_ancestors(self):
        self.directory("chapter/child")
        original_lstat = Path.lstat
        for target in (self.root, self.root / "chapter"):
            def fake_lstat(path):
                info = original_lstat(path)
                if path == target:
                    return SimpleNamespace(st_mode=info.st_mode,
                                           st_file_attributes=stat.FILE_ATTRIBUTE_REPARSE_POINT)
                return info

            with self.subTest(target=target), patch.object(Path, "lstat", autospec=True, side_effect=fake_lstat):
                for requested in (target, self.root / "chapter/child"):
                    with self.assertRaisesRegex(DiscoveryError, "real directories"):
                        discover(requested)

    def test_windows_missing_file_attributes_fails_closed(self):
        plain = SimpleNamespace(st_mode=stat.S_IFDIR)
        with patch("fixture_discovery.os.name", "nt"):
            with self.assertRaisesRegex(DiscoveryError, "attributes unavailable"):
                _is_reparse_point(plain)
            self.assertFalse(_is_reparse_point(SimpleNamespace(st_file_attributes=0)))

    def test_unc_and_device_namespace_anchors_rejected(self):
        for anchor in ("\\\\server\\share\\", "\\\\?\\C:\\", "\\\\.\\C:\\"):
            with self.subTest(anchor=anchor), self.assertRaises(DiscoveryError):
                _check_anchor(anchor)
        _check_anchor("C:\\")
        _check_anchor("/")

    def test_simulated_reparse_points_in_tree(self):
        self.file("chapter/case.t/sai.package")
        original_scandir = os.scandir
        for target in (self.root / "chapter/case.t", self.root / "chapter/case.t/sai.package"):
            @contextmanager
            def fake_scandir(path):
                with original_scandir(path) as entries:
                    values = []
                    for entry in entries:
                        if Path(entry.path) == target:
                            info = entry.stat(follow_symlinks=False)
                            replacement = SimpleNamespace(
                                st_mode=info.st_mode,
                                st_file_attributes=stat.FILE_ATTRIBUTE_REPARSE_POINT,
                            )
                            entry = SimpleNamespace(name=entry.name, path=entry.path,
                                                    stat=lambda **kwargs: replacement)
                        values.append(entry)
                    yield iter(values)

            with self.subTest(target=target), patch("fixture_discovery.os.scandir", side_effect=fake_scandir):
                with self.assertRaisesRegex(DiscoveryError, "reparse point"):
                    discover(self.root)

    @unittest.skipUnless(os.name == "nt", "Windows junction runtime required")
    def test_windows_junction_rejected_in_tree_and_root_path(self):
        target = Path(self.temporary.name).resolve() / "junction-target"
        (target / "child").mkdir(parents=True)
        junction = self.root / "junction"
        result = subprocess.run(["cmd", "/c", "mklink", "/J", str(junction), str(target)],
                                capture_output=True, check=False)
        if result.returncode:
            self.skipTest(f"cannot create junction: {result.stderr!r}")
        try:
            with self.assertRaisesRegex(DiscoveryError, "reparse point"):
                discover(self.root)
            with self.assertRaisesRegex(DiscoveryError, "real directories"):
                discover(junction)
            with self.assertRaisesRegex(DiscoveryError, "real directories"):
                discover(junction / "child")
        finally:
            os.rmdir(junction)

    @unittest.skipUnless(os.name == "posix", "raw byte paths require POSIX")
    def test_invalid_utf8_name_rejected(self):
        raw = os.fsencode(self.root) + b"/bad-\xff.sai"
        try:
            descriptor = os.open(raw, os.O_WRONLY | os.O_CREAT, 0o600)
        except OSError as error:
            self.skipTest(f"filesystem rejects non-UTF-8 names: {error}")
        os.close(descriptor)
        with self.assertRaisesRegex(DiscoveryError, "non-UTF-8"):
            discover(self.root)

    def test_io_errors_and_empty_tree(self):
        self.assertEqual(discover(self.root), [])
        with self.assertRaises(DiscoveryError):
            discover(self.root / "absent")
        with patch("fixture_discovery.os.scandir", side_effect=PermissionError("denied")):
            with self.assertRaisesRegex(DiscoveryError, "cannot inspect"):
                discover(self.root)

    def test_cli_reports_discovery_only_and_errors(self):
        script = Path(__file__).with_name("fixture_discovery.py")
        self.file("chapter/test.sai")
        result = subprocess.run([sys.executable, "-B", str(script), str(self.root)],
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)["status"], "discovery-only")
        result = subprocess.run([sys.executable, "-B", str(script), str(self.root / "missing")],
                                capture_output=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, b"")
        self.assertIn(b"discovery failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
