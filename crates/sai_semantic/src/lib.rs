#![forbid(unsafe_code)]

//! Stable symbol identifiers used by semantic tooling.

mod canonical_path;
mod index_journal;
mod symbol_index;

pub use index_journal::{
    FileState, IndexJournalCodecError, IndexJournalOrigin, IndexJournalState,
    IndexOperationJournal, IndexSnapshot, MAX_OPERATION_JOURNAL_BYTES, PackageFileState,
    SourceFileChange, SymbolIdMapping,
};
pub use symbol_index::{
    EncodedSymbolIndex, PackageIndexFileName, PackageIndexFileRecord, SymbolIndex,
    SymbolIndexCodecError, SymbolIndexEntry, SymbolKind, UnvalidatedSymbolIndex,
};

use sai_stable_hash::StableHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const HEX: &[u8; 16] = b"0123456789ABCDEF";
const SYMBOL_ID_SPACE: usize = 1 << 24;

/// The six-uppercase-hex-digit portion of a [`SymbolId`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolHex([u8; 3]);

impl SymbolHex {
    pub const fn from_bytes(bytes: [u8; 3]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(self) -> [u8; 3] {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, SymbolIdError> {
        let bytes = value.as_bytes();
        if bytes.len() != 6
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(byte))
        {
            return Err(SymbolIdError::InvalidHex);
        }

        Ok(Self([
            (hex_value(bytes[0]) << 4) | hex_value(bytes[1]),
            (hex_value(bytes[2]) << 4) | hex_value(bytes[3]),
            (hex_value(bytes[4]) << 4) | hex_value(bytes[5]),
        ]))
    }

    fn from_u24(value: u32) -> Self {
        Self([
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        ])
    }
}

impl fmt::Display for SymbolHex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            formatter.write_str(
                std::str::from_utf8(&[HEX[(byte >> 4) as usize], HEX[(byte & 0x0F) as usize]])
                    .expect("hexadecimal digits are UTF-8"),
            )?;
        }
        Ok(())
    }
}

/// A URI-form symbol reference.
///
/// The package and encoded path are descriptive text. The semantic identity
/// used for lookup is the containing package node plus [`SymbolHex`].
///
/// This is a parsed/formatted reference, not an identity key: it intentionally
/// does not implement `Eq`, `Hash`, or ordering because its path can change
/// while the symbol identity stays the same, and its URI omits the package
/// node's compatibility line.
#[derive(Clone, Debug)]
pub struct SymbolId {
    package_id: Box<str>,
    encoded_path_segments: Vec<Box<str>>,
    hex: SymbolHex,
    rendered: Box<str>,
}

impl SymbolId {
    /// Constructs an ID from a package identity, structured path components,
    /// and the stable hexadecimal identity.
    ///
    /// Callers supply path components rather than splitting a rendered
    /// canonical path: a component such as `impl[I]` can itself contain `/`.
    /// The components must be the corresponding parts of a validated
    /// canonical path; this constructor only validates and encodes their URI
    /// representation.
    pub fn new(
        package_id: &str,
        path_segments: &[impl AsRef<str>],
        hex: SymbolHex,
    ) -> Result<Self, SymbolIdError> {
        if !is_valid_package_id(&package_id) {
            return Err(SymbolIdError::InvalidPackageId);
        }
        if path_segments.is_empty() {
            return Err(SymbolIdError::MissingPath);
        }
        if path_segments
            .iter()
            .any(|segment| segment.as_ref().is_empty())
        {
            return Err(SymbolIdError::EmptyPathSegment);
        }

        let encoded_path_segments = path_segments
            .iter()
            .map(|segment| percent_encode(segment.as_ref()).into_boxed_str())
            .collect::<Vec<_>>();
        Ok(Self::from_encoded_parts(
            package_id.into(),
            encoded_path_segments,
            hex,
        ))
    }

    /// Parses a symbol ID in the normative URI form.
    ///
    /// Percent escapes accept either hexadecimal case as permitted by RFC
    /// 3986 and are normalized to uppercase in the returned representation.
    pub fn parse(value: &str) -> Result<Self, SymbolIdError> {
        let body = value
            .strip_prefix("symbol://")
            .ok_or(SymbolIdError::InvalidPrefix)?;
        let (path, raw_hex) = body.split_once('#').ok_or(SymbolIdError::InvalidHex)?;
        let hex = SymbolHex::parse(raw_hex)?;

        let separator = path.find('/').ok_or(SymbolIdError::MissingPath)?;
        let package_authority = &path[..separator];
        let package_id = decode_uri_component(package_authority, is_reg_name_char)
            .ok_or(SymbolIdError::InvalidPackageId)?;
        if !is_valid_package_id(&package_id) {
            return Err(SymbolIdError::InvalidPackageId);
        }

        let raw_segments = &path[separator + 1..];
        let mut decoded_path_segments = Vec::new();
        for segment in raw_segments.split('/') {
            if segment.is_empty() {
                return Err(SymbolIdError::EmptyPathSegment);
            }
            if !is_valid_uri_segment(segment) {
                return Err(SymbolIdError::InvalidPathSegment);
            }
            decoded_path_segments.push(
                decode_uri_component(segment, is_pchar).ok_or(SymbolIdError::InvalidPathSegment)?,
            );
        }
        let encoded_path_segments = decoded_path_segments
            .iter()
            .map(|segment| percent_encode(segment).into_boxed_str())
            .collect();

        Ok(Self::from_encoded_parts(
            package_id.into_boxed_str(),
            encoded_path_segments,
            hex,
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.rendered
    }

    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Returns path segments in their normalized URI-encoded form.
    pub fn encoded_path_segments(&self) -> &[Box<str>] {
        &self.encoded_path_segments
    }

    pub const fn hex(&self) -> SymbolHex {
        self.hex
    }

    fn from_encoded_parts(
        package_id: Box<str>,
        encoded_path_segments: Vec<Box<str>>,
        hex: SymbolHex,
    ) -> Self {
        let mut rendered = String::from("symbol://");
        rendered.push_str(&percent_encode_package_id(&package_id));
        for segment in &encoded_path_segments {
            rendered.push('/');
            rendered.push_str(segment);
        }
        rendered.push('#');
        rendered.push_str(&hex.to_string());

        Self {
            package_id,
            encoded_path_segments,
            hex,
            rendered: rendered.into_boxed_str(),
        }
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.rendered)
    }
}

/// Errors produced while parsing or constructing a [`SymbolId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolIdError {
    InvalidPrefix,
    InvalidPackageId,
    MissingPath,
    EmptyPathSegment,
    InvalidPathSegment,
    InvalidHex,
}

impl fmt::Display for SymbolIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPrefix => "symbol ID must start with `symbol://`",
            Self::InvalidPackageId => "symbol ID has an invalid package identity",
            Self::MissingPath => "symbol ID must name at least one path component",
            Self::EmptyPathSegment => "symbol ID path components must not be empty",
            Self::InvalidPathSegment => "symbol ID contains an invalid URI path component",
            Self::InvalidHex => "symbol ID must end with six uppercase hexadecimal digits",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SymbolIdError {}

/// Errors produced while assigning fresh symbol identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolAllocationError {
    InvalidPackageId,
    EmptyCanonicalPath,
    CanonicalPathOutsidePackage,
    DuplicateCanonicalPath,
    SymbolSpaceExhausted,
    ProbeSpaceExhausted,
}

impl fmt::Display for SymbolAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPackageId => "symbol allocation received an invalid package identity",
            Self::EmptyCanonicalPath => "symbol allocation received an empty canonical path",
            Self::CanonicalPathOutsidePackage => {
                "symbol allocation received a path outside the requested package"
            }
            Self::DuplicateCanonicalPath => "symbol allocation received a duplicate canonical path",
            Self::SymbolSpaceExhausted => "symbol allocation exhausted the 24-bit ID space",
            Self::ProbeSpaceExhausted => "symbol allocation exhausted the u32 probe space",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SymbolAllocationError {}

/// Allocates stable hexadecimal IDs for fresh symbols in one package node.
///
/// Paths are sorted by UTF-8 byte order before allocation, independent of
/// input iteration order. Values in `reserved` are unavailable to every
/// fresh symbol, including when the current path hashes to that value.
/// Every path must be a canonical symbol path in `package_id`; the package
/// boundary (`.` for an item or `/` for a module path) is checked here, while
/// the semantic inventory supplies paths that have already passed the full
/// canonical-path grammar and symbol-kind checks.
pub fn allocate_fresh(
    package_id: &str,
    canonical_paths: impl IntoIterator<Item = impl AsRef<str>>,
    reserved: &BTreeSet<SymbolHex>,
) -> Result<BTreeMap<String, SymbolHex>, SymbolAllocationError> {
    if !is_valid_package_id(package_id) {
        return Err(SymbolAllocationError::InvalidPackageId);
    }

    let mut paths = BTreeSet::new();
    for path in canonical_paths {
        let path = path.as_ref();
        if path.is_empty() {
            return Err(SymbolAllocationError::EmptyCanonicalPath);
        }
        let Some(package_suffix) = path.strip_prefix(package_id) else {
            return Err(SymbolAllocationError::CanonicalPathOutsidePackage);
        };
        if !matches!(package_suffix.as_bytes().first(), Some(b'.' | b'/')) {
            return Err(SymbolAllocationError::CanonicalPathOutsidePackage);
        }
        if !paths.insert(path.to_owned()) {
            return Err(SymbolAllocationError::DuplicateCanonicalPath);
        }
    }

    allocate_fresh_with(paths, reserved, |path, probe| {
        symbol_seed(package_id, path, probe)
    })
}

fn allocate_fresh_with(
    paths: BTreeSet<String>,
    reserved: &BTreeSet<SymbolHex>,
    seed: impl FnMut(&str, u32) -> SymbolHex,
) -> Result<BTreeMap<String, SymbolHex>, SymbolAllocationError> {
    allocate_fresh_with_capacity(paths, reserved, SYMBOL_ID_SPACE, seed)
}

fn allocate_fresh_with_capacity(
    paths: BTreeSet<String>,
    reserved: &BTreeSet<SymbolHex>,
    id_capacity: usize,
    mut seed: impl FnMut(&str, u32) -> SymbolHex,
) -> Result<BTreeMap<String, SymbolHex>, SymbolAllocationError> {
    if paths.len() > id_capacity.saturating_sub(reserved.len()) {
        return Err(SymbolAllocationError::SymbolSpaceExhausted);
    }

    let mut allocated = BTreeSet::new();
    let mut result = BTreeMap::new();

    for path in paths {
        let mut probe = 0_u32;
        let hex = loop {
            let candidate = seed(&path, probe);
            if !reserved.contains(&candidate) && !allocated.contains(&candidate) {
                break candidate;
            }
            probe = probe
                .checked_add(1)
                .ok_or(SymbolAllocationError::ProbeSpaceExhausted)?;
        };

        allocated.insert(hex);
        result.insert(path, hex);
    }

    Ok(result)
}

fn symbol_seed(package_id: &str, canonical_path: &str, probe: u32) -> SymbolHex {
    let mut hasher = StableHasher::new();
    hasher.write_raw_bytes(b"sai-symbol\0");
    hasher.write_raw_bytes(package_id.as_bytes());
    hasher.write_raw_bytes(&[0]);
    hasher.write_raw_bytes(canonical_path.as_bytes());
    hasher.write_raw_bytes(&[0]);
    hasher.write_u32(probe);

    SymbolHex::from_u24((hasher.finish().raw() >> 104) as u32)
}

fn is_valid_package_id(value: &str) -> bool {
    let Some((namespace, package)) = value.split_once(':') else {
        return false;
    };
    !package.contains(':')
        && is_valid_package_segment(namespace)
        && is_valid_package_segment(package)
}

fn is_valid_package_segment(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return false;
    }

    let mut previous_was_hyphen = false;
    for (index, byte) in bytes.iter().copied().enumerate().skip(1) {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_was_hyphen = false;
        } else if byte == b'-' && !previous_was_hyphen && index + 1 < bytes.len() {
            previous_was_hyphen = true;
        } else {
            return false;
        }
    }
    true
}

fn percent_encode(component: &str) -> String {
    let mut encoded = String::with_capacity(component.len());
    for byte in component.bytes() {
        if is_pchar(byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0F) as usize]));
        }
    }
    encoded
}

fn is_valid_uri_segment(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else if is_pchar(bytes[index]) {
            index += 1;
        } else {
            return false;
        }
    }
    !bytes.is_empty()
}

fn decode_uri_component(value: &str, allowed_unescaped: fn(u8) -> bool) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return None;
            }
            decoded.push(
                (hex_value_any_case(bytes[index + 1]) << 4) | hex_value_any_case(bytes[index + 2]),
            );
            index += 3;
        } else if allowed_unescaped(bytes[index]) {
            decoded.push(bytes[index]);
            index += 1;
        } else {
            return None;
        }
    }
    String::from_utf8(decoded).ok()
}

fn percent_encode_package_id(package_id: &str) -> String {
    let (namespace, package) = package_id
        .split_once(':')
        .expect("package ID was validated before formatting");
    format!("{namespace}%3A{package}")
}

fn is_pchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
        )
}

fn is_reg_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
        )
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!("validated uppercase hexadecimal input"),
    }
}

fn hex_value_any_case(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("validated hexadecimal input"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SymbolAllocationError, SymbolHex, SymbolId, SymbolIdError, allocate_fresh,
        allocate_fresh_with, is_pchar,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn hex(value: &str) -> SymbolHex {
        SymbolHex::parse(value).expect("valid symbol hex")
    }

    #[test]
    fn formats_the_normative_symbol_id_example() {
        let id = SymbolId::new("acme:user", &["User", "create"], hex("8F21A7"))
            .expect("valid symbol ID");

        assert_eq!(id.as_str(), "symbol://acme%3Auser/User/create#8F21A7");
        assert_eq!(id.to_string(), id.as_str());
        assert_eq!(id.package_id(), "acme:user");
        assert_eq!(
            id.encoded_path_segments()
                .iter()
                .map(Box::as_ref)
                .collect::<Vec<_>>(),
            ["User", "create"]
        );
        assert_eq!(id.hex(), hex("8F21A7"));
    }

    #[test]
    fn percent_encodes_components_without_splitting_embedded_slashes() {
        let field = SymbolId::new(
            "acme:shop",
            &["model", "Order", "field[total]"],
            hex("3C0E91"),
        )
        .expect("valid field symbol");
        assert_eq!(
            field.as_str(),
            "symbol://acme%3Ashop/model/Order/field%5Btotal%5D#3C0E91"
        );

        let implementation = SymbolId::new(
            "acme:shop",
            &[
                "model",
                "impl[sai:core/fmt.Format for acme:shop/model.Order]",
                "fmt",
            ],
            hex("D41B07"),
        )
        .expect("valid implementation symbol");
        assert_eq!(
            implementation.as_str(),
            "symbol://acme%3Ashop/model/impl%5Bsai:core%2Ffmt.Format%20for%20acme:shop%2Fmodel.Order%5D/fmt#D41B07"
        );
    }

    #[test]
    fn package_id_validation_allows_reserved_toolchain_and_wit_names() {
        for package_id in ["sai:core", "sai:alloc", "wasi:http", "acme:my-package"] {
            assert!(
                SymbolId::new(package_id, &["item"], hex("00000A")).is_ok(),
                "{package_id}"
            );
        }
        for package_id in [
            "",
            "acme",
            ":name",
            "acme:",
            "acme:name:extra",
            "Acme:name",
            "acme:two--hyphens",
            "acme:trailing-",
            "acme:under_score",
            "acme:é",
        ] {
            assert_eq!(
                SymbolId::new(package_id, &["item"], hex("00000A")).unwrap_err(),
                SymbolIdError::InvalidPackageId,
                "{package_id}"
            );
        }
    }

    #[test]
    fn parses_uri_segments_and_uppercase_identity_hex() {
        let id = SymbolId::parse(
            "symbol://acme%3Ashop/model/impl%5Bsai:core%2Ffmt.Format%20for%20acme:shop%2Fmodel.Order%5D/fmt#D41B07",
        )
        .expect("valid ID");
        assert_eq!(id.package_id(), "acme:shop");
        assert_eq!(
            id.encoded_path_segments()
                .iter()
                .map(Box::as_ref)
                .collect::<Vec<_>>(),
            [
                "model",
                "impl%5Bsai:core%2Ffmt.Format%20for%20acme:shop%2Fmodel.Order%5D",
                "fmt"
            ]
        );
        assert_eq!(
            id.as_str(),
            "symbol://acme%3Ashop/model/impl%5Bsai:core%2Ffmt.Format%20for%20acme:shop%2Fmodel.Order%5D/fmt#D41B07"
        );

        let lowercase_escape = SymbolId::parse("symbol://acme%3ashop/field%5bwidth%5d#00FF0A")
            .expect("RFC 3986 permits either case in percent escapes");
        assert_eq!(
            lowercase_escape.as_str(),
            "symbol://acme%3Ashop/field%5Bwidth%5D#00FF0A"
        );
        assert_eq!(lowercase_escape.package_id(), "acme:shop");
    }

    #[test]
    fn rejects_malformed_symbol_ids() {
        for (value, expected) in [
            ("acme:user/User#create#8F21A7", SymbolIdError::InvalidPrefix),
            ("symbol://acme%3Auser#8F21A7", SymbolIdError::MissingPath),
            (
                "symbol://acme%3Auser/#8F21A7",
                SymbolIdError::EmptyPathSegment,
            ),
            (
                "symbol://acme%3Auser/a//b#8F21A7",
                SymbolIdError::EmptyPathSegment,
            ),
            (
                "symbol://acme%3Auser/a?b#8F21A7",
                SymbolIdError::InvalidPathSegment,
            ),
            (
                "symbol://acme%3Auser/a%2#8F21A7",
                SymbolIdError::InvalidPathSegment,
            ),
            ("symbol://acme%3Auser/a#8f21a7", SymbolIdError::InvalidHex),
            ("symbol://acme%3Auser/a#8F21A70", SymbolIdError::InvalidHex),
            (
                "symbol://acme%3Aname%3Aextra/a#8F21A7",
                SymbolIdError::InvalidPackageId,
            ),
            (
                "symbol://acme:user/a#8F21A7",
                SymbolIdError::InvalidPackageId,
            ),
        ] {
            assert_eq!(SymbolId::parse(value).unwrap_err(), expected, "{value}");
        }
    }

    #[test]
    fn requires_a_component_when_constructing_a_symbol_id() {
        assert_eq!(
            SymbolId::new("acme:user", &[] as &[&str], hex("8F21A7")).unwrap_err(),
            SymbolIdError::MissingPath
        );
        assert_eq!(
            SymbolId::new("acme:user", &["User", ""], hex("8F21A7")).unwrap_err(),
            SymbolIdError::EmptyPathSegment
        );
    }

    #[test]
    fn symbol_hex_is_fixed_width_uppercase() {
        assert_eq!(hex("00000A").to_string(), "00000A");
        assert_eq!(hex("ABCDEF").as_bytes(), [0xAB, 0xCD, 0xEF]);
        for invalid in ["", "00000", "0000000", "abcdef", "ABCDEG"] {
            assert_eq!(SymbolHex::parse(invalid), Err(SymbolIdError::InvalidHex));
        }
    }

    #[test]
    fn percent_encoder_preserves_only_rfc_3986_pchar() {
        for byte in 0_u8..=u8::MAX {
            assert_eq!(
                is_pchar(byte),
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'-' | b'.'
                            | b'_'
                            | b'~'
                            | b'!'
                            | b'$'
                            | b'&'
                            | b'\''
                            | b'('
                            | b')'
                            | b'*'
                            | b'+'
                            | b','
                            | b';'
                            | b'='
                            | b':'
                            | b'@'
                    )
            );
        }
        let id = SymbolId::new("acme:shop", &["%A /雪"], hex("00000A")).unwrap();
        assert_eq!(
            id.as_str(),
            "symbol://acme%3Ashop/%25A%20%2F%E9%9B%AA#00000A"
        );
    }

    #[test]
    fn fresh_allocation_uses_canonical_path_byte_order_not_input_order() {
        let first = allocate_fresh(
            "acme:user",
            ["acme:user.Z", "acme:user.a", "acme:user.é"],
            &BTreeSet::new(),
        )
        .expect("valid paths");
        let reversed = allocate_fresh(
            "acme:user",
            ["acme:user.é", "acme:user.a", "acme:user.Z"],
            &BTreeSet::new(),
        )
        .expect("valid paths");

        assert_eq!(first, reversed);
        assert_eq!(
            first.keys().map(String::as_str).collect::<Vec<_>>(),
            ["acme:user.Z", "acme:user.a", "acme:user.é"]
        );
    }

    #[test]
    fn fresh_allocation_skips_reserved_and_colliding_ids_in_sorted_order() {
        let paths = ["b", "a"].into_iter().map(str::to_owned).collect();
        let reserved = BTreeSet::from([SymbolHex::from_bytes([0, 0, 0])]);
        let allocated =
            allocate_fresh_with(paths, &reserved, |_, probe| SymbolHex::from_u24(probe))
                .expect("the probe sequence has available values");

        assert_eq!(
            allocated,
            BTreeMap::from([
                ("a".to_owned(), SymbolHex::from_bytes([0, 0, 1])),
                ("b".to_owned(), SymbolHex::from_bytes([0, 0, 2])),
            ])
        );
    }

    #[test]
    fn fresh_allocation_reports_exhausted_identifier_space_before_probing() {
        let paths = ["a"].into_iter().map(str::to_owned).collect();
        let reserved = BTreeSet::from([
            SymbolHex::from_bytes([0, 0, 0]),
            SymbolHex::from_bytes([0, 0, 1]),
        ]);
        let mut seed_called = false;

        assert_eq!(
            super::allocate_fresh_with_capacity(paths, &reserved, 2, |_, _| {
                seed_called = true;
                SymbolHex::from_bytes([0, 0, 2])
            }),
            Err(SymbolAllocationError::SymbolSpaceExhausted)
        );
        assert!(
            !seed_called,
            "a full ID space must not probe up to u32::MAX"
        );
    }

    #[test]
    fn fresh_allocation_rejects_duplicate_and_empty_canonical_paths() {
        assert_eq!(
            allocate_fresh(
                "acme:user",
                ["acme:user.F", "acme:user.F"],
                &BTreeSet::new()
            ),
            Err(SymbolAllocationError::DuplicateCanonicalPath)
        );
        assert_eq!(
            allocate_fresh("acme:user", [""], &BTreeSet::new()),
            Err(SymbolAllocationError::EmptyCanonicalPath)
        );
        assert_eq!(
            allocate_fresh("not-a-package", ["some.path"], &BTreeSet::new()),
            Err(SymbolAllocationError::InvalidPackageId)
        );
    }

    #[test]
    fn fresh_allocation_rejects_paths_outside_the_requested_package() {
        let reserved = BTreeSet::new();
        for path in ["acme:user", "acme:users.Item", "other:pkg.Item"] {
            assert_eq!(
                allocate_fresh("acme:user", [path], &reserved),
                Err(SymbolAllocationError::CanonicalPathOutsidePackage),
                "{path:?}"
            );
        }
        assert!(allocate_fresh("acme:user", ["acme:user.Item"], &reserved).is_ok());
        assert!(allocate_fresh("acme:user", ["acme:user/module.Item"], &reserved).is_ok());
    }

    #[test]
    fn seed_matches_the_fixed_siphash_1_3_vector() {
        assert_eq!(
            super::symbol_seed("acme:user", "acme:user.User.create", 0),
            hex("287B7E")
        );
    }
}
