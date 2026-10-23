use crate::canonical_path::is_canonical_symbol_path;
use crate::symbol_index::decode_package_entries;
use crate::{PackageIndexFileName, SymbolHex};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sai_stable_hash::hash_bytes;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const JOURNAL_FORMAT: &str = "sai-operation-journal";
const JOURNAL_VERSION: u32 = 1;
/// Maximum encoded journal size, including JSON and base64 expansion.
pub const MAX_OPERATION_JOURNAL_BYTES: usize = 256 * 1024 * 1024;

/// The kind of operation represented by an index journal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexJournalOrigin {
    Cli,
    LspApplyEdit,
    LspReturnedEdit,
}

/// The durable progress marker written by an index transaction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexJournalState {
    Prepared,
    AwaitingClient,
    ClientApplied,
    ClientRejected,
    SourcesApplied,
    IndexCommitted,
}

/// An exact file snapshot stored in an operation journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileState {
    Absent,
    RegularFile(Vec<u8>),
}

impl FileState {
    fn to_json(&self) -> FileStateJson {
        match self {
            Self::Absent => FileStateJson::Absent,
            Self::RegularFile(bytes) => FileStateJson::RegularFile {
                bytes: URL_SAFE_NO_PAD.encode(bytes),
            },
        }
    }

    fn from_json(value: FileStateJson) -> Result<Self, IndexJournalCodecError> {
        match value {
            FileStateJson::Absent => Ok(Self::Absent),
            FileStateJson::RegularFile { bytes } => {
                let decoded = URL_SAFE_NO_PAD
                    .decode(&bytes)
                    .map_err(|_| IndexJournalCodecError::InvalidBase64)?;
                if URL_SAFE_NO_PAD.encode(&decoded) != bytes {
                    return Err(IndexJournalCodecError::InvalidBase64);
                }
                Ok(Self::RegularFile(decoded))
            }
        }
    }

    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Absent => None,
            Self::RegularFile(bytes) => Some(bytes),
        }
    }
}

/// One changed source file and its exact pre-operation and planned contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFileChange {
    pub package_node_key: String,
    pub package_root: String,
    pub path: String,
    pub before: FileState,
    pub planned_after: FileState,
}

/// A package-index file that may change as part of an index generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFileState {
    pub file: PackageIndexFileName,
    pub state: FileState,
}

/// The header and affected package files for one exact index generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexSnapshot {
    pub header: FileState,
    pub package_files: Vec<PackageFileState>,
}

/// A symbol identity's path before and after an operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolIdMapping {
    pub package_node_key: String,
    pub hex: SymbolHex,
    pub before_path: Option<String>,
    pub after_path: Option<String>,
}

/// A canonical, versioned description of a pending source/index transaction.
///
/// This is an in-memory value only. It performs no file I/O, locking, package
/// discovery, durability, or recovery. Callers must still enforce the
/// workspace-relative paths against the current package roots and use safe
/// filesystem operations when persisting or recovering the record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexOperationJournal {
    pub operation_id: String,
    pub origin: IndexJournalOrigin,
    pub state: IndexJournalState,
    pub sources: Vec<SourceFileChange>,
    pub index_before: IndexSnapshot,
    pub index_after: IndexSnapshot,
    pub symbol_mappings: Vec<SymbolIdMapping>,
}

impl IndexOperationJournal {
    /// Parses one canonical journal document from caller-provided bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, IndexJournalCodecError> {
        check_document_size(bytes.len())?;
        let json: JournalJson = parse_canonical_document(bytes)?;
        if json.format != JOURNAL_FORMAT {
            return Err(IndexJournalCodecError::UnsupportedFormat);
        }
        if json.version != JOURNAL_VERSION {
            return Err(IndexJournalCodecError::UnsupportedVersion);
        }

        let journal = Self {
            operation_id: json.operation_id,
            origin: json.origin,
            state: json.state,
            sources: json
                .sources
                .into_iter()
                .map(|source| {
                    Ok(SourceFileChange {
                        package_node_key: source.package_node_key,
                        package_root: source.package_root,
                        path: source.path,
                        before: FileState::from_json(source.before)?,
                        planned_after: FileState::from_json(source.planned_after)?,
                    })
                })
                .collect::<Result<_, IndexJournalCodecError>>()?,
            index_before: decode_index_snapshot(json.index_before)?,
            index_after: decode_index_snapshot(json.index_after)?,
            symbol_mappings: json
                .symbol_mappings
                .into_iter()
                .map(|mapping| {
                    Ok(SymbolIdMapping {
                        package_node_key: mapping.package_node_key,
                        hex: SymbolHex::parse(&mapping.hex)
                            .map_err(|_| IndexJournalCodecError::InvalidSymbolHex)?,
                        before_path: mapping.before_path,
                        after_path: mapping.after_path,
                    })
                })
                .collect::<Result<_, IndexJournalCodecError>>()?,
        };

        journal.validate()?;
        Ok(journal)
    }

    /// Serializes this journal using its canonical v1 JSON representation.
    pub fn encode(&self) -> Result<Vec<u8>, IndexJournalCodecError> {
        let estimated_len = estimate_journal_encoded_len(self)?;
        self.validate()?;
        let json = JournalJson {
            format: JOURNAL_FORMAT.to_owned(),
            version: JOURNAL_VERSION,
            operation_id: self.operation_id.clone(),
            origin: self.origin,
            state: self.state,
            sources: self
                .sources
                .iter()
                .map(|source| SourceFileChangeJson {
                    package_node_key: source.package_node_key.clone(),
                    package_root: source.package_root.clone(),
                    path: source.path.clone(),
                    before: source.before.to_json(),
                    planned_after: source.planned_after.to_json(),
                })
                .collect(),
            index_before: encode_index_snapshot(&self.index_before),
            index_after: encode_index_snapshot(&self.index_after),
            symbol_mappings: self
                .symbol_mappings
                .iter()
                .map(|mapping| SymbolIdMappingJson {
                    package_node_key: mapping.package_node_key.clone(),
                    hex: mapping.hex.to_string(),
                    before_path: mapping.before_path.clone(),
                    after_path: mapping.after_path.clone(),
                })
                .collect(),
        };
        let bytes = encode_canonical_document(&json)?;
        check_document_size(bytes.len())?;
        debug_assert_eq!(bytes.len(), estimated_len);
        Ok(bytes)
    }

    fn validate(&self) -> Result<(), IndexJournalCodecError> {
        if !is_canonical_uuid_v4(&self.operation_id) {
            return Err(IndexJournalCodecError::InvalidOperationId);
        }
        if self.sources.is_empty() {
            return Err(IndexJournalCodecError::EmptySources);
        }

        let mut previous_source: Option<(&str, &str, &str)> = None;
        let mut source_identities = BTreeSet::new();
        let mut package_roots = BTreeMap::new();
        for source in &self.sources {
            if !is_canonical_package_node_key(&source.package_node_key) {
                return Err(IndexJournalCodecError::InvalidPackageNodeKey);
            }
            if !is_canonical_relative_path(&source.package_root, true) {
                return Err(IndexJournalCodecError::InvalidPackageRoot);
            }
            if !is_canonical_relative_path(&source.path, false) {
                return Err(IndexJournalCodecError::InvalidSourcePath);
            }
            if source.before == source.planned_after {
                return Err(IndexJournalCodecError::UnchangedSource);
            }
            let identity = (&source.package_node_key, &source.path);
            if !source_identities.insert(identity) {
                return Err(IndexJournalCodecError::DuplicateSource);
            }
            if package_roots
                .insert(&source.package_node_key, &source.package_root)
                .is_some_and(|existing| existing != &source.package_root)
            {
                return Err(IndexJournalCodecError::ConflictingPackageRoot);
            }

            let key = (
                source.package_node_key.as_str(),
                source.package_root.as_str(),
                source.path.as_str(),
            );
            if previous_source.is_some_and(|previous| compare_source_keys(previous, key).is_ge()) {
                return Err(IndexJournalCodecError::UnorderedSources);
            }
            previous_source = Some(key);
        }

        let before = validate_index_snapshot(&self.index_before, false)?;
        let after = validate_index_snapshot(&self.index_after, true)?;
        if after.header.is_empty() {
            return Err(IndexJournalCodecError::EmptyPlannedIndex);
        }
        if self.index_before.header == self.index_after.header {
            return Err(IndexJournalCodecError::UnchangedIndexGeneration);
        }
        if before.files.keys().collect::<Vec<_>>() != after.files.keys().collect::<Vec<_>>() {
            return Err(IndexJournalCodecError::IndexFileSetMismatch);
        }

        let affected_files = before.files.keys().copied().collect::<BTreeSet<_>>();
        for file in before
            .header
            .keys()
            .chain(after.header.keys())
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
        {
            if before.header.get(file) != after.header.get(file) && !affected_files.contains(file) {
                return Err(IndexJournalCodecError::IndexFileSetIncomplete);
            }
        }

        let mut previous_mapping: Option<(&str, String)> = None;
        let mut mapping_identities = BTreeSet::new();
        let mut mapped_index_symbols = BTreeSet::new();
        for mapping in &self.symbol_mappings {
            let package = parse_package_node_key(&mapping.package_node_key)
                .ok_or(IndexJournalCodecError::InvalidPackageNodeKey)?;
            if mapping.before_path.is_none() && mapping.after_path.is_none() {
                return Err(IndexJournalCodecError::EmptySymbolMapping);
            }
            if mapping.before_path == mapping.after_path {
                return Err(IndexJournalCodecError::UnchangedSymbolMapping);
            }
            if !mapping_identities.insert((&mapping.package_node_key, mapping.hex)) {
                return Err(IndexJournalCodecError::DuplicateSymbolMapping);
            }
            if !mapped_index_symbols.insert((package.index_file.as_str().to_owned(), mapping.hex)) {
                return Err(IndexJournalCodecError::DuplicateSymbolMapping);
            }
            let hex = mapping.hex.to_string();
            if previous_mapping
                .as_ref()
                .is_some_and(|(previous_package, previous_hex)| {
                    compare_mapping_keys(
                        (previous_package, previous_hex.as_str()),
                        (&mapping.package_node_key, &hex),
                    )
                    .is_ge()
                })
            {
                return Err(IndexJournalCodecError::UnorderedSymbolMappings);
            }
            previous_mapping = Some((&mapping.package_node_key, hex));

            for path in [
                mapping.before_path.as_deref(),
                mapping.after_path.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if !is_canonical_symbol_path(path) {
                    return Err(IndexJournalCodecError::InvalidMappingPath);
                }
                let suffix = path
                    .strip_prefix(&package.package_id)
                    .ok_or(IndexJournalCodecError::MappingOutsidePackage)?;
                if !suffix.starts_with('/') && !suffix.starts_with('.') {
                    return Err(IndexJournalCodecError::MappingOutsidePackage);
                }
            }

            let package_file = package.index_file.as_str();
            if !before.files.contains_key(package_file) {
                return Err(IndexJournalCodecError::MissingMappedPackageFile);
            }
            validate_mapping_side(
                &before,
                package_file,
                mapping.hex,
                mapping.before_path.as_deref(),
            )?;
            validate_mapping_side(
                &after,
                package_file,
                mapping.hex,
                mapping.after_path.as_deref(),
            )?;
        }

        let changed_symbols = before
            .symbol_paths
            .keys()
            .chain(after.symbol_paths.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for (file, hex) in changed_symbols {
            let before_path = before
                .symbol_paths
                .get(&(file.clone(), hex))
                .map(String::as_str);
            let after_path = after
                .symbol_paths
                .get(&(file.clone(), hex))
                .map(String::as_str);
            if before_path != after_path && !mapped_index_symbols.contains(&(file, hex)) {
                return Err(IndexJournalCodecError::MissingSymbolMapping);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexJournalCodecError {
    DocumentTooLarge,
    InvalidJson,
    NonCanonicalJson,
    MissingFinalLineFeed,
    SerializationFailed,
    UnsupportedFormat,
    UnsupportedVersion,
    InvalidBase64,
    InvalidOperationId,
    InvalidPackageNodeKey,
    InvalidPackageRoot,
    InvalidSourcePath,
    EmptySources,
    UnchangedSource,
    DuplicateSource,
    ConflictingPackageRoot,
    UnorderedSources,
    InvalidIndexHeader,
    EmptyPlannedIndex,
    UnchangedIndexGeneration,
    InvalidIndexPackageFile,
    IndexFileSetMismatch,
    IndexFileSetIncomplete,
    IndexFileStateMismatch,
    InvalidIndexPackageContents,
    InvalidSymbolHex,
    EmptySymbolMapping,
    UnchangedSymbolMapping,
    DuplicateSymbolMapping,
    UnorderedSymbolMappings,
    InvalidMappingPath,
    MappingOutsidePackage,
    MissingMappedPackageFile,
    MissingSymbolMapping,
    SymbolMappingIndexMismatch,
}

impl fmt::Display for IndexJournalCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::DocumentTooLarge => "operation journal exceeds the 256 MiB limit",
            Self::InvalidJson => "invalid operation-journal JSON",
            Self::NonCanonicalJson => "operation-journal JSON is not canonical",
            Self::MissingFinalLineFeed => "operation-journal JSON is missing its final LF",
            Self::SerializationFailed => "failed to serialize operation-journal JSON",
            Self::UnsupportedFormat => "unsupported operation-journal format",
            Self::UnsupportedVersion => "unsupported operation-journal version",
            Self::InvalidBase64 => "operation-journal file bytes are not canonical base64url",
            Self::InvalidOperationId => "operation ID is not a canonical UUIDv4",
            Self::InvalidPackageNodeKey => "invalid or noncanonical package-node key",
            Self::InvalidPackageRoot => "invalid workspace-relative package root",
            Self::InvalidSourcePath => "invalid package-relative source path",
            Self::EmptySources => "operation journal contains no source changes",
            Self::UnchangedSource => "operation journal contains an unchanged source",
            Self::DuplicateSource => "operation journal contains a duplicate source identity",
            Self::ConflictingPackageRoot => "one package-node key has conflicting package roots",
            Self::UnorderedSources => "operation-journal sources are not in canonical order",
            Self::InvalidIndexHeader => "invalid or noncanonical symbol-index header snapshot",
            Self::EmptyPlannedIndex => "planned symbol-index generation has no package nodes",
            Self::UnchangedIndexGeneration => {
                "a journaled source transaction does not change the symbol-index generation"
            }
            Self::InvalidIndexPackageFile => "invalid symbol-index package filename",
            Self::IndexFileSetMismatch => {
                "before and planned index snapshots have different affected file sets"
            }
            Self::IndexFileSetIncomplete => {
                "an index header change is missing its affected package-file snapshot"
            }
            Self::IndexFileStateMismatch => {
                "symbol-index package-file bytes contradict their header snapshot"
            }
            Self::InvalidIndexPackageContents => {
                "symbol-index package-file snapshot is not canonical"
            }
            Self::InvalidSymbolHex => "invalid symbol hexadecimal identity",
            Self::EmptySymbolMapping => "symbol mapping has no before or after path",
            Self::UnchangedSymbolMapping => "unchanged symbol mappings must be omitted",
            Self::DuplicateSymbolMapping => "duplicate symbol identity mapping",
            Self::UnorderedSymbolMappings => "symbol mappings are not in canonical order",
            Self::InvalidMappingPath => "symbol mapping path is not canonical",
            Self::MappingOutsidePackage => "symbol mapping path belongs to another package",
            Self::MissingMappedPackageFile => {
                "a mapped symbol's package file is absent from the index snapshot"
            }
            Self::MissingSymbolMapping => {
                "a changed symbol path is missing from the operation mapping"
            }
            Self::SymbolMappingIndexMismatch => {
                "symbol mapping disagrees with its package-file snapshot"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for IndexJournalCodecError {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalJson {
    format: String,
    version: u32,
    operation_id: String,
    origin: IndexJournalOrigin,
    state: IndexJournalState,
    sources: Vec<SourceFileChangeJson>,
    index_before: IndexSnapshotJson,
    index_after: IndexSnapshotJson,
    symbol_mappings: Vec<SymbolIdMappingJson>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceFileChangeJson {
    package_node_key: String,
    package_root: String,
    path: String,
    before: FileStateJson,
    planned_after: FileStateJson,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexSnapshotJson {
    header: FileStateJson,
    package_files: Vec<PackageFileStateJson>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PackageFileStateJson {
    file: String,
    state: FileStateJson,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "state", deny_unknown_fields)]
enum FileStateJson {
    #[serde(rename = "absent")]
    Absent,
    #[serde(rename = "regular_file")]
    RegularFile { bytes: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SymbolIdMappingJson {
    package_node_key: String,
    hex: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    before_path: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    after_path: Option<String>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexHeaderJson {
    format: String,
    version: u32,
    packages: Vec<IndexPackageFileJson>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexPackageFileJson {
    file: String,
    blake3: String,
}

struct PackageNodeKey {
    package_id: String,
    index_file: PackageIndexFileName,
}

struct ValidatedIndexSnapshot<'a> {
    header: BTreeMap<String, String>,
    files: BTreeMap<&'a str, &'a FileState>,
    symbol_paths: BTreeMap<(String, SymbolHex), String>,
}

impl IndexSnapshot {
    /// Returns the exact header file state.
    pub fn header(&self) -> &FileState {
        &self.header
    }

    /// Returns the affected package-file snapshots in canonical order.
    pub fn package_files(&self) -> &[PackageFileState] {
        &self.package_files
    }
}

fn decode_index_snapshot(json: IndexSnapshotJson) -> Result<IndexSnapshot, IndexJournalCodecError> {
    let mut package_files = Vec::with_capacity(json.package_files.len());
    for package_file in json.package_files {
        package_files.push(PackageFileState {
            file: PackageIndexFileName::parse(&package_file.file)
                .map_err(|_| IndexJournalCodecError::InvalidIndexPackageFile)?,
            state: FileState::from_json(package_file.state)?,
        });
    }
    Ok(IndexSnapshot {
        header: FileState::from_json(json.header)?,
        package_files,
    })
}

fn encode_index_snapshot(snapshot: &IndexSnapshot) -> IndexSnapshotJson {
    IndexSnapshotJson {
        header: snapshot.header.to_json(),
        package_files: snapshot
            .package_files
            .iter()
            .map(|package_file| PackageFileStateJson {
                file: package_file.file.as_str().to_owned(),
                state: package_file.state.to_json(),
            })
            .collect(),
    }
}

fn validate_index_snapshot<'a>(
    snapshot: &'a IndexSnapshot,
    planned_after: bool,
) -> Result<ValidatedIndexSnapshot<'a>, IndexJournalCodecError> {
    let header = parse_index_header(&snapshot.header)?;
    let mut files = BTreeMap::new();
    let mut symbol_paths = BTreeMap::new();
    let mut previous_name: Option<&str> = None;

    for package_file in &snapshot.package_files {
        let file_name = package_file.file.as_str();
        if previous_name.is_some_and(|previous| previous.as_bytes() >= file_name.as_bytes()) {
            return Err(IndexJournalCodecError::InvalidIndexPackageFile);
        }
        previous_name = Some(file_name);
        files.insert(file_name, &package_file.state);

        match header.get(file_name) {
            Some(expected_digest) => {
                let Some(bytes) = package_file.state.bytes() else {
                    return Err(IndexJournalCodecError::IndexFileStateMismatch);
                };
                if hash_bytes(bytes).to_hex() != *expected_digest {
                    return Err(IndexJournalCodecError::IndexFileStateMismatch);
                }
                let entries = decode_package_entries(bytes, &package_file.file)
                    .map_err(|_| IndexJournalCodecError::InvalidIndexPackageContents)?;
                for entry in entries {
                    symbol_paths
                        .insert((file_name.to_owned(), entry.hex()), entry.path().to_owned());
                }
            }
            None if planned_after && package_file.state != FileState::Absent => {
                return Err(IndexJournalCodecError::IndexFileStateMismatch);
            }
            // Before snapshots can preserve an ignored orphan that will be
            // overwritten by a new committed package file.
            None => {}
        }
    }

    Ok(ValidatedIndexSnapshot {
        header,
        files,
        symbol_paths,
    })
}

fn parse_index_header(
    state: &FileState,
) -> Result<BTreeMap<String, String>, IndexJournalCodecError> {
    let bytes = match state {
        FileState::Absent => return Ok(BTreeMap::new()),
        FileState::RegularFile(bytes) => bytes,
    };
    let header: IndexHeaderJson =
        parse_canonical_document(bytes).map_err(|_| IndexJournalCodecError::InvalidIndexHeader)?;
    if header.format != "sai-index" || header.version != 1 {
        return Err(IndexJournalCodecError::InvalidIndexHeader);
    }

    let mut files = BTreeMap::new();
    let mut previous_name: Option<String> = None;
    for package_file in header.packages {
        let parsed_file = PackageIndexFileName::parse(&package_file.file)
            .map_err(|_| IndexJournalCodecError::InvalidIndexHeader)?;
        if !is_canonical_blake3(&package_file.blake3)
            || previous_name
                .as_ref()
                .is_some_and(|previous| previous.as_bytes() >= parsed_file.as_str().as_bytes())
        {
            return Err(IndexJournalCodecError::InvalidIndexHeader);
        }
        previous_name = Some(parsed_file.as_str().to_owned());
        if files
            .insert(parsed_file.as_str().to_owned(), package_file.blake3)
            .is_some()
        {
            return Err(IndexJournalCodecError::InvalidIndexHeader);
        }
    }
    Ok(files)
}

fn validate_mapping_side(
    snapshot: &ValidatedIndexSnapshot<'_>,
    package_file: &str,
    hex: SymbolHex,
    path: Option<&str>,
) -> Result<(), IndexJournalCodecError> {
    let exists = snapshot
        .symbol_paths
        .get(&(package_file.to_owned(), hex))
        .map(String::as_str);
    let matches = match (path, exists) {
        (Some(expected), Some(actual)) => expected == actual,
        (None, None) => true,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(IndexJournalCodecError::SymbolMappingIndexMismatch)
    }
}

fn parse_package_node_key(value: &str) -> Option<PackageNodeKey> {
    let body = value.strip_prefix("package:")?;
    let (package_id, version) = body.rsplit_once('@')?;
    if !crate::is_valid_package_id(package_id) {
        return None;
    }
    let parsed_version = Version::parse(version).ok()?;
    if !parsed_version.build.is_empty()
        || parsed_version.to_string() != version
        || (!parsed_version.pre.is_empty()
            && parsed_version.pre.as_str().split('.').any(|identifier| {
                identifier.bytes().all(|byte| byte.is_ascii_digit())
                    && identifier.parse::<u64>().is_err()
            }))
    {
        return None;
    }

    let line = compatibility_line(&parsed_version);
    let index_file = PackageIndexFileName::new(
        package_id.split_once(':')?.0,
        package_id.split_once(':')?.1,
        &line,
    )
    .ok()?;
    Some(PackageNodeKey {
        package_id: package_id.to_owned(),
        index_file,
    })
}

fn is_canonical_package_node_key(value: &str) -> bool {
    parse_package_node_key(value).is_some()
}

fn compatibility_line(version: &Version) -> String {
    if version.major >= 1 {
        version.major.to_string()
    } else if version.minor >= 1 {
        format!("0.{}", version.minor)
    } else {
        format!("0.0.{}", version.patch)
    }
}

fn is_canonical_relative_path(value: &str, allow_workspace_root: bool) -> bool {
    if allow_workspace_root && value == "." {
        return true;
    }
    if value.is_empty() || value.starts_with('/') || value.contains('\\') || value.contains('\0') {
        return false;
    }
    let mut components = value.split('/');
    let Some(first) = components.next() else {
        return false;
    };
    if first.len() >= 2 && first.as_bytes()[0].is_ascii_alphabetic() && first.as_bytes()[1] == b':'
    {
        return false;
    }
    std::iter::once(first)
        .chain(components)
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn is_canonical_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(*byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

fn is_canonical_blake3(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn compare_source_keys(left: (&str, &str, &str), right: (&str, &str, &str)) -> std::cmp::Ordering {
    left.0
        .as_bytes()
        .cmp(right.0.as_bytes())
        .then_with(|| left.1.as_bytes().cmp(right.1.as_bytes()))
        .then_with(|| left.2.as_bytes().cmp(right.2.as_bytes()))
}

fn compare_mapping_keys(left: (&str, &str), right: (&str, &str)) -> std::cmp::Ordering {
    left.0
        .as_bytes()
        .cmp(right.0.as_bytes())
        .then_with(|| left.1.as_bytes().cmp(right.1.as_bytes()))
}

fn check_document_size(length: usize) -> Result<(), IndexJournalCodecError> {
    if length > MAX_OPERATION_JOURNAL_BYTES {
        Err(IndexJournalCodecError::DocumentTooLarge)
    } else {
        Ok(())
    }
}

fn estimate_journal_encoded_len(
    journal: &IndexOperationJournal,
) -> Result<usize, IndexJournalCodecError> {
    let sources = json_array_len(journal.sources.iter().map(source_file_change_json_len))?;
    let index_before = index_snapshot_json_len(&journal.index_before)?;
    let index_after = index_snapshot_json_len(&journal.index_after)?;
    let mappings = json_array_len(journal.symbol_mappings.iter().map(symbol_mapping_json_len))?;

    let document_len = json_object_len([
        ("format", Ok(json_string_len(JOURNAL_FORMAT)?)),
        ("version", Ok(1)),
        ("operation_id", json_string_len(&journal.operation_id)),
        ("origin", json_string_len(origin_as_str(journal.origin))),
        ("state", json_string_len(state_as_str(journal.state))),
        ("sources", Ok(sources)),
        ("index_before", Ok(index_before)),
        ("index_after", Ok(index_after)),
        ("symbol_mappings", Ok(mappings)),
    ])?;
    add_json_len(document_len, 1)
}

fn source_file_change_json_len(source: &SourceFileChange) -> Result<usize, IndexJournalCodecError> {
    json_object_len([
        (
            "package_node_key",
            json_string_len(&source.package_node_key),
        ),
        ("package_root", json_string_len(&source.package_root)),
        ("path", json_string_len(&source.path)),
        ("before", file_state_json_len(&source.before)),
        ("planned_after", file_state_json_len(&source.planned_after)),
    ])
}

fn index_snapshot_json_len(snapshot: &IndexSnapshot) -> Result<usize, IndexJournalCodecError> {
    let package_files = json_array_len(snapshot.package_files.iter().map(|package_file| {
        json_object_len([
            ("file", json_string_len(package_file.file.as_str())),
            ("state", file_state_json_len(&package_file.state)),
        ])
    }))?;
    json_object_len([
        ("header", file_state_json_len(&snapshot.header)),
        ("package_files", Ok(package_files)),
    ])
}

fn symbol_mapping_json_len(mapping: &SymbolIdMapping) -> Result<usize, IndexJournalCodecError> {
    let hex = mapping.hex.to_string();
    json_object_len([
        (
            "package_node_key",
            json_string_len(&mapping.package_node_key),
        ),
        ("hex", json_string_len(&hex)),
        (
            "before_path",
            optional_json_string_len(mapping.before_path.as_deref()),
        ),
        (
            "after_path",
            optional_json_string_len(mapping.after_path.as_deref()),
        ),
    ])
}

fn file_state_json_len(state: &FileState) -> Result<usize, IndexJournalCodecError> {
    match state {
        FileState::Absent => json_object_len([("state", json_string_len("absent"))]),
        FileState::RegularFile(bytes) => json_object_len([
            ("state", json_string_len("regular_file")),
            ("bytes", base64_json_string_len(bytes.len())),
        ]),
    }
}

fn optional_json_string_len(value: Option<&str>) -> Result<usize, IndexJournalCodecError> {
    match value {
        Some(value) => json_string_len(value),
        None => Ok(4),
    }
}

fn origin_as_str(origin: IndexJournalOrigin) -> &'static str {
    match origin {
        IndexJournalOrigin::Cli => "cli",
        IndexJournalOrigin::LspApplyEdit => "lsp_apply_edit",
        IndexJournalOrigin::LspReturnedEdit => "lsp_returned_edit",
    }
}

fn state_as_str(state: IndexJournalState) -> &'static str {
    match state {
        IndexJournalState::Prepared => "prepared",
        IndexJournalState::AwaitingClient => "awaiting_client",
        IndexJournalState::ClientApplied => "client_applied",
        IndexJournalState::ClientRejected => "client_rejected",
        IndexJournalState::SourcesApplied => "sources_applied",
        IndexJournalState::IndexCommitted => "index_committed",
    }
}

fn json_string_len(value: &str) -> Result<usize, IndexJournalCodecError> {
    let mut length = 2;
    for byte in value.bytes() {
        length = add_json_len(
            length,
            match byte {
                b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 2,
                0..=0x1f => 6,
                _ => 1,
            },
        )?;
    }
    Ok(length)
}

fn base64_json_string_len(byte_len: usize) -> Result<usize, IndexJournalCodecError> {
    let mut encoded_len = (byte_len / 3)
        .checked_mul(4)
        .ok_or(IndexJournalCodecError::DocumentTooLarge)?;
    encoded_len = add_json_len(
        encoded_len,
        match byte_len % 3 {
            0 => 0,
            1 => 2,
            2 => 3,
            _ => unreachable!(),
        },
    )?;
    add_json_len(encoded_len, 2)
}

fn json_array_len(
    items: impl IntoIterator<Item = Result<usize, IndexJournalCodecError>>,
) -> Result<usize, IndexJournalCodecError> {
    let mut length = 2;
    let mut first = true;
    for item in items {
        if !first {
            length = add_json_len(length, 1)?;
        }
        first = false;
        length = add_json_len(length, item?)?;
    }
    Ok(length)
}

fn json_object_len<'a>(
    fields: impl IntoIterator<Item = (&'a str, Result<usize, IndexJournalCodecError>)>,
) -> Result<usize, IndexJournalCodecError> {
    let mut length = 2;
    let mut first = true;
    for (name, value_len) in fields {
        if !first {
            length = add_json_len(length, 1)?;
        }
        first = false;
        length = add_json_len(length, json_string_len(name)?)?;
        length = add_json_len(length, 1)?;
        length = add_json_len(length, value_len?)?;
    }
    Ok(length)
}

fn add_json_len(length: usize, addition: usize) -> Result<usize, IndexJournalCodecError> {
    let length = length
        .checked_add(addition)
        .ok_or(IndexJournalCodecError::DocumentTooLarge)?;
    check_document_size(length)?;
    Ok(length)
}

fn parse_canonical_document<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
) -> Result<T, IndexJournalCodecError> {
    if !bytes.ends_with(b"\n") {
        return Err(IndexJournalCodecError::MissingFinalLineFeed);
    }
    let payload = &bytes[..bytes.len() - 1];
    let value: T =
        serde_json::from_slice(payload).map_err(|_| IndexJournalCodecError::InvalidJson)?;
    if encode_canonical_document(&value)? != bytes {
        return Err(IndexJournalCodecError::NonCanonicalJson);
    }
    Ok(value)
}

fn encode_canonical_document<T: Serialize>(value: &T) -> Result<Vec<u8>, IndexJournalCodecError> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|_| IndexJournalCodecError::SerializationFailed)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        FileState, IndexHeaderJson, IndexJournalCodecError, IndexJournalOrigin, IndexJournalState,
        IndexOperationJournal, IndexPackageFileJson, IndexSnapshot, JournalJson,
        MAX_OPERATION_JOURNAL_BYTES, PackageFileState, SourceFileChange, SymbolIdMapping,
        check_document_size, encode_canonical_document,
    };
    use crate::{PackageIndexFileName, SymbolHex, SymbolIndexEntry, SymbolKind};
    use sai_stable_hash::{Fingerprint, hash_bytes};

    const OPERATION_ID: &str = "1c71050c-1e25-4acd-8a56-03af72c9ce7a";
    const PACKAGE_NODE_KEY: &str = "package:acme:user@1.0.0";
    const INDEX_FILE: &str = "acme.user.1.jsonl";
    const BEFORE_PATH: &str = "acme:user/src.greet";
    const AFTER_PATH: &str = "acme:user/src.say";

    fn package_file_state(bytes: &[u8]) -> PackageFileState {
        PackageFileState {
            file: PackageIndexFileName::parse(INDEX_FILE).expect("valid package index filename"),
            state: FileState::RegularFile(bytes.to_vec()),
        }
    }

    fn index_entry(path: &str) -> Vec<u8> {
        super::super::symbol_index::encode_package_entries(&[SymbolIndexEntry::new(
            SymbolHex::parse("000001").expect("valid symbol hex"),
            Fingerprint::new(1),
            path,
            SymbolKind::Fn,
            Fingerprint::new(2),
        )
        .expect("valid symbol entry")])
        .expect("entry encodes")
    }

    fn index_snapshot(entry_bytes: Vec<u8>) -> IndexSnapshot {
        let header = IndexHeaderJson {
            format: "sai-index".into(),
            version: 1,
            packages: vec![IndexPackageFileJson {
                file: INDEX_FILE.into(),
                blake3: hash_bytes(&entry_bytes).to_hex(),
            }],
        };
        IndexSnapshot {
            header: FileState::RegularFile(
                encode_canonical_document(&header).expect("header serializes"),
            ),
            package_files: vec![package_file_state(&entry_bytes)],
        }
    }

    fn empty_index_snapshot() -> IndexSnapshot {
        index_snapshot(Vec::new())
    }

    fn empty_generation_snapshot() -> IndexSnapshot {
        let header = IndexHeaderJson {
            format: "sai-index".into(),
            version: 1,
            packages: Vec::new(),
        };
        IndexSnapshot {
            header: FileState::RegularFile(
                encode_canonical_document(&header).expect("empty header serializes"),
            ),
            package_files: vec![PackageFileState {
                file: PackageIndexFileName::parse(INDEX_FILE)
                    .expect("valid package index filename"),
                state: FileState::Absent,
            }],
        }
    }

    fn sample_journal() -> IndexOperationJournal {
        IndexOperationJournal {
            operation_id: OPERATION_ID.into(),
            origin: IndexJournalOrigin::Cli,
            state: IndexJournalState::Prepared,
            sources: vec![SourceFileChange {
                package_node_key: PACKAGE_NODE_KEY.into(),
                package_root: ".".into(),
                path: "src/lib.sai".into(),
                before: FileState::RegularFile(b"pub fn greet() {}\n".to_vec()),
                planned_after: FileState::RegularFile(b"pub fn say() {}\n".to_vec()),
            }],
            index_before: index_snapshot(index_entry(BEFORE_PATH)),
            index_after: index_snapshot(index_entry(AFTER_PATH)),
            symbol_mappings: vec![SymbolIdMapping {
                package_node_key: PACKAGE_NODE_KEY.into(),
                hex: SymbolHex::parse("000001").expect("valid symbol hex"),
                before_path: Some(BEFORE_PATH.into()),
                after_path: Some(AFTER_PATH.into()),
            }],
        }
    }

    fn encoded_json(value: &JournalJson) -> Vec<u8> {
        encode_canonical_document(value).expect("journal serializes")
    }

    #[test]
    fn round_trips_a_canonical_complete_journal() {
        let journal = sample_journal();
        let bytes = journal.encode().expect("valid journal encodes");
        assert!(bytes.ends_with(b"\n"));
        assert!(bytes.starts_with(
            br#"{"format":"sai-operation-journal","version":1,"operation_id":"1c71050c-1e25-4acd-8a56-03af72c9ce7a","origin":"cli","state":"prepared","sources""#
        ));
        assert_eq!(IndexOperationJournal::decode(&bytes), Ok(journal));
    }

    #[test]
    fn estimates_exact_document_size_before_encoding_file_snapshots() {
        let mut journal = sample_journal();
        journal.sources[0].path = "src/日本語\"file.sai".into();
        let expected_len = super::estimate_journal_encoded_len(&journal).unwrap();
        assert_eq!(journal.encode().unwrap().len(), expected_len);
        assert_eq!(
            super::json_string_len("a\n\"é"),
            Ok(9),
            "JSON escaping and multibyte UTF-8 lengths are included"
        );
        assert_eq!(
            super::base64_json_string_len(MAX_OPERATION_JOURNAL_BYTES),
            Err(IndexJournalCodecError::DocumentTooLarge)
        );
    }

    #[test]
    fn rejects_alternate_json_encodings_and_members() {
        let bytes = sample_journal().encode().unwrap();
        assert_eq!(
            IndexOperationJournal::decode(&bytes[..bytes.len() - 1]),
            Err(IndexJournalCodecError::MissingFinalLineFeed)
        );

        let mut extra_lf = bytes.clone();
        extra_lf.push(b'\n');
        assert_eq!(
            IndexOperationJournal::decode(&extra_lf),
            Err(IndexJournalCodecError::NonCanonicalJson)
        );

        let pretty = serde_json::to_vec_pretty(
            &serde_json::from_slice::<serde_json::Value>(&bytes[..bytes.len() - 1]).unwrap(),
        )
        .unwrap();
        let mut pretty = pretty;
        pretty.push(b'\n');
        assert_eq!(
            IndexOperationJournal::decode(&pretty),
            Err(IndexJournalCodecError::NonCanonicalJson)
        );

        let duplicate = String::from_utf8(bytes).unwrap().replacen(
            "\"format\":\"sai-operation-journal\"",
            "\"format\":\"sai-operation-journal\",\"format\":\"sai-operation-journal\"",
            1,
        );
        assert_eq!(
            IndexOperationJournal::decode(duplicate.as_bytes()),
            Err(IndexJournalCodecError::InvalidJson)
        );
    }

    #[test]
    fn rejects_unknown_members_missing_required_nullable_mapping_values_and_versions() {
        let journal = sample_journal();
        let json: JournalJson = serde_json::from_slice(
            &journal.encode().unwrap()[..journal.encode().unwrap().len() - 1],
        )
        .unwrap();
        let bytes = encoded_json(&json);
        let text = String::from_utf8(bytes).unwrap().replacen(
            "\"format\":\"sai-operation-journal\",",
            "\"format\":\"sai-operation-journal\",\"unexpected\":true,",
            1,
        );
        assert_eq!(
            IndexOperationJournal::decode(text.as_bytes()),
            Err(IndexJournalCodecError::InvalidJson)
        );

        let journal = sample_journal();
        let bytes = journal.encode().unwrap();
        let text = String::from_utf8(bytes).unwrap().replacen(
            &format!("\"before_path\":\"{BEFORE_PATH}\","),
            "",
            1,
        );
        assert_eq!(
            IndexOperationJournal::decode(text.as_bytes()),
            Err(IndexJournalCodecError::InvalidJson)
        );

        let bytes = sample_journal().encode().unwrap();
        let text = String::from_utf8(bytes)
            .unwrap()
            .replacen("\"version\":1", "\"version\":2", 1);
        assert_eq!(
            IndexOperationJournal::decode(text.as_bytes()),
            Err(IndexJournalCodecError::UnsupportedVersion)
        );
    }

    #[test]
    fn rejects_noncanonical_base64url() {
        let mut journal = sample_journal();
        journal.sources[0].before = FileState::RegularFile(vec![0xFF]);
        journal.sources[0].planned_after = FileState::RegularFile(vec![0xFE]);
        let bytes = journal.encode().unwrap();
        let text =
            String::from_utf8(bytes)
                .unwrap()
                .replacen("\"bytes\":\"_w\"", "\"bytes\":\"_w==\"", 1);
        assert_eq!(
            IndexOperationJournal::decode(text.as_bytes()),
            Err(IndexJournalCodecError::InvalidBase64)
        );
    }

    #[test]
    fn rejects_invalid_uuid_package_paths_and_source_order() {
        let mut journal = sample_journal();
        journal.operation_id = "1C71050C-1E25-4ACD-8A56-03AF72C9CE7A".into();
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::InvalidOperationId)
        );

        let mut journal = sample_journal();
        journal.sources[0].package_node_key = "package:acme:user@1.0.0+build".into();
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::InvalidPackageNodeKey)
        );

        for invalid in [
            "",
            "/src/lib.sai",
            "C:/src/lib.sai",
            "C:src/lib.sai",
            "src//lib.sai",
            "src/../lib.sai",
            "./src/lib.sai",
            "src\\lib.sai",
            "src\0/lib.sai",
        ] {
            let mut journal = sample_journal();
            journal.sources[0].path = invalid.into();
            assert_eq!(
                journal.encode(),
                Err(IndexJournalCodecError::InvalidSourcePath),
                "{invalid:?}"
            );
        }

        let mut journal = sample_journal();
        journal.sources.push(SourceFileChange {
            package_node_key: "package:acme:admin@1.0.0".into(),
            package_root: "packages/admin".into(),
            path: "src/lib.sai".into(),
            before: FileState::Absent,
            planned_after: FileState::RegularFile(b"pub fn run() {}\n".to_vec()),
        });
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::UnorderedSources)
        );
    }

    #[test]
    fn rejects_duplicate_semantic_sources_and_conflicting_roots() {
        let mut journal = sample_journal();
        let mut duplicate = journal.sources[0].clone();
        duplicate.package_root = "packages/user".into();
        journal.sources.push(duplicate);
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::DuplicateSource)
        );

        let mut journal = sample_journal();
        let mut other = journal.sources[0].clone();
        other.package_root = "packages/user".into();
        other.path = "src/other.sai".into();
        other.before = FileState::Absent;
        other.planned_after = FileState::RegularFile(b"pub fn other() {}\n".to_vec());
        journal.sources.push(other);
        journal.sources.sort_by(|left, right| {
            left.package_node_key
                .as_bytes()
                .cmp(right.package_node_key.as_bytes())
                .then_with(|| {
                    left.package_root
                        .as_bytes()
                        .cmp(right.package_root.as_bytes())
                })
                .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
        });
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::ConflictingPackageRoot)
        );
    }

    #[test]
    fn rejects_mismatched_index_sets_headers_and_mappings() {
        let mut journal = sample_journal();
        journal.index_after.package_files.clear();
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::IndexFileSetMismatch)
        );

        let mut journal = sample_journal();
        let changed_header = match &journal.index_after.header {
            FileState::RegularFile(header) => {
                let parsed: IndexHeaderJson =
                    serde_json::from_slice(&header[..header.len() - 1]).unwrap();
                let mut changed = parsed;
                changed.packages[0].blake3 = "a".repeat(64);
                FileState::RegularFile(encode_canonical_document(&changed).unwrap())
            }
            FileState::Absent => unreachable!(),
        };
        journal.index_after.header = changed_header;
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::IndexFileStateMismatch)
        );

        let mut journal = sample_journal();
        journal.symbol_mappings[0].after_path = Some("acme:user/src.wrong".into());
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::SymbolMappingIndexMismatch)
        );
    }

    #[test]
    fn requires_mappings_for_created_deleted_and_moved_symbols() {
        let mut journal = sample_journal();
        journal.symbol_mappings.clear();
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::MissingSymbolMapping)
        );

        let mut created = sample_journal();
        created.index_before = empty_index_snapshot();
        created.symbol_mappings.clear();
        assert_eq!(
            created.encode(),
            Err(IndexJournalCodecError::MissingSymbolMapping)
        );

        let mut deleted = sample_journal();
        deleted.index_after = empty_index_snapshot();
        deleted.symbol_mappings.clear();
        assert_eq!(
            deleted.encode(),
            Err(IndexJournalCodecError::MissingSymbolMapping)
        );

        let mut created = sample_journal();
        created.index_before = empty_index_snapshot();
        created.symbol_mappings[0].before_path = None;
        assert!(created.encode().is_ok());

        let mut deleted = sample_journal();
        deleted.index_after = empty_index_snapshot();
        deleted.symbol_mappings[0].after_path = None;
        assert!(deleted.encode().is_ok());
    }

    #[test]
    fn rejects_unchanged_and_empty_planned_index_generations() {
        let mut unchanged = sample_journal();
        unchanged.index_after = unchanged.index_before.clone();
        assert_eq!(
            unchanged.encode(),
            Err(IndexJournalCodecError::UnchangedIndexGeneration)
        );

        let mut orphan_only = sample_journal();
        let committed_header = orphan_only.index_before.header.clone();
        let committed_file = orphan_only.index_before.package_files[0].state.clone();
        orphan_only
            .index_before
            .package_files
            .push(PackageFileState {
                file: PackageIndexFileName::parse("acme.user.2.jsonl").unwrap(),
                state: FileState::RegularFile(b"ignored orphan".to_vec()),
            });
        orphan_only.index_after.header = committed_header;
        orphan_only.index_after.package_files[0].state = committed_file;
        orphan_only
            .index_after
            .package_files
            .push(PackageFileState {
                file: PackageIndexFileName::parse("acme.user.2.jsonl").unwrap(),
                state: FileState::Absent,
            });
        orphan_only.symbol_mappings.clear();
        assert_eq!(
            orphan_only.encode(),
            Err(IndexJournalCodecError::UnchangedIndexGeneration)
        );

        let mut absent = sample_journal();
        absent.index_after = IndexSnapshot {
            header: FileState::Absent,
            package_files: vec![PackageFileState {
                file: PackageIndexFileName::parse(INDEX_FILE).unwrap(),
                state: FileState::Absent,
            }],
        };
        assert_eq!(
            absent.encode(),
            Err(IndexJournalCodecError::EmptyPlannedIndex)
        );

        let mut empty = sample_journal();
        empty.index_after = empty_generation_snapshot();
        assert_eq!(
            empty.encode(),
            Err(IndexJournalCodecError::EmptyPlannedIndex)
        );
    }

    #[test]
    fn preserves_a_before_orphan_but_requires_planned_unlisted_files_absent() {
        let mut journal = sample_journal();
        journal.index_before.header = FileState::Absent;
        journal.index_before.package_files[0].state =
            FileState::RegularFile(b"ignored stale orphan\n".to_vec());
        journal.symbol_mappings[0].before_path = None;
        assert!(journal.encode().is_ok());

        let mut journal = sample_journal();
        journal.index_after.header = FileState::Absent;
        journal.index_after.package_files[0].state =
            FileState::RegularFile(b"uncommitted after state\n".to_vec());
        assert_eq!(
            journal.encode(),
            Err(IndexJournalCodecError::IndexFileStateMismatch)
        );
    }

    #[test]
    fn accepts_index_file_creation_with_a_complete_symbol_mapping() {
        let mut journal = sample_journal();
        journal.symbol_mappings[0].before_path = None;
        journal.index_before = IndexSnapshot {
            header: FileState::Absent,
            package_files: vec![PackageFileState {
                file: PackageIndexFileName::parse(INDEX_FILE).unwrap(),
                state: FileState::Absent,
            }],
        };
        journal.index_after = index_snapshot(index_entry(AFTER_PATH));
        assert!(journal.encode().is_ok());
    }

    #[test]
    fn enforces_the_document_size_limit_before_parsing() {
        assert_eq!(check_document_size(MAX_OPERATION_JOURNAL_BYTES), Ok(()));
        assert_eq!(
            check_document_size(MAX_OPERATION_JOURNAL_BYTES + 1),
            Err(IndexJournalCodecError::DocumentTooLarge)
        );
    }
}
