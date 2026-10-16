//! Embedded root WIT files for the pinned WASI 0.2.3 package set.
//!
//! Each source file is included from `wasi/` as bytes so parsing uses the same
//! in-memory path as package-provided WIT and does not access the filesystem.

use crate::WitSourceFile;

/// The WASI release selected by the compiler for all targets.
pub const EMBEDDED_WASI_VERSION: &str = "0.2.3";

/// Package names available as embedded WASI WIT units, in stable order.
pub const EMBEDDED_WASI_PACKAGE_NAMES: [&str; 7] = [
    "cli",
    "clocks",
    "filesystem",
    "http",
    "io",
    "random",
    "sockets",
];

/// One direct-child WIT source file from an embedded package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddedWasiFile {
    /// Filename within the package's `wit` directory.
    pub relative_path: &'static str,
    /// Exact bytes from the pinned upstream source commit.
    pub contents: &'static [u8],
}

/// One embedded package's stable identity, provenance and WIT source files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddedWasiPackage {
    /// Package name following the `wasi:` namespace.
    pub name: &'static str,
    /// Repository containing the source package.
    pub upstream_repository: &'static str,
    /// Exact upstream commit from which the root WIT files were copied.
    pub upstream_commit: &'static str,
    /// Root WIT files in bytewise filename order; dependency directories are
    /// intentionally omitted.
    pub files: &'static [EmbeddedWasiFile],
}

impl EmbeddedWasiPackage {
    /// Return this package's canonical package identity and pinned version.
    pub fn canonical_name(&self) -> String {
        format!("wasi:{}@{}", self.name, EMBEDDED_WASI_VERSION)
    }

    /// Copy the embedded sources into the parser's owned input type.
    pub fn source_files(&self) -> Vec<WitSourceFile> {
        self.files
            .iter()
            .map(|file| WitSourceFile::new(file.relative_path, file.contents.to_vec()))
            .collect()
    }
}

const CLI_FILES: [EmbeddedWasiFile; 7] = [
    EmbeddedWasiFile {
        relative_path: "command.wit",
        contents: include_bytes!("../wasi/cli/command.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "environment.wit",
        contents: include_bytes!("../wasi/cli/environment.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "exit.wit",
        contents: include_bytes!("../wasi/cli/exit.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "imports.wit",
        contents: include_bytes!("../wasi/cli/imports.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "run.wit",
        contents: include_bytes!("../wasi/cli/run.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "stdio.wit",
        contents: include_bytes!("../wasi/cli/stdio.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "terminal.wit",
        contents: include_bytes!("../wasi/cli/terminal.wit"),
    },
];

const CLOCKS_FILES: [EmbeddedWasiFile; 4] = [
    EmbeddedWasiFile {
        relative_path: "monotonic-clock.wit",
        contents: include_bytes!("../wasi/clocks/monotonic-clock.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "timezone.wit",
        contents: include_bytes!("../wasi/clocks/timezone.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "wall-clock.wit",
        contents: include_bytes!("../wasi/clocks/wall-clock.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "world.wit",
        contents: include_bytes!("../wasi/clocks/world.wit"),
    },
];

const FILESYSTEM_FILES: [EmbeddedWasiFile; 3] = [
    EmbeddedWasiFile {
        relative_path: "preopens.wit",
        contents: include_bytes!("../wasi/filesystem/preopens.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "types.wit",
        contents: include_bytes!("../wasi/filesystem/types.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "world.wit",
        contents: include_bytes!("../wasi/filesystem/world.wit"),
    },
];

const HTTP_FILES: [EmbeddedWasiFile; 3] = [
    EmbeddedWasiFile {
        relative_path: "handler.wit",
        contents: include_bytes!("../wasi/http/handler.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "proxy.wit",
        contents: include_bytes!("../wasi/http/proxy.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "types.wit",
        contents: include_bytes!("../wasi/http/types.wit"),
    },
];

const IO_FILES: [EmbeddedWasiFile; 4] = [
    EmbeddedWasiFile {
        relative_path: "error.wit",
        contents: include_bytes!("../wasi/io/error.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "poll.wit",
        contents: include_bytes!("../wasi/io/poll.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "streams.wit",
        contents: include_bytes!("../wasi/io/streams.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "world.wit",
        contents: include_bytes!("../wasi/io/world.wit"),
    },
];

const RANDOM_FILES: [EmbeddedWasiFile; 4] = [
    EmbeddedWasiFile {
        relative_path: "insecure-seed.wit",
        contents: include_bytes!("../wasi/random/insecure-seed.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "insecure.wit",
        contents: include_bytes!("../wasi/random/insecure.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "random.wit",
        contents: include_bytes!("../wasi/random/random.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "world.wit",
        contents: include_bytes!("../wasi/random/world.wit"),
    },
];

const SOCKETS_FILES: [EmbeddedWasiFile; 8] = [
    EmbeddedWasiFile {
        relative_path: "instance-network.wit",
        contents: include_bytes!("../wasi/sockets/instance-network.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "ip-name-lookup.wit",
        contents: include_bytes!("../wasi/sockets/ip-name-lookup.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "network.wit",
        contents: include_bytes!("../wasi/sockets/network.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "tcp-create-socket.wit",
        contents: include_bytes!("../wasi/sockets/tcp-create-socket.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "tcp.wit",
        contents: include_bytes!("../wasi/sockets/tcp.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "udp-create-socket.wit",
        contents: include_bytes!("../wasi/sockets/udp-create-socket.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "udp.wit",
        contents: include_bytes!("../wasi/sockets/udp.wit"),
    },
    EmbeddedWasiFile {
        relative_path: "world.wit",
        contents: include_bytes!("../wasi/sockets/world.wit"),
    },
];

/// All embedded WASI packages in canonical package-name order.
pub const EMBEDDED_WASI_CATALOG: [EmbeddedWasiPackage; 7] = [
    EmbeddedWasiPackage {
        name: "cli",
        upstream_repository: "WebAssembly/wasi-cli",
        upstream_commit: "d4fddec89fb9354509dbfa29a5557c58983f327a",
        files: &CLI_FILES,
    },
    EmbeddedWasiPackage {
        name: "clocks",
        upstream_repository: "WebAssembly/wasi-clocks",
        upstream_commit: "e9ef266c368fd7bfb3442e6cc9e7a546d4b029ff",
        files: &CLOCKS_FILES,
    },
    EmbeddedWasiPackage {
        name: "filesystem",
        upstream_repository: "WebAssembly/wasi-filesystem",
        upstream_commit: "70096e90b70ca876a9656bfebd59e3dd8686e661",
        files: &FILESYSTEM_FILES,
    },
    EmbeddedWasiPackage {
        name: "http",
        upstream_repository: "WebAssembly/wasi-http",
        upstream_commit: "3c1734c55239ccb700a1f61443047ab4cfa5deb5",
        files: &HTTP_FILES,
    },
    EmbeddedWasiPackage {
        name: "io",
        upstream_repository: "WebAssembly/wasi-io",
        upstream_commit: "f73bfb43c3b31d0afab7167c7f7f0515bebba704",
        files: &IO_FILES,
    },
    EmbeddedWasiPackage {
        name: "random",
        upstream_repository: "WebAssembly/wasi-random",
        upstream_commit: "bf93507b67b3d1e03bb9330cdaf71aafe432557f",
        files: &RANDOM_FILES,
    },
    EmbeddedWasiPackage {
        name: "sockets",
        upstream_repository: "WebAssembly/wasi-sockets",
        upstream_commit: "a584c2bfb4e8e4f30f7ba7a49b09b3ab2cd08730",
        files: &SOCKETS_FILES,
    },
];

/// Find an embedded WASI package by its package name.
pub fn embedded_wasi_package(name: &str) -> Option<&'static EmbeddedWasiPackage> {
    EMBEDDED_WASI_CATALOG
        .iter()
        .find(|package| package.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WitUnit, parse_source_files};
    use sai_intern::Interner;
    use sai_package::version::Version;

    const EXPECTED_FILES: [(&str, &[&str]); 7] = [
        (
            "cli",
            &[
                "command.wit",
                "environment.wit",
                "exit.wit",
                "imports.wit",
                "run.wit",
                "stdio.wit",
                "terminal.wit",
            ],
        ),
        (
            "clocks",
            &[
                "monotonic-clock.wit",
                "timezone.wit",
                "wall-clock.wit",
                "world.wit",
            ],
        ),
        ("filesystem", &["preopens.wit", "types.wit", "world.wit"]),
        ("http", &["handler.wit", "proxy.wit", "types.wit"]),
        ("io", &["error.wit", "poll.wit", "streams.wit", "world.wit"]),
        (
            "random",
            &[
                "insecure-seed.wit",
                "insecure.wit",
                "random.wit",
                "world.wit",
            ],
        ),
        (
            "sockets",
            &[
                "instance-network.wit",
                "ip-name-lookup.wit",
                "network.wit",
                "tcp-create-socket.wit",
                "tcp.wit",
                "udp-create-socket.wit",
                "udp.wit",
                "world.wit",
            ],
        ),
    ];

    #[test]
    fn embedded_catalog_is_complete_sorted_and_parses_at_its_pinned_identity() {
        assert_eq!(
            EMBEDDED_WASI_CATALOG
                .iter()
                .map(|package| package.name)
                .collect::<Vec<_>>(),
            EMBEDDED_WASI_PACKAGE_NAMES
        );
        assert_eq!(
            EMBEDDED_WASI_CATALOG
                .iter()
                .map(|package| (package.name, package.files.len()))
                .collect::<Vec<_>>(),
            EXPECTED_FILES.map(|(name, files)| (name, files.len()))
        );

        let symbols = Interner::new();
        let version = Version::parse(EMBEDDED_WASI_VERSION).expect("valid embedded version");
        for (package, (expected_name, expected_files)) in
            EMBEDDED_WASI_CATALOG.iter().zip(EXPECTED_FILES)
        {
            assert_eq!(package.name, expected_name);
            assert_eq!(
                package.canonical_name(),
                format!("wasi:{}@0.2.3", package.name)
            );
            assert!(!package.upstream_commit.is_empty());

            let filenames = package
                .files
                .iter()
                .map(|file| file.relative_path)
                .collect::<Vec<_>>();
            assert_eq!(filenames, expected_files, "inventory for {}", package.name);
            let mut sorted_filenames = filenames.clone();
            sorted_filenames.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            assert_eq!(filenames, sorted_filenames, "{}", package.name);
            assert!(!filenames.is_empty(), "{}", package.name);

            let package_symbol = symbols.intern(package.name).expect("valid package symbol");
            let unit = WitUnit::wasi(package_symbol, &symbols, &version)
                .expect("catalog entry has a valid embedded unit identity");
            let parsed = parse_source_files(unit, package.source_files())
                .unwrap_or_else(|error| panic!("failed to parse {}: {error}", package.name));
            assert_eq!(
                parsed.main_name().to_string(),
                package.canonical_name(),
                "package declaration in {}",
                package.name
            );
            assert_eq!(
                parsed.source_names().collect::<Vec<_>>(),
                filenames,
                "parser source order for {}",
                package.name
            );
        }
    }

    #[test]
    fn lookup_is_limited_to_catalog_packages() {
        assert_eq!(
            embedded_wasi_package("io").map(|package| package.name),
            Some("io")
        );
        assert!(embedded_wasi_package("preview1").is_none());
    }
}
