#![forbid(unsafe_code)]

mod closure;
mod contract;
mod convert;
mod dependency;
mod generated;
mod model;
mod source_scan;
mod stable_hash;
mod stable_view_hash;
mod view;
mod wasi;
mod wasi_loader;

pub use closure::*;
pub use contract::*;
pub use convert::*;
pub use dependency::*;
pub use generated::*;
pub use model::*;
pub use view::*;
pub use wasi::*;
pub use wasi_loader::*;

use sai_hir::{PackageId, Symbol};
use sai_intern::{Interner, LookupError, PackageIdError, PackageInterner};
use sai_package::{mvs::MvsPackageVersion, version::Version};
use sai_stable_hash::{StableHash, StableHasher};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use wit_parser::{ParseError, SourceMap, UnresolvedPackageGroup};

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
        if !wasi::EMBEDDED_WASI_PACKAGE_NAMES.contains(&name.as_ref()) {
            return Err(WitUnitIdentityError::UnknownWasiPackage);
        }
        if version.to_string() != wasi::EMBEDDED_WASI_VERSION {
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
    source_texts: Vec<WitSourceText>,
    source_mappings: Vec<WitSourceMapping>,
    foreign_references: Vec<WitForeignReference>,
    package_name_location: Option<WitSourceLocation>,
    nested_package_locations: Vec<WitSourceLocation>,
    named_result_function_spans: Vec<Range<u32>>,
}

#[derive(Clone, Debug)]
struct WitSourceText {
    path: String,
    text: String,
}

/// A foreign WIT package reference in one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitForeignReference {
    /// Package namespace and name, without the referenced interface or world.
    pub identity: String,
    /// The optional SemVer text written after the interface or world name.
    pub version: Option<String>,
    /// Original source file and complete package-reference byte range.
    pub location: WitSourceLocation,
    version_range: Option<Range<usize>>,
    item_range: Range<usize>,
}

/// A parsed WIT package group whose foreign references have been checked
/// against the active package graph and normalized to selected versions.
#[derive(Debug)]
pub struct ResolvedWitPackageGroup {
    parsed: ParsedWitPackageGroup,
    direct_dependencies: Vec<WitUnit>,
}

impl ResolvedWitPackageGroup {
    /// Returns the package group with foreign package names normalized to the
    /// versions selected by the package graph.
    pub fn parsed(&self) -> &ParsedWitPackageGroup {
        &self.parsed
    }

    /// Returns direct foreign units in first-reference order, without
    /// duplicates or self-references.
    pub fn direct_dependencies(&self) -> &[WitUnit] {
        &self.direct_dependencies
    }

    /// Returns the normalized parser group and its ordered direct units.
    pub fn into_parts(self) -> (ParsedWitPackageGroup, Vec<WitUnit>) {
        (self.parsed, self.direct_dependencies)
    }
}

/// A failure while resolving and normalizing foreign WIT references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitReferenceResolutionError {
    kind: WitReferenceResolutionErrorKind,
    location: Option<WitSourceLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitReferenceResolutionErrorKind {
    NestedPackage,
    PackageMismatch { expected: String, actual: String },
    InvalidVersion(String),
    Reference(WitPackageReferenceError),
    Reparse(WitParseError),
    InvalidSourceRange,
}

impl WitReferenceResolutionError {
    /// Returns the category and details of the failure.
    pub fn kind(&self) -> &WitReferenceResolutionErrorKind {
        &self.kind
    }

    /// Returns the original location of the failing reference, when available.
    pub fn location(&self) -> Option<&WitSourceLocation> {
        self.location.as_ref()
    }
}

impl fmt::Display for WitReferenceResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WitReferenceResolutionErrorKind::NestedPackage => {
                formatter.write_str("WIT package groups cannot contain nested packages")
            }
            WitReferenceResolutionErrorKind::PackageMismatch { expected, actual } => write!(
                formatter,
                "WIT package declaration `{actual}` does not match expected package `{expected}`"
            ),
            WitReferenceResolutionErrorKind::InvalidVersion(version) => {
                write!(formatter, "invalid WIT package version `{version}`")
            }
            WitReferenceResolutionErrorKind::Reference(error) => {
                write!(formatter, "{error}")
            }
            WitReferenceResolutionErrorKind::Reparse(error) => {
                write!(formatter, "failed to parse normalized WIT package: {error}")
            }
            WitReferenceResolutionErrorKind::InvalidSourceRange => {
                formatter.write_str("WIT reference scanner produced an invalid source range")
            }
        }
    }
}

impl std::error::Error for WitReferenceResolutionError {}

impl fmt::Debug for ParsedWitPackageGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedWitPackageGroup")
            .field("unit", &self.unit)
            .field("main_name", &self.group.main.name)
            .field("nested_count", &self.group.nested.len())
            .field("source_names", &self.source_names)
            .field("foreign_references", &self.foreign_references)
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

    /// Returns foreign package references in source-file and lexical order.
    pub fn foreign_references(&self) -> &[WitForeignReference] {
        &self.foreign_references
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

    let source_texts = files
        .into_iter()
        .map(|file| {
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
            Ok(WitSourceText {
                path: file.relative_path,
                text: contents,
            })
        })
        .collect::<Result<Vec<_>, WitParseError>>()?;

    parse_source_texts(unit, source_texts, &[])
}

fn parse_source_texts(
    unit: WitUnit,
    source_texts: Vec<WitSourceText>,
    rewrites: &[SourceRewrite],
) -> Result<ParsedWitPackageGroup, WitParseError> {
    let source_names = source_texts
        .iter()
        .map(|source| source.path.clone())
        .collect();
    let mut source_map = SourceMap::new();
    let mut source_mappings = Vec::with_capacity(source_texts.len());
    let mut foreign_references = Vec::new();
    let mut parser_size = 0usize;
    let mut package_name_location = None;
    let mut nested_package_locations = Vec::new();
    let mut named_result_function_spans = Vec::new();
    for source in &source_texts {
        let (parser_source, rewritten_to_original) =
            rewrite_source(source, rewrites).map_err(|_| WitParseError {
                kind: WitParseErrorKind::Parse(
                    "WIT source rewrite produced an invalid source range".to_owned(),
                ),
                location: None,
            })?;
        let scan = source_scan::scan_source(&parser_source);
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
            path: source.path.clone(),
            offset: parser_offset,
            parser_len: scan.parser_text.len(),
            parser_to_source: scan
                .parser_to_source
                .iter()
                .map(|offset| rewritten_to_original[*offset])
                .collect(),
        };
        if package_name_location.is_none() {
            package_name_location = scan
                .package_name_range
                .clone()
                .map(|range| source_location(source, &rewritten_to_original, range));
        }
        nested_package_locations.extend(
            scan.nested_package_ranges
                .into_iter()
                .map(|range| source_location(source, &rewritten_to_original, range)),
        );
        foreign_references.extend(
            source_scan::scan_foreign_references(&source.text)
                .into_iter()
                .map(|reference| WitForeignReference {
                    identity: reference.identity,
                    version: reference.version,
                    location: WitSourceLocation {
                        path: source.path.clone(),
                        range: reference.range,
                    },
                    version_range: reference.version_range,
                    item_range: reference.item_range,
                }),
        );
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
        source_map.push_str(&source.path, scan.parser_text);
        source_mappings.push(mapping);
    }

    match source_map.parse() {
        Ok(group) => Ok(ParsedWitPackageGroup {
            unit,
            group,
            source_names,
            source_texts,
            source_mappings,
            foreign_references,
            package_name_location,
            nested_package_locations,
            named_result_function_spans,
        }),
        Err((_, error)) => Err(map_parse_error(&source_mappings, error)),
    }
}

#[derive(Clone, Debug)]
struct SourceRewrite {
    path: String,
    range: Range<usize>,
    mapped_range: Range<usize>,
    replacement: String,
}

fn rewrite_source(
    source: &WitSourceText,
    rewrites: &[SourceRewrite],
) -> Result<(String, Vec<usize>), ()> {
    let mut source_rewrites = rewrites
        .iter()
        .filter(|rewrite| rewrite.path == source.path)
        .collect::<Vec<_>>();
    source_rewrites.sort_by(|left, right| {
        left.range
            .start
            .cmp(&right.range.start)
            .then_with(|| left.range.end.cmp(&right.range.end))
    });

    let mut text = String::with_capacity(source.text.len());
    let mut rewritten_to_original = vec![0];
    let mut source_cursor = 0;
    for rewrite in source_rewrites {
        if rewrite.range.start < source_cursor
            || rewrite.range.end < rewrite.range.start
            || rewrite.range.end > source.text.len()
            || !source.text.is_char_boundary(rewrite.range.start)
            || !source.text.is_char_boundary(rewrite.range.end)
            || rewrite.mapped_range.start > rewrite.mapped_range.end
            || rewrite.mapped_range.end > source.text.len()
            || !source.text.is_char_boundary(rewrite.mapped_range.start)
            || !source.text.is_char_boundary(rewrite.mapped_range.end)
        {
            return Err(());
        }
        append_original_mapping(
            &source.text,
            source_cursor..rewrite.range.start,
            &mut text,
            &mut rewritten_to_original,
        );
        append_replacement_mapping(
            &rewrite.replacement,
            rewrite.mapped_range.start,
            rewrite.mapped_range.end,
            &mut text,
            &mut rewritten_to_original,
        );
        source_cursor = rewrite.range.end;
    }
    append_original_mapping(
        &source.text,
        source_cursor..source.text.len(),
        &mut text,
        &mut rewritten_to_original,
    );

    Ok((text, rewritten_to_original))
}

fn append_original_mapping(
    source: &str,
    range: Range<usize>,
    output: &mut String,
    rewritten_to_original: &mut Vec<usize>,
) {
    *rewritten_to_original
        .last_mut()
        .expect("source map has an initial boundary") = range.start;
    output.push_str(&source[range.clone()]);
    rewritten_to_original.extend((range.start..range.end).map(|offset| offset + 1));
}

fn append_replacement_mapping(
    replacement: &str,
    source_start: usize,
    source_end: usize,
    output: &mut String,
    rewritten_to_original: &mut Vec<usize>,
) {
    *rewritten_to_original
        .last_mut()
        .expect("source map has an initial boundary") = source_start;
    output.push_str(replacement);
    rewritten_to_original.extend((0..replacement.len()).map(|offset| {
        if offset + 1 == replacement.len() {
            source_end
        } else {
            source_start
        }
    }));
}

fn source_location(
    source: &WitSourceText,
    rewritten_to_original: &[usize],
    range: Range<usize>,
) -> WitSourceLocation {
    WitSourceLocation {
        path: source.path.clone(),
        range: rewritten_to_original[range.start]..rewritten_to_original[range.end],
    }
}

/// Resolve and normalize each foreign WIT reference in a parsed package.
///
/// References are checked against active direct package edges, then rewritten
/// to the exact selected version before the parser group is returned. A
/// package's own qualified WIT references become local interface or world
/// references so the parser does not treat a package as depending on itself.
pub fn resolve_package_references(
    parsed: ParsedWitPackageGroup,
    graph: &WitPackageGraph,
) -> Result<ResolvedWitPackageGroup, WitReferenceResolutionError> {
    let unit = parsed.unit.clone();
    if let Some(location) = parsed.nested_package_locations.first() {
        return Err(WitReferenceResolutionError {
            kind: WitReferenceResolutionErrorKind::NestedPackage,
            location: Some(location.clone()),
        });
    }
    let actual_package_name = parsed.group.main.name.to_string();
    if actual_package_name != unit.canonical_name() {
        return Err(WitReferenceResolutionError {
            kind: WitReferenceResolutionErrorKind::PackageMismatch {
                expected: unit.canonical_name().to_owned(),
                actual: actual_package_name,
            },
            location: parsed.package_name_location.clone(),
        });
    }
    let source_by_path = parsed
        .source_texts
        .iter()
        .map(|source| (source.path.as_str(), source))
        .collect::<HashMap<_, _>>();
    let mut rewrites = Vec::with_capacity(parsed.foreign_references.len());
    let mut direct_dependencies = Vec::new();
    let mut seen_dependencies = HashSet::new();

    for reference in &parsed.foreign_references {
        let source = source_by_path
            .get(reference.location.path.as_str())
            .copied()
            .ok_or_else(|| WitReferenceResolutionError {
                kind: WitReferenceResolutionErrorKind::InvalidSourceRange,
                location: Some(reference.location.clone()),
            })?;
        let requested_version = reference
            .version
            .as_deref()
            .map(|version| {
                // SemVer build metadata has no precedence and SAI's canonical
                // package versions intentionally omit it. The WIT parser has
                // already validated the complete version token.
                Version::parse(
                    version
                        .split_once('+')
                        .map_or(version, |(without_build, _)| without_build),
                )
            })
            .transpose()
            .map_err(|_| WitReferenceResolutionError {
                kind: WitReferenceResolutionErrorKind::InvalidVersion(
                    reference.version.clone().unwrap_or_default(),
                ),
                location: Some(reference.location.clone()),
            })?;
        let target = graph
            .resolve_direct_reference(&unit, &reference.identity, requested_version.as_ref())
            .map_err(|error| WitReferenceResolutionError {
                kind: WitReferenceResolutionErrorKind::Reference(error),
                location: Some(reference.location.clone()),
            })?;

        let replacement_range;
        let replacement;
        if target.unit() == &unit {
            if reference.item_range.start < reference.location.range.start
                || reference.item_range.end > reference.location.range.end
                || reference.item_range.start > reference.item_range.end
            {
                return Err(WitReferenceResolutionError {
                    kind: WitReferenceResolutionErrorKind::InvalidSourceRange,
                    location: Some(reference.location.clone()),
                });
            }
            replacement_range = reference.location.range.clone();
            let mapped_range = reference.item_range.clone();
            replacement = source.text[reference.item_range.clone()].to_owned();
            rewrites.push(SourceRewrite {
                path: reference.location.path.clone(),
                range: replacement_range,
                mapped_range,
                replacement,
            });
            if target.unit() != &unit && seen_dependencies.insert(target.unit().clone()) {
                direct_dependencies.push(target.unit().clone());
            }
            continue;
        } else if let Some(version) = &reference.version {
            let version_range = reference
                .version_range
                .clone()
                .filter(|range| {
                    range.start >= reference.location.range.start
                        && range.end <= reference.location.range.end
                        && range.start < range.end
                        && source.text.get(range.clone()) == Some(version.as_str())
                })
                .ok_or_else(|| WitReferenceResolutionError {
                    kind: WitReferenceResolutionErrorKind::InvalidSourceRange,
                    location: Some(reference.location.clone()),
                })?;
            if version_range.start <= reference.location.range.start {
                return Err(WitReferenceResolutionError {
                    kind: WitReferenceResolutionErrorKind::InvalidSourceRange,
                    location: Some(reference.location.clone()),
                });
            }
            replacement_range = version_range;
            replacement = target.selected_version().to_string();
        } else {
            replacement_range = reference.location.range.end..reference.location.range.end;
            replacement = format!("@{}", target.selected_version());
        }
        rewrites.push(SourceRewrite {
            path: reference.location.path.clone(),
            mapped_range: replacement_range.clone(),
            range: replacement_range,
            replacement,
        });

        if target.unit() != &unit && seen_dependencies.insert(target.unit().clone()) {
            direct_dependencies.push(target.unit().clone());
        }
    }

    let normalized = parse_source_texts(unit, parsed.source_texts, &rewrites).map_err(|error| {
        let location = error.location().cloned();
        WitReferenceResolutionError {
            kind: WitReferenceResolutionErrorKind::Reparse(error),
            location,
        }
    })?;

    Ok(ResolvedWitPackageGroup {
        parsed: normalized,
        direct_dependencies,
    })
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
        ParsedWitPackageGroup, ResolvedWitPackageGroup, WitParseError,
        WitReferenceResolutionErrorKind, WitResolveContext, WitSourceFile, WitSourceLocation,
        WitUnit, parse_source_files, resolve_package_references,
    };
    use super::{WitPackageEdge, WitPackageGraph, WitPackageNode};
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

    fn package_parts(
        identity: &str,
        version: &str,
    ) -> (WitUnit, WitPackageNode, MvsPackageVersion) {
        let selected = MvsPackageVersion::new(
            identity,
            Version::parse(version).expect("valid test version"),
        )
        .expect("valid test package identity");
        let packages = PackageInterner::new();
        let key = format!("{}#{}", identity, selected.version().compatibility_line());
        let package = packages.intern(&key).expect("valid package key");
        let unit =
            WitUnit::dir(package, &packages, &selected).expect("unit matches selected version");
        let node =
            WitPackageNode::new(unit.clone(), selected.clone()).expect("node matches package");
        (unit, node, selected)
    }

    fn package_graph(
        root_identity: &str,
        root_version: &str,
        dependencies: &[(&str, &str)],
    ) -> (WitUnit, WitPackageGraph) {
        let (root, root_node, _) = package_parts(root_identity, root_version);
        let mut nodes = vec![root_node];
        let mut edges = Vec::new();
        for (identity, version) in dependencies {
            let (unit, node, selected) = package_parts(identity, version);
            nodes.push(node);
            edges.push(WitPackageEdge::new(
                root.clone(),
                *identity,
                selected,
                Some(unit),
            ));
        }
        let graph =
            WitPackageGraph::new(root.clone(), nodes, edges).expect("valid WIT package graph");
        (root, graph)
    }

    fn parse_for_unit(unit: WitUnit, path: &str, source: &str) -> ParsedWitPackageGroup {
        parse_source_files(unit, [WitSourceFile::new(path, source)])
            .expect("valid WIT source files")
    }

    fn resolved(
        unit: WitUnit,
        graph: &WitPackageGraph,
        path: &str,
        source: &str,
    ) -> ResolvedWitPackageGroup {
        resolve_package_references(parse_for_unit(unit, path, source), graph)
            .expect("foreign WIT references resolve")
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
    fn resolves_references_in_first_use_order_and_normalizes_selected_versions() {
        let (root, graph) = package_graph(
            "acme:root",
            "1.0.0",
            &[("acme:first", "1.4.12"), ("acme:second", "0.9.3")],
        );
        let source = "\
package acme:root@1.0.0;
world app {
    import acme:first/api@1.2.0;
    import acme:second/base;
    export acme:first/other@1.4.12;
}
";

        let resolved = resolved(root, &graph, "root.wit", source);

        assert_eq!(
            resolved
                .direct_dependencies()
                .iter()
                .map(WitUnit::canonical_name)
                .collect::<Vec<_>>(),
            ["acme:first@1.4.12", "acme:second@0.9.3"]
        );
        assert_eq!(
            resolved
                .parsed()
                .foreign_references()
                .iter()
                .map(|reference| reference.version.as_deref())
                .collect::<Vec<_>>(),
            [Some("1.2.0"), None, Some("1.4.12")]
        );
        let package_names = resolved
            .parsed
            .group
            .main
            .foreign_deps
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(package_names, ["acme:first@1.4.12", "acme:second@0.9.3"]);
    }

    #[test]
    fn reports_version_and_missing_dependency_errors_at_original_utf8_ranges() {
        let (root, graph) = package_graph("acme:root", "1.0.0", &[("acme:dep", "1.4.0")]);
        let source =
            "// 日本語\npackage acme:root@1.0.0;\nworld app { import acme:dep/api@1.5.0; }";
        let error =
            resolve_package_references(parse_for_unit(root.clone(), "version.wit", source), &graph)
                .expect_err("a newer requested package version is invalid");
        let location = error.location().expect("version error has a source range");
        assert_eq!(location.path, "version.wit");
        assert_eq!(&source[location.range.clone()], "acme:dep/api@1.5.0");
        assert!(error.to_string().contains("newer than the build-provided"));

        let source = "package acme:root@1.0.0;\nworld app { import acme:missing/api; }";
        let error = resolve_package_references(parse_for_unit(root, "missing.wit", source), &graph)
            .expect_err("undeclared package references are rejected");
        let location = error
            .location()
            .expect("missing reference has a source range");
        assert_eq!(&source[location.range.clone()], "acme:missing/api");
        assert!(
            error
                .to_string()
                .contains("add a dependency on `acme:missing`")
        );
    }

    #[test]
    fn accepts_build_metadata_and_comments_between_at_and_version() {
        let (root, graph) = package_graph("acme:root", "1.0.0", &[("acme:dep", "1.4.123")]);
        let source = "\
package acme:root@1.0.0;
world app {
    import acme:dep/api@ /* selected version */ 1.4.123+build.7;
}
";
        let resolved = resolved(root, &graph, "version.wit", source);

        assert_eq!(
            resolved.direct_dependencies(),
            &[graph
                .nodes()
                .iter()
                .find(|node| node.package_identity() == "acme:dep")
                .expect("graph contains the dependency")
                .unit()
                .clone()]
        );
        assert_eq!(
            resolved
                .parsed
                .group
                .main
                .foreign_deps
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["acme:dep@1.4.123"]
        );
        let reference = &resolved.parsed.foreign_references()[0];
        assert_eq!(reference.version.as_deref(), Some("1.4.123+build.7"));
        assert_eq!(
            &source[reference.location.range.clone()],
            "acme:dep/api@ /* selected version */ 1.4.123+build.7"
        );
    }

    #[test]
    fn validates_package_declaration_before_foreign_references() {
        let (root, graph) = package_graph("acme:root", "1.0.0", &[]);
        let source = "\
package acme:other@1.0.0;
world app { import acme:missing/api; }
";
        let error = resolve_package_references(
            parse_for_unit(root.clone(), "mismatch.wit", source),
            &graph,
        )
        .expect_err("package identity mismatch precedes missing references");
        assert!(matches!(
            error.kind(),
            WitReferenceResolutionErrorKind::PackageMismatch { .. }
        ));
        let location = error.location().expect("package mismatch is located");
        assert_eq!(location.path, "mismatch.wit");
        assert_eq!(&source[location.range.clone()], "acme:other@1.0.0");

        let source = "\
package acme:root@1.0.0;
package acme:nested { interface hidden {} }
world app { import acme:missing/api; }
";
        let error = resolve_package_references(parse_for_unit(root, "nested.wit", source), &graph)
            .expect_err("nested package is reported before foreign references");
        assert!(matches!(
            error.kind(),
            WitReferenceResolutionErrorKind::NestedPackage
        ));
        let location = error.location().expect("nested package is located");
        assert_eq!(location.path, "nested.wit");
        assert_eq!(&source[location.range.clone()], "acme:nested");
    }

    #[test]
    fn normalizes_qualified_self_references_to_local_wit_items() {
        let (root, graph) = package_graph("acme:root", "1.0.0", &[]);
        let source = "\
package acme:root@1.0.0;
interface api { type value = u32; }
interface client { use acme:root/api.{value}; }
world app { import acme:root/api; include acme:root/base; }
world base {}
";
        let resolved = resolved(root, &graph, "self.wit", source);

        assert!(resolved.direct_dependencies().is_empty());
        assert!(
            resolved.parsed.group.main.foreign_deps.is_empty(),
            "self references must be parsed as local WIT items"
        );
    }

    #[test]
    fn preserves_source_byte_mappings_after_version_normalization() {
        let (root, graph) = package_graph("acme:root", "1.0.0", &[("acme:dep", "1.4.123")]);
        let source = "\
// 日本語
package acme:root@1.0.0;
world app { import acme:dep/api@1.2.0; }
interface api { read: func() -> (left: u32, right: u32); }
";
        let resolved = resolved(root, &graph, "mapped.wit", source);
        let mapping = &resolved.parsed.source_mappings[0];

        assert_eq!(mapping.parser_to_source.len(), mapping.parser_len + 1);
        assert!(
            mapping
                .parser_to_source
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
        let original_reference = source
            .find("acme:dep/api@1.2.0")
            .expect("source contains the foreign reference");
        let original_version_start = original_reference + "acme:dep/api@".len();
        let normalized_version_end = original_version_start + "1.4.123".len();
        assert_eq!(
            mapping.parser_to_source[original_version_start],
            original_version_start
        );
        assert_eq!(
            mapping.parser_to_source[normalized_version_end],
            original_version_start + "1.2.0".len()
        );
        assert!(
            resolved
                .parsed
                .group
                .main
                .foreign_deps
                .keys()
                .any(|name| name.to_string() == "acme:dep@1.4.123")
        );
        let named_result_span = resolved
            .parsed
            .named_result_function_spans
            .first()
            .expect("named result function is retained");
        let original_function_range = mapping.parser_to_source[named_result_span.start as usize]
            ..mapping.parser_to_source[named_result_span.end as usize];
        assert_eq!(&source[original_function_range], "read");
    }

    #[test]
    fn normalized_foreign_packages_push_after_the_selected_dependency() {
        let (root, graph) = package_graph("acme:root", "1.0.0", &[("acme:dep", "1.4.12")]);
        let dependency = graph
            .nodes()
            .iter()
            .find(|node| node.package_identity() == "acme:dep")
            .expect("graph contains the selected dependency")
            .unit()
            .clone();
        let dependency_source = "package acme:dep@1.4.12;\ninterface api { type value = u32; }";
        let dependency_package = parse_for_unit(dependency, "dependency.wit", dependency_source);

        let root_source = "\
package acme:root@1.0.0;
world app { import acme:dep/api@1.2.0; }
";
        let normalized_root = resolved(root, &graph, "root.wit", root_source);
        let (parsed_root, direct_dependencies) = normalized_root.into_parts();

        let mut resolve = wit_parser::Resolve::default();
        let mut context = WitResolveContext::default();
        context
            .push_package(&mut resolve, dependency_package)
            .expect("selected dependency package parses");
        context
            .push_package(&mut resolve, parsed_root)
            .expect("normalized foreign package name resolves");

        assert_eq!(direct_dependencies.len(), 1);
        assert_eq!(resolve.packages.len(), 2);
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
