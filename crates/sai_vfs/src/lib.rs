#![forbid(unsafe_code)]

use sai_query::{DerivedQuery, Durability, InputQuery, QueryDatabase, QueryError};
use sai_stable_hash::{StableHash, StableHasher};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Maximum source-file size, measured in bytes as read from disk.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// A session-local identity for a source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileId(u32);

impl FileId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A session-independent file identity used as a stable query key.
///
/// The package key and canonical source path are explicit. The document
/// version and `FileId` are deliberately excluded: versions describe
/// snapshots, and `FileId` is assigned within one compiler session.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileKey {
    package_key: String,
    path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKeyError {
    InvalidPackageKey,
    InvalidSourcePath,
    PackageIdentityMismatch,
}

impl FileKey {
    /// Constructs a stable identity from a package key and a source path.
    ///
    /// Dependency source references are checked against the package key's
    /// compatibility line, then reduced to their package-relative path.
    /// For workspace files, `path` must already be relative to the owning
    /// package; a `SnapshotFile::path` may instead be relative to the workspace.
    pub fn new(
        package_key: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<Self, FileKeyError> {
        let package_key = package_key.into();
        let path = path.into();
        if !is_stable_package_key(&package_key) {
            return Err(FileKeyError::InvalidPackageKey);
        }
        if !is_canonical_source_path(&path) {
            return Err(FileKeyError::InvalidSourcePath);
        }
        let path = if let Some(dependency) = path.strip_prefix("dep:") {
            let Some((release_identity, source_path)) = dependency.split_once('/') else {
                return Err(FileKeyError::InvalidSourcePath);
            };
            if !package_key_matches_release(&package_key, release_identity) {
                return Err(FileKeyError::PackageIdentityMismatch);
            }
            source_path.to_owned()
        } else {
            path
        };
        Ok(Self { package_key, path })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn package_key(&self) -> &str {
        &self.package_key
    }
}

impl StableHash for FileKey {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xA7);
        self.package_key.stable_hash(hasher);
        self.path.stable_hash(hasher);
    }
}

/// A session-local interner from stable file keys to syntax-facing file IDs.
///
/// The interner must be created once per compiler session. File IDs are
/// assigned in insertion order and must not be persisted or used as stable
/// query keys.
#[derive(Default)]
pub struct FileInterner {
    by_key: BTreeMap<FileKey, FileId>,
    keys: Vec<FileKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileInternerError {
    TooManyFiles,
}

impl FileInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the session-local `FileId` for a validated stable file key.
    pub fn intern(&mut self, key: FileKey) -> Result<FileId, FileInternerError> {
        if let Some(file_id) = self.by_key.get(&key) {
            return Ok(*file_id);
        }

        let raw_id = u32::try_from(self.keys.len()).map_err(|_| FileInternerError::TooManyFiles)?;
        let file_id = FileId::new(raw_id);
        self.keys.push(key.clone());
        self.by_key.insert(key, file_id);
        Ok(file_id)
    }

    /// Resolves a session ID to its stable file key, if it belongs to this interner.
    pub fn file_key(&self, file_id: FileId) -> Option<&FileKey> {
        self.keys.get(file_id.raw() as usize)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// A validated stable package identity suitable for use as a query key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageKey(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageKeyError {
    InvalidPackageKey,
}

impl PackageKey {
    pub fn new(package_key: impl Into<String>) -> Result<Self, PackageKeyError> {
        let package_key = package_key.into();
        if !is_stable_package_key(&package_key) {
            return Err(PackageKeyError::InvalidPackageKey);
        }
        Ok(Self(package_key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl StableHash for PackageKey {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xAB);
        self.0.stable_hash(hasher);
    }
}

/// A canonical path relative to the package root.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageRelativePath(String);

/// A canonical path relative to a package's configured source directory.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageSourcePath(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackagePathError {
    InvalidRelativePath,
}

impl PackageRelativePath {
    pub fn new(path: impl Into<String>) -> Result<Self, PackagePathError> {
        let path = path.into();
        if !is_canonical_relative_path(&path) {
            return Err(PackagePathError::InvalidRelativePath);
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PackageSourcePath {
    pub fn new(path: impl Into<String>) -> Result<Self, PackagePathError> {
        let path = path.into();
        if !is_canonical_relative_path(&path) {
            return Err(PackagePathError::InvalidRelativePath);
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl StableHash for PackageRelativePath {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xAC);
        self.0.stable_hash(hasher);
    }
}

impl StableHash for PackageSourcePath {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xAD);
        self.0.stable_hash(hasher);
    }
}

/// A UTF-8 byte offset into decoded source text.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextSize(u32);

impl TextSize {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub fn from_usize(raw: usize) -> Option<Self> {
        u32::try_from(raw).ok().map(Self)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn to_usize(self) -> usize {
        self.0 as usize
    }
}

/// A half-open UTF-8 byte range.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextRange {
    start: TextSize,
    end: TextSize,
}

impl TextRange {
    pub const fn new(start: TextSize, end: TextSize) -> Option<Self> {
        if start.raw() <= end.raw() {
            Some(Self { start, end })
        } else {
            None
        }
    }

    pub const fn empty(at: TextSize) -> Self {
        Self { start: at, end: at }
    }

    pub const fn start(self) -> TextSize {
        self.start
    }

    pub const fn end(self) -> TextSize {
        self.end
    }

    pub const fn len(self) -> u32 {
        self.end.raw() - self.start.raw()
    }

    pub const fn is_empty(self) -> bool {
        self.start.raw() == self.end.raw()
    }

    pub const fn contains(self, offset: TextSize) -> bool {
        self.start.raw() <= offset.raw() && offset.raw() < self.end.raw()
    }
}

/// A source range paired with the session-local identity of its file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Span {
    pub file: FileId,
    pub range: TextRange,
}

impl Span {
    pub const fn new(file: FileId, range: TextRange) -> Self {
        Self { file, range }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineCol {
    /// One-based line number.
    pub line: u32,
    /// One-based UTF-8 byte column.
    pub col: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    /// Absolute byte offset in the original file, including a leading BOM if present.
    pub byte: u32,
    /// One-based line number.
    pub line: u32,
    /// One-based UTF-8 byte column.
    pub col: u32,
    /// One-based UTF-16 code-unit column.
    pub utf16_col: u32,
}

/// A maximal ill-formed UTF-8 subsequence in the original file.
///
/// The decoded text contains U+FFFD at `decoded_range`; `raw_len` preserves the
/// number of original bytes for diagnostic positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidUtf8Sequence {
    decoded_range: TextRange,
    raw_len: u32,
}

impl InvalidUtf8Sequence {
    pub const fn decoded_range(self) -> TextRange {
        self.decoded_range
    }

    pub const fn raw_len(self) -> u32 {
        self.raw_len
    }
}

/// Maps source byte offsets to one-based lines and UTF-8/UTF-16 columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineIndex {
    text: Arc<str>,
    line_starts: Vec<TextSize>,
    bom_shift: u32,
    invalid_utf8: Arc<[InvalidUtf8Sequence]>,
    invalid_utf8_prefix_delta: Vec<i64>,
}

impl LineIndex {
    pub fn new(text: impl Into<Arc<str>>) -> Self {
        Self::with_bom(text, false)
    }

    /// Builds an index for decoded source text with an optional removed UTF-8 BOM.
    pub fn with_bom(text: impl Into<Arc<str>>, had_bom: bool) -> Self {
        Self::with_decode_metadata(text.into(), had_bom, Arc::from([]))
    }

    fn with_decode_metadata(
        text: Arc<str>,
        had_bom: bool,
        invalid_utf8: Arc<[InvalidUtf8Sequence]>,
    ) -> Self {
        let mut invalid_utf8_prefix_delta = Vec::with_capacity(invalid_utf8.len());
        let mut prefix_delta = 0_i64;
        for sequence in invalid_utf8.iter().copied() {
            let decoded_width = sequence
                .decoded_range
                .end()
                .to_usize()
                .saturating_sub(sequence.decoded_range.start().to_usize());
            prefix_delta += i64::from(sequence.raw_len) - decoded_width as i64;
            invalid_utf8_prefix_delta.push(prefix_delta);
        }

        let mut line_starts = vec![TextSize::new(0)];
        let bytes = text.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            match bytes[i] {
                b'\n' => {
                    i += 1;
                    if let Some(start) = TextSize::from_usize(i) {
                        line_starts.push(start);
                    }
                }
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    i += 2;
                    if let Some(start) = TextSize::from_usize(i) {
                        line_starts.push(start);
                    }
                }
                b'\r' => {
                    i += 1;
                    if let Some(start) = TextSize::from_usize(i) {
                        line_starts.push(start);
                    }
                }
                _ => i += 1,
            }
        }

        Self {
            text,
            line_starts,
            bom_shift: if had_bom { 3 } else { 0 },
            invalid_utf8,
            invalid_utf8_prefix_delta,
        }
    }

    pub fn line_count(&self) -> u32 {
        u32::try_from(self.line_starts.len()).unwrap_or(u32::MAX)
    }

    pub fn line_col(&self, offset: TextSize) -> LineCol {
        let offset = self.normalized_offset(offset);
        let line_index = self
            .line_starts
            .partition_point(|start| start.raw() <= offset.raw())
            .saturating_sub(1);
        let start = self.line_starts[line_index].raw();
        let offset = offset.to_usize();
        let start = start as usize;
        LineCol {
            line: u32::try_from(line_index + 1).unwrap_or(u32::MAX),
            col: self.utf8_units_between(start, offset).saturating_add(1),
        }
    }

    pub fn position(&self, offset: TextSize) -> Position {
        let offset = self.normalized_offset(offset);
        let line_col = self.line_col(offset);
        let line_start = self.line_starts[(line_col.line - 1) as usize].to_usize();
        let offset = offset.to_usize();
        let utf16_col = self
            .utf16_units_between(line_start, offset)
            .saturating_add(1);

        Position {
            byte: self.original_byte_offset(offset),
            line: line_col.line,
            col: line_col.col,
            utf16_col: u32::try_from(utf16_col).unwrap_or(u32::MAX),
        }
    }

    /// Maps an internal source offset to the zero-based UTF-16 LSP position.
    pub fn to_lsp_utf16(&self, offset: TextSize) -> LspPosition {
        let position = self.position(offset);
        LspPosition {
            line: position.line.saturating_sub(1),
            character: position
                .utf16_col
                .saturating_sub(1)
                .saturating_add(u32::from(self.bom_shift > 0 && position.line == 1)),
        }
    }

    /// Maps an internal source offset to the zero-based UTF-8 LSP position.
    pub fn to_lsp_utf8(&self, offset: TextSize) -> LspPosition {
        let position = self.position(offset);
        LspPosition {
            line: position.line.saturating_sub(1),
            character: position.col.saturating_sub(1).saturating_add(
                if self.bom_shift > 0 && position.line == 1 {
                    self.bom_shift
                } else {
                    0
                },
            ),
        }
    }

    /// Converts a one-based UTF-8 byte position to an offset, clamping at line end.
    pub fn offset(&self, line: u32, col: u32) -> TextSize {
        let line_index = self.clamped_line_index(line);
        let start = self.line_starts[line_index].to_usize();
        let end = self.line_end(line_index);
        let target = col.saturating_sub(1);
        let mut units = 0u32;
        let mut offset = start;

        while offset < end {
            if let Some(sequence) = self.invalid_sequence_at(offset) {
                let width = sequence.raw_len;
                if target < units.saturating_add(width) {
                    return TextSize::new(offset as u32);
                }
                units = units.saturating_add(width);
                offset = sequence.decoded_range.end().to_usize();
                continue;
            }

            let Some(ch) = self.text[offset..end].chars().next() else {
                break;
            };
            let width = u32::try_from(ch.len_utf8()).unwrap_or(u32::MAX);
            if target < units.saturating_add(width) {
                return TextSize::new(offset as u32);
            }
            units = units.saturating_add(width);
            offset += ch.len_utf8();
        }

        TextSize::new(offset as u32)
    }

    /// Converts a one-based UTF-16 position to an offset, rounding down inside a surrogate pair.
    pub fn from_utf16(&self, line: u32, utf16_col: u32) -> TextSize {
        let line_index = self.clamped_line_index(line);
        let start = self.line_starts[line_index].to_usize();
        let end = self.line_end(line_index);
        let target = utf16_col
            .saturating_sub(1)
            .saturating_sub(u32::from(self.bom_shift > 0 && line_index == 0));
        let mut units = 0u32;
        let mut offset = start;

        while offset < end {
            if let Some(sequence) = self.invalid_sequence_at(offset) {
                if target < units.saturating_add(sequence.raw_len) {
                    return TextSize::new(offset as u32);
                }
                units = units.saturating_add(sequence.raw_len);
                offset = sequence.decoded_range.end().to_usize();
                continue;
            }

            let Some(ch) = self.text[offset..end].chars().next() else {
                break;
            };
            let width = ch.len_utf16() as u32;
            if units.saturating_add(width) > target {
                return TextSize::new(offset as u32);
            }
            units += width;
            offset += ch.len_utf8();
        }

        TextSize::new(offset as u32)
    }

    /// Maps a zero-based UTF-16 LSP position to an internal source offset.
    pub fn from_lsp_utf16(&self, line: u32, character: u32) -> TextSize {
        self.from_utf16(line.saturating_add(1), character.saturating_add(1))
    }

    /// Maps a zero-based UTF-8 LSP position to an internal source offset.
    pub fn from_lsp_utf8(&self, line: u32, character: u32) -> TextSize {
        let line_number = line.saturating_add(1);
        let bom_shift = if self.bom_shift > 0 && line == 0 {
            self.bom_shift
        } else {
            0
        };
        let character = character.saturating_sub(bom_shift).saturating_add(1);
        self.offset(line_number, character)
    }

    fn clamped_line_index(&self, line: u32) -> usize {
        (line.saturating_sub(1) as usize).min(self.line_starts.len().saturating_sub(1))
    }

    fn line_end(&self, line_index: usize) -> usize {
        let next_start = self
            .line_starts
            .get(line_index + 1)
            .map_or(self.text.len(), |start| start.to_usize())
            .min(self.text.len());
        let bytes = self.text.as_bytes();

        if next_start > 0 && bytes[next_start - 1] == b'\n' {
            let before_lf = next_start - 1;
            if before_lf > 0 && bytes[before_lf - 1] == b'\r' {
                before_lf - 1
            } else {
                before_lf
            }
        } else if next_start > 0 && bytes[next_start - 1] == b'\r' {
            next_start - 1
        } else {
            next_start
        }
    }

    fn normalized_offset(&self, offset: TextSize) -> TextSize {
        let mut raw = offset.to_usize().min(self.text.len());
        if raw > 0
            && raw < self.text.len()
            && self.text.as_bytes()[raw - 1] == b'\r'
            && self.text.as_bytes()[raw] == b'\n'
        {
            raw -= 1;
        }
        TextSize::new(self.round_down_to_char_boundary(0, raw) as u32)
    }

    fn round_down_to_char_boundary(&self, min: usize, mut offset: usize) -> usize {
        while offset > min && !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn invalid_sequence_at(&self, offset: usize) -> Option<InvalidUtf8Sequence> {
        let index = self
            .invalid_utf8
            .partition_point(|sequence| sequence.decoded_range.start().to_usize() < offset);
        self.invalid_utf8
            .get(index)
            .copied()
            .filter(|sequence| sequence.decoded_range.start().to_usize() == offset)
    }

    fn utf8_units_between(&self, start: usize, end: usize) -> u32 {
        let mut total = 0u32;
        let mut offset = start;

        while offset < end {
            if let Some(sequence) = self.invalid_sequence_at(offset) {
                total = total.saturating_add(sequence.raw_len);
                offset = sequence.decoded_range.end().to_usize();
                continue;
            }
            let Some(ch) = self.text[offset..end].chars().next() else {
                break;
            };
            total = total.saturating_add(u32::try_from(ch.len_utf8()).unwrap_or(u32::MAX));
            offset += ch.len_utf8();
        }

        total
    }

    fn utf16_units_between(&self, start: usize, end: usize) -> usize {
        let mut total = 0usize;
        let mut offset = start;

        while offset < end {
            if let Some(sequence) = self.invalid_sequence_at(offset) {
                total = total.saturating_add(sequence.raw_len as usize);
                offset = sequence.decoded_range.end().to_usize();
                continue;
            }
            let Some(ch) = self.text[offset..end].chars().next() else {
                break;
            };
            total = total.saturating_add(ch.len_utf16());
            offset += ch.len_utf8();
        }

        total
    }

    fn original_byte_offset(&self, offset: usize) -> u32 {
        let sequence_count = self
            .invalid_utf8
            .partition_point(|sequence| sequence.decoded_range.end().to_usize() <= offset);
        let delta = sequence_count
            .checked_sub(1)
            .and_then(|index| self.invalid_utf8_prefix_delta.get(index))
            .copied()
            .unwrap_or(0);
        let original = (self.bom_shift as i64)
            .saturating_add(offset as i64)
            .saturating_add(delta)
            .clamp(0, u32::MAX as i64);
        original as u32
    }
}

impl StableHash for LineIndex {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xB4);
        self.text.stable_hash(hasher);
        hasher.write_tag(0);
        hasher.write_len(u64::try_from(self.line_starts.len()).unwrap_or(u64::MAX));
        for start in &self.line_starts {
            start.raw().stable_hash(hasher);
        }
        self.bom_shift.stable_hash(hasher);
        hasher.write_tag(1);
        hasher.write_len(u64::try_from(self.invalid_utf8.len()).unwrap_or(u64::MAX));
        for sequence in self.invalid_utf8.iter() {
            sequence.decoded_range.start().raw().stable_hash(hasher);
            sequence.decoded_range.end().raw().stable_hash(hasher);
            sequence.raw_len.stable_hash(hasher);
        }
        self.invalid_utf8_prefix_delta.stable_hash(hasher);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LspPosition {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based character offset using the negotiated position encoding.
    pub character: u32,
}

/// The decoded source snapshot used by syntax and diagnostic reporting.
#[derive(Clone, Debug)]
pub struct SourceFile {
    id: FileId,
    text: Arc<str>,
    had_bom: bool,
    line_index: Arc<LineIndex>,
    raw_bytes: Arc<[u8]>,
    invalid_utf8: Arc<[InvalidUtf8Sequence]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceDecodeError {
    SourceTooLarge,
    UnremovedLeadingBom,
}

impl SourceDecodeError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::SourceTooLarge => "source file exceeds the 16 MiB implementation limit",
            Self::UnremovedLeadingBom => {
                "decoded source text starts with a BOM that was not recorded"
            }
        }
    }
}

/// The source-decoding outcome recorded alongside a decoded source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceTextStatus {
    Ready,
    Missing,
    ReadError { io_error_kind: String },
    TooLarge { byte_len: usize },
}

impl StableHash for SourceTextStatus {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xA9);
        match self {
            Self::Ready => hasher.write_tag(0),
            Self::Missing => hasher.write_tag(1),
            Self::ReadError { io_error_kind } => {
                hasher.write_tag(2);
                io_error_kind.stable_hash(hasher);
            }
            Self::TooLarge { byte_len } => {
                hasher.write_tag(3);
                (*byte_len as u64).stable_hash(hasher);
            }
        }
    }
}

/// Decoded text plus the input condition needed by diagnostics and the driver.
///
/// The cached value's `FileId` is deliberately excluded from equality and
/// stable hashing. `VfsInputs::source_text` attaches the requesting session ID
/// after reading the memoized result.
#[derive(Clone, Debug)]
pub struct SourceText {
    source: SourceFile,
    status: SourceTextStatus,
}

impl SourceText {
    pub const fn source(&self) -> &SourceFile {
        &self.source
    }

    pub const fn status(&self) -> &SourceTextStatus {
        &self.status
    }

    fn for_file_id(&self, file_id: FileId) -> Self {
        Self {
            source: self.source.with_file_id(file_id),
            status: self.status.clone(),
        }
    }
}

impl PartialEq for SourceText {
    fn eq(&self, other: &Self) -> bool {
        self.status == other.status && self.source.raw_bytes == other.source.raw_bytes
    }
}

impl Eq for SourceText {}

impl StableHash for SourceText {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xAA);
        self.status.stable_hash(hasher);
        self.source.raw_bytes.stable_hash(hasher);
    }
}

fn decode_source_text(contents: Option<FileContents>) -> SourceText {
    let (bytes, status) = match contents {
        None => (Arc::<[u8]>::from([]), SourceTextStatus::Missing),
        Some(FileContents::ReadError { io_error_kind }) => (
            Arc::<[u8]>::from([]),
            SourceTextStatus::ReadError { io_error_kind },
        ),
        Some(FileContents::Bytes(bytes)) if bytes.len() > MAX_SOURCE_BYTES => (
            Arc::<[u8]>::from([]),
            SourceTextStatus::TooLarge {
                byte_len: bytes.len(),
            },
        ),
        Some(FileContents::Bytes(bytes)) => (bytes, SourceTextStatus::Ready),
    };
    let source = match SourceFile::decode(FileId::new(0), &bytes) {
        Ok(source) => source,
        Err(SourceDecodeError::SourceTooLarge) => {
            return SourceText {
                source: SourceFile::new(FileId::new(0), "", false)
                    .expect("empty source always fits the source size limit"),
                status: SourceTextStatus::TooLarge {
                    byte_len: bytes.len(),
                },
            };
        }
        Err(SourceDecodeError::UnremovedLeadingBom) => {
            unreachable!("byte decoding always records and removes a leading BOM")
        }
    };
    SourceText { source, status }
}

/// Opaque identity for one consistent VFS snapshot of a workspace's file set.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct WorkspaceSnapshotId(u64);

impl std::fmt::Debug for WorkspaceSnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkspaceSnapshotId")
    }
}

static NEXT_WORKSPACE_SNAPSHOT_ID: Mutex<u64> = Mutex::new(1);

/// One file and its source-reference metadata inside a workspace snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotFile {
    source: Arc<SourceFile>,
    path: String,
    package: Option<String>,
    document_version: Option<u64>,
}

impl SnapshotFile {
    pub fn new(
        source: Arc<SourceFile>,
        path: impl Into<String>,
        package: Option<String>,
        document_version: Option<u64>,
    ) -> Self {
        Self {
            source,
            path: path.into(),
            package,
            document_version,
        }
    }

    pub fn source(&self) -> &Arc<SourceFile> {
        &self.source
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    pub const fn document_version(&self) -> Option<u64> {
        self.document_version
    }

    pub fn matches_reference(
        &self,
        path: &str,
        package: Option<&str>,
        document_version: Option<u64>,
    ) -> bool {
        self.path == path
            && self.package.as_deref() == package
            && self.document_version == document_version
    }
}

/// The content or read failure observed for one file input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileContents {
    Bytes(Arc<[u8]>),
    ReadError { io_error_kind: String },
}

impl FileContents {
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::ReadError { .. } => None,
        }
    }

    pub fn io_error_kind(&self) -> Option<&str> {
        match self {
            Self::Bytes(_) => None,
            Self::ReadError { io_error_kind } => Some(io_error_kind),
        }
    }
}

impl StableHash for FileContents {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xA8);
        match self {
            Self::Bytes(bytes) => {
                hasher.write_tag(0);
                bytes.stable_hash(hasher);
            }
            Self::ReadError { io_error_kind } => {
                hasher.write_tag(1);
                io_error_kind.stable_hash(hasher);
            }
        }
    }
}

/// The kind recorded by the package source-directory inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageFileKind {
    File,
    Directory,
    Symlink,
}

impl StableHash for PackageFileKind {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xAE);
        hasher.write_tag(match self {
            Self::File => 0,
            Self::Directory => 1,
            Self::Symlink => 2,
        });
    }
}

/// One entry below a package's source directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFileEntry {
    path: PackageSourcePath,
    kind: PackageFileKind,
}

impl PackageFileEntry {
    pub fn new(path: impl Into<String>, kind: PackageFileKind) -> Result<Self, PackagePathError> {
        Ok(Self {
            path: PackageSourcePath::new(path)?,
            kind,
        })
    }

    pub fn path(&self) -> &PackageSourcePath {
        &self.path
    }

    pub const fn kind(&self) -> PackageFileKind {
        self.kind
    }
}

impl StableHash for PackageFileEntry {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xAF);
        self.path.stable_hash(hasher);
        self.kind.stable_hash(hasher);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageFilesSnapshotError {
    EntriesForMissingSourceDirectory,
    DuplicatePath,
    MissingDirectoryAncestor,
    DescendantOfNonDirectory,
}

/// The observed state of one package's source directory and all its entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFilesSnapshot {
    source_directory_exists: bool,
    entries: Arc<[PackageFileEntry]>,
}

impl PackageFilesSnapshot {
    pub fn new(
        source_directory_exists: bool,
        entries: impl IntoIterator<Item = PackageFileEntry>,
    ) -> Result<Self, PackageFilesSnapshotError> {
        let mut entries = entries.into_iter().collect::<Vec<_>>();
        if !source_directory_exists && !entries.is_empty() {
            return Err(PackageFilesSnapshotError::EntriesForMissingSourceDirectory);
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(PackageFilesSnapshotError::DuplicatePath);
        }
        let kinds_by_path = entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.kind))
            .collect::<BTreeMap<_, _>>();
        for entry in &entries {
            let mut path = entry.path.as_str();
            while let Some((parent, _)) = path.rsplit_once('/') {
                match kinds_by_path.get(parent) {
                    Some(PackageFileKind::Directory) => {}
                    Some(PackageFileKind::File | PackageFileKind::Symlink) => {
                        return Err(PackageFilesSnapshotError::DescendantOfNonDirectory);
                    }
                    None => return Err(PackageFilesSnapshotError::MissingDirectoryAncestor),
                }
                path = parent;
            }
        }
        Ok(Self {
            source_directory_exists,
            entries: entries.into(),
        })
    }

    pub const fn source_directory_exists(&self) -> bool {
        self.source_directory_exists
    }

    pub fn entries(&self) -> &[PackageFileEntry] {
        &self.entries
    }
}

impl StableHash for PackageFilesSnapshot {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xB0);
        self.source_directory_exists.stable_hash(hasher);
        self.entries.stable_hash(hasher);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WitFilesSnapshotError {
    NotDirectWitFile,
    DuplicatePath,
    InvalidDirectory,
}

/// The WIT file paths selected for one package, in package-relative form.
///
/// `NotApplicable` represents a package without a usable declared WIT
/// directory; `Listed([])` represents an existing declared directory with no
/// direct `.wit` files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitFilesSnapshot {
    NotApplicable,
    Listed {
        directory: String,
        files: Arc<[PackageRelativePath]>,
    },
}

impl WitFilesSnapshot {
    pub fn listed(
        directory: impl Into<String>,
        paths: impl IntoIterator<Item = PackageRelativePath>,
    ) -> Result<Self, WitFilesSnapshotError> {
        let directory = directory.into();
        if directory != "." && !is_canonical_relative_path(&directory) {
            return Err(WitFilesSnapshotError::InvalidDirectory);
        }
        let directory_prefix = (directory != ".").then(|| format!("{directory}/"));
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        if paths.iter().any(|path| {
            let relative_path = match directory_prefix.as_deref() {
                None => path.as_str(),
                Some(prefix) => {
                    let Some(relative_path) = path.as_str().strip_prefix(prefix) else {
                        return true;
                    };
                    relative_path
                }
            };
            relative_path.contains('/')
                || relative_path.len() <= 4
                || !relative_path.ends_with(".wit")
        }) {
            return Err(WitFilesSnapshotError::NotDirectWitFile);
        }
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(WitFilesSnapshotError::DuplicatePath);
        }
        Ok(Self::Listed {
            directory,
            files: paths.into(),
        })
    }

    pub const fn not_applicable() -> Self {
        Self::NotApplicable
    }

    pub fn files(&self) -> Option<&[PackageRelativePath]> {
        match self {
            Self::NotApplicable => None,
            Self::Listed { files, .. } => Some(files),
        }
    }

    pub fn directory(&self) -> Option<&str> {
        match self {
            Self::NotApplicable => None,
            Self::Listed { directory, .. } => Some(directory),
        }
    }
}

impl StableHash for WitFilesSnapshot {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xB1);
        match self {
            Self::NotApplicable => hasher.write_tag(0),
            Self::Listed { directory, files } => {
                hasher.write_tag(1);
                directory.stable_hash(hasher);
                files.stable_hash(hasher);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum VfsInputKey {
    FileContents(FileKey),
    PackageFiles(PackageKey),
    WitFiles(PackageKey),
}

impl StableHash for VfsInputKey {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xB2);
        match self {
            Self::FileContents(key) => {
                hasher.write_tag(0);
                key.stable_hash(hasher);
            }
            Self::PackageFiles(key) => {
                hasher.write_tag(1);
                key.stable_hash(hasher);
            }
            Self::WitFiles(key) => {
                hasher.write_tag(2);
                key.stable_hash(hasher);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum VfsInputValue {
    FileContents(FileContents),
    PackageFiles(PackageFilesSnapshot),
    WitFiles(WitFilesSnapshot),
}

impl StableHash for VfsInputValue {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xB3);
        match self {
            Self::FileContents(contents) => {
                hasher.write_tag(0);
                contents.stable_hash(hasher);
            }
            Self::PackageFiles(snapshot) => {
                hasher.write_tag(1);
                snapshot.stable_hash(hasher);
            }
            Self::WitFiles(snapshot) => {
                hasher.write_tag(2);
                snapshot.stable_hash(hasher);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsInputSnapshotError {
    DuplicateFileKey,
    DuplicatePackageFilesKey,
    DuplicateWitFilesKey,
}

/// An immutable set of filesystem inputs, collected before source decoding.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VfsInputSnapshot {
    files: BTreeMap<FileKey, (FileContents, Durability)>,
    package_files: BTreeMap<PackageKey, (PackageFilesSnapshot, Durability)>,
    wit_files: BTreeMap<PackageKey, (WitFilesSnapshot, Durability)>,
}

impl VfsInputSnapshot {
    pub fn new(
        files: impl IntoIterator<Item = (FileKey, FileContents, Durability)>,
    ) -> Result<Self, VfsInputSnapshotError> {
        let mut snapshot = Self::default();
        for (key, contents, durability) in files {
            if snapshot.files.insert(key, (contents, durability)).is_some() {
                return Err(VfsInputSnapshotError::DuplicateFileKey);
            }
        }
        Ok(snapshot)
    }

    pub fn with_package_files(
        mut self,
        packages: impl IntoIterator<Item = (PackageKey, PackageFilesSnapshot, Durability)>,
    ) -> Result<Self, VfsInputSnapshotError> {
        for (key, files, durability) in packages {
            if self
                .package_files
                .insert(key, (files, durability))
                .is_some()
            {
                return Err(VfsInputSnapshotError::DuplicatePackageFilesKey);
            }
        }
        Ok(self)
    }

    pub fn with_wit_files(
        mut self,
        packages: impl IntoIterator<Item = (PackageKey, WitFilesSnapshot, Durability)>,
    ) -> Result<Self, VfsInputSnapshotError> {
        for (key, files, durability) in packages {
            if self.wit_files.insert(key, (files, durability)).is_some() {
                return Err(VfsInputSnapshotError::DuplicateWitFilesKey);
            }
        }
        Ok(self)
    }

    pub fn len(&self) -> usize {
        self.files.len() + self.package_files.len() + self.wit_files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.package_files.is_empty() && self.wit_files.is_empty()
    }

    fn file_iter(&self) -> impl Iterator<Item = (&FileKey, &FileContents, Durability)> {
        self.files
            .iter()
            .map(|(key, (contents, durability))| (key, contents, *durability))
    }

    fn package_files_iter(
        &self,
    ) -> impl Iterator<Item = (&PackageKey, &PackageFilesSnapshot, Durability)> {
        self.package_files
            .iter()
            .map(|(key, (files, durability))| (key, files, *durability))
    }

    fn wit_files_iter(&self) -> impl Iterator<Item = (&PackageKey, &WitFilesSnapshot, Durability)> {
        self.wit_files
            .iter()
            .map(|(key, (files, durability))| (key, files, *durability))
    }
}

/// One filesystem input change in a snapshot update.
pub enum VfsInputUpdate {
    Set {
        key: FileKey,
        contents: FileContents,
        durability: Durability,
    },
    Remove {
        key: FileKey,
    },
    SetPackageFiles {
        key: PackageKey,
        files: PackageFilesSnapshot,
        durability: Durability,
    },
    RemovePackageFiles {
        key: PackageKey,
    },
    SetWitFiles {
        key: PackageKey,
        files: WitFilesSnapshot,
        durability: Durability,
    },
    RemoveWitFiles {
        key: PackageKey,
    },
}

/// Query-backed raw source inputs for the virtual file system.
///
/// This layer owns tracked raw inputs and memoized source decoding; syntax
/// queries consume the decoded source values above it.
#[derive(Clone)]
pub struct VfsInputs {
    inputs: InputQuery<VfsInputKey, VfsInputValue>,
    source_text_query: DerivedQuery<FileKey, SourceText>,
    line_index_query: DerivedQuery<FileKey, Arc<LineIndex>>,
}

impl VfsInputs {
    pub fn new() -> Result<Self, QueryError> {
        let inputs = InputQuery::new("vfs_inputs", Durability::Low)?;
        let source_text_inputs = inputs.clone();
        let source_text_query = DerivedQuery::<FileKey, SourceText>::new(
            "source_text",
            move |database: &mut QueryDatabase, key| {
                let contents = source_text_inputs
                    .get_opt(database, VfsInputKey::FileContents(key.clone()))?
                    .map(|value: Arc<VfsInputValue>| match value.as_ref() {
                        VfsInputValue::FileContents(contents) => contents.clone(),
                        _ => unreachable!("file-content keys only store file-content values"),
                    });
                Ok(decode_source_text(contents))
            },
        )?;
        let line_index_source_text_query = source_text_query.clone();
        let line_index_query = DerivedQuery::<FileKey, Arc<LineIndex>>::new(
            "line_index",
            move |database: &mut QueryDatabase, key| {
                let source_text = line_index_source_text_query.get(database, key.clone())?;
                Ok(Arc::clone(&source_text.source.line_index))
            },
        )?;
        Ok(Self {
            inputs,
            source_text_query,
            line_index_query,
        })
    }

    /// Sets the exact raw bytes for a stable file identity and package role.
    pub fn set_file_contents(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
        contents: Arc<[u8]>,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.set_file_input(database, key, FileContents::Bytes(contents), durability)
    }

    /// Records an I/O failure as a file input value.
    pub fn set_file_read_error(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
        io_error_kind: impl Into<String>,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.set_file_input(
            database,
            key,
            FileContents::ReadError {
                io_error_kind: io_error_kind.into(),
            },
            durability,
        )
    }

    fn set_file_input(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
        contents: FileContents,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.inputs.set_at_durability(
            database,
            VfsInputKey::FileContents(key),
            VfsInputValue::FileContents(contents),
            durability,
        )
    }

    /// Removes a file input when it leaves the current source snapshot.
    pub fn remove_file(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
    ) -> Result<bool, QueryError> {
        self.inputs.remove(database, VfsInputKey::FileContents(key))
    }

    /// Reads raw bytes and tracks absence as a dependency.
    pub fn file_contents(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
    ) -> Result<Option<FileContents>, QueryError> {
        self.inputs
            .get_opt(database, VfsInputKey::FileContents(key))
            .map(|value| {
                value.map(|value| match value.as_ref() {
                    VfsInputValue::FileContents(contents) => contents.clone(),
                    _ => unreachable!("file-content keys only store file-content values"),
                })
            })
    }

    pub fn set_package_files(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
        files: PackageFilesSnapshot,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.inputs.set_at_durability(
            database,
            VfsInputKey::PackageFiles(key),
            VfsInputValue::PackageFiles(files),
            durability,
        )
    }

    pub fn remove_package_files(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
    ) -> Result<bool, QueryError> {
        self.inputs.remove(database, VfsInputKey::PackageFiles(key))
    }

    pub fn package_files(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
    ) -> Result<Option<PackageFilesSnapshot>, QueryError> {
        self.inputs
            .get_opt(database, VfsInputKey::PackageFiles(key))
            .map(|value| {
                value.map(|value| match value.as_ref() {
                    VfsInputValue::PackageFiles(files) => files.clone(),
                    _ => unreachable!("package-file keys only store package-file values"),
                })
            })
    }

    pub fn set_wit_files(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
        files: WitFilesSnapshot,
        durability: Durability,
    ) -> Result<bool, QueryError> {
        self.inputs.set_at_durability(
            database,
            VfsInputKey::WitFiles(key),
            VfsInputValue::WitFiles(files),
            durability,
        )
    }

    pub fn remove_wit_files(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
    ) -> Result<bool, QueryError> {
        self.inputs.remove(database, VfsInputKey::WitFiles(key))
    }

    pub fn wit_files(
        &self,
        database: &mut QueryDatabase,
        key: PackageKey,
    ) -> Result<Option<WitFilesSnapshot>, QueryError> {
        self.inputs
            .get_opt(database, VfsInputKey::WitFiles(key))
            .map(|value| {
                value.map(|value| match value.as_ref() {
                    VfsInputValue::WitFiles(files) => files.clone(),
                    _ => unreachable!("WIT-file keys only store WIT-file values"),
                })
            })
    }

    /// Decodes a file as a tracked query and binds it to the requested session ID.
    ///
    /// Missing files and read failures carry an empty source file so syntax
    /// recovery can continue; their status remains available for diagnostics.
    pub fn source_text(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
        file_id: FileId,
    ) -> Result<SourceText, QueryError> {
        self.source_text_query
            .get(database, key)
            .map(|source| source.for_file_id(file_id))
    }

    /// Returns the memoized line index derived from a file's decoded source.
    ///
    /// The query is keyed by stable file identity and shares the `source_text`
    /// query's decoding dependency. `LineIndex` contains no session-local
    /// `FileId`, so callers can bind spans to their own session IDs.
    pub fn line_index(
        &self,
        database: &mut QueryDatabase,
        key: FileKey,
    ) -> Result<Arc<LineIndex>, QueryError> {
        self.line_index_query
            .get(database, key)
            .map(|line_index| Arc::clone(line_index.as_ref()))
    }

    /// Applies a group of file changes in one query revision.
    pub fn apply_updates(
        &self,
        database: &mut QueryDatabase,
        updates: impl IntoIterator<Item = VfsInputUpdate>,
    ) -> Result<usize, QueryError> {
        self.inputs.apply_changes(
            database,
            updates.into_iter().map(|update| match update {
                VfsInputUpdate::Set {
                    key,
                    contents,
                    durability,
                } => sai_query::InputChange::Set {
                    key: VfsInputKey::FileContents(key),
                    value: VfsInputValue::FileContents(contents),
                    durability,
                },
                VfsInputUpdate::Remove { key } => sai_query::InputChange::Remove {
                    key: VfsInputKey::FileContents(key),
                },
                VfsInputUpdate::SetPackageFiles {
                    key,
                    files,
                    durability,
                } => sai_query::InputChange::Set {
                    key: VfsInputKey::PackageFiles(key),
                    value: VfsInputValue::PackageFiles(files),
                    durability,
                },
                VfsInputUpdate::RemovePackageFiles { key } => sai_query::InputChange::Remove {
                    key: VfsInputKey::PackageFiles(key),
                },
                VfsInputUpdate::SetWitFiles {
                    key,
                    files,
                    durability,
                } => sai_query::InputChange::Set {
                    key: VfsInputKey::WitFiles(key),
                    value: VfsInputValue::WitFiles(files),
                    durability,
                },
                VfsInputUpdate::RemoveWitFiles { key } => sai_query::InputChange::Remove {
                    key: VfsInputKey::WitFiles(key),
                },
            }),
        )
    }

    /// Reconciles raw input snapshots, removing omitted files and applying
    /// every content change in one query revision.
    ///
    /// Pass `None` only when initializing an empty database's VFS inputs.
    pub fn replace_snapshot(
        &self,
        database: &mut QueryDatabase,
        previous: Option<&VfsInputSnapshot>,
        current: &VfsInputSnapshot,
    ) -> Result<usize, QueryError> {
        let mut updates = Vec::with_capacity(current.len());
        let mut current_keys = BTreeSet::new();
        for (key, contents, durability) in current.file_iter() {
            let key = key.clone();
            current_keys.insert(VfsInputKey::FileContents(key.clone()));
            updates.push(VfsInputUpdate::Set {
                key,
                contents: contents.clone(),
                durability,
            });
        }
        for (key, files, durability) in current.package_files_iter() {
            let key = key.clone();
            current_keys.insert(VfsInputKey::PackageFiles(key.clone()));
            updates.push(VfsInputUpdate::SetPackageFiles {
                key,
                files: files.clone(),
                durability,
            });
        }
        for (key, files, durability) in current.wit_files_iter() {
            let key = key.clone();
            current_keys.insert(VfsInputKey::WitFiles(key.clone()));
            updates.push(VfsInputUpdate::SetWitFiles {
                key,
                files: files.clone(),
                durability,
            });
        }
        if let Some(previous) = previous {
            for (key, _, _) in previous.file_iter() {
                if !current_keys.contains(&VfsInputKey::FileContents(key.clone())) {
                    updates.push(VfsInputUpdate::Remove { key: key.clone() });
                }
            }
            for (key, _, _) in previous.package_files_iter() {
                if !current_keys.contains(&VfsInputKey::PackageFiles(key.clone())) {
                    updates.push(VfsInputUpdate::RemovePackageFiles { key: key.clone() });
                }
            }
            for (key, _, _) in previous.wit_files_iter() {
                if !current_keys.contains(&VfsInputKey::WitFiles(key.clone())) {
                    updates.push(VfsInputUpdate::RemoveWitFiles { key: key.clone() });
                }
            }
        }
        self.apply_updates(database, updates)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceSnapshotError {
    EmptySnapshot,
    DuplicateFileId,
    DuplicateSourcePath,
    InvalidSourcePath,
    InvalidPackageIdentity,
    PackageIdentityMismatch,
    IdExhausted,
    LockPoisoned,
}

/// An immutable source-file set and document versions observed together.
///
/// The package graph and build configuration are session inputs owned by the
/// driver/query layer; this VFS value does not represent those inputs.
#[derive(Clone, Debug)]
pub struct WorkspaceSnapshot {
    id: WorkspaceSnapshotId,
    files: Arc<BTreeMap<FileId, SnapshotFile>>,
}

impl PartialEq for WorkspaceSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for WorkspaceSnapshot {}

impl WorkspaceSnapshot {
    pub fn new(
        files: impl IntoIterator<Item = SnapshotFile>,
    ) -> Result<Self, WorkspaceSnapshotError> {
        let mut by_id = BTreeMap::new();
        let mut by_path = BTreeMap::new();
        for file in files {
            if !is_canonical_source_path(&file.path) {
                return Err(WorkspaceSnapshotError::InvalidSourcePath);
            }
            if let Some(package) = file.package.as_deref() {
                if !is_package_identity(package) {
                    return Err(WorkspaceSnapshotError::InvalidPackageIdentity);
                }
            }
            if let Some(path_identity) = dependency_path_identity(&file.path) {
                if file.package.as_deref() != Some(path_identity) {
                    return Err(WorkspaceSnapshotError::PackageIdentityMismatch);
                }
            }
            let id = file.source.id;
            if by_id.contains_key(&id) {
                return Err(WorkspaceSnapshotError::DuplicateFileId);
            }
            if by_path.insert(file.path.clone(), id).is_some() {
                return Err(WorkspaceSnapshotError::DuplicateSourcePath);
            }
            by_id.insert(id, file);
        }
        if by_id.is_empty() {
            return Err(WorkspaceSnapshotError::EmptySnapshot);
        }

        let mut next_id = NEXT_WORKSPACE_SNAPSHOT_ID
            .lock()
            .map_err(|_| WorkspaceSnapshotError::LockPoisoned)?;
        let id = *next_id;
        *next_id = next_id
            .checked_add(1)
            .ok_or(WorkspaceSnapshotError::IdExhausted)?;

        Ok(Self {
            id: WorkspaceSnapshotId(id),
            files: Arc::new(by_id),
        })
    }

    pub const fn id(&self) -> WorkspaceSnapshotId {
        self.id
    }

    pub fn file(&self, id: FileId) -> Option<&SnapshotFile> {
        self.files.get(&id)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &SnapshotFile> {
        self.files.values()
    }
}

impl SourceFile {
    fn with_file_id(&self, id: FileId) -> Self {
        Self {
            id,
            text: Arc::clone(&self.text),
            had_bom: self.had_bom,
            line_index: Arc::clone(&self.line_index),
            raw_bytes: Arc::clone(&self.raw_bytes),
            invalid_utf8: Arc::clone(&self.invalid_utf8),
        }
    }

    pub fn new(
        id: FileId,
        text: impl Into<Arc<str>>,
        had_bom: bool,
    ) -> Result<Self, SourceDecodeError> {
        let text = text.into();
        if !had_bom && text.starts_with('\u{FEFF}') {
            return Err(SourceDecodeError::UnremovedLeadingBom);
        }
        let original_len = text
            .len()
            .checked_add(if had_bom { 3 } else { 0 })
            .ok_or(SourceDecodeError::SourceTooLarge)?;
        if original_len > MAX_SOURCE_BYTES {
            return Err(SourceDecodeError::SourceTooLarge);
        }
        let mut raw_bytes = Vec::with_capacity(original_len);
        if had_bom {
            raw_bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        raw_bytes.extend_from_slice(text.as_bytes());
        let line_index = Arc::new(LineIndex::with_bom(Arc::clone(&text), had_bom));
        Ok(Self {
            id,
            text,
            had_bom,
            line_index,
            raw_bytes: Arc::from(raw_bytes),
            invalid_utf8: Arc::from([]),
        })
    }

    /// Decodes source bytes, removes a leading BOM, and replaces each maximal
    /// ill-formed UTF-8 subsequence with U+FFFD while retaining its raw width.
    pub fn decode(id: FileId, bytes: &[u8]) -> Result<Self, SourceDecodeError> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(SourceDecodeError::SourceTooLarge);
        }

        let raw_bytes: Arc<[u8]> = Arc::from(bytes);
        let had_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let bytes = if had_bom { &bytes[3..] } else { bytes };
        let mut text = String::with_capacity(bytes.len());
        let mut invalid_utf8 = Vec::new();
        let mut offset = 0usize;

        while offset < bytes.len() {
            match std::str::from_utf8(&bytes[offset..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    offset = bytes.len();
                }
                Err(error) => {
                    let valid_len = error.valid_up_to();
                    if valid_len > 0 {
                        let valid = std::str::from_utf8(&bytes[offset..offset + valid_len])
                            .map_err(|_| SourceDecodeError::SourceTooLarge)?;
                        text.push_str(valid);
                        offset += valid_len;
                    }

                    let raw_len = error
                        .error_len()
                        .unwrap_or_else(|| bytes.len().saturating_sub(offset));
                    if raw_len == 0 {
                        return Err(SourceDecodeError::SourceTooLarge);
                    }

                    let start = TextSize::from_usize(text.len())
                        .ok_or(SourceDecodeError::SourceTooLarge)?;
                    text.push('\u{FFFD}');
                    let end = TextSize::from_usize(text.len())
                        .ok_or(SourceDecodeError::SourceTooLarge)?;
                    let raw_len =
                        u32::try_from(raw_len).map_err(|_| SourceDecodeError::SourceTooLarge)?;
                    invalid_utf8.push(InvalidUtf8Sequence {
                        decoded_range: TextRange::new(start, end)
                            .ok_or(SourceDecodeError::SourceTooLarge)?,
                        raw_len,
                    });
                    offset = offset.saturating_add(raw_len as usize);
                }
            }
        }

        let text: Arc<str> = Arc::from(text);
        let invalid_utf8: Arc<[InvalidUtf8Sequence]> = Arc::from(invalid_utf8);
        let line_index = Arc::new(LineIndex::with_decode_metadata(
            Arc::clone(&text),
            had_bom,
            Arc::clone(&invalid_utf8),
        ));

        Ok(Self {
            id,
            text,
            had_bom,
            line_index,
            raw_bytes,
            invalid_utf8,
        })
    }

    pub fn invalid_utf8_sequences(&self) -> &[InvalidUtf8Sequence] {
        &self.invalid_utf8
    }

    pub fn invalid_utf8_sequence_at(&self, decoded_start: TextSize) -> Option<InvalidUtf8Sequence> {
        let index = self
            .invalid_utf8
            .partition_point(|sequence| sequence.decoded_range().start() < decoded_start);
        self.invalid_utf8
            .get(index)
            .copied()
            .filter(|sequence| sequence.decoded_range().start() == decoded_start)
    }

    pub const fn id(&self) -> FileId {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub const fn had_bom(&self) -> bool {
        self.had_bom
    }

    pub fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    /// The exact bytes read for this source file, including a leading BOM.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw_bytes
    }

    /// Shares the immutable original source bytes without copying them.
    pub fn raw_bytes_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.raw_bytes)
    }

    /// Maps a decoded-text offset to its byte offset in the original source.
    pub fn to_original_offset(&self, decoded: TextSize) -> Option<TextSize> {
        if decoded.to_usize() > self.text.len() || !self.text.is_char_boundary(decoded.to_usize()) {
            return None;
        }
        TextSize::from_usize(self.line_index.original_byte_offset(decoded.to_usize()) as usize)
    }

    /// Maps a decoded-text range to the corresponding raw-source byte range.
    pub fn to_original_range(&self, decoded: TextRange) -> Option<TextRange> {
        let original = TextRange::new(
            self.to_original_offset(decoded.start())?,
            self.to_original_offset(decoded.end())?,
        )?;
        (self.is_original_char_boundary(original.start())
            && self.is_original_char_boundary(original.end())
            && self.is_original_crlf_boundary(original.start())
            && self.is_original_crlf_boundary(original.end()))
        .then_some(original)
    }

    /// Returns whether an original-source byte offset is between UTF-8 scalars.
    ///
    /// Every byte of malformed UTF-8 is treated as its own boundary.
    pub fn is_original_char_boundary(&self, offset: TextSize) -> bool {
        let offset = offset.to_usize();
        let bytes = self.raw_bytes();
        if offset > bytes.len() {
            return false;
        }
        if offset == 0 || offset == bytes.len() {
            return true;
        }

        for start in offset.saturating_sub(3)..offset {
            let width = match bytes[start] {
                0x00..=0x7F => 1,
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => continue,
            };
            let end = start + width;
            if start < offset
                && offset < end
                && end <= bytes.len()
                && std::str::from_utf8(&bytes[start..end]).is_ok()
            {
                return false;
            }
        }
        true
    }

    /// Returns whether an original-source byte offset does not split CR LF.
    pub fn is_original_crlf_boundary(&self, offset: TextSize) -> bool {
        let offset = offset.to_usize();
        let bytes = self.raw_bytes();
        offset == 0 || offset >= bytes.len() || bytes[offset - 1] != b'\r' || bytes[offset] != b'\n'
    }
}

fn is_canonical_source_path(path: &str) -> bool {
    if path == "<stdin>" {
        return true;
    }
    let relative_path = if let Some(dependency) = path.strip_prefix("dep:") {
        let Some((identity, relative_path)) = dependency.split_once('/') else {
            return false;
        };
        if !is_package_identity(identity) {
            return false;
        }
        relative_path
    } else {
        path
    };

    is_canonical_relative_path(relative_path)
}

fn is_canonical_relative_path(path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || is_windows_drive_path(path)
        || path.contains('\\')
        || path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return false;
    }
    !path
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
}

fn is_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_package_identity(identity: &str) -> bool {
    let Some((namespace, package_version)) = identity.split_once(':') else {
        return false;
    };
    let Some((package, version)) = package_version.split_once('@') else {
        return false;
    };
    !version.contains('@')
        && is_package_segment(namespace)
        && is_package_segment(package)
        && is_semver_without_build(version)
}

fn is_stable_package_key(package_key: &str) -> bool {
    if matches!(package_key, "sai:core" | "sai:alloc") {
        return true;
    }
    if let Some(wasi_key) = package_key.strip_prefix("wit:wasi:") {
        let Some((name, version)) = wasi_key.split_once('@') else {
            return false;
        };
        return !version.contains('@')
            && is_package_segment(name)
            && is_semver_without_build(version);
    }
    if let Some(package_key) = package_key
        .strip_prefix("wit:dir:")
        .or_else(|| package_key.strip_prefix("wit:gen:"))
    {
        return matches!(package_key, "sai:core" | "sai:alloc")
            || is_regular_package_key(package_key);
    }
    is_regular_package_key(package_key)
}

fn is_regular_package_key(package_key: &str) -> bool {
    let Some((identity, line)) = package_key.split_once('#') else {
        return false;
    };
    let Some((namespace, name)) = identity.split_once(':') else {
        return false;
    };
    is_package_segment(namespace) && is_package_segment(name) && is_compatibility_line(line)
}

fn is_compatibility_line(line: &str) -> bool {
    let components = line.split('.').collect::<Vec<_>>();
    match components.as_slice() {
        [major] => canonical_decimal(major).is_some_and(|major| major >= 1),
        ["0", minor] => canonical_decimal(minor).is_some_and(|minor| minor >= 1),
        ["0", "0", patch] => canonical_decimal(patch).is_some(),
        _ => false,
    }
}

fn canonical_decimal(value: &str) -> Option<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

fn package_key_matches_release(package_key: &str, package_identity: &str) -> bool {
    let Some((identity, version)) = package_identity.split_once('@') else {
        return false;
    };
    if !is_package_identity(package_identity) {
        return false;
    }
    if matches!(package_key, "sai:core" | "sai:alloc") {
        return identity == package_key;
    }
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let mut components = core.split('.');
    let (Some(major), Some(minor), Some(patch)) = (
        components.next().and_then(canonical_decimal),
        components.next().and_then(canonical_decimal),
        components.next().and_then(canonical_decimal),
    ) else {
        return false;
    };
    if components.next().is_some() {
        return false;
    }
    let line = if major >= 1 {
        major.to_string()
    } else if minor >= 1 {
        format!("0.{minor}")
    } else {
        format!("0.0.{patch}")
    };
    package_key == format!("{identity}#{line}")
}

fn dependency_path_identity(path: &str) -> Option<&str> {
    Some(path.strip_prefix("dep:")?.split_once('/')?.0)
}

fn is_package_segment(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > 64 {
        return false;
    }
    let bytes = segment.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || (*byte == b'-'
                && index > 0
                && index + 1 < bytes.len()
                && bytes[index - 1] != b'-'
                && (bytes[index + 1].is_ascii_lowercase() || bytes[index + 1].is_ascii_digit()))
    })
}

fn is_semver_without_build(version: &str) -> bool {
    if version.contains('+') {
        return false;
    }
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    let components = core.split('.').collect::<Vec<_>>();
    if components.len() != 3
        || components.iter().any(|component| {
            component.is_empty()
                || (component.len() > 1 && component.starts_with('0'))
                || !component.bytes().all(|byte| byte.is_ascii_digit())
                || component.parse::<u64>().is_err()
        })
    {
        return false;
    }
    let Some(prerelease) = prerelease else {
        return true;
    };
    !prerelease.is_empty()
        && prerelease.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !(identifier.len() > 1
                    && identifier.bytes().all(|byte| byte.is_ascii_digit())
                    && identifier.starts_with('0'))
                && (!identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || identifier.parse::<u64>().is_ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_query::DerivedQuery;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn size(raw: usize) -> TextSize {
        TextSize::from_usize(raw).expect("test offset fits in u32")
    }

    #[test]
    fn recognizes_lf_crlf_and_lone_cr_as_line_breaks() {
        let index = LineIndex::new("a\nb\r\nc\rd");

        assert_eq!(index.line_count(), 4);
        assert_eq!(index.line_col(size(0)), LineCol { line: 1, col: 1 });
        assert_eq!(index.line_col(size(2)), LineCol { line: 2, col: 1 });
        assert_eq!(index.line_col(size(5)), LineCol { line: 3, col: 1 });
        assert_eq!(index.line_col(size(7)), LineCol { line: 4, col: 1 });
    }

    #[test]
    fn reports_utf8_and_utf16_columns() {
        let index = LineIndex::new("a🦀é");

        assert_eq!(
            index.position(size(5)),
            Position {
                byte: 5,
                line: 1,
                col: 6,
                utf16_col: 4,
            }
        );
    }

    #[test]
    fn rounds_offsets_down_at_character_and_crlf_boundaries() {
        let index = LineIndex::new("a🦀\r\nb");

        assert_eq!(index.offset(1, 3), size(1));
        assert_eq!(index.line_col(size(6)), LineCol { line: 1, col: 6 });
        assert_eq!(index.position(size(7)).line, 2);
    }

    #[test]
    fn utf16_mapping_clamps_and_rounds_down_inside_surrogate_pairs() {
        let index = LineIndex::new("a🦀b");

        assert_eq!(index.from_utf16(1, 3), size(1));
        assert_eq!(index.from_utf16(1, 4), size(5));
        assert_eq!(index.from_utf16(1, 99), size(6));
    }

    #[test]
    fn text_ranges_reject_reversed_endpoints() {
        assert_eq!(TextRange::new(TextSize::new(4), TextSize::new(3)), None);
        assert_eq!(
            TextRange::new(TextSize::new(3), TextSize::new(4))
                .expect("ordered range")
                .len(),
            1
        );
    }

    #[test]
    fn trailing_line_break_creates_an_empty_final_line() {
        let index = LineIndex::new("value\n");

        assert_eq!(index.line_count(), 2);
        assert_eq!(index.line_col(size(6)), LineCol { line: 2, col: 1 });
    }

    #[test]
    fn reverse_position_mappings_clamp_before_line_terminators() {
        let index = LineIndex::new("a\r\nb");

        assert_eq!(index.offset(1, 99), size(1));
        assert_eq!(index.from_utf16(1, 99), size(1));
    }

    #[test]
    fn bom_shift_applies_to_file_bytes_and_lsp_positions() {
        let file = SourceFile::new(FileId::new(1), "a🦀b", true).expect("small source file");

        assert_eq!(
            file.line_index().position(size(1)),
            Position {
                byte: 4,
                line: 1,
                col: 2,
                utf16_col: 2,
            }
        );
        assert_eq!(
            file.line_index().to_lsp_utf16(size(1)),
            LspPosition {
                line: 0,
                character: 2,
            }
        );
        assert_eq!(file.line_index().from_lsp_utf16(0, 2), size(1));
        assert_eq!(
            file.line_index().to_lsp_utf8(size(1)),
            LspPosition {
                line: 0,
                character: 4,
            }
        );
        assert_eq!(file.line_index().from_lsp_utf8(0, 0), size(0));
        assert_eq!(file.line_index().from_lsp_utf8(0, 4), size(1));
    }

    #[test]
    fn bom_does_not_shift_decoded_columns_or_utf16_diagnostics() {
        let file = SourceFile::new(FileId::new(1), "a", true).expect("small source file");

        assert_eq!(file.raw_bytes(), b"\xEF\xBB\xBFa");
        assert_eq!(
            file.to_original_range(TextRange::new(size(0), size(1)).expect("ordered range")),
            TextRange::new(size(3), size(4))
        );
        assert_eq!(
            file.line_index().position(size(0)),
            Position {
                byte: 3,
                line: 1,
                col: 1,
                utf16_col: 1,
            }
        );
        assert_eq!(file.line_index().from_lsp_utf16(0, 0), size(0));
    }

    #[test]
    fn decoded_to_original_ranges_reject_character_and_crlf_interiors() {
        let file = SourceFile::new(FileId::new(1), "é\r\n", false).expect("small source file");

        assert_eq!(file.to_original_range(TextRange::empty(size(1))), None);
        assert_eq!(file.to_original_range(TextRange::empty(size(3))), None);
    }

    #[test]
    fn decoded_text_constructor_enforces_the_raw_source_size_limit() {
        let text = "a".repeat(MAX_SOURCE_BYTES + 1);

        assert!(matches!(
            SourceFile::new(FileId::new(1), text, false),
            Err(SourceDecodeError::SourceTooLarge)
        ));
    }

    #[test]
    fn decoded_text_constructor_preserves_the_leading_bom_contract() {
        assert!(matches!(
            SourceFile::new(FileId::new(1), "\u{FEFF}x", false),
            Err(SourceDecodeError::UnremovedLeadingBom)
        ));

        let file = SourceFile::new(FileId::new(1), "\u{FEFF}x", true)
            .expect("the second leading BOM is decoded source text");
        let decoded =
            SourceFile::decode(FileId::new(1), file.raw_bytes()).expect("raw bytes decode");

        assert_eq!(decoded.text(), file.text());
        assert_eq!(
            file.to_original_range(TextRange::new(size(0), size(3)).expect("ordered range")),
            TextRange::new(size(3), size(6))
        );
    }

    #[test]
    fn decoding_invalid_utf8_preserves_original_byte_and_column_positions() {
        let file = SourceFile::decode(FileId::new(1), b"\xFFa").expect("small source file");

        assert_eq!(file.text(), "\u{FFFD}a");
        assert_eq!(file.invalid_utf8_sequences().len(), 1);
        assert_eq!(
            file.invalid_utf8_sequence_at(size(0)),
            Some(file.invalid_utf8_sequences()[0])
        );
        assert_eq!(file.invalid_utf8_sequence_at(size(3)), None);
        assert_eq!(
            file.invalid_utf8_sequences()[0].decoded_range(),
            TextRange::new(size(0), size(3)).expect("ordered range")
        );
        assert_eq!(file.invalid_utf8_sequences()[0].raw_len(), 1);
        assert_eq!(
            file.line_index().position(size(3)),
            Position {
                byte: 1,
                line: 1,
                col: 2,
                utf16_col: 2,
            }
        );
        assert_eq!(file.line_index().offset(1, 2), size(3));
        assert_eq!(file.line_index().from_utf16(1, 2), size(3));
    }

    #[test]
    fn invalid_utf8_sequences_count_each_original_byte_as_a_character() {
        let file = SourceFile::decode(FileId::new(1), b"\xF0\x90\x80").expect("small source file");

        assert_eq!(file.text(), "\u{FFFD}");
        assert_eq!(file.invalid_utf8_sequences()[0].raw_len(), 3);
        assert_eq!(
            file.line_index().position(size(3)),
            Position {
                byte: 3,
                line: 1,
                col: 4,
                utf16_col: 4,
            }
        );
        assert_eq!(file.line_index().from_utf16(1, 2), size(0));
        assert_eq!(file.line_index().from_utf16(1, 4), size(3));
    }

    #[test]
    fn maps_offsets_after_many_invalid_utf8_sequences() {
        let count = 4096;
        let bytes = vec![0xFF; count];
        let file =
            SourceFile::decode(FileId::new(1), &bytes).expect("invalid UTF-8 is recoverable");

        assert_eq!(file.invalid_utf8_sequences().len(), count);
        for index in [0, 1, count / 2, count - 1, count] {
            let decoded_offset = size(index * 3);
            assert_eq!(file.to_original_offset(decoded_offset), Some(size(index)));
            assert_eq!(
                file.line_index().position(decoded_offset).byte,
                index as u32
            );
        }
    }

    #[test]
    fn decoding_tracks_invalid_utf8_after_a_removed_bom() {
        let file =
            SourceFile::decode(FileId::new(1), b"\xEF\xBB\xBF\xFFa").expect("small source file");

        assert!(file.had_bom());
        assert_eq!(file.raw_bytes(), b"\xEF\xBB\xBF\xFFa");
        assert_eq!(file.text(), "\u{FFFD}a");
        assert_eq!(file.line_index().position(size(3)).byte, 4);
        assert_eq!(file.line_index().position(size(3)).col, 2);
        assert_eq!(
            file.to_original_range(TextRange::new(size(3), size(4)).expect("ordered range")),
            TextRange::new(size(4), size(5))
        );
        assert!(file.is_original_char_boundary(size(4)));
        assert!(!file.is_original_char_boundary(size(1)));
    }

    #[test]
    fn decoding_rejects_files_over_the_documented_source_limit() {
        let bytes = vec![b'a'; MAX_SOURCE_BYTES + 1];

        assert!(matches!(
            SourceFile::decode(FileId::new(1), &bytes),
            Err(SourceDecodeError::SourceTooLarge)
        ));
    }

    #[test]
    fn workspace_snapshots_bind_file_paths_versions_and_contents() {
        let source =
            Arc::new(SourceFile::new(FileId::new(7), "source", false).expect("small source file"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::clone(&source),
            "src/lib.sai",
            Some("sai:test@0.1.0".to_owned()),
            Some(12),
        )])
        .expect("unique snapshot files");

        let file = snapshot.file(FileId::new(7)).expect("file exists");
        assert_eq!(file.path(), "src/lib.sai");
        assert_eq!(file.package(), Some("sai:test@0.1.0"));
        assert_eq!(file.document_version(), Some(12));
        assert!(Arc::ptr_eq(file.source(), &source));
        assert_eq!(snapshot.len(), 1);
    }

    #[test]
    fn workspace_snapshots_reject_duplicate_file_identities() {
        let source =
            |id| Arc::new(SourceFile::new(FileId::new(id), "x", false).expect("small source file"));

        assert_eq!(
            WorkspaceSnapshot::new([
                SnapshotFile::new(source(1), "src/a.sai", None, None),
                SnapshotFile::new(source(1), "src/b.sai", None, None),
            ])
            .map(|_| ()),
            Err(WorkspaceSnapshotError::DuplicateFileId)
        );
        assert_eq!(
            WorkspaceSnapshot::new([
                SnapshotFile::new(source(1), "src/a.sai", None, None),
                SnapshotFile::new(source(2), "src/a.sai", None, None),
            ])
            .map(|_| ()),
            Err(WorkspaceSnapshotError::DuplicateSourcePath)
        );
        assert_eq!(
            WorkspaceSnapshot::new([
                SnapshotFile::new(
                    source(1),
                    "src/a.sai",
                    Some("sai:first@1.0.0".to_owned()),
                    None,
                ),
                SnapshotFile::new(
                    source(2),
                    "src/a.sai",
                    Some("sai:second@1.0.0".to_owned()),
                    None,
                ),
            ])
            .map(|_| ()),
            Err(WorkspaceSnapshotError::DuplicateSourcePath)
        );
    }

    #[test]
    fn workspace_snapshots_reject_noncanonical_source_paths() {
        let source =
            || Arc::new(SourceFile::new(FileId::new(1), "x", false).expect("small source file"));

        for path in [
            "",
            "/src/a.sai",
            "C:/src/a.sai",
            "src/./a.sai",
            "src/../a.sai",
            "src//a.sai",
            "src\\a.sai",
            "dep:sai:core@1.0.0/../a.sai",
            "dep:bad namespace:bad name@not-a-semver/src/lib.sai",
            "dep:sai:core@1.0/src/lib.sai",
        ] {
            assert_eq!(
                WorkspaceSnapshot::new([SnapshotFile::new(source(), path, None, None)]).map(|_| ()),
                Err(WorkspaceSnapshotError::InvalidSourcePath),
                "path should be rejected: {path}"
            );
        }
    }

    #[test]
    fn workspace_snapshots_reject_invalid_package_identities() {
        for package in [
            "sai:bad name@1.0.0",
            "sai:bad--name@1.0.0",
            "sai:core@01.0.0",
            "sai:core@1.0.0+build",
            "sai:core@1.0.0-01",
            "sai:core@1.0.0-18446744073709551616",
        ] {
            let source =
                Arc::new(SourceFile::new(FileId::new(1), "x", false).expect("small source file"));
            assert_eq!(
                WorkspaceSnapshot::new([SnapshotFile::new(
                    source,
                    "src/lib.sai",
                    Some(package.to_owned()),
                    None,
                )])
                .map(|_| ()),
                Err(WorkspaceSnapshotError::InvalidPackageIdentity),
                "package identity should be rejected: {package}"
            );
        }
    }

    #[test]
    fn workspace_snapshots_accept_documented_source_path_forms() {
        for (id, path, package) in [
            (1, "src/user/model.sai", None),
            (2, "sai.package", None),
            (
                3,
                "dep:sai:http@1.4.2/src/client.sai",
                Some("sai:http@1.4.2"),
            ),
            (4, "<stdin>", None),
        ] {
            let source =
                Arc::new(SourceFile::new(FileId::new(id), "x", false).expect("small source file"));
            WorkspaceSnapshot::new([SnapshotFile::new(
                source,
                path,
                package.map(str::to_owned),
                None,
            )])
            .expect("documented path and package form is accepted");
        }
    }

    #[test]
    fn dependency_path_identity_must_match_the_package_reference() {
        for package in [None, Some("sai:other@1.4.2"), Some("sai:http@1.4.3")] {
            let source =
                Arc::new(SourceFile::new(FileId::new(3), "x", false).expect("small source file"));
            assert_eq!(
                WorkspaceSnapshot::new([SnapshotFile::new(
                    source,
                    "dep:sai:http@1.4.2/src/client.sai",
                    package.map(str::to_owned),
                    None,
                )])
                .map(|_| ()),
                Err(WorkspaceSnapshotError::PackageIdentityMismatch),
                "package must match the identity embedded in the dependency path"
            );
        }
    }

    #[test]
    fn workspace_snapshots_reject_empty_file_sets() {
        assert_eq!(
            WorkspaceSnapshot::new([]),
            Err(WorkspaceSnapshotError::EmptySnapshot)
        );
    }

    #[test]
    fn separately_created_workspace_snapshots_have_distinct_ids() {
        let make_snapshot = || {
            WorkspaceSnapshot::new([SnapshotFile::new(
                Arc::new(SourceFile::new(FileId::new(1), "x", false).expect("small source file")),
                "src/lib.sai",
                None,
                None,
            )])
            .expect("single-file snapshot")
        };

        assert_ne!(make_snapshot().id(), make_snapshot().id());
    }

    #[test]
    fn file_keys_and_fingerprints_use_explicit_stable_identity() {
        let first = FileKey::new("acme:app#0.1", "src/lib.sai").expect("file key is valid");
        let second = FileKey::new("acme:app#0.1", "src/lib.sai").expect("file key is valid");

        assert_eq!(first, second);
        assert_eq!(
            sai_stable_hash::fingerprint(&first),
            sai_stable_hash::fingerprint(&second)
        );
    }

    #[test]
    fn file_keys_include_package_and_path_identity() {
        let first_package =
            FileKey::new("acme:first#0.1", "src/lib.sai").expect("file key is valid");
        let second_package =
            FileKey::new("acme:second#0.1", "src/lib.sai").expect("file key is valid");
        let second_path =
            FileKey::new("acme:first#0.1", "src/main.sai").expect("file key is valid");

        assert_ne!(first_package, second_package);
        assert_ne!(first_package, second_path);
    }

    #[test]
    fn file_interner_reuses_stable_keys_and_rebinds_vfs_source_ids() {
        let vfs = VfsInputs::new().expect("VFS queries are available");
        let mut database = QueryDatabase::new();
        let mut interner = FileInterner::new();
        let first_key = FileKey::new("acme:app#0.1", "dep:acme:app@0.1.9/src/lib.sai")
            .expect("first source key is valid");
        let same_key = FileKey::new("acme:app#0.1", "dep:acme:app@0.1.0/src/lib.sai")
            .expect("equivalent source key is valid");
        let second_key =
            FileKey::new("acme:app#0.1", "src/main.sai").expect("second source key is valid");

        vfs.set_file_contents(
            &mut database,
            first_key.clone(),
            Arc::from(&b"fn lib() {}"[..]),
            Durability::Low,
        )
        .expect("file contents are set");

        assert!(interner.is_empty());
        let first_id = interner
            .intern(first_key.clone())
            .expect("file key is interned");
        assert_eq!(
            first_id,
            interner.intern(same_key).expect("same key is reused")
        );
        let second_id = interner
            .intern(second_key.clone())
            .expect("second key is interned");
        assert_ne!(first_id, second_id);
        assert_eq!(first_id.raw(), 0);
        assert_eq!(second_id.raw(), 1);
        assert_eq!(interner.len(), 2);
        assert_eq!(interner.file_key(first_id), Some(&first_key));
        assert_eq!(interner.file_key(second_id), Some(&second_key));
        assert_eq!(interner.file_key(FileId::new(u32::MAX)), None);

        let source = vfs
            .source_text(&mut database, first_key, first_id)
            .expect("VFS source uses the session-local ID");
        assert_eq!(source.source().id(), first_id);
    }

    #[test]
    fn file_keys_require_explicit_canonical_package_and_path_identity() {
        assert_eq!(
            FileKey::new("acme:app#0.1", "src/../lib.sai"),
            Err(FileKeyError::InvalidSourcePath)
        );
        assert_eq!(
            FileKey::new("acme:app#0.1", "dep:acme:other@0.1.0/src/lib.sai"),
            Err(FileKeyError::PackageIdentityMismatch)
        );
        assert_eq!(
            FileKey::new("acme:app@0.1.0", "src/lib.sai"),
            Err(FileKeyError::InvalidPackageKey)
        );
        assert_eq!(
            FileKey::new("acme:app#0.2", "dep:acme:app@0.1.9/src/lib.sai"),
            Err(FileKeyError::PackageIdentityMismatch)
        );
        let dependency_091 = FileKey::new("acme:app#0.1", "dep:acme:app@0.1.9/src/lib.sai")
            .expect("package reference is valid");
        let dependency_010 = FileKey::new("acme:app#0.1", "dep:acme:app@0.1.0/src/lib.sai")
            .expect("same compatibility line is valid");
        assert_eq!(dependency_091, dependency_010);
        assert_eq!(dependency_091.path(), "src/lib.sai");

        let core_dependency = FileKey::new("sai:core", "dep:sai:core@1.2.3/src/lib.sai")
            .expect("built-in package dependency is valid");
        assert_eq!(core_dependency.package_key(), "sai:core");
        assert_eq!(core_dependency.path(), "src/lib.sai");
        let alloc_dependency = FileKey::new("sai:alloc", "dep:sai:alloc@1.2.3/src/lib.sai")
            .expect("built-in package dependency is valid");
        assert_eq!(alloc_dependency.package_key(), "sai:alloc");
        assert_eq!(alloc_dependency.path(), "src/lib.sai");

        for package_key in [
            "sai:core",
            "sai:alloc",
            "acme:json#1",
            "acme:json#0.4",
            "acme:json#0.0.7",
            "wit:wasi:cli@0.2.0",
            "wit:dir:acme:json#0.4",
            "wit:gen:sai:core",
        ] {
            assert!(
                FileKey::new(package_key, "src/lib.sai").is_ok(),
                "stable package key should be accepted: {package_key}"
            );
        }
    }

    #[test]
    fn raw_file_inputs_invalidate_derived_queries_and_track_removal() {
        let inputs = VfsInputs::new().expect("input query ID is available");
        let mut database = QueryDatabase::new();
        let key = FileKey::new("acme:app#0.1", "src/main.sai").expect("file key is valid");
        assert_eq!(
            inputs
                .file_contents(&mut database, key.clone())
                .expect("absent input is tracked"),
            None
        );

        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_query = Arc::clone(&runs);
        let inputs_in_query = inputs.clone();
        let raw_length =
            DerivedQuery::<FileKey, u32>::new("raw_file_length", move |database, key| {
                runs_in_query.fetch_add(1, Ordering::SeqCst);
                Ok(inputs_in_query
                    .file_contents(database, key.clone())?
                    .and_then(|contents| contents.bytes().map(<[u8]>::len))
                    .map_or(0, |length| length as u32))
            })
            .expect("query ID is available");

        assert_eq!(
            *raw_length
                .get(&mut database, key.clone())
                .expect("query runs"),
            0
        );
        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::from(&b"old"[..]),
                    Durability::Low,
                )
                .expect("snapshot bytes set")
        );
        assert_eq!(
            *raw_length
                .get(&mut database, key.clone())
                .expect("input invalidates"),
            3
        );

        assert!(
            !inputs
                .set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::from(&b"old"[..]),
                    Durability::Low,
                )
                .expect("equal bytes remain unchanged")
        );
        assert_eq!(
            *raw_length
                .get(&mut database, key.clone())
                .expect("memo stays green"),
            3
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);

        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::from(&b"new source"[..]),
                    Durability::Low,
                )
                .expect("changed bytes set")
        );
        assert_eq!(
            *raw_length
                .get(&mut database, key.clone())
                .expect("changed input invalidates"),
            10
        );

        assert!(
            inputs
                .remove_file(&mut database, key.clone())
                .expect("file removal recorded")
        );
        assert_eq!(
            *raw_length
                .get(&mut database, key.clone())
                .expect("removal invalidates"),
            0
        );
        assert_eq!(runs.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn source_text_decoding_is_memoized_by_file_key_and_rebound_to_file_id() {
        let inputs = VfsInputs::new().expect("input queries are available");
        let mut database = QueryDatabase::new();
        let key = FileKey::new("acme:app#0.1", "src/main.sai").expect("file key is valid");
        let raw = b"\xEF\xBB\xBFA\xFF";
        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::from(&raw[..]),
                    Durability::Low,
                )
                .expect("raw bytes are set")
        );

        let first = inputs
            .source_text(&mut database, key.clone(), FileId::new(17))
            .expect("source text is decoded");
        assert_eq!(first.status(), &SourceTextStatus::Ready);
        assert_eq!(first.source().id(), FileId::new(17));
        assert_eq!(first.source().text(), "A\u{FFFD}");
        assert_eq!(first.source().raw_bytes(), raw);
        assert!(first.source().had_bom());
        assert_eq!(first.source().invalid_utf8_sequences().len(), 1);

        let second = inputs
            .source_text(&mut database, key.clone(), FileId::new(29))
            .expect("memoized text can be rebound");
        assert_eq!(second.source().id(), FileId::new(29));
        assert_eq!(
            first, second,
            "session-local IDs do not affect query values"
        );

        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::from(&raw[..]),
                    Durability::High,
                )
                .expect("durability can change without changing bytes")
        );
        let rebound = inputs
            .source_text(&mut database, key.clone(), FileId::new(17))
            .expect("source text remains available after a durability change");
        assert_eq!(rebound.source().text(), "A\u{FFFD}");

        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::from(&b"changed"[..]),
                    Durability::Low,
                )
                .expect("updated raw bytes are set")
        );
        let changed = inputs
            .source_text(&mut database, key, FileId::new(17))
            .expect("changed bytes invalidate source text");
        assert_eq!(changed.source().text(), "changed");
        assert_eq!(changed.status(), &SourceTextStatus::Ready);
    }

    #[test]
    fn line_index_query_reuses_decoded_source_and_tracks_content_changes() {
        let inputs = VfsInputs::new().expect("input queries are available");
        let mut database = QueryDatabase::new();
        let key = FileKey::new("acme:app#0.1", "src/main.sai").expect("file key is valid");
        let raw = b"\xEF\xBB\xBFA\r\nB";
        inputs
            .set_file_contents(
                &mut database,
                key.clone(),
                Arc::from(&raw[..]),
                Durability::Low,
            )
            .expect("raw bytes are set");

        let first = inputs
            .line_index(&mut database, key.clone())
            .expect("line index query succeeds");
        assert_eq!(first.line_count(), 2);
        assert_eq!(first.position(TextSize::new(0)).byte, 3);
        assert_eq!(first.position(TextSize::new(1)).byte, 4);

        inputs
            .set_file_contents(
                &mut database,
                key.clone(),
                Arc::from(&raw[..]),
                Durability::High,
            )
            .expect("durability-only update is accepted");
        let unchanged = inputs
            .line_index(&mut database, key.clone())
            .expect("line index remains available");
        assert!(
            Arc::ptr_eq(&first, &unchanged),
            "equal source text keeps the memoized line index"
        );

        inputs
            .set_file_contents(
                &mut database,
                key.clone(),
                Arc::from(&b"A\r\nB"[..]),
                Durability::Low,
            )
            .expect("BOM removal changes source bytes");
        let bom_removed = inputs
            .line_index(&mut database, key.clone())
            .expect("line index reflects BOM removal");
        assert_eq!(bom_removed.line_count(), 2);
        assert_eq!(bom_removed.position(TextSize::new(0)).byte, 0);
        assert!(
            !Arc::ptr_eq(&unchanged, &bom_removed),
            "BOM metadata participates in line-index equality"
        );

        inputs
            .set_file_contents(
                &mut database,
                key.clone(),
                Arc::from(&b"A\r\nB\nC"[..]),
                Durability::Low,
            )
            .expect("changed source bytes are set");
        let changed = inputs
            .line_index(&mut database, key)
            .expect("changed line index is recomputed");
        assert_eq!(changed.line_count(), 3);
        assert!(
            !Arc::ptr_eq(&bom_removed, &changed),
            "changed source bytes produce a new line index"
        );
    }

    #[test]
    fn source_text_turns_missing_read_errors_and_oversized_files_into_values() {
        let inputs = VfsInputs::new().expect("input queries are available");
        let mut database = QueryDatabase::new();
        let missing_key =
            FileKey::new("acme:app#0.1", "src/missing.sai").expect("file key is valid");
        let missing = inputs
            .source_text(&mut database, missing_key.clone(), FileId::new(1))
            .expect("missing input is a source-text value");
        assert_eq!(missing.status(), &SourceTextStatus::Missing);
        assert_eq!(missing.source().text(), "");

        assert!(
            inputs
                .set_file_read_error(
                    &mut database,
                    missing_key.clone(),
                    "permission denied",
                    Durability::Low,
                )
                .expect("read error is set")
        );
        let failed = inputs
            .source_text(&mut database, missing_key, FileId::new(1))
            .expect("read error is a source-text value");
        assert_eq!(
            failed.status(),
            &SourceTextStatus::ReadError {
                io_error_kind: "permission denied".to_owned()
            }
        );
        assert_eq!(failed.source().text(), "");

        let oversized_key =
            FileKey::new("acme:app#0.1", "src/large.sai").expect("file key is valid");
        let oversized_bytes = vec![b'x'; MAX_SOURCE_BYTES + 1];
        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    oversized_key.clone(),
                    Arc::from(oversized_bytes),
                    Durability::Low,
                )
                .expect("oversized raw input is retained")
        );
        let oversized = inputs
            .source_text(&mut database, oversized_key, FileId::new(2))
            .expect("size limit is a source-text value");
        assert_eq!(
            oversized.status(),
            &SourceTextStatus::TooLarge {
                byte_len: MAX_SOURCE_BYTES + 1
            }
        );
        assert_eq!(oversized.source().text(), "");
    }

    #[test]
    fn snapshot_replacement_sets_and_removes_files_together() {
        let inputs = VfsInputs::new().expect("input query ID is available");
        let mut database = QueryDatabase::new();
        let a = FileKey::new("acme:app#0.1", "src/a.sai").expect("file key is valid");
        let b = FileKey::new("acme:app#0.1", "src/b.sai").expect("file key is valid");
        let c = FileKey::new("acme:app#0.1", "src/c.sai").expect("file key is valid");
        let previous = VfsInputSnapshot::new([
            (
                a.clone(),
                FileContents::Bytes(Arc::from(&b"a"[..])),
                Durability::Low,
            ),
            (
                b.clone(),
                FileContents::Bytes(Arc::from(&b"bb"[..])),
                Durability::High,
            ),
        ])
        .expect("initial raw input snapshot is valid");
        let current = VfsInputSnapshot::new([
            (
                a.clone(),
                FileContents::Bytes(Arc::from(&b"\xffa"[..])),
                Durability::Low,
            ),
            (
                c.clone(),
                FileContents::ReadError {
                    io_error_kind: "permission denied".to_owned(),
                },
                Durability::Medium,
            ),
        ])
        .expect("updated raw input snapshot is valid");

        assert_eq!(
            inputs
                .replace_snapshot(&mut database, None, &previous)
                .expect("initial snapshot applies"),
            2
        );
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_query = Arc::clone(&runs);
        let inputs_in_query = inputs.clone();
        let b_in_query = b.clone();
        let c_in_query = c.clone();
        let total_bytes =
            DerivedQuery::<u32, u32>::new("snapshot_byte_count", move |database, _| {
                runs_in_query.fetch_add(1, Ordering::SeqCst);
                let mut total = 0;
                for key in [a.clone(), b_in_query.clone(), c_in_query.clone()] {
                    if let Some(contents) = inputs_in_query.file_contents(database, key)? {
                        total += contents.bytes().map_or(0, <[u8]>::len) as u32;
                    }
                }
                Ok(total)
            })
            .expect("query ID is available");

        assert_eq!(
            *total_bytes
                .get(&mut database, 0)
                .expect("query reads initial snapshot"),
            3
        );
        assert_eq!(
            inputs
                .replace_snapshot(&mut database, Some(&previous), &current)
                .expect("snapshot delta applies"),
            3
        );
        assert_eq!(
            *total_bytes
                .get(&mut database, 0)
                .expect("query reads new snapshot"),
            2
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(
            inputs
                .file_contents(&mut database, b)
                .expect("removed input is readable"),
            None
        );
        assert_eq!(
            inputs
                .file_contents(&mut database, c)
                .expect("read error remains an input"),
            Some(FileContents::ReadError {
                io_error_kind: "permission denied".to_owned()
            })
        );
    }

    #[test]
    fn snapshot_replacement_reconciles_content_package_and_wit_inputs_together() {
        let inputs = VfsInputs::new().expect("input query ID is available");
        let mut database = QueryDatabase::new();
        let package = PackageKey::new("acme:app#0.1").expect("package key is valid");
        let departed_package = PackageKey::new("acme:old#1").expect("package key is valid");
        let source_file = FileKey::new(package.as_str(), "src/lib.sai").expect("file key is valid");
        let previous = VfsInputSnapshot::new([(
            source_file.clone(),
            FileContents::Bytes(Arc::from(&b"a"[..])),
            Durability::Low,
        )])
        .expect("source content snapshot is valid")
        .with_package_files([
            (
                package.clone(),
                PackageFilesSnapshot::new(
                    true,
                    [PackageFileEntry::new("lib.sai", PackageFileKind::File)
                        .expect("source path is valid")],
                )
                .expect("package listing is valid"),
                Durability::Low,
            ),
            (
                departed_package.clone(),
                PackageFilesSnapshot::new(
                    true,
                    [
                        PackageFileEntry::new("a", PackageFileKind::Directory)
                            .expect("source path is valid"),
                        PackageFileEntry::new("b", PackageFileKind::Symlink)
                            .expect("source path is valid"),
                    ],
                )
                .expect("departed package listing is valid"),
                Durability::Medium,
            ),
        ])
        .expect("package listings are unique")
        .with_wit_files([(
            package.clone(),
            WitFilesSnapshot::listed(
                "wit",
                [PackageRelativePath::new("wit/app.wit").expect("WIT path is valid")],
            )
            .expect("WIT listing is valid"),
            Durability::Low,
        )])
        .expect("WIT listing is unique");
        let current = VfsInputSnapshot::new([(
            source_file.clone(),
            FileContents::Bytes(Arc::from(&b"bb"[..])),
            Durability::Low,
        )])
        .expect("updated source content snapshot is valid")
        .with_package_files([(
            package.clone(),
            PackageFilesSnapshot::new(
                true,
                [
                    PackageFileEntry::new("lib.sai", PackageFileKind::File)
                        .expect("source path is valid"),
                    PackageFileEntry::new("nested", PackageFileKind::Directory)
                        .expect("source path is valid"),
                ],
            )
            .expect("updated package listing is valid"),
            Durability::Low,
        )])
        .expect("package listing is unique")
        .with_wit_files([(
            package.clone(),
            WitFilesSnapshot::not_applicable(),
            Durability::Low,
        )])
        .expect("WIT listing is unique");

        assert_eq!(
            inputs
                .replace_snapshot(&mut database, None, &previous)
                .expect("initial filesystem snapshot applies"),
            4
        );
        let inputs_in_query = inputs.clone();
        let source_in_query = source_file.clone();
        let departed_in_query = departed_package.clone();
        let package_in_query = package.clone();
        let summary =
            DerivedQuery::<u32, u64>::new("filesystem_snapshot_summary", move |database, _| {
                let bytes = inputs_in_query
                    .file_contents(database, source_in_query.clone())?
                    .and_then(|contents| contents.bytes().map(<[u8]>::len))
                    .unwrap_or(0);
                let mut package_entries = 0;
                for key in [package_in_query.clone(), departed_in_query.clone()] {
                    if let Some(files) = inputs_in_query.package_files(database, key)? {
                        package_entries += files.entries().len();
                    }
                }
                let wit_files = inputs_in_query
                    .wit_files(database, package_in_query.clone())?
                    .and_then(|files| files.files().map(<[PackageRelativePath]>::len))
                    .unwrap_or(0);
                Ok((bytes + package_entries + wit_files) as u64)
            })
            .expect("query ID is available");
        assert_eq!(
            *summary
                .get(&mut database, 0)
                .expect("query reads all filesystem input kinds"),
            5
        );

        assert_eq!(
            inputs
                .replace_snapshot(&mut database, Some(&previous), &current)
                .expect("complete filesystem snapshot delta applies"),
            4
        );
        assert_eq!(
            *summary
                .get(&mut database, 0)
                .expect("query sees one coherent replacement"),
            4
        );
        assert_eq!(
            inputs
                .package_files(&mut database, departed_package)
                .expect("departed package absence is readable"),
            None
        );
        assert_eq!(
            inputs
                .wit_files(&mut database, package)
                .expect("not-applicable WIT state is readable"),
            Some(WitFilesSnapshot::NotApplicable)
        );
    }

    #[test]
    fn workspace_member_file_keys_use_package_relative_paths() {
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(FileId::new(1), "x", false).expect("small source file")),
            "members/widget/src/lib.sai",
            Some("acme:widget@1.2.3".to_owned()),
            None,
        )])
        .expect("workspace snapshot is valid");
        let package_file = snapshot.iter().next().expect("snapshot contains a file");
        let file_key =
            FileKey::new("acme:widget#1", "src/lib.sai").expect("package-relative key is valid");

        assert_eq!(package_file.path(), "members/widget/src/lib.sai");
        assert_eq!(file_key.path(), "src/lib.sai");
        assert_ne!(file_key.path(), package_file.path());
    }

    #[test]
    fn raw_input_snapshots_reject_duplicate_keys() {
        let key = FileKey::new("acme:app#0.1", "src/main.sai").expect("file key is valid");
        let package_key = PackageKey::new("acme:app#0.1").expect("package key is valid");
        let empty_files =
            PackageFilesSnapshot::new(true, []).expect("empty source listing is valid");
        let empty_wit = WitFilesSnapshot::listed("wit", []).expect("empty WIT listing is valid");

        assert_eq!(
            VfsInputSnapshot::new([
                (
                    key.clone(),
                    FileContents::Bytes(Arc::from(&b"first"[..])),
                    Durability::Low,
                ),
                (
                    key,
                    FileContents::Bytes(Arc::from(&b"second"[..])),
                    Durability::Low,
                ),
            ]),
            Err(VfsInputSnapshotError::DuplicateFileKey)
        );
        assert_eq!(
            VfsInputSnapshot::new([])
                .expect("empty input snapshot is valid")
                .with_package_files([
                    (package_key.clone(), empty_files.clone(), Durability::Low),
                    (package_key.clone(), empty_files, Durability::Low),
                ]),
            Err(VfsInputSnapshotError::DuplicatePackageFilesKey)
        );
        assert_eq!(
            VfsInputSnapshot::new([])
                .expect("empty input snapshot is valid")
                .with_wit_files([
                    (package_key.clone(), empty_wit.clone(), Durability::Low),
                    (package_key, empty_wit, Durability::Low),
                ]),
            Err(VfsInputSnapshotError::DuplicateWitFilesKey)
        );
    }

    #[test]
    fn package_file_snapshots_sort_paths_and_reject_invalid_states() {
        let entry = |path, kind| PackageFileEntry::new(path, kind).expect("path is valid");
        let snapshot = PackageFilesSnapshot::new(
            true,
            [
                entry("z.sai", PackageFileKind::File),
                entry("link", PackageFileKind::Symlink),
                entry("a", PackageFileKind::Directory),
            ],
        )
        .expect("source directory snapshot is valid");
        assert_eq!(
            snapshot
                .entries()
                .iter()
                .map(|entry| (entry.path().as_str(), entry.kind()))
                .collect::<Vec<_>>(),
            [
                ("a", PackageFileKind::Directory),
                ("link", PackageFileKind::Symlink),
                ("z.sai", PackageFileKind::File),
            ]
        );
        assert_eq!(
            snapshot.fingerprint(),
            PackageFilesSnapshot::new(
                true,
                [
                    entry("a", PackageFileKind::Directory),
                    entry("link", PackageFileKind::Symlink),
                    entry("z.sai", PackageFileKind::File),
                ],
            )
            .expect("same entries form a valid snapshot")
            .fingerprint()
        );
        assert!(
            !PackageFilesSnapshot::new(false, [])
                .expect("missing source dir is valid")
                .source_directory_exists()
        );
        assert_eq!(
            PackageFilesSnapshot::new(false, [entry("lib.sai", PackageFileKind::File)]),
            Err(PackageFilesSnapshotError::EntriesForMissingSourceDirectory)
        );
        assert_eq!(
            PackageFilesSnapshot::new(
                true,
                [
                    entry("lib.sai", PackageFileKind::File),
                    entry("lib.sai", PackageFileKind::Symlink),
                ],
            ),
            Err(PackageFilesSnapshotError::DuplicatePath)
        );
        assert!(
            PackageFilesSnapshot::new(
                true,
                [
                    entry("src", PackageFileKind::Directory),
                    entry("src/lib.sai", PackageFileKind::File),
                ],
            )
            .is_ok(),
            "valid directories must be listed before their children"
        );
        assert_eq!(
            PackageFilesSnapshot::new(
                true,
                [
                    entry("link", PackageFileKind::Symlink),
                    entry("link/outside.sai", PackageFileKind::File),
                ],
            ),
            Err(PackageFilesSnapshotError::DescendantOfNonDirectory)
        );
        assert_eq!(
            PackageFilesSnapshot::new(
                true,
                [
                    entry("plain.sai", PackageFileKind::File),
                    entry("plain.sai/child.sai", PackageFileKind::File),
                ],
            ),
            Err(PackageFilesSnapshotError::DescendantOfNonDirectory)
        );
        assert_eq!(
            PackageFilesSnapshot::new(true, [entry("missing/child.sai", PackageFileKind::File)],),
            Err(PackageFilesSnapshotError::MissingDirectoryAncestor)
        );
        for path in ["", "/lib.sai", "a/../lib.sai", "a//lib.sai", "a\\lib.sai"] {
            assert_eq!(
                PackageFileEntry::new(path, PackageFileKind::File),
                Err(PackagePathError::InvalidRelativePath),
                "path should be rejected: {path}"
            );
        }
    }

    #[test]
    fn wit_file_snapshots_are_sorted_and_distinguish_empty_from_not_applicable() {
        let path = |path| PackageRelativePath::new(path).expect("package-relative path is valid");
        let not_applicable = WitFilesSnapshot::not_applicable();
        let empty = WitFilesSnapshot::listed("wit", []).expect("an empty WIT directory is valid");
        assert_eq!(not_applicable.files(), None);
        assert_eq!(empty.files(), Some([].as_slice()));
        assert_ne!(
            not_applicable.fingerprint(),
            empty.fingerprint(),
            "no declared directory differs from an existing empty directory"
        );

        let files = WitFilesSnapshot::listed("wit", [path("wit/z.wit"), path("wit/a.wit")])
            .expect("WIT file list is valid");
        assert_eq!(files.directory(), Some("wit"));
        assert_eq!(
            files
                .files()
                .expect("listed WIT files are available")
                .iter()
                .map(PackageRelativePath::as_str)
                .collect::<Vec<_>>(),
            ["wit/a.wit", "wit/z.wit"]
        );
        assert_eq!(
            WitFilesSnapshot::listed("wit", [path("wit/a.txt")]),
            Err(WitFilesSnapshotError::NotDirectWitFile)
        );
        assert_eq!(
            WitFilesSnapshot::listed("wit", [path("wit/a.wit"), path("wit/a.wit")]),
            Err(WitFilesSnapshotError::DuplicatePath)
        );
        assert_eq!(
            WitFilesSnapshot::listed("wit", [path("wit/deps/extra.wit")]),
            Err(WitFilesSnapshotError::NotDirectWitFile)
        );
        assert_eq!(
            WitFilesSnapshot::listed("wit/../wit", []),
            Err(WitFilesSnapshotError::InvalidDirectory)
        );
        assert_ne!(
            WitFilesSnapshot::listed("interfaces", [])
                .expect("directory key is canonical")
                .fingerprint(),
            empty.fingerprint(),
            "the declared WIT directory affects the input value even when empty"
        );
    }

    #[test]
    fn package_and_wit_file_inputs_track_absence_and_changes() {
        let inputs = VfsInputs::new().expect("input query ID is available");
        let mut database = QueryDatabase::new();
        let key = PackageKey::new("acme:app#0.1").expect("package key is valid");
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_query = Arc::clone(&runs);
        let inputs_in_query = inputs.clone();
        let query =
            DerivedQuery::<PackageKey, u64>::new("package_file_summary", move |database, key| {
                runs_in_query.fetch_add(1, Ordering::SeqCst);
                let package_count = inputs_in_query
                    .package_files(database, key.clone())?
                    .map_or(0, |files| files.entries().len());
                let wit_count = inputs_in_query
                    .wit_files(database, key.clone())?
                    .and_then(|files| files.files().map(<[PackageRelativePath]>::len))
                    .unwrap_or(0);
                Ok((package_count + wit_count) as u64)
            })
            .expect("query ID is available");

        assert_eq!(
            *query
                .get(&mut database, key.clone())
                .expect("absent listing inputs are tracked"),
            0
        );
        let package_files = PackageFilesSnapshot::new(
            true,
            [
                PackageFileEntry::new("lib.sai", PackageFileKind::File)
                    .expect("source path is valid"),
                PackageFileEntry::new("nested", PackageFileKind::Directory)
                    .expect("directory path is valid"),
            ],
        )
        .expect("package listing is valid");
        let wit_files = WitFilesSnapshot::listed(
            "wit",
            [PackageRelativePath::new("wit/main.wit").expect("WIT path is valid")],
        )
        .expect("WIT listing is valid");
        assert!(
            inputs
                .set_package_files(&mut database, key.clone(), package_files, Durability::Low,)
                .expect("package listing is set")
        );
        assert!(
            inputs
                .set_wit_files(&mut database, key.clone(), wit_files, Durability::Low,)
                .expect("WIT listing is set")
        );
        assert_eq!(
            *query
                .get(&mut database, key.clone())
                .expect("both listings invalidate"),
            3
        );
        assert!(
            inputs
                .remove_package_files(&mut database, key.clone())
                .expect("package listing removal is recorded")
        );
        assert_eq!(
            *query
                .get(&mut database, key.clone())
                .expect("package listing removal invalidates"),
            1
        );
        assert!(
            inputs
                .remove_wit_files(&mut database, key.clone())
                .expect("WIT listing removal is recorded")
        );
        assert_eq!(
            *query
                .get(&mut database, key)
                .expect("WIT listing removal invalidates"),
            0
        );
        assert_eq!(runs.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn file_read_failures_are_query_inputs() {
        let inputs = VfsInputs::new().expect("input query ID is available");
        let mut database = QueryDatabase::new();
        let key = FileKey::new("acme:app#0.1", "src/main.sai").expect("file key is valid");
        let key_in_query = key.clone();
        let inputs_in_query = inputs.clone();
        let query = DerivedQuery::<u32, String>::new("file_read_status", move |database, _| {
            Ok(
                match inputs_in_query.file_contents(database, key_in_query.clone())? {
                    None => "absent".to_owned(),
                    Some(FileContents::Bytes(_)) => "read".to_owned(),
                    Some(FileContents::ReadError { io_error_kind }) => io_error_kind,
                },
            )
        })
        .expect("query ID is available");

        assert_eq!(
            *query.get(&mut database, 0).expect("absence is tracked"),
            "absent"
        );
        assert!(
            inputs
                .set_file_read_error(
                    &mut database,
                    key.clone(),
                    "permission denied",
                    Durability::Low,
                )
                .expect("read failure is stored")
        );
        assert_eq!(
            *query.get(&mut database, 0).expect("read error invalidates"),
            "permission denied"
        );
        assert!(
            inputs
                .set_file_contents(
                    &mut database,
                    key,
                    Arc::from(&b"contents"[..]),
                    Durability::Low,
                )
                .expect("bytes replace read failure")
        );
        assert_eq!(
            *query.get(&mut database, 0).expect("bytes invalidate"),
            "read"
        );
    }
}
