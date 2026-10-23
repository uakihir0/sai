use crate::canonical_path::is_canonical_symbol_path;
use sai_stable_hash::{ContentHash, Fingerprint, hash_bytes};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const INDEX_FORMAT: &str = "sai-index";
const INDEX_VERSION: u32 = 1;

/// A canonical package-index basename such as `acme.user.0.4.jsonl`.
///
/// This value is a logical basename only, not a filesystem safety check.
/// Callers must still validate the index directory and the opened file's
/// type, and use no-follow operations when interacting with the filesystem.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageIndexFileName {
    namespace: Box<str>,
    package: Box<str>,
    compatibility_line: Box<str>,
    rendered: Box<str>,
}

impl PackageIndexFileName {
    /// Parses a basename and validates its package identity and compatibility
    /// line grammar. Separators, traversal components, reserved package
    /// identities, and noncanonical line spellings are rejected.
    pub fn parse(value: &str) -> Result<Self, SymbolIndexCodecError> {
        let body = value
            .strip_suffix(".jsonl")
            .ok_or(SymbolIndexCodecError::InvalidPackageFileName)?;
        let components = body.split('.').collect::<Vec<_>>();
        if components.len() < 3 {
            return Err(SymbolIndexCodecError::InvalidPackageFileName);
        }

        let namespace = components[0];
        let package = components[1];
        let compatibility_line = components[2..].join(".");
        if !is_valid_package_segment(namespace)
            || !is_valid_package_segment(package)
            || !is_valid_compatibility_line(&compatibility_line)
            || namespace == "wasi"
            || (namespace == "sai" && matches!(package, "core" | "alloc"))
        {
            return Err(SymbolIndexCodecError::InvalidPackageFileName);
        }

        let rendered = format!("{namespace}.{package}.{compatibility_line}.jsonl");
        if rendered != value {
            return Err(SymbolIndexCodecError::InvalidPackageFileName);
        }

        Ok(Self {
            namespace: namespace.into(),
            package: package.into(),
            compatibility_line: compatibility_line.into_boxed_str(),
            rendered: rendered.into_boxed_str(),
        })
    }

    /// Builds a filename from validated components.
    pub fn new(
        namespace: &str,
        package: &str,
        compatibility_line: &str,
    ) -> Result<Self, SymbolIndexCodecError> {
        Self::parse(&format!("{namespace}.{package}.{compatibility_line}.jsonl"))
    }

    /// Returns the canonical basename.
    pub fn as_str(&self) -> &str {
        &self.rendered
    }

    /// Returns the package identity without its compatibility line.
    pub fn package_id(&self) -> String {
        format!("{}:{}", self.namespace, self.package)
    }

    /// Returns the canonical compatibility line.
    pub fn compatibility_line(&self) -> &str {
        &self.compatibility_line
    }
}

impl fmt::Display for PackageIndexFileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.rendered)
    }
}

/// The closed set of symbol kinds persisted by semantic tooling.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SymbolKind {
    #[serde(rename = "fn")]
    Fn,
    #[serde(rename = "assoc_fn")]
    AssocFn,
    #[serde(rename = "extern_fn")]
    ExternFn,
    #[serde(rename = "record")]
    Record,
    #[serde(rename = "alias")]
    Alias,
    #[serde(rename = "refined")]
    Refined,
    #[serde(rename = "enum")]
    Enum,
    #[serde(rename = "distinct")]
    Distinct,
    #[serde(rename = "resource")]
    Resource,
    #[serde(rename = "variant")]
    Variant,
    #[serde(rename = "field")]
    Field,
    #[serde(rename = "trait")]
    Trait,
    #[serde(rename = "assoc_type")]
    AssocType,
    #[serde(rename = "impl")]
    Impl,
    #[serde(rename = "const")]
    Const,
    #[serde(rename = "effect")]
    Effect,
    #[serde(rename = "interface")]
    Interface,
    #[serde(rename = "world")]
    World,
    #[serde(rename = "generic_param")]
    GenericParam,
    #[serde(rename = "test")]
    Test,
    #[serde(rename = "property")]
    Property,
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Fn => "fn",
            Self::AssocFn => "assoc_fn",
            Self::ExternFn => "extern_fn",
            Self::Record => "record",
            Self::Alias => "alias",
            Self::Refined => "refined",
            Self::Enum => "enum",
            Self::Distinct => "distinct",
            Self::Resource => "resource",
            Self::Variant => "variant",
            Self::Field => "field",
            Self::Trait => "trait",
            Self::AssocType => "assoc_type",
            Self::Impl => "impl",
            Self::Const => "const",
            Self::Effect => "effect",
            Self::Interface => "interface",
            Self::World => "world",
            Self::GenericParam => "generic_param",
            Self::Test => "test",
            Self::Property => "property",
        };
        formatter.write_str(value)
    }
}

/// One persisted symbol mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolIndexEntry {
    hex: super::SymbolHex,
    def_path_hash: Fingerprint,
    path: Box<str>,
    kind: SymbolKind,
    fingerprint: Fingerprint,
}

impl SymbolIndexEntry {
    /// Creates a storage record after validating canonical-path syntax.
    /// Package ownership is checked when the entry is added to an index, and
    /// path resolution and kind agreement are checked against its inventory.
    pub fn new(
        hex: super::SymbolHex,
        def_path_hash: Fingerprint,
        path: impl Into<Box<str>>,
        kind: SymbolKind,
        fingerprint: Fingerprint,
    ) -> Result<Self, SymbolIndexCodecError> {
        let path = path.into();
        if !is_canonical_symbol_path(&path) {
            return Err(SymbolIndexCodecError::InvalidSymbolPath);
        }

        Ok(Self {
            hex,
            def_path_hash,
            path,
            kind,
            fingerprint,
        })
    }

    pub const fn hex(&self) -> super::SymbolHex {
        self.hex
    }

    pub const fn def_path_hash(&self) -> Fingerprint {
        self.def_path_hash
    }

    /// Returns the stored canonical path string.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub const fn kind(&self) -> SymbolKind {
        self.kind
    }

    pub const fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }
}

/// The filename and exact-byte digest committed by the index header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIndexFileRecord {
    file: PackageIndexFileName,
    digest: ContentHash,
}

impl PackageIndexFileRecord {
    pub fn file(&self) -> &PackageIndexFileName {
        &self.file
    }

    pub const fn digest(&self) -> ContentHash {
        self.digest
    }
}

/// The canonical package-file records from one symbol-index header.
///
/// This validates the header's exact JSON encoding, format, version, package
/// filenames, digests, uniqueness, and ordering. It does not read or verify
/// any package-file contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolIndexHeader {
    records: Vec<PackageIndexFileRecord>,
}

impl SymbolIndexHeader {
    /// Decodes one canonical symbol-index header from caller-provided bytes.
    ///
    /// An empty package list is valid at this codec layer. Journal validation
    /// applies the stricter nonempty rule to a planned-after generation.
    pub fn decode(bytes: &[u8]) -> Result<Self, SymbolIndexCodecError> {
        let header: HeaderJson = parse_canonical_document(bytes)?;
        if header.format != INDEX_FORMAT {
            return Err(SymbolIndexCodecError::UnsupportedFormat);
        }
        if header.version != INDEX_VERSION {
            return Err(SymbolIndexCodecError::UnsupportedVersion);
        }

        let mut seen_files = BTreeSet::new();
        let mut previous_file: Option<PackageIndexFileName> = None;
        let mut records = Vec::with_capacity(header.packages.len());
        for record in header.packages {
            let file = PackageIndexFileName::parse(&record.file)?;
            if !seen_files.insert(file.clone()) {
                return Err(SymbolIndexCodecError::DuplicatePackageFile);
            }
            if previous_file
                .as_ref()
                .is_some_and(|previous| previous.as_str().as_bytes() >= file.as_str().as_bytes())
            {
                return Err(SymbolIndexCodecError::UnorderedPackageFiles);
            }
            previous_file = Some(file.clone());

            records.push(PackageIndexFileRecord {
                file,
                digest: parse_content_hash(&record.blake3)?,
            });
        }

        Ok(Self { records })
    }

    /// Returns package records in canonical filename order.
    pub fn records(&self) -> &[PackageIndexFileRecord] {
        &self.records
    }
}

/// An in-memory symbol index whose entries were matched against a semantic
/// inventory.
///
/// This type can only be obtained by validating an [`UnvalidatedSymbolIndex`]
/// against the current package inventory. The codec alone cannot establish
/// that a path resolves to the declared symbol or that its kind agrees.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolIndex {
    packages: BTreeMap<PackageIndexFileName, Vec<SymbolIndexEntry>>,
}

/// A symbol index that passed codec and path-syntax validation but has not
/// been checked against a semantic inventory.
///
/// Entries are intentionally not exposed and this type cannot be encoded.
/// This codec step does not expose a production promotion operation. Future
/// semantic integration must verify the complete package and symbol set
/// before making a trusted [`SymbolIndex`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnvalidatedSymbolIndex {
    packages: BTreeMap<PackageIndexFileName, Vec<SymbolIndexEntry>>,
}

impl UnvalidatedSymbolIndex {
    /// Builds an index from package entries. Package files and entries are
    /// sorted into their canonical byte order; duplicate identities are
    /// rejected rather than dropped.
    pub fn new(
        packages: impl IntoIterator<Item = (PackageIndexFileName, Vec<SymbolIndexEntry>)>,
    ) -> Result<Self, SymbolIndexCodecError> {
        let mut normalized = BTreeMap::new();
        for (file, mut entries) in packages {
            if normalized.contains_key(&file) {
                return Err(SymbolIndexCodecError::DuplicatePackageFile);
            }

            entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
            validate_entries(&file, &entries, true)?;
            normalized.insert(file, entries);
        }

        Ok(Self {
            packages: normalized,
        })
    }

    /// Reads one complete index from caller-provided byte snapshots.
    ///
    /// The same package-file byte slice is hashed and parsed. This function
    /// performs no file I/O, path joining, locking, or symlink inspection.
    /// The result remains unvalidated until matched against a semantic
    /// inventory.
    pub fn decode(
        header_bytes: &[u8],
        package_files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Self, SymbolIndexCodecError> {
        let header = SymbolIndexHeader::decode(header_bytes)?;
        let mut packages = Vec::with_capacity(header.records.len());

        for record in header.records {
            let file = record.file;
            let snapshot = package_files
                .get(file.as_str())
                .ok_or(SymbolIndexCodecError::MissingPackageFile)?;
            let actual_digest = hash_bytes(snapshot);
            if actual_digest != record.digest {
                return Err(SymbolIndexCodecError::PackageDigestMismatch);
            }

            let entries = decode_package_entries(snapshot, &file)?;
            packages.push((file, entries));
        }

        Ok(Self {
            packages: packages.into_iter().collect(),
        })
    }

    /// Validates every entry against its package's semantic inventory.
    ///
    /// The callback must verify that the path resolves to the intended
    /// declaration and that the persisted kind agrees with the declaration.
    /// It must also reject any semantic noncanonicality that depends on HIR
    /// or package context. Failure rejects the whole index.
    #[cfg(test)]
    fn validate_against_inventory(
        self,
        mut validate: impl FnMut(&PackageIndexFileName, &SymbolIndexEntry) -> bool,
    ) -> Result<SymbolIndex, SymbolIndexCodecError> {
        for (file, entries) in &self.packages {
            if entries.iter().any(|entry| !validate(file, entry)) {
                return Err(SymbolIndexCodecError::InventoryValidationFailed);
            }
        }
        Ok(SymbolIndex {
            packages: self.packages,
        })
    }
}

impl SymbolIndex {
    /// Serializes all package files and the matching committed header.
    pub fn encode(&self) -> Result<EncodedSymbolIndex, SymbolIndexCodecError> {
        if self.packages.is_empty() {
            return Err(SymbolIndexCodecError::EmptyIndex);
        }

        let mut package_files = BTreeMap::new();
        let mut records = Vec::with_capacity(self.packages.len());

        for (file, entries) in &self.packages {
            let bytes = encode_package_entries(entries)?;
            let digest = hash_bytes(&bytes);
            package_files.insert(file.clone(), bytes);
            records.push(PackageFileJson {
                file: file.as_str().to_owned(),
                blake3: digest.to_hex(),
            });
        }

        records.sort_by(|left, right| left.file.as_bytes().cmp(right.file.as_bytes()));
        let header = HeaderJson {
            format: INDEX_FORMAT.to_owned(),
            version: INDEX_VERSION,
            packages: records,
        };
        let header = encode_canonical_document(&header)?;

        Ok(EncodedSymbolIndex {
            header,
            package_files,
        })
    }

    /// Returns package records in the header's canonical filename order.
    pub fn records(&self) -> Result<Vec<PackageIndexFileRecord>, SymbolIndexCodecError> {
        let encoded = self.encode()?;
        Ok(SymbolIndexHeader::decode(&encoded.header)?.records)
    }

    /// Iterates over package filenames and entries in canonical order.
    pub fn packages(&self) -> impl Iterator<Item = (&PackageIndexFileName, &[SymbolIndexEntry])> {
        self.packages
            .iter()
            .map(|(file, entries)| (file, entries.as_slice()))
    }
}

/// Canonical bytes ready for a filesystem layer to publish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedSymbolIndex {
    header: Vec<u8>,
    package_files: BTreeMap<PackageIndexFileName, Vec<u8>>,
}

impl EncodedSymbolIndex {
    pub fn header(&self) -> &[u8] {
        &self.header
    }

    pub fn package_files(&self) -> &BTreeMap<PackageIndexFileName, Vec<u8>> {
        &self.package_files
    }
}

/// Errors found while parsing or serializing the symbol-index format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolIndexCodecError {
    InvalidPackageFileName,
    InvalidJson,
    NonCanonicalJson,
    EmptyIndex,
    MissingFinalLineFeed,
    EmptyEntryLine,
    UnsupportedFormat,
    UnsupportedVersion,
    InvalidDigest,
    MissingPackageFile,
    PackageDigestMismatch,
    DuplicatePackageFile,
    UnorderedPackageFiles,
    InvalidSymbolHex,
    InvalidDefPathHash,
    InvalidSymbolPath,
    SymbolPathOutsidePackage,
    InventoryValidationFailed,
    InvalidFingerprint,
    DuplicateSymbolHex,
    DuplicateSymbolPath,
    DuplicateDefPathHash,
    UnorderedSymbolPaths,
    SerializationFailed,
}

impl fmt::Display for SymbolIndexCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPackageFileName => "invalid or noncanonical symbol-index package filename",
            Self::InvalidJson => "invalid symbol-index JSON",
            Self::NonCanonicalJson => "symbol-index JSON is not in canonical form",
            Self::EmptyIndex => "symbol index has no workspace package files",
            Self::MissingFinalLineFeed => "symbol-index JSON is missing its final line feed",
            Self::EmptyEntryLine => "symbol-index JSONL contains an empty entry line",
            Self::UnsupportedFormat => "unsupported symbol-index format",
            Self::UnsupportedVersion => "unsupported symbol-index version",
            Self::InvalidDigest => "invalid symbol-index BLAKE3 digest",
            Self::MissingPackageFile => {
                "a package file listed in the symbol-index header is missing"
            }
            Self::PackageDigestMismatch => {
                "a package file does not match its recorded BLAKE3 digest"
            }
            Self::DuplicatePackageFile => "duplicate symbol-index package filename",
            Self::UnorderedPackageFiles => "symbol-index package files are not in canonical order",
            Self::InvalidSymbolHex => "invalid symbol hexadecimal identity",
            Self::InvalidDefPathHash => "invalid symbol DefPathHash",
            Self::InvalidSymbolPath => "invalid or empty symbol path",
            Self::SymbolPathOutsidePackage => "symbol path does not belong to its package file",
            Self::InventoryValidationFailed => {
                "symbol-index entries do not match the semantic inventory"
            }
            Self::InvalidFingerprint => "invalid symbol fingerprint",
            Self::DuplicateSymbolHex => "duplicate symbol hexadecimal identity",
            Self::DuplicateSymbolPath => "duplicate symbol path",
            Self::DuplicateDefPathHash => "duplicate symbol DefPathHash",
            Self::UnorderedSymbolPaths => "symbol-index entries are not in canonical path order",
            Self::SerializationFailed => "failed to serialize symbol-index JSON",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SymbolIndexCodecError {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HeaderJson {
    format: String,
    version: u32,
    packages: Vec<PackageFileJson>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PackageFileJson {
    file: String,
    blake3: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EntryJson {
    hex: String,
    def_path_hash: String,
    path: String,
    kind: SymbolKind,
    fingerprint: String,
}

fn parse_canonical_document<T>(bytes: &[u8]) -> Result<T, SymbolIndexCodecError>
where
    T: DeserializeOwned + Serialize,
{
    let payload = bytes
        .strip_suffix(b"\n")
        .ok_or(SymbolIndexCodecError::MissingFinalLineFeed)?;
    let parsed =
        serde_json::from_slice::<T>(payload).map_err(|_| SymbolIndexCodecError::InvalidJson)?;
    let mut canonical =
        serde_json::to_vec(&parsed).map_err(|_| SymbolIndexCodecError::SerializationFailed)?;
    canonical.push(b'\n');
    if canonical != bytes {
        return Err(SymbolIndexCodecError::NonCanonicalJson);
    }
    Ok(parsed)
}

fn encode_canonical_document<T: Serialize>(value: &T) -> Result<Vec<u8>, SymbolIndexCodecError> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|_| SymbolIndexCodecError::SerializationFailed)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(crate) fn encode_package_entries(
    entries: &[SymbolIndexEntry],
) -> Result<Vec<u8>, SymbolIndexCodecError> {
    let mut bytes = Vec::new();
    for entry in entries {
        let json = EntryJson {
            hex: entry.hex.to_string(),
            def_path_hash: entry.def_path_hash.to_string(),
            path: entry.path.to_string(),
            kind: entry.kind,
            fingerprint: entry.fingerprint.to_string(),
        };
        bytes.extend(
            serde_json::to_vec(&json).map_err(|_| SymbolIndexCodecError::SerializationFailed)?,
        );
        bytes.push(b'\n');
    }
    Ok(bytes)
}

pub(crate) fn decode_package_entries(
    bytes: &[u8],
    file: &PackageIndexFileName,
) -> Result<Vec<SymbolIndexEntry>, SymbolIndexCodecError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(b"\n") {
        return Err(SymbolIndexCodecError::MissingFinalLineFeed);
    }

    let mut entries = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let payload = &line[..line.len() - 1];
        if payload.is_empty() {
            return Err(SymbolIndexCodecError::EmptyEntryLine);
        }
        let entry: EntryJson =
            serde_json::from_slice(payload).map_err(|_| SymbolIndexCodecError::InvalidJson)?;
        let canonical =
            serde_json::to_vec(&entry).map_err(|_| SymbolIndexCodecError::SerializationFailed)?;
        if canonical != payload {
            return Err(SymbolIndexCodecError::NonCanonicalJson);
        }

        let hex = super::SymbolHex::parse(&entry.hex)
            .map_err(|_| SymbolIndexCodecError::InvalidSymbolHex)?;
        let def_path_hash = parse_fingerprint(&entry.def_path_hash)
            .ok_or(SymbolIndexCodecError::InvalidDefPathHash)?;
        let fingerprint = parse_fingerprint(&entry.fingerprint)
            .ok_or(SymbolIndexCodecError::InvalidFingerprint)?;
        let parsed =
            SymbolIndexEntry::new(hex, def_path_hash, entry.path, entry.kind, fingerprint)?;
        entries.push(parsed);
    }

    validate_entries(file, &entries, false)?;
    Ok(entries)
}

fn validate_entries(
    file: &PackageIndexFileName,
    entries: &[SymbolIndexEntry],
    input_was_sorted_by_constructor: bool,
) -> Result<(), SymbolIndexCodecError> {
    let package_id = file.package_id();
    let mut seen_hex = BTreeSet::new();
    let mut seen_paths = BTreeSet::new();
    let mut seen_hashes = BTreeSet::new();
    let mut previous_path: Option<&str> = None;

    for entry in entries {
        if !path_belongs_to_package(&entry.path, &package_id) {
            return Err(SymbolIndexCodecError::SymbolPathOutsidePackage);
        }
        if !seen_hex.insert(entry.hex) {
            return Err(SymbolIndexCodecError::DuplicateSymbolHex);
        }
        if !seen_paths.insert(entry.path.as_ref()) {
            return Err(SymbolIndexCodecError::DuplicateSymbolPath);
        }
        if !seen_hashes.insert(entry.def_path_hash) {
            return Err(SymbolIndexCodecError::DuplicateDefPathHash);
        }
        if !input_was_sorted_by_constructor
            && previous_path.is_some_and(|previous| previous.as_bytes() >= entry.path.as_bytes())
        {
            return Err(SymbolIndexCodecError::UnorderedSymbolPaths);
        }
        previous_path = Some(&entry.path);
    }

    Ok(())
}

fn path_belongs_to_package(path: &str, package_id: &str) -> bool {
    path.strip_prefix(package_id)
        .and_then(|suffix| suffix.as_bytes().first())
        .is_some_and(|separator| matches!(separator, b'.' | b'/'))
}

fn parse_content_hash(value: &str) -> Result<ContentHash, SymbolIndexCodecError> {
    let bytes = value.as_bytes();
    if bytes.len() != 64
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(SymbolIndexCodecError::InvalidDigest);
    }

    let mut hash = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        hash[index] = (lower_hex_value(pair[0]) << 4) | lower_hex_value(pair[1]);
    }
    Ok(ContentHash::from_bytes(hash))
}

fn parse_fingerprint(value: &str) -> Option<Fingerprint> {
    let bytes = value.as_bytes();
    if bytes.len() != 32
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return None;
    }
    u128::from_str_radix(value, 16).ok().map(Fingerprint::new)
}

fn lower_hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("validated lowercase hexadecimal input"),
    }
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

fn is_valid_compatibility_line(value: &str) -> bool {
    let components = value.split('.').collect::<Vec<_>>();
    match components.as_slice() {
        [major] => parse_canonical_u64(major).is_some_and(|value| value > 0),
        [major, minor] if *major == "0" => {
            parse_canonical_u64(minor).is_some_and(|value| value > 0)
        }
        [major, minor, patch] if *major == "0" && *minor == "0" => {
            parse_canonical_u64(patch).is_some()
        }
        _ => false,
    }
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        EntryJson, HeaderJson, INDEX_FORMAT, PackageFileJson, PackageIndexFileName, SymbolIndex,
        SymbolIndexCodecError, SymbolIndexEntry, SymbolIndexHeader, SymbolKind,
        UnvalidatedSymbolIndex, encode_canonical_document,
    };
    use crate::SymbolHex;
    use sai_stable_hash::{Fingerprint, hash_bytes};
    use std::collections::BTreeMap;

    fn file(value: &str) -> PackageIndexFileName {
        PackageIndexFileName::parse(value).expect("valid index filename")
    }

    fn entry(
        hex: &str,
        def_path_hash: u128,
        path: &str,
        kind: SymbolKind,
        fingerprint: u128,
    ) -> SymbolIndexEntry {
        SymbolIndexEntry::new(
            SymbolHex::parse(hex).expect("valid symbol hex"),
            Fingerprint::new(def_path_hash),
            path,
            kind,
            Fingerprint::new(fingerprint),
        )
        .expect("valid entry")
    }

    fn header_for(file: &str, package_bytes: &[u8]) -> Vec<u8> {
        encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: vec![PackageFileJson {
                file: file.into(),
                blake3: hash_bytes(package_bytes).to_hex(),
            }],
        })
        .expect("valid header")
    }

    fn new_index(
        packages: impl IntoIterator<Item = (PackageIndexFileName, Vec<SymbolIndexEntry>)>,
    ) -> Result<SymbolIndex, SymbolIndexCodecError> {
        UnvalidatedSymbolIndex::new(packages)?.validate_against_inventory(|_, _| true)
    }

    fn decode_index(
        header_bytes: &[u8],
        package_files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<SymbolIndex, SymbolIndexCodecError> {
        UnvalidatedSymbolIndex::decode(header_bytes, package_files)?
            .validate_against_inventory(|_, _| true)
    }

    fn decode_one(package_bytes: Vec<u8>) -> Result<SymbolIndex, SymbolIndexCodecError> {
        let filename = "acme.user.1.jsonl";
        let header = header_for(filename, &package_bytes);
        let files = BTreeMap::from([(filename.to_owned(), package_bytes)]);
        decode_index(&header, &files)
    }

    fn canonical_entry_line(
        hex: &str,
        def_path_hash: &str,
        path: &str,
        kind: SymbolKind,
        fingerprint: &str,
    ) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&EntryJson {
            hex: hex.into(),
            def_path_hash: def_path_hash.into(),
            path: path.into(),
            kind,
            fingerprint: fingerprint.into(),
        })
        .expect("valid entry JSON");
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn parses_canonical_package_index_filenames() {
        for (name, line) in [
            ("acme.user.1.jsonl", "1"),
            ("acme.user.0.4.jsonl", "0.4"),
            ("acme.user.0.0.0.jsonl", "0.0.0"),
            (
                "acme.user.18446744073709551615.jsonl",
                "18446744073709551615",
            ),
            (
                "acme.user.0.18446744073709551615.jsonl",
                "0.18446744073709551615",
            ),
            (
                "acme.user.0.0.18446744073709551615.jsonl",
                "0.0.18446744073709551615",
            ),
        ] {
            let parsed = file(name);
            assert_eq!(parsed.as_str(), name);
            assert_eq!(parsed.package_id(), "acme:user");
            assert_eq!(parsed.compatibility_line(), line);
            assert_eq!(
                PackageIndexFileName::new("acme", "user", line).unwrap(),
                parsed
            );
        }
    }

    #[test]
    fn rejects_unsafe_or_noncanonical_package_index_filenames() {
        for name in [
            "",
            "../acme.user.1.jsonl",
            "acme/user.1.jsonl",
            "acme\\user.1.jsonl",
            "acme.user",
            ".user.1.jsonl",
            "acme..1.jsonl",
            "Acme.user.1.jsonl",
            "acme.two--hyphens.1.jsonl",
            "acme.trailing-.1.jsonl",
            "wasi.http.1.jsonl",
            "sai.core.1.jsonl",
            "sai.alloc.1.jsonl",
            "acme.user.01.jsonl",
            "acme.user.0.04.jsonl",
            "acme.user.0.0.00.jsonl",
            "acme.user.0.jsonl",
            "acme.user.0.1.2.jsonl",
            "acme.user.18446744073709551616.jsonl",
            "acme.user.0.18446744073709551616.jsonl",
            "acme.user.0.0.18446744073709551616.jsonl",
            "acme.user.1.jsonl\n",
        ] {
            assert_eq!(
                PackageIndexFileName::parse(name),
                Err(SymbolIndexCodecError::InvalidPackageFileName),
                "{name:?}"
            );
        }
    }

    #[test]
    fn serializes_exact_header_and_package_entry_bytes() {
        let file = file("acme.user.1.jsonl");
        let index = new_index([(
            file.clone(),
            vec![entry(
                "5B0C3E",
                0x9f3c0a7e51d2b84466a01c3e7f90b2d1,
                "acme:user/service.create_user",
                SymbolKind::Fn,
                0xc41b07e2a9d35f6680e1b4c27d9a03f5,
            )],
        )])
        .unwrap();
        let encoded = index.encode().unwrap();

        let package = encoded.package_files().get(&file).unwrap();
        assert_eq!(
            package,
            br#"{"hex":"5B0C3E","def_path_hash":"9f3c0a7e51d2b84466a01c3e7f90b2d1","path":"acme:user/service.create_user","kind":"fn","fingerprint":"c41b07e2a9d35f6680e1b4c27d9a03f5"}
"#
        );
        assert_eq!(
            encoded.header(),
            format!(
                "{{\"format\":\"sai-index\",\"version\":1,\"packages\":[{{\"file\":\"acme.user.1.jsonl\",\"blake3\":\"{}\"}}]}}\n",
                hash_bytes(package)
            )
            .as_bytes()
        );
    }

    #[test]
    fn canonical_encoding_is_independent_of_input_order() {
        let package_file = file("acme.user.1.jsonl");
        let first = entry("000001", 1, "acme:user.Z", SymbolKind::Fn, 11);
        let second = entry("000002", 2, "acme:user.a", SymbolKind::Record, 22);
        let forward = new_index([(package_file.clone(), vec![first.clone(), second.clone()])])
            .unwrap()
            .encode()
            .unwrap();
        let reversed = new_index([(package_file, vec![second, first])])
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(forward, reversed);
        let parsed = decode_index(
            forward.header(),
            &forward
                .package_files()
                .iter()
                .map(|(file, bytes)| (file.as_str().to_owned(), bytes.clone()))
                .collect(),
        )
        .unwrap();
        assert_eq!(
            parsed
                .packages()
                .next()
                .unwrap()
                .1
                .iter()
                .map(|entry| entry.path())
                .collect::<Vec<_>>(),
            ["acme:user.Z", "acme:user.a"]
        );
    }

    #[test]
    fn decoder_hashes_and_parses_the_same_package_snapshot() {
        let package_file = file("acme.user.1.jsonl");
        let index = new_index([(
            package_file,
            vec![entry("5B0C3E", 9, "acme:user.fn", SymbolKind::Fn, 10)],
        )])
        .unwrap()
        .encode()
        .unwrap();
        let files = index
            .package_files()
            .iter()
            .map(|(file, bytes)| (file.as_str().to_owned(), bytes.clone()))
            .collect();

        assert_eq!(
            decode_index(index.header(), &files)
                .unwrap()
                .encode()
                .unwrap(),
            index
        );
    }

    #[test]
    fn accepts_a_zero_byte_file_for_a_package_without_symbols() {
        let index = new_index([(file("acme.user.1.jsonl"), Vec::new())]).unwrap();
        let encoded = index.encode().unwrap();
        assert_eq!(
            encoded.package_files().values().next().unwrap(),
            &Vec::<u8>::new()
        );
        assert_eq!(
            decode_index(
                encoded.header(),
                &BTreeMap::from([("acme.user.1.jsonl".into(), Vec::new())])
            )
            .unwrap(),
            index
        );
        assert_eq!(
            decode_one(b"\n".to_vec()),
            Err(SymbolIndexCodecError::EmptyEntryLine)
        );
    }

    #[test]
    fn decode_ignores_unlisted_orphan_files() {
        let header = encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: Vec::new(),
        })
        .unwrap();
        let index = decode_index(
            &header,
            &BTreeMap::from([("acme.orphan.1.jsonl".into(), b"not committed\n".to_vec())]),
        )
        .unwrap();

        assert_eq!(index.packages().count(), 0);
    }

    #[test]
    fn refuses_to_encode_an_index_without_workspace_packages() {
        let index = UnvalidatedSymbolIndex::new(std::iter::empty::<(
            PackageIndexFileName,
            Vec<SymbolIndexEntry>,
        )>())
        .unwrap()
        .validate_against_inventory(|_, _| true)
        .unwrap();

        assert_eq!(index.encode(), Err(SymbolIndexCodecError::EmptyIndex));
    }

    #[test]
    fn inventory_validation_rejects_the_whole_decoded_index() {
        let package_file = "acme.user.1.jsonl";
        let package_bytes = canonical_entry_line(
            "000001",
            "00000000000000000000000000000001",
            "acme:user/item.fn",
            SymbolKind::Fn,
            "00000000000000000000000000000002",
        );
        let header = header_for(package_file, &package_bytes);
        let unvalidated = UnvalidatedSymbolIndex::decode(
            &header,
            &BTreeMap::from([(package_file.into(), package_bytes)]),
        )
        .unwrap();

        assert_eq!(
            unvalidated.validate_against_inventory(|_, _| false),
            Err(SymbolIndexCodecError::InventoryValidationFailed)
        );
    }

    #[test]
    fn syntax_validation_rejects_malformed_canonical_paths() {
        for path in [
            "acme:user.",
            "acme:user/../../outside",
            "acme:user//module.item",
        ] {
            assert_eq!(
                SymbolIndexEntry::new(
                    SymbolHex::parse("000001").unwrap(),
                    Fingerprint::new(1),
                    path,
                    SymbolKind::Fn,
                    Fingerprint::new(2),
                ),
                Err(SymbolIndexCodecError::InvalidSymbolPath),
                "{path:?}"
            );
        }
    }

    #[test]
    fn rejects_noncanonical_header_encodings() {
        for bytes in [
            br#"{"format": "sai-index","version":1,"packages":[]}"#.as_slice(),
            br#"{"version":1,"format":"sai-index","packages":[]}"#.as_slice(),
            br#"{"format":"sai-index","version":1,"packages":[],"other":0}"#.as_slice(),
            br#"{"format":"sai-index","format":"sai-index","version":1,"packages":[]}"#.as_slice(),
            br#"{"format":"sai-index","version":1.0,"packages":[]}"#.as_slice(),
        ] {
            let mut document = bytes.to_vec();
            document.push(b'\n');
            assert!(matches!(
                SymbolIndexHeader::decode(&document),
                Err(SymbolIndexCodecError::InvalidJson | SymbolIndexCodecError::NonCanonicalJson)
            ));
        }

        for bytes in [
            br#"{"format":"sai-index","version":1,"packages":[]}"#.as_slice(),
            b"\xef\xbb\xbf{\"format\":\"sai-index\",\"version\":1,\"packages\":[]}\n".as_slice(),
            b"{\"format\":\"sai-index\",\"version\":1,\"packages\":[]}\r\n".as_slice(),
            b"{\"format\":\"sai-index\",\"version\":1,\"packages\":[]}\n\n".as_slice(),
        ] {
            assert!(SymbolIndexHeader::decode(bytes).is_err());
        }
    }

    #[test]
    fn decodes_empty_canonical_headers_and_preserves_package_records() {
        let empty = encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: Vec::new(),
        })
        .unwrap();
        assert_eq!(SymbolIndexHeader::decode(&empty).unwrap().records(), &[]);

        let record = |name: &str| PackageFileJson {
            file: name.into(),
            blake3: hash_bytes(&[]).to_hex(),
        };
        let encoded = encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: vec![record("acme.a.1.jsonl"), record("acme.z.1.jsonl")],
        })
        .unwrap();
        let header = SymbolIndexHeader::decode(&encoded).unwrap();
        assert_eq!(
            header
                .records()
                .iter()
                .map(|record| (record.file().as_str(), record.digest()))
                .collect::<Vec<_>>(),
            [
                ("acme.a.1.jsonl", hash_bytes(&[])),
                ("acme.z.1.jsonl", hash_bytes(&[])),
            ]
        );
    }

    #[test]
    fn rejects_unsupported_header_format_and_version() {
        let files = BTreeMap::new();
        for (format, version, expected) in [
            ("other-index", 1, SymbolIndexCodecError::UnsupportedFormat),
            ("sai-index", 2, SymbolIndexCodecError::UnsupportedVersion),
        ] {
            let header = encode_canonical_document(&HeaderJson {
                format: format.into(),
                version,
                packages: Vec::new(),
            })
            .unwrap();
            assert_eq!(SymbolIndexHeader::decode(&header), Err(expected));
            assert_eq!(decode_index(&header, &files), Err(expected));
        }
    }

    #[test]
    fn rejects_duplicate_and_unordered_package_records() {
        let record = |file: &str| PackageFileJson {
            file: file.into(),
            blake3: hash_bytes(&[]).to_hex(),
        };
        let duplicate = encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: vec![record("acme.user.1.jsonl"), record("acme.user.1.jsonl")],
        })
        .unwrap();
        assert_eq!(
            SymbolIndexHeader::decode(&duplicate),
            Err(SymbolIndexCodecError::DuplicatePackageFile)
        );
        assert_eq!(
            decode_index(
                &duplicate,
                &BTreeMap::from([("acme.user.1.jsonl".into(), Vec::new())])
            ),
            Err(SymbolIndexCodecError::DuplicatePackageFile)
        );

        let unordered = encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: vec![record("acme.z.1.jsonl"), record("acme.a.1.jsonl")],
        })
        .unwrap();
        assert_eq!(
            SymbolIndexHeader::decode(&unordered),
            Err(SymbolIndexCodecError::UnorderedPackageFiles)
        );
        assert_eq!(
            decode_index(
                &unordered,
                &BTreeMap::from([
                    ("acme.a.1.jsonl".into(), Vec::new()),
                    ("acme.z.1.jsonl".into(), Vec::new())
                ])
            ),
            Err(SymbolIndexCodecError::UnorderedPackageFiles)
        );
    }

    #[test]
    fn rejects_invalid_header_package_filenames_and_digests() {
        let invalid_filename = encode_canonical_document(&HeaderJson {
            format: INDEX_FORMAT.into(),
            version: 1,
            packages: vec![PackageFileJson {
                file: "../outside.jsonl".into(),
                blake3: hash_bytes(&[]).to_hex(),
            }],
        })
        .unwrap();
        assert_eq!(
            SymbolIndexHeader::decode(&invalid_filename),
            Err(SymbolIndexCodecError::InvalidPackageFileName)
        );

        for digest in [String::new(), "A".repeat(64), "0".repeat(63)] {
            let invalid_digest = encode_canonical_document(&HeaderJson {
                format: INDEX_FORMAT.into(),
                version: 1,
                packages: vec![PackageFileJson {
                    file: "acme.user.1.jsonl".into(),
                    blake3: digest.clone(),
                }],
            })
            .unwrap();
            assert_eq!(
                SymbolIndexHeader::decode(&invalid_digest),
                Err(SymbolIndexCodecError::InvalidDigest),
                "{digest:?}"
            );
        }
    }

    #[test]
    fn rejects_missing_files_bad_digests_and_invalid_digest_text() {
        let empty_digest = hash_bytes(&[]).to_hex();
        let make_header = |digest: String| {
            encode_canonical_document(&HeaderJson {
                format: INDEX_FORMAT.into(),
                version: 1,
                packages: vec![PackageFileJson {
                    file: "acme.user.1.jsonl".into(),
                    blake3: digest,
                }],
            })
            .unwrap()
        };

        assert_eq!(
            decode_index(&make_header(empty_digest.clone()), &BTreeMap::new()),
            Err(SymbolIndexCodecError::MissingPackageFile)
        );
        assert_eq!(
            decode_index(
                &make_header("0".repeat(64)),
                &BTreeMap::from([("acme.user.1.jsonl".into(), Vec::new())])
            ),
            Err(SymbolIndexCodecError::PackageDigestMismatch)
        );
        assert_eq!(
            decode_index(
                &make_header("A".repeat(64)),
                &BTreeMap::from([("acme.user.1.jsonl".into(), Vec::new())])
            ),
            Err(SymbolIndexCodecError::InvalidDigest)
        );
    }

    #[test]
    fn rejects_invalid_entry_fields_and_duplicate_members() {
        let valid = |hex: &str, def_hash: &str, path: &str, kind: &str, fingerprint: &str| {
            format!(
                "{{\"hex\":\"{hex}\",\"def_path_hash\":\"{def_hash}\",\"path\":\"{path}\",\"kind\":\"{kind}\",\"fingerprint\":\"{fingerprint}\"}}\n"
            )
            .into_bytes()
        };
        let valid_hash = "00000000000000000000000000000001";
        let cases = [
            (
                valid("5b0c3e", valid_hash, "acme:user.fn", "fn", valid_hash),
                SymbolIndexCodecError::InvalidSymbolHex,
            ),
            (
                valid(
                    "5B0C3E",
                    "z".repeat(32).as_str(),
                    "acme:user.fn",
                    "fn",
                    valid_hash,
                ),
                SymbolIndexCodecError::InvalidDefPathHash,
            ),
            (
                valid("5B0C3E", valid_hash, "", "fn", valid_hash),
                SymbolIndexCodecError::InvalidSymbolPath,
            ),
            (
                valid("5B0C3E", valid_hash, "acme:user.", "fn", valid_hash),
                SymbolIndexCodecError::InvalidSymbolPath,
            ),
            (
                valid(
                    "5B0C3E",
                    valid_hash,
                    "acme:user/../../outside.fn",
                    "fn",
                    valid_hash,
                ),
                SymbolIndexCodecError::InvalidSymbolPath,
            ),
            (
                valid("5B0C3E", valid_hash, "other:user.fn", "fn", valid_hash),
                SymbolIndexCodecError::SymbolPathOutsidePackage,
            ),
            (
                valid("5B0C3E", valid_hash, "acme:user.fn", "module", valid_hash),
                SymbolIndexCodecError::InvalidJson,
            ),
            (
                valid(
                    "5B0C3E",
                    valid_hash,
                    "acme:user.fn",
                    "fn",
                    "Z".repeat(32).as_str(),
                ),
                SymbolIndexCodecError::InvalidFingerprint,
            ),
        ];

        for (package_bytes, expected) in cases {
            assert_eq!(decode_one(package_bytes), Err(expected));
        }
        assert_eq!(
            decode_one(
                br#"{"hex":"5B0C3E","hex":"000000","def_path_hash":"00000000000000000000000000000001","path":"acme:user.fn","kind":"fn","fingerprint":"00000000000000000000000000000001"}
"#
                .to_vec()
            ),
            Err(SymbolIndexCodecError::InvalidJson)
        );
        assert_eq!(
            decode_one(
                br#"{"hex":"5B0C3E","def_path_hash":"00000000000000000000000000000001","path":"acme:user.fn","kind":"fn","fingerprint":"00000000000000000000000000000001","extra":0}
"#
                .to_vec()
            ),
            Err(SymbolIndexCodecError::InvalidJson)
        );
    }

    #[test]
    fn rejects_noncanonical_package_lines_and_unsorted_entries() {
        let valid_hash = "00000000000000000000000000000001";
        let noncanonical = [
            br#"{"hex":"5B0C3E", "def_path_hash":"00000000000000000000000000000001","path":"acme:user.fn","kind":"fn","fingerprint":"00000000000000000000000000000001"}
"#
            .to_vec(),
            br#"{"hex":"5B0C3E","def_path_hash":"00000000000000000000000000000001","path":"acme:user\/fn","kind":"fn","fingerprint":"00000000000000000000000000000001"}
"#
            .to_vec(),
            canonical_entry_line("5B0C3E", valid_hash, "acme:user.fn", SymbolKind::Fn, valid_hash)
                .into_iter()
                .take_while(|byte| *byte != b'\n')
                .collect(),
            b"\n".to_vec(),
            b"\r\n".to_vec(),
        ];
        for bytes in noncanonical {
            assert!(decode_one(bytes).is_err());
        }

        let first = canonical_entry_line(
            "000001",
            "00000000000000000000000000000001",
            "acme:user.z",
            SymbolKind::Fn,
            "00000000000000000000000000000001",
        );
        let second = canonical_entry_line(
            "000002",
            "00000000000000000000000000000002",
            "acme:user.a",
            SymbolKind::Fn,
            "00000000000000000000000000000002",
        );
        assert_eq!(
            decode_one([first, second].concat()),
            Err(SymbolIndexCodecError::UnorderedSymbolPaths)
        );
    }

    #[test]
    fn rejects_duplicate_hex_path_and_def_path_hash() {
        let hash = "00000000000000000000000000000001";
        let records = [
            (
                vec![
                    entry("000001", 1, "acme:user.a", SymbolKind::Fn, 1),
                    entry("000001", 2, "acme:user.b", SymbolKind::Fn, 2),
                ],
                SymbolIndexCodecError::DuplicateSymbolHex,
            ),
            (
                vec![
                    entry("000001", 1, "acme:user.a", SymbolKind::Fn, 1),
                    entry("000002", 2, "acme:user.a", SymbolKind::Fn, 2),
                ],
                SymbolIndexCodecError::DuplicateSymbolPath,
            ),
            (
                vec![
                    entry("000001", 1, "acme:user.a", SymbolKind::Fn, 1),
                    entry("000002", 1, "acme:user.b", SymbolKind::Fn, 2),
                ],
                SymbolIndexCodecError::DuplicateDefPathHash,
            ),
        ];
        for (entries, expected) in records {
            assert_eq!(
                new_index([(file("acme.user.1.jsonl"), entries)]),
                Err(expected)
            );
        }

        let same_hash = canonical_entry_line("000001", hash, "acme:user.a", SymbolKind::Fn, hash);
        let repeated_hash =
            canonical_entry_line("000002", hash, "acme:user.b", SymbolKind::Fn, hash);
        assert_eq!(
            decode_one([same_hash, repeated_hash].concat()),
            Err(SymbolIndexCodecError::DuplicateDefPathHash)
        );
    }

    #[test]
    fn serde_serialization_matches_diagnostic_string_escaping() {
        let package_file = file("acme.user.1.jsonl");
        let path = "acme:user.test[\"雪\"]";
        let index = new_index([(
            package_file,
            vec![entry("000001", 1, path, SymbolKind::Test, 2)],
        )])
        .unwrap()
        .encode()
        .unwrap();
        let bytes = index.package_files().values().next().unwrap();
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.contains(r#"test[\"雪\"]"#));
        assert!(!text.contains(r#"\u96ea"#));
        assert_eq!(
            decode_index(
                index.header(),
                &index
                    .package_files()
                    .iter()
                    .map(|(file, bytes)| (file.as_str().to_owned(), bytes.clone()))
                    .collect()
            )
            .unwrap(),
            new_index([(
                file("acme.user.1.jsonl"),
                vec![entry("000001", 1, path, SymbolKind::Test, 2)]
            )])
            .unwrap()
        );
    }

    #[test]
    fn package_header_record_preserves_digest_and_filename_order() {
        let a = file("acme.a.1.jsonl");
        let z = file("acme.z.1.jsonl");
        let index = new_index([(z, Vec::new()), (a, Vec::new())]).unwrap();
        let records = index.records().unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.file().as_str())
                .collect::<Vec<_>>(),
            ["acme.a.1.jsonl", "acme.z.1.jsonl"]
        );
        assert!(
            records
                .iter()
                .all(|record| record.digest() == hash_bytes(&[]))
        );
    }
}
