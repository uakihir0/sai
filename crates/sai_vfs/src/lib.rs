#![forbid(unsafe_code)]

use std::collections::BTreeMap;
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
#[derive(Clone, Debug)]
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
}

impl SourceFile {
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

    !relative_path.is_empty()
        && !relative_path
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
}
