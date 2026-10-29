#!/usr/bin/env python3
"""Discover conformance fixture paths without executing or validating tests.

Usage: python3 fixture_discovery.py CONFORMANCE_DIRECTORY

The directory represents tests/conformance; returned paths are relative to
tests and sorted by UTF-8 bytes. A .t directory is one fixture and must have
a regular root sai.package. Its contents are inspected for unsafe filesystem
entries but not searched for tests. Since the format does not reserve test or
subdirectory names, .expected.sai and .fixed.sai suffixes are reserved as
single-file sidecars and require one unambiguous existing source owner, using
the supported target/profile suffixes. .wit/.mir directories require one
unambiguous owner and are rejected if they contain a possible nested test.
Other sidecar-shaped entries must have exactly one owner and the required file
or directory type.

This conservative subset rejects symlinks and Windows reparse points in the
supplied root path and tree, UNC/device-namespace roots, special files,
non-ASCII names in both the root path and tree, Windows device names and ASCII
case collisions, including inside fixtures. It does not validate directives,
sidecar contents, chapters or test completeness, and assumes the tree is not
concurrently modified. Results are discovery only, not a conformance verdict
or a secure copy operation.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import sys
from pathlib import Path


TARGETS = {"wasm32-core", "wasm64-core", "wasm32-wasi", "wasm64-wasi"}
PROFILES = {"dev", "release", "tiny"}
FILE_SIDECARS = (".stdin", ".stdout", ".stderr", ".panic", ".json",
                 ".color-human", ".human")
SOURCE_FILE_SIDECARS = (".expected.sai", ".fixed.sai")
WINDOWS_DEVICES = {"con", "prn", "aux", "nul", "conin$", "conout$", "clock$"} | {
    f"{prefix}{number}" for prefix in ("com", "lpt") for number in range(10)
}


class DiscoveryError(ValueError):
    """The tree is unsafe, ambiguous or unsupported for discovery."""


def _name_key(name: str) -> str:
    try:
        name.encode("utf-8", "strict")
    except UnicodeEncodeError:
        raise DiscoveryError(f"non-UTF-8 name: {name!a}") from None
    if (not name or name in (".", "..") or name[-1] in " ."
            or not name.isascii()
            or any(character in '\\/:*?"<>|' or ord(character) < 32 or ord(character) == 127
                   for character in name)
            or name.split(".", 1)[0].rstrip(" ").lower() in WINDOWS_DEVICES):
        raise DiscoveryError(f"nonportable name: {name!a}")
    return name.lower()


def _is_reparse_point(info: os.stat_result) -> bool:
    attributes = getattr(info, "st_file_attributes", None)
    if attributes is None:
        if os.name == "nt":
            raise DiscoveryError("Windows file attributes unavailable")
        return False
    return bool(attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT)


def _check_anchor(anchor: str) -> None:
    if anchor.startswith("\\\\"):
        raise DiscoveryError("UNC and Windows device-namespace roots are unsupported")


def _owners(stem: str, sources: set[str]) -> list[str]:
    candidates = {stem}
    parts = stem.split(".")
    if len(parts) >= 2 and (parts[-1] in TARGETS or parts[-1] in PROFILES):
        candidates.add(".".join(parts[:-1]))
    if len(parts) >= 3 and parts[-2] in TARGETS and parts[-1] in PROFILES:
        candidates.add(".".join(parts[:-2]))
    return sorted((candidate for candidate in candidates if candidate in sources),
                  key=lambda name: name.encode("utf-8"))


def _owner(stem: str, sources: set[str], path: str) -> str:
    owners = _owners(stem, sources)
    if len(owners) != 1:
        raise DiscoveryError(f"unowned or ambiguous sidecar: {path!a}")
    return owners[0]


def _checked_root(conformance_directory: os.PathLike | str) -> Path:
    requested = Path(conformance_directory)
    if requested.anchor and not requested.is_absolute():
        raise DiscoveryError("drive-relative conformance root is unsupported")
    if not requested.is_absolute():
        requested = Path.cwd() / requested
    _check_anchor(requested.anchor)
    root = Path(requested.anchor)
    for part in requested.parts[1:]:
        if part != "..":
            _name_key(part)
    parts = requested.parts[1:] if requested.is_absolute() else requested.parts
    root_stat = root.lstat()
    if _is_reparse_point(root_stat) or not stat.S_ISDIR(root_stat.st_mode):
        raise DiscoveryError("conformance root path must contain only real directories")
    for part in parts:
        if part == "..":
            root = root.parent
        else:
            root = root / part
        root_stat = root.lstat()
        if _is_reparse_point(root_stat) or not stat.S_ISDIR(root_stat.st_mode):
            raise DiscoveryError(f"conformance root path must contain only real directories: {str(root)!a}")
    return root


def discover(conformance_directory: os.PathLike | str) -> list[dict[str, str]]:
    """Return fixture descriptors, or raise DiscoveryError on uncertainty."""
    try:
        root = _checked_root(conformance_directory)
        fixtures = []
        pending = [(root, "conformance", True, False, False)]
        while pending:
            directory, relative, search, require_manifest, possible_sidecar = pending.pop()
            with os.scandir(directory) as iterator:
                entries = list(iterator)
            entries.sort(key=lambda entry: os.fsencode(entry.name))
            keys = set()
            kinds = {}
            for entry in entries:
                key = _name_key(entry.name)
                if key in keys:
                    raise DiscoveryError(f"portable name collision in {relative!a}")
                keys.add(key)
                info = entry.stat(follow_symlinks=False)
                if _is_reparse_point(info):
                    raise DiscoveryError(f"reparse point: {(relative + '/' + entry.name)!a}")
                mode = info.st_mode
                if stat.S_ISREG(mode):
                    kinds[entry.name] = "file"
                elif stat.S_ISDIR(mode):
                    kinds[entry.name] = "directory"
                else:
                    raise DiscoveryError(f"symlink or special file: {(relative + '/' + entry.name)!a}")

            if require_manifest and kinds.get("sai.package") != "file":
                raise DiscoveryError(f"missing regular sai.package in {relative!a}")

            if possible_sidecar:
                for entry in entries:
                    if ((kinds[entry.name] == "file" and entry.name.endswith(".sai"))
                            or (kinds[entry.name] == "directory" and entry.name.endswith(".t"))):
                        raise DiscoveryError(f"possible nested test in sidecar directory: {(relative + '/' + entry.name)!a}")

            sources = {
                name[:-4] for name, kind in kinds.items()
                if (search and kind == "file" and name.endswith(".sai") and name != ".sai"
                    and not name.endswith(SOURCE_FILE_SIDECARS))
            }
            source_sidecars = set()
            for name, kind in kinds.items():
                if not search:
                    continue
                path = relative + "/" + name
                for suffix in SOURCE_FILE_SIDECARS:
                    if name.endswith(suffix):
                        if kind != "file":
                            raise DiscoveryError(f"sidecar is not a file: {path!a}")
                        _owner(name[:-len(suffix)], sources, path)
                        source_sidecars.add(name)
                        break
                for suffix in FILE_SIDECARS:
                    if name.endswith(suffix):
                        if kind != "file":
                            raise DiscoveryError(f"sidecar is not a file: {path!a}")
                        _owner(name[:-len(suffix)], sources, path)
                        break

            for entry in reversed(entries):
                name = entry.name
                path = relative + "/" + name
                kind = kinds[name]
                if search and kind == "file" and name.endswith(".sai"):
                    if name == ".sai":
                        raise DiscoveryError(f"empty fixture name: {path!a}")
                    if name not in source_sidecars:
                        fixtures.append({"path": path, "kind": "single-file", "root_file": path})
                elif kind == "directory":
                    if search and name.endswith(".t"):
                        if name == ".t":
                            raise DiscoveryError(f"empty fixture name: {path!a}")
                        fixtures.append({"path": path, "kind": "directory", "root_file": path + "/sai.package"})
                        pending.append((Path(entry.path), path, False, True, False))
                    elif search and name.endswith((".wit", ".mir")):
                        _owner(name[:-4], sources, path)
                        pending.append((Path(entry.path), path, False, False, True))
                    else:
                        pending.append((Path(entry.path), path, search, False, possible_sidecar))
                elif search and name.endswith((".t", ".wit", ".mir")):
                    raise DiscoveryError(f"fixture or sidecar is not a directory: {path!a}")
        return sorted(fixtures, key=lambda fixture: fixture["path"].encode("utf-8"))
    except OSError as error:
        raise DiscoveryError(f"cannot inspect fixture tree: {error}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("conformance_directory")
    args = parser.parse_args()
    try:
        fixtures = discover(args.conformance_directory)
    except (DiscoveryError, ValueError) as error:
        print(f"discovery failed: {ascii(str(error))}", file=sys.stderr)
        return 2
    print(json.dumps({"status": "discovery-only", "fixtures": fixtures}, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
