#![forbid(unsafe_code)]

mod convert;
mod model;
mod source_scan;
mod stable_hash;

pub use convert::*;
pub use model::*;

use sai_hir::{PackageId, Symbol};
use sai_intern::{Interner, LookupError, PackageIdError, PackageInterner};
use sai_package::{mvs::MvsPackageVersion, version::Version};
use sai_stable_hash::{StableHash, StableHasher};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use wit_parser::{ParseError, SourceMap, UnresolvedPackageGroup};

const EMBEDDED_WASI_PACKAGES: [&str; 7] = [
    "cli",
    "clocks",
    "filesystem",
    "http",
    "io",
    "random",
    "sockets",
];
const EMBEDDED_WASI_VERSION: &str = "0.2.3";

/// The WIT unit through which a package is viewed.
///
/// Package IDs and symbols are session-local. Persistent view identities are
/// derived from canonical package keys and WIT names instead.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitUnitKind {
    Wasi { package: Symbol },
    Dir { package: PackageId },
    Generated { package: PackageId },
}

#[derive(Clone, Debug)]
pub struct WitUnit {
    kind: WitUnitKind,
    stable_key: Arc<str>,
    canonical_name: Arc<str>,
}

impl PartialEq for WitUnit {
    fn eq(&self, other: &Self) -> bool {
        self.identity_tag() == other.identity_tag() && self.stable_key == other.stable_key
    }
}

impl Eq for WitUnit {}

impl Hash for WitUnit {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity_tag().hash(state);
        self.stable_key.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WitUnitIdentityError {
    UnknownPackage(PackageIdError),
    UnknownSymbol(LookupError),
    UnknownWasiPackage,
    UnsupportedWasiVersion,
    PackageKeyMismatch,
}

impl WitUnit {
    pub fn wasi(
        package: Symbol,
        symbols: &Interner,
        version: &Version,
    ) -> Result<Self, WitUnitIdentityError> {
        let name = symbols
            .resolve(package)
            .map_err(WitUnitIdentityError::UnknownSymbol)?;
        if !EMBEDDED_WASI_PACKAGES.contains(&name.as_ref()) {
            return Err(WitUnitIdentityError::UnknownWasiPackage);
        }
        if version.to_string() != EMBEDDED_WASI_VERSION {
            return Err(WitUnitIdentityError::UnsupportedWasiVersion);
        }
        let name_with_version = format!("{name}@{version}");
        Ok(Self {
            kind: WitUnitKind::Wasi { package },
            stable_key: Arc::from(format!("wit:wasi:{name_with_version}")),
            canonical_name: Arc::from(format!("wasi:{name_with_version}")),
        })
    }

    pub fn dir(
        package: PackageId,
        packages: &PackageInterner,
        package_version: &MvsPackageVersion,
    ) -> Result<Self, WitUnitIdentityError> {
        let package_key = packages
            .resolve(package)
            .map_err(WitUnitIdentityError::UnknownPackage)?;
        let expected_package_key = format!(
            "{}#{}",
            package_version.package(),
            package_version.version().compatibility_line()
        );
        if package_key.as_ref() != expected_package_key {
            return Err(WitUnitIdentityError::PackageKeyMismatch);
        }
        let canonical_name = format!(
            "{}@{}",
            package_version.package(),
            package_version.version()
        );
        Ok(Self {
            kind: WitUnitKind::Dir { package },
            stable_key: Arc::from(format!("wit:dir:{package_key}")),
            canonical_name: Arc::from(canonical_name),
        })
    }

    pub fn generated(
        package: PackageId,
        packages: &PackageInterner,
        package_version: &MvsPackageVersion,
    ) -> Result<Self, WitUnitIdentityError> {
        let package_key = packages
            .resolve(package)
            .map_err(WitUnitIdentityError::UnknownPackage)?;
        let expected_package_key = format!(
            "{}#{}",
            package_version.package(),
            package_version.version().compatibility_line()
        );
        if package_key.as_ref() != expected_package_key {
            return Err(WitUnitIdentityError::PackageKeyMismatch);
        }
        let canonical_name = format!(
            "{}@{}",
            package_version.package(),
            package_version.version()
        );
        Ok(Self {
            kind: WitUnitKind::Generated { package },
            stable_key: Arc::from(format!("wit:gen:{package_key}")),
            canonical_name: Arc::from(canonical_name),
        })
    }

    pub fn kind(&self) -> WitUnitKind {
        self.kind
    }

    /// Returns the canonical identity used by persistent views and stable hashing.
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    pub fn canonical_name(&self) -> &str {
        &self.canonical_name
    }

    fn identity_tag(&self) -> u8 {
        match self.kind {
            WitUnitKind::Wasi { .. } => 0x01,
            WitUnitKind::Dir { .. } => 0x02,
            WitUnitKind::Generated { .. } => 0x03,
        }
    }
}

impl StableHash for WitUnit {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xB0);
        hasher.write_tag(self.identity_tag());
        self.stable_key.stable_hash(hasher);
    }
}

/// One VFS-provided WIT file.
///
/// `relative_path` is the direct-child filename within the package's `wit`
/// directory. The loader consumes bytes so invalid UTF-8 can be diagnosed at
/// its first offending byte.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitSourceFile {
    pub relative_path: String,
    pub contents: Vec<u8>,
}

impl WitSourceFile {
    pub fn new(relative_path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            relative_path: relative_path.into(),
            contents: contents.into(),
        }
    }
}

/// A byte range in one VFS-provided WIT file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitSourceLocation {
    pub path: String,
    pub range: Range<usize>,
}

/// A failure while validating or parsing a WIT file inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitParseError {
    kind: WitParseErrorKind,
    location: Option<WitSourceLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WitParseErrorKind {
    InvalidPath(String),
    DuplicatePath(String),
    InvalidUtf8 { valid_up_to: usize },
    InputTooLarge,
    Parse(String),
}

impl WitParseError {
    pub fn location(&self) -> Option<&WitSourceLocation> {
        self.location.as_ref()
    }
}

impl fmt::Display for WitParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WitParseErrorKind::InvalidPath(path) => write!(
                formatter,
                "WIT source path `{path}` is not a direct `.wit` filename"
            ),
            WitParseErrorKind::DuplicatePath(path) => {
                write!(
                    formatter,
                    "WIT source path `{path}` is listed more than once"
                )
            }
            WitParseErrorKind::InvalidUtf8 { valid_up_to } => {
                write!(
                    formatter,
                    "WIT source is not valid UTF-8 at byte {valid_up_to}"
                )
            }
            WitParseErrorKind::InputTooLarge => {
                formatter.write_str("WIT source files exceed the parser's 32-bit span limit")
            }
            WitParseErrorKind::Parse(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for WitParseError {}

/// Parsed WIT files before foreign packages have been connected to the package
/// graph.
///
/// The wrapped `wit_parser` value is an intermediate loader result, not a
/// compiler query value. `sai_wit` converts it into its parser-independent
/// model before exposing any query output.
pub struct ParsedWitPackageGroup {
    unit: WitUnit,
    group: UnresolvedPackageGroup,
    source_names: Vec<String>,
    source_mappings: Vec<WitSourceMapping>,
    package_name_location: Option<WitSourceLocation>,
    nested_package_locations: Vec<WitSourceLocation>,
    named_result_function_spans: Vec<Range<u32>>,
}

impl fmt::Debug for ParsedWitPackageGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedWitPackageGroup")
            .field("unit", &self.unit)
            .field("main_name", &self.group.main.name)
            .field("nested_count", &self.group.nested.len())
            .field("source_names", &self.source_names)
            .field("package_name_location", &self.package_name_location)
            .field("nested_package_locations", &self.nested_package_locations)
            .finish()
    }
}

impl ParsedWitPackageGroup {
    pub fn unit(&self) -> WitUnit {
        self.unit.clone()
    }

    pub fn main_name(&self) -> &wit_parser::PackageName {
        &self.group.main.name
    }

    pub fn nested_names(&self) -> impl ExactSizeIterator<Item = &wit_parser::PackageName> {
        self.group.nested.iter().map(|package| &package.name)
    }

    pub fn source_names(&self) -> impl Iterator<Item = &str> {
        self.source_names.iter().map(String::as_str)
    }
}

/// Parse an in-memory inventory of direct-child `.wit` files.
///
/// This function deliberately uses `SourceMap::push_str`; it never asks
/// `wit-parser` to read from disk. Files are ordered by their relative path
/// bytes before being added, matching the compiler's deterministic input
/// contract.
pub fn parse_source_files(
    unit: WitUnit,
    files: impl IntoIterator<Item = WitSourceFile>,
) -> Result<ParsedWitPackageGroup, WitParseError> {
    let mut files = files.into_iter().collect::<Vec<_>>();
    files.sort_by(|left, right| {
        left.relative_path
            .as_bytes()
            .cmp(right.relative_path.as_bytes())
    });

    let mut previous_path: Option<&str> = None;
    let mut source_size = 0usize;
    for file in &files {
        if !is_direct_wit_filename(&file.relative_path) {
            return Err(WitParseError {
                kind: WitParseErrorKind::InvalidPath(file.relative_path.clone()),
                location: None,
            });
        }
        if previous_path == Some(file.relative_path.as_str()) {
            return Err(WitParseError {
                kind: WitParseErrorKind::DuplicatePath(file.relative_path.clone()),
                location: None,
            });
        }
        previous_path = Some(&file.relative_path);

        source_size = source_size
            .checked_add(file.contents.len())
            .and_then(|size| size.checked_add(1))
            .filter(|size| *size <= u32::MAX as usize)
            .ok_or_else(|| WitParseError {
                kind: WitParseErrorKind::InputTooLarge,
                location: None,
            })?;
    }

    let source_names = files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect();
    let mut source_map = SourceMap::new();
    let mut source_mappings = Vec::with_capacity(files.len());
    let mut parser_size = 0usize;
    let mut package_name_location = None;
    let mut nested_package_locations = Vec::new();
    let mut named_result_function_spans = Vec::new();
    for file in files {
        let contents = String::from_utf8(file.contents).map_err(|error| {
            let valid_up_to = error.utf8_error().valid_up_to();
            let end = (valid_up_to + 1).min(error.as_bytes().len());
            WitParseError {
                kind: WitParseErrorKind::InvalidUtf8 { valid_up_to },
                location: Some(WitSourceLocation {
                    path: file.relative_path.clone(),
                    range: valid_up_to..end,
                }),
            }
        })?;
        let scan = source_scan::scan_source(&contents);
        let parser_offset = u32::try_from(parser_size).expect("source size was bounded to u32");
        parser_size = parser_size
            .checked_add(scan.parser_text.len())
            .and_then(|size| size.checked_add(1))
            .filter(|size| *size <= u32::MAX as usize)
            .ok_or_else(|| WitParseError {
                kind: WitParseErrorKind::InputTooLarge,
                location: None,
            })?;
        let mapping = WitSourceMapping {
            path: file.relative_path.clone(),
            offset: parser_offset,
            parser_len: scan.parser_text.len(),
            parser_to_source: scan.parser_to_source,
        };
        if package_name_location.is_none() {
            package_name_location =
                scan.package_name_range
                    .clone()
                    .map(|range| WitSourceLocation {
                        path: file.relative_path.clone(),
                        range,
                    });
        }
        nested_package_locations.extend(scan.nested_package_ranges.into_iter().map(|range| {
            WitSourceLocation {
                path: file.relative_path.clone(),
                range,
            }
        }));
        named_result_function_spans.extend(scan.named_result_function_ranges.into_iter().map(
            |range| {
                let start = mapping
                    .offset
                    .checked_add(u32::try_from(range.start).expect("file size is bounded"));
                let end = mapping
                    .offset
                    .checked_add(u32::try_from(range.end).expect("file size is bounded"));
                start.expect("group source spans are bounded")
                    ..end.expect("group source spans are bounded")
            },
        ));
        source_map.push_str(&file.relative_path, scan.parser_text);
        source_mappings.push(mapping);
    }

    match source_map.parse() {
        Ok(group) => Ok(ParsedWitPackageGroup {
            unit,
            group,
            source_names,
            source_mappings,
            package_name_location,
            nested_package_locations,
            named_result_function_spans,
        }),
        Err((_, error)) => Err(map_parse_error(&source_mappings, error)),
    }
}

fn is_direct_wit_filename(path: &str) -> bool {
    let bytes = path.as_bytes();
    !path.is_empty()
        && path.ends_with(".wit")
        && !path.contains('/')
        && !path.contains('\\')
        && !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        && !bytes
            .iter()
            .any(|byte| *byte == 0 || byte.is_ascii_control())
        && path != "."
        && path != ".."
}

#[derive(Clone, Debug)]
struct WitSourceMapping {
    path: String,
    offset: u32,
    parser_len: usize,
    parser_to_source: Vec<usize>,
}

fn map_parse_error(sources: &[WitSourceMapping], error: ParseError) -> WitParseError {
    let span = error.kind().span();
    let location = sources
        .iter()
        .find(|source| {
            let start = u64::from(source.offset);
            let end = start + source.parser_len as u64;
            u64::from(span.start()) >= start && u64::from(span.start()) <= end
        })
        .map(|source| {
            let start = usize::try_from(span.start() - source.offset)
                .expect("span starts within the source file")
                .min(source.parser_len);
            let end = usize::try_from(span.end() - source.offset)
                .expect("span ends within the source file")
                .clamp(start, source.parser_len);
            WitSourceLocation {
                path: source.path.clone(),
                range: source.parser_to_source[start]..source.parser_to_source[end],
            }
        });
    WitParseError {
        kind: WitParseErrorKind::Parse(error.to_string()),
        location,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ParsedWitPackageGroup, WitParseError, WitSourceFile, WitSourceLocation, WitUnit,
        parse_source_files,
    };
    use sai_intern::{Interner, PackageInterner};
    use sai_package::{mvs::MvsPackageVersion, version::Version};
    use sai_stable_hash::StableHash;

    fn dir_unit() -> WitUnit {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:graphics#1")
            .expect("valid test package key");
        let version = MvsPackageVersion::new(
            "acme:graphics",
            Version::parse("1.2.3").expect("valid test version"),
        )
        .expect("valid test package version");
        WitUnit::dir(package, &packages, &version).expect("package ID matches package version")
    }

    fn parse(files: impl IntoIterator<Item = WitSourceFile>) -> ParsedWitPackageGroup {
        parse_source_files(dir_unit(), files).expect("valid WIT source files")
    }

    #[test]
    fn parses_in_memory_files_in_deterministic_path_order() {
        let files = [
            WitSourceFile::new("z-last.wit", "package acme:graphics;\ninterface last {}"),
            WitSourceFile::new("a-first.wit", "package acme:graphics;\ninterface first {}"),
        ];
        let unit = dir_unit();

        let forward =
            parse_source_files(unit.clone(), files.clone()).expect("valid WIT source files");
        let reverse =
            parse_source_files(unit, files.into_iter().rev()).expect("valid WIT source files");

        assert_eq!(forward.main_name().to_string(), "acme:graphics");
        assert_eq!(
            forward.source_names().collect::<Vec<_>>(),
            ["a-first.wit", "z-last.wit"]
        );
        assert_eq!(
            reverse.source_names().collect::<Vec<_>>(),
            ["a-first.wit", "z-last.wit"]
        );
        assert_eq!(forward.nested_names().len(), 0);
        assert_eq!(forward.unit(), reverse.unit());
    }

    #[test]
    fn maps_parser_errors_back_to_the_exact_source_file_range() {
        let first_content = "package acme:graphics;\n/// 日本語\ninterface first {}";
        let broken_content = "package acme:graphics;\ninterface {";
        let error = parse_source_files(
            dir_unit(),
            [
                WitSourceFile::new("z-broken.wit", broken_content),
                WitSourceFile::new("a-first.wit", first_content),
            ],
        )
        .expect_err("malformed WIT must fail");

        let location = error.location().expect("parser span maps to a file");
        assert_eq!(location.path, "z-broken.wit");
        assert_eq!(
            &broken_content[location.range.clone()],
            "{",
            "parser byte offsets must map across preceding UTF-8 source files"
        );
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn maps_parser_errors_after_named_results_back_to_original_offsets() {
        let source = "package acme:graphics@1.2.3;\ninterface shapes { named: func() -> (left: u32, right: string); broken: func() -> ?; }";
        let error = parse_source_files(
            dir_unit(),
            [WitSourceFile::new("after-rewrite.wit", source)],
        )
        .expect_err("malformed declaration after named results must fail");

        let location = error
            .location()
            .expect("parser error maps to a source file");
        assert_eq!(location.path, "after-rewrite.wit");
        assert_eq!(&source[location.range.clone()], "?");
    }

    #[test]
    fn reports_the_first_invalid_utf8_byte_in_path_order() {
        let error = parse_source_files(
            dir_unit(),
            [
                WitSourceFile::new("z-later.wit", b"\xff".to_vec()),
                WitSourceFile::new("a-first.wit", b"package acme:graphics;\n\xff".to_vec()),
            ],
        )
        .expect_err("invalid UTF-8 must fail");

        assert_eq!(
            error.location(),
            Some(&WitSourceLocation {
                path: "a-first.wit".to_owned(),
                range: 23..24,
            })
        );
        assert_eq!(
            error.to_string(),
            "WIT source is not valid UTF-8 at byte 23"
        );
    }

    #[test]
    fn rejects_nested_or_non_wit_paths_and_duplicate_paths() {
        for path in [
            "deps/dependency.wit",
            "C:drive-relative.wit",
            "line\nbreak.wit",
            "nul\0byte.wit",
        ] {
            let invalid_path = parse_source_files(
                dir_unit(),
                [WitSourceFile::new(path, "package acme:graphics;")],
            )
            .expect_err("only safe direct-child WIT filenames are accepted");
            assert!(
                invalid_path.to_string().contains("direct `.wit` filename"),
                "unexpected error for {path:?}: {invalid_path}"
            );
        }

        let duplicate = parse_source_files(
            dir_unit(),
            [
                WitSourceFile::new("same.wit", "package acme:graphics;"),
                WitSourceFile::new("same.wit", "package acme:graphics;"),
            ],
        )
        .expect_err("duplicate paths are ambiguous");
        assert!(duplicate.to_string().contains("listed more than once"));
    }

    #[test]
    fn empty_inventory_is_a_parse_failure_without_a_source_location() {
        let error =
            parse_source_files(dir_unit(), []).expect_err("a WIT unit needs a package declaration");

        assert_eq!(error.location(), None);
        assert!(matches!(error.kind, super::WitParseErrorKind::Parse(_)));
    }

    #[test]
    fn parser_receives_virtual_names_instead_of_opening_files() {
        let package = parse([WitSourceFile::new(
            "does-not-exist-on-disk.wit",
            "package acme:graphics;\ninterface rendered {}",
        )]);

        assert_eq!(
            package.source_names().collect::<Vec<_>>(),
            ["does-not-exist-on-disk.wit"]
        );
    }

    #[test]
    fn directory_and_generated_units_are_distinct_nominal_views() {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:graphics#1")
            .expect("valid test package key");
        let version = MvsPackageVersion::new(
            "acme:graphics",
            Version::parse("1.2.3").expect("valid test version"),
        )
        .expect("valid test package version");

        assert_ne!(
            WitUnit::dir(package, &packages, &version).expect("package ID matches package version"),
            WitUnit::generated(package, &packages, &version)
                .expect("package ID matches package version")
        );
    }

    #[test]
    fn wasi_units_are_limited_to_the_embedded_catalog_and_version() {
        let symbols = Interner::new();
        let unbundled = symbols.intern("unbundled").expect("valid test symbol");
        let io = symbols.intern("io").expect("valid test symbol");
        let version = Version::parse("0.2.3").expect("valid test version");

        assert_eq!(
            WitUnit::wasi(unbundled, &symbols, &version).unwrap_err(),
            super::WitUnitIdentityError::UnknownWasiPackage
        );
        assert_eq!(
            WitUnit::wasi(
                io,
                &symbols,
                &Version::parse("0.2.2").expect("valid test version")
            )
            .unwrap_err(),
            super::WitUnitIdentityError::UnsupportedWasiVersion
        );
        assert_eq!(
            WitUnit::wasi(io, &symbols, &version)
                .expect("embedded WASI package and version are accepted")
                .canonical_name(),
            "wasi:io@0.2.3"
        );
    }

    #[test]
    fn stable_unit_hash_ignores_session_local_package_ids() {
        let first_packages = PackageInterner::new();
        let second_packages = PackageInterner::new();
        let first_id = first_packages
            .intern("acme:graphics#1")
            .expect("valid test package key");
        let second_id = second_packages
            .intern("acme:graphics#1")
            .expect("valid test package key");
        assert_ne!(first_id, second_id);

        let version = MvsPackageVersion::new(
            "acme:graphics",
            Version::parse("1.2.3").expect("valid test version"),
        )
        .expect("valid test package version");
        let first = WitUnit::dir(first_id, &first_packages, &version)
            .expect("package ID matches package version");
        let second = WitUnit::dir(second_id, &second_packages, &version)
            .expect("package ID matches package version");
        let generated = WitUnit::generated(second_id, &second_packages, &version)
            .expect("package ID matches package version");

        assert_eq!(first.stable_key(), "wit:dir:acme:graphics#1");
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_ne!(first.fingerprint(), generated.fingerprint());
    }

    #[test]
    fn parse_errors_implement_std_error() {
        fn assert_error<T: std::error::Error>() {}
        assert_error::<WitParseError>();
    }
}
