//! Parsing and syntax-tree types for `sai.lock`.

use std::ops::Range;

use crate::lexer::{
    PackageLexError, PackagePunctuator, PackageToken, PackageTokenKind, PackageTrivia,
    is_forbidden_non_ascii_whitespace, lex_lock,
};
use crate::lock_conflict::first_conflict_marker;
use crate::manifest::{RequirementSource, Spanned};
use crate::version::{Version, VersionParseError};
use crate::version_req::{VersionReq, VersionReqParseError};

/// A parsed `sai.lock` file and its lexical or syntax diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockParseOutput {
    pub(crate) file: LockFile,
    pub(crate) lexical_errors: Vec<PackageLexError>,
    pub(crate) errors: Vec<LockParseError>,
}

impl LockParseOutput {
    pub const fn file(&self) -> &LockFile {
        &self.file
    }

    pub fn lexical_errors(&self) -> &[PackageLexError] {
        &self.lexical_errors
    }

    pub fn errors(&self) -> &[LockParseError] {
        &self.errors
    }

    pub fn is_syntax_valid(&self) -> bool {
        self.lexical_errors.is_empty() && self.errors.is_empty()
    }
}

/// The syntax tree for one `sai.lock`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockFile {
    pub format_version: Option<u64>,
    pub format_version_span: Option<Range<usize>>,
    pub root: Option<Spanned<LockRoot>>,
    pub packages: Vec<Spanned<LockPackage>>,
    pub trivia: Vec<PackageTrivia>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockRoot {
    /// Kept as spelling so validation can report reserved identities at their
    /// original source span.
    pub package: Box<str>,
    pub version: Version,
    pub fields: Vec<Spanned<LockRootField>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockRootField {
    Targets(Vec<Spanned<Box<str>>>),
    Requires(Box<LockRequirement>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockPackage {
    /// Kept as spelling so validation can report reserved identities at their
    /// original source span.
    pub package: Box<str>,
    pub version: Version,
    pub fields: Vec<Spanned<LockPackageField>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockPackageField {
    Source(LockSource),
    Directory(Box<str>),
    Commit(Box<str>),
    Checksum(Box<str>),
    WitChecksum(Box<str>),
    Artifact {
        target: Box<str>,
        checksum: Box<str>,
    },
    Compiler(VersionReq),
    Capabilities(Vec<Spanned<Box<str>>>),
    Imports {
        target: Box<str>,
        capabilities: Vec<Spanned<Box<str>>>,
    },
    Requires(Box<LockRequirement>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSource {
    Git(Box<str>),
    Path(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockRequirement {
    pub test: bool,
    pub package: Box<str>,
    pub requirement: VersionReq,
    pub selected: Version,
    pub source: Option<Spanned<RequirementSource>>,
    pub only: Option<Vec<Spanned<Box<str>>>>,
    pub grant: Option<Vec<Spanned<Box<str>>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockParseError {
    pub kind: LockParseErrorKind,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockParseErrorKind {
    ExpectedToken(Box<str>),
    ExpectedNewline,
    MissingRootBlock,
    DuplicateRootBlock,
    UnexpectedTopLevelToken,
    UnknownRootField(Box<str>),
    UnknownPackageField(Box<str>),
    InvalidFormatVersion,
    HeaderNotFirstLine,
    InvalidVersion(VersionParseError),
    InvalidVersionRequirement(VersionReqParseError),
    LockMergeConflict,
}

/// Lexes and parses a `sai.lock` file.
///
/// Lexical diagnostics are returned separately from lock-structure errors.
/// Merge markers are checked before tokenization as required by the lock
/// format, so a conflicted lock is rejected without interpreting either side.
pub fn parse(source: &[u8]) -> LockParseOutput {
    let empty = LockFile::default();
    if let Some(span) = first_conflict_marker(source) {
        return LockParseOutput {
            file: empty,
            lexical_errors: Vec::new(),
            errors: vec![LockParseError {
                kind: LockParseErrorKind::LockMergeConflict,
                span,
            }],
        };
    }

    let mut header = LockFile::default();
    if let Some((version, span)) = detect_format_version(source) {
        header.format_version = Some(version);
        header.format_version_span = Some(span);
    }
    let lexed = lex_lock(source, header.format_version.unwrap_or(1));
    if !lexed.errors.is_empty() {
        return LockParseOutput {
            file: LockFile {
                trivia: lexed.trivia,
                ..header
            },
            lexical_errors: lexed.errors,
            errors: Vec::new(),
        };
    }
    let text = std::str::from_utf8(source).expect("lexer only returns tokens for UTF-8 input");
    let mut parser = Parser::new(text, lexed.tokens);
    let mut file = LockFile {
        trivia: lexed.trivia,
        ..LockFile::default()
    };
    parser.parse_file(&mut file);
    LockParseOutput {
        file,
        lexical_errors: Vec::new(),
        errors: parser.errors,
    }
}

fn detect_format_version(source: &[u8]) -> Option<(u64, Range<usize>)> {
    let newline = source.iter().position(|byte| *byte == b'\n');
    let line_end = newline.unwrap_or(source.len());
    let mut end = line_end;
    if newline.is_some() && end > 0 && source[end - 1] == b'\r' {
        end -= 1;
    } else if newline.is_none() && source.last() == Some(&b'\r') {
        return None;
    }
    let header_text = std::str::from_utf8(&source[..end]).ok()?;
    if header_text.contains('\r')
        || header_text.chars().any(|character| {
            is_forbidden_non_ascii_whitespace(character)
                || matches!(
                    character as u32,
                    0x202a..=0x202e | 0x2066..=0x2069
                )
        })
    {
        return None;
    }
    let mut position = if source.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    skip_header_trivia(source, &mut position, end)?;
    if !source[position..end].starts_with(b"lock") {
        return None;
    }
    position += 4;
    if position < end
        && !matches!(source[position], b' ' | b'\t')
        && !source[position..end].starts_with(b"/*")
        && !source[position..end].starts_with(b"//")
    {
        return None;
    }
    skip_header_trivia(source, &mut position, end)?;
    let digits_start = position;
    while position < end && source[position].is_ascii_digit() {
        position += 1;
    }
    if position == digits_start || (position - digits_start > 1 && source[digits_start] == b'0') {
        return None;
    }
    let digits_end = position;
    let version = std::str::from_utf8(&source[digits_start..position])
        .ok()?
        .parse()
        .ok()?;
    skip_header_trailing_trivia(source, &mut position)?;
    Some((version, digits_start..digits_end))
}

fn skip_header_trivia(source: &[u8], position: &mut usize, end: usize) -> Option<()> {
    loop {
        while *position < end && matches!(source[*position], b' ' | b'\t') {
            *position += 1;
        }
        if source[*position..end].starts_with(b"//") {
            *position = end;
            return Some(());
        }
        if !source[*position..end].starts_with(b"/*") {
            return Some(());
        }

        *position += 2;
        let mut depth = 1usize;
        while *position < end {
            if source[*position..end].starts_with(b"/*") {
                depth += 1;
                *position += 2;
            } else if source[*position..end].starts_with(b"*/") {
                depth -= 1;
                *position += 2;
                if depth == 0 {
                    break;
                }
            } else {
                *position += 1;
            }
        }
        if depth != 0 {
            return None;
        }
    }
}

fn skip_header_trailing_trivia(source: &[u8], position: &mut usize) -> Option<()> {
    loop {
        while matches!(source.get(*position), Some(b' ' | b'\t')) {
            *position += 1;
        }
        match source.get(*position..) {
            None | Some([]) | Some([b'\n', ..]) => return Some(()),
            Some([b'\r', b'\n', ..]) => return Some(()),
            Some([b'\r', ..]) => return None,
            Some([b'/', b'/', ..]) => return Some(()),
            Some([b'/', b'*', ..]) => {
                let comment_start = *position;
                *position += 2;
                let mut depth = 1usize;
                while *position < source.len() {
                    if source[*position..].starts_with(b"/*") {
                        depth += 1;
                        *position += 2;
                    } else if source[*position..].starts_with(b"*/") {
                        depth -= 1;
                        *position += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        *position += 1;
                    }
                }
                if depth != 0 {
                    return None;
                }
                if source[comment_start..*position].contains(&b'\n') {
                    return Some(());
                }
            }
            Some(_) => return None,
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<PackageToken>,
    position: usize,
    errors: Vec<LockParseError>,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str, tokens: Vec<PackageToken>) -> Self {
        Self {
            text,
            tokens,
            position: 0,
            errors: Vec::new(),
        }
    }

    fn parse_file(&mut self, file: &mut LockFile) {
        if let Some(first_token) = self.tokens.iter().find(|token| {
            !matches!(
                token.kind,
                PackageTokenKind::Newline | PackageTokenKind::Eof
            )
        }) && self.text[..first_token.span.start].contains('\n')
        {
            self.error(
                LockParseErrorKind::HeaderNotFirstLine,
                0..first_token.span.start,
            );
            return;
        }
        self.skip_newlines();
        if !self.at_word("lock") {
            self.error_here(LockParseErrorKind::ExpectedToken("`lock`".into()));
            return;
        }
        self.bump();
        let header_error_start = self.errors.len();
        let mut parsed_version = None;
        if self.at_kind(PackageTokenKind::Integer) {
            let token = self.bump();
            match self.token_text(token).parse::<u64>() {
                Ok(version) => parsed_version = Some((version, self.tokens[token].span.clone())),
                Err(_) => self.error(
                    LockParseErrorKind::InvalidFormatVersion,
                    self.tokens[token].span.clone(),
                ),
            }
        } else {
            self.error_here(LockParseErrorKind::ExpectedToken(
                "lock format version".into(),
            ));
        }
        self.finish_line();
        if self.errors.len() == header_error_start {
            if let Some((version, span)) = parsed_version {
                file.format_version = Some(version);
                file.format_version_span = Some(span);
            }
        }

        self.skip_newlines();
        if self.at_word("root") {
            file.root = self.parse_root_block();
        } else {
            self.error_here(LockParseErrorKind::MissingRootBlock);
        }

        while !self.at_eof() {
            self.skip_newlines();
            if self.at_eof() {
                break;
            }
            if self.at_word("package") {
                if let Some(package) = self.parse_package_block() {
                    file.packages.push(package);
                }
            } else if self.at_word("root") {
                self.error_here(LockParseErrorKind::DuplicateRootBlock);
                let _ = self.parse_root_block();
            } else {
                self.error_here(LockParseErrorKind::UnexpectedTopLevelToken);
                self.recover_to_newline();
            }
        }
    }

    fn parse_root_block(&mut self) -> Option<Spanned<LockRoot>> {
        let start = self.position;
        self.bump(); // `root`
        let package = self.expect_kind(PackageTokenKind::PackagePath, "package identity")?;
        self.expect_punctuator(PackagePunctuator::At, "`@`")?;
        let version = self.parse_version("root package version")?;
        self.finish_line();
        let fields = self.parse_root_fields();
        Some(Spanned {
            value: LockRoot {
                package: self.token_text(package).into(),
                version,
                fields,
            },
            span: self.span_from(start),
        })
    }

    fn parse_root_fields(&mut self) -> Vec<Spanned<LockRootField>> {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.at_eof() && !self.at_block_start() {
            let start = self.position;
            let field = if self.at_word("targets") {
                self.bump();
                self.parse_list(&[PackageTokenKind::Word])
                    .map(LockRootField::Targets)
            } else if self.at_word("requires") {
                self.parse_requirement(true).map(LockRootField::Requires)
            } else {
                let unknown = self.current_text().to_owned();
                self.error_here(LockParseErrorKind::UnknownRootField(unknown.into()));
                None
            };
            if let Some(field) = field {
                fields.push(Spanned {
                    value: field,
                    span: self.span_from(start),
                });
            }
            self.finish_line();
            self.skip_newlines();
        }
        fields
    }

    fn parse_package_block(&mut self) -> Option<Spanned<LockPackage>> {
        let start = self.position;
        self.bump(); // `package`
        let package = self.expect_kind(PackageTokenKind::PackagePath, "package identity")?;
        self.expect_punctuator(PackagePunctuator::At, "`@`")?;
        let version = self.parse_version("package version")?;
        self.finish_line();
        let fields = self.parse_package_fields();
        Some(Spanned {
            value: LockPackage {
                package: self.token_text(package).into(),
                version,
                fields,
            },
            span: self.span_from(start),
        })
    }

    fn parse_package_fields(&mut self) -> Vec<Spanned<LockPackageField>> {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.at_eof() && !self.at_block_start() {
            let start = self.position;
            let field = self.parse_package_field();
            if let Some(field) = field {
                fields.push(Spanned {
                    value: field,
                    span: self.span_from(start),
                });
            }
            self.finish_line();
            self.skip_newlines();
        }
        fields
    }

    fn parse_package_field(&mut self) -> Option<LockPackageField> {
        if self.at_word("source") {
            self.bump();
            let source_kind = if self.at_word("git") {
                self.bump();
                0
            } else if self.at_word("path") {
                self.bump();
                1
            } else {
                self.error_here(LockParseErrorKind::ExpectedToken("`git` or `path`".into()));
                return None;
            };
            let value = self.parse_string("source URL or path")?;
            return Some(LockPackageField::Source(if source_kind == 0 {
                LockSource::Git(value)
            } else {
                LockSource::Path(value)
            }));
        }
        if self.at_word("dir") {
            self.bump();
            return self
                .parse_string("repository package directory")
                .map(LockPackageField::Directory);
        }
        if self.at_word("commit") {
            self.bump();
            return self.parse_string("commit ID").map(LockPackageField::Commit);
        }
        if self.at_word("checksum") {
            self.bump();
            return self
                .parse_string("source checksum")
                .map(LockPackageField::Checksum);
        }
        if self.at_word("wit") {
            self.bump();
            return self
                .parse_string("WIT checksum")
                .map(LockPackageField::WitChecksum);
        }
        if self.at_word("artifact") {
            self.bump();
            let target = self.expect_word("artifact target")?;
            let checksum = self.parse_string("artifact checksum")?;
            return Some(LockPackageField::Artifact {
                target: self.token_text(target).into(),
                checksum,
            });
        }
        if self.at_word("compiler") {
            self.bump();
            return self
                .parse_requirement_text(false)
                .map(LockPackageField::Compiler);
        }
        if self.at_word("capabilities") {
            self.bump();
            return self
                .parse_list(&[PackageTokenKind::Word, PackageTokenKind::PackagePath])
                .map(LockPackageField::Capabilities);
        }
        if self.at_word("imports") {
            self.bump();
            let target = self.expect_word("import target")?;
            let capabilities =
                self.parse_list(&[PackageTokenKind::Word, PackageTokenKind::PackagePath])?;
            return Some(LockPackageField::Imports {
                target: self.token_text(target).into(),
                capabilities,
            });
        }
        if self.at_word("requires") {
            return self
                .parse_requirement(false)
                .map(LockPackageField::Requires);
        }
        let unknown = self.current_text().to_owned();
        self.error_here(LockParseErrorKind::UnknownPackageField(unknown.into()));
        None
    }

    fn parse_requirement(&mut self, allow_test: bool) -> Option<Box<LockRequirement>> {
        self.expect_word("`requires`")?;
        let test = if allow_test && self.at_word("test") {
            self.bump();
            true
        } else {
            false
        };
        let package = self.expect_kind(PackageTokenKind::PackagePath, "required package")?;
        self.expect_punctuator(PackagePunctuator::At, "`@`")?;
        let requirement = self.parse_requirement_text(true)?;
        self.expect_punctuator(PackagePunctuator::Arrow, "`->`")?;
        let selected = self.parse_version("selected package version")?;
        let source = if self.at_word("from") {
            Some(self.parse_requirement_source()?)
        } else {
            None
        };
        let only = if self.at_word("only") {
            self.bump();
            Some(self.parse_list(&[PackageTokenKind::Word])?)
        } else {
            None
        };
        let grant = if self.at_word("grant") {
            self.bump();
            Some(self.parse_list(&[PackageTokenKind::Word, PackageTokenKind::PackagePath])?)
        } else {
            None
        };
        Some(Box::new(LockRequirement {
            test,
            package: self.token_text(package).into(),
            requirement,
            selected,
            source,
            only,
            grant,
        }))
    }

    fn parse_requirement_source(&mut self) -> Option<Spanned<RequirementSource>> {
        let start = self.position;
        self.expect_word("`from`")?;
        let source = if self.at_word("git") {
            self.bump();
            let url = self.parse_string("Git source URL")?;
            let directory = if self.at_word("dir") {
                self.bump();
                Some(self.parse_string("Git package directory")?)
            } else {
                None
            };
            RequirementSource::Git { url, directory }
        } else if self.at_word("path") {
            self.bump();
            RequirementSource::Path(self.parse_string("path source")?)
        } else {
            self.error_here(LockParseErrorKind::ExpectedToken("`git` or `path`".into()));
            return None;
        };
        Some(Spanned {
            value: source,
            span: self.span_from(start),
        })
    }

    fn parse_requirement_text(&mut self, stop_at_arrow: bool) -> Option<VersionReq> {
        let start_position = self.position;
        while !self.at_eof()
            && !self.at_newline()
            && !(stop_at_arrow && self.at_punctuator(PackagePunctuator::Arrow))
        {
            self.bump();
        }
        if self.position == start_position {
            self.error_here(LockParseErrorKind::ExpectedToken(
                "version requirement".into(),
            ));
            return None;
        }
        let span = self.tokens[start_position].span.start..self.tokens[self.position - 1].span.end;
        let requirement_text = self.tokens[start_position..self.position]
            .iter()
            .map(|token| &self.text[token.span.clone()])
            .collect::<Vec<_>>()
            .join(" ");
        match VersionReq::parse(&requirement_text) {
            Ok(requirement) => Some(requirement),
            Err(error) => {
                self.error(LockParseErrorKind::InvalidVersionRequirement(error), span);
                None
            }
        }
    }

    fn parse_list(&mut self, item_kinds: &[PackageTokenKind]) -> Option<Vec<Spanned<Box<str>>>> {
        self.expect_punctuator(PackagePunctuator::LeftBrace, "`{`")?;
        let mut items = Vec::new();
        self.skip_newlines();
        if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
            return Some(items);
        }

        loop {
            if self.at_eof() {
                self.error_here(LockParseErrorKind::ExpectedToken("`}`".into()));
                return Some(items);
            }
            if item_kinds.contains(&self.current().kind) {
                let token = self.bump();
                items.push(Spanned {
                    value: self.token_text(token).into(),
                    span: self.tokens[token].span.clone(),
                });
            } else {
                self.error_here(LockParseErrorKind::ExpectedToken("list item".into()));
                self.recover_list_item();
                if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                    return Some(items);
                }
                if self.at_newline() {
                    self.skip_newlines();
                    if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                        return Some(items);
                    }
                }
                if self.at_eof() {
                    return Some(items);
                }
                continue;
            }

            if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                break;
            }
            if self.eat_punctuator(PackagePunctuator::Comma).is_some() {
                self.skip_newlines();
                if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                    break;
                }
                continue;
            }
            if self.at_newline() {
                self.skip_newlines();
                if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                    break;
                }
                continue;
            }
            self.error_here(LockParseErrorKind::ExpectedToken("`,` or newline".into()));
            self.recover_list_item();
            if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                break;
            }
            if self.at_newline() {
                self.skip_newlines();
            }
        }
        Some(items)
    }

    fn recover_list_item(&mut self) {
        while !self.at_eof()
            && !self.at_newline()
            && !self.at_punctuator(PackagePunctuator::Comma)
            && !self.at_punctuator(PackagePunctuator::RightBrace)
        {
            self.bump();
        }
        if self.at_punctuator(PackagePunctuator::Comma) {
            self.bump();
        }
    }

    fn parse_version(&mut self, expected: &str) -> Option<Version> {
        let token = self.expect_kind(PackageTokenKind::Version, expected)?;
        match Version::parse(self.token_text(token)) {
            Ok(version) => Some(version),
            Err(error) => {
                self.error(
                    LockParseErrorKind::InvalidVersion(error),
                    self.tokens[token].span.clone(),
                );
                None
            }
        }
    }

    fn parse_string(&mut self, expected: &str) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::String, expected)?;
        Some(
            self.tokens[token]
                .string_value
                .clone()
                .expect("string token has a decoded value"),
        )
    }

    fn expect_word(&mut self, expected: &str) -> Option<usize> {
        self.expect_kind(PackageTokenKind::Word, expected)
    }

    fn expect_kind(&mut self, kind: PackageTokenKind, expected: &str) -> Option<usize> {
        if self.at_kind(kind) {
            Some(self.bump())
        } else {
            self.error_here(LockParseErrorKind::ExpectedToken(expected.into()));
            None
        }
    }

    fn expect_punctuator(
        &mut self,
        punctuator: PackagePunctuator,
        expected: &str,
    ) -> Option<usize> {
        if self.at_punctuator(punctuator) {
            Some(self.bump())
        } else {
            self.error_here(LockParseErrorKind::ExpectedToken(expected.into()));
            None
        }
    }

    fn eat_punctuator(&mut self, punctuator: PackagePunctuator) -> Option<usize> {
        self.at_punctuator(punctuator).then(|| self.bump())
    }

    fn finish_line(&mut self) {
        if self.at_newline() {
            self.skip_newlines();
        } else if !self.at_eof() {
            self.error_here(LockParseErrorKind::ExpectedNewline);
            self.recover_to_newline();
        }
    }

    fn recover_to_newline(&mut self) {
        while !self.at_eof() && !self.at_newline() {
            self.bump();
        }
        self.skip_newlines();
    }

    fn skip_newlines(&mut self) {
        while self.at_newline() {
            self.bump();
        }
    }

    fn span_from(&self, position: usize) -> Range<usize> {
        self.tokens[position].span.start..self.tokens[self.position.saturating_sub(1)].span.end
    }

    fn error_here(&mut self, kind: LockParseErrorKind) {
        self.error(kind, self.current().span.clone());
    }

    fn error(&mut self, kind: LockParseErrorKind, span: Range<usize>) {
        self.errors.push(LockParseError { kind, span });
    }

    fn token_text(&self, index: usize) -> &str {
        &self.text[self.tokens[index].span.clone()]
    }

    fn current_text(&self) -> &str {
        self.token_text(self.position)
    }

    fn at_word(&self, word: &str) -> bool {
        self.at_kind(PackageTokenKind::Word) && self.current_text() == word
    }

    fn at_block_start(&self) -> bool {
        self.at_word("package") || self.at_word("root")
    }

    fn at_kind(&self, kind: PackageTokenKind) -> bool {
        self.current().kind == kind
    }

    fn at_punctuator(&self, punctuator: PackagePunctuator) -> bool {
        self.current().kind == PackageTokenKind::Punctuator(punctuator)
    }

    fn at_newline(&self) -> bool {
        self.at_kind(PackageTokenKind::Newline)
    }

    fn at_eof(&self) -> bool {
        self.at_kind(PackageTokenKind::Eof)
    }

    fn current(&self) -> &PackageToken {
        &self.tokens[self.position]
    }

    fn bump(&mut self) -> usize {
        let position = self.position;
        self.position += 1;
        position
    }
}

#[cfg(test)]
mod tests {
    use crate::manifest::RequirementSource;
    use crate::version::Version;
    use crate::version_req::VersionReq;

    use super::{LockPackageField, LockParseErrorKind, LockRootField, LockSource, parse};

    fn valid_lock() -> &'static str {
        "lock 1\n\
         \n\
         root acme:app @ 0.3.0\n\
             targets { wasm32-wasi }\n\
             requires acme:json @ ^1.6 -> 1.8.0\n\
         \n\
         package acme:json @ 1.8.0\n\
             source git \"https://github.com/acme/json\"\n\
             commit \"0095142504f87ba0c83406017d3637a65db29876\"\n\
             checksum \"sai1:495a0e04c51e574dc6fbd7ac377914a1e76031b8df86acf72c4aea8630318e26\"\n\
             compiler ^0.1\n\
             capabilities { alloc }\n"
    }

    #[test]
    fn parses_a_root_and_git_package_entry() {
        let output = parse(valid_lock().as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        let root = &output.file.root.as_ref().expect("root").value;
        assert_eq!(root.package.as_ref(), "acme:app");
        assert_eq!(root.version, Version::parse("0.3.0").unwrap());
        assert!(matches!(root.fields[0].value, LockRootField::Targets(_)));
        assert!(matches!(root.fields[1].value, LockRootField::Requires(_)));

        let package = &output.file.packages[0].value;
        assert_eq!(package.package.as_ref(), "acme:json");
        assert!(matches!(
            package.fields[0].value,
            LockPackageField::Source(LockSource::Git(_))
        ));
        assert!(matches!(
            package.fields[3].value,
            LockPackageField::Compiler(_)
        ));
    }

    #[test]
    fn parses_path_entries_multiline_lists_and_test_edges() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets {\n\
                     wasm32-wasi,\n\
                     wasm32-core\n\
                 }\n\
                 requires test acme:test-kit @ >=1.2, <2.0 -> 1.4.0 only { wasm32-wasi }\n\
             package sai:core @ 0.1.0\n\
                 source path \"../core\"\n\
                 requires acme:dep @ ^2 -> 2.1.0 only {\n\
                     wasm32-core\n\
                 }\n";
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        let root = &output.file.root.as_ref().unwrap().value;
        let LockRootField::Requires(edge) = &root.fields[1].value else {
            panic!("expected requirement");
        };
        assert!(edge.test);
        assert_eq!(edge.only.as_ref().unwrap().len(), 1);

        let package = &output.file.packages[0].value;
        assert!(matches!(
            package.fields[0].value,
            LockPackageField::Source(LockSource::Path(_))
        ));
        let requirement_span = package.fields[1].span.clone();
        assert!(source[requirement_span].ends_with('}'));
    }

    #[test]
    fn parses_all_entry_field_shapes() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:component @ 2.0.0\n\
                 source git \"https://example.test/repo\"\n\
                 dir \"packages/component\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 wit \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 artifact wasm32-wasi \"sai1:2123456789012345678901234567890123456789012345678901234567890123\"\n\
                 imports wasm32-wasi { wasi:http/types }\n\
                 capabilities { wasi:http }\n\
                 requires acme:runtime @ ^1 -> 1.2.0\n";
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        assert_eq!(output.file.packages[0].value.fields.len(), 9);
        assert!(matches!(
            output.file.packages[0].value.fields[5].value,
            LockPackageField::Artifact { .. }
        ));
        assert!(matches!(
            output.file.packages[0].value.fields[6].value,
            LockPackageField::Imports { .. }
        ));
    }

    #[test]
    fn parses_requirement_comparators_using_the_shared_version_model() {
        let output = parse(
            "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 requires acme:dep @ >=1.2, <2.0 -> 1.5.0\n"
                .as_bytes(),
        );
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        let LockRootField::Requires(requirement) =
            &output.file.root.as_ref().unwrap().value.fields[1].value
        else {
            panic!("expected requirement");
        };
        assert_eq!(
            requirement.requirement,
            VersionReq::parse(">=1.2, <2.0").unwrap()
        );
    }

    #[test]
    fn parses_v2_requirement_source_then_only_and_grant() {
        let source = "lock 2\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 requires acme:git-dep @ ^1 -> 1.2.0 from git \"https://example.test/repo\" dir \"packages/dep\" only { wasm32-wasi } grant { wasi:http, alloc }\n\
                 requires acme:path-dep @ ^1 -> 1.3.0 from path \"../path-dep\" grant { }\n";
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        let root = &output.file.root.as_ref().unwrap().value;
        let LockRootField::Requires(git_requirement) = &root.fields[1].value else {
            panic!("expected Git requirement");
        };
        assert_eq!(
            git_requirement.source.as_ref().unwrap().value,
            RequirementSource::Git {
                url: "https://example.test/repo".into(),
                directory: Some("packages/dep".into()),
            }
        );
        assert!(
            source[git_requirement.source.as_ref().unwrap().span.clone()].starts_with("from git ")
        );
        assert_eq!(git_requirement.only.as_ref().unwrap().len(), 1);
        assert_eq!(
            git_requirement
                .grant
                .as_ref()
                .unwrap()
                .iter()
                .map(|capability| capability.value.as_ref())
                .collect::<Vec<_>>(),
            ["wasi:http", "alloc"]
        );

        let LockRootField::Requires(path_requirement) = &root.fields[2].value else {
            panic!("expected path requirement");
        };
        assert_eq!(
            path_requirement.source.as_ref().unwrap().value,
            RequirementSource::Path("../path-dep".into())
        );
        assert_eq!(path_requirement.grant.as_ref().unwrap().len(), 0);
        assert!(path_requirement.only.is_none());
    }

    #[test]
    fn parses_format_two_requirement_clauses_on_continuation_lines() {
        let source = r#"lock 2
root acme:app @ 1.0.0
    targets { wasm32-wasi }
    requires acme:dep @ ^1 -> 1.2.0
        from git "https://example.test/repo"
        dir "packages/dep"
        only { wasm32-wasi }
        grant { wasi:http, alloc }
"#;
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);

        let LockRootField::Requires(requirement) =
            &output.file.root.as_ref().unwrap().value.fields[1].value
        else {
            panic!("expected requirement");
        };
        assert_eq!(
            requirement.source.as_ref().unwrap().value,
            RequirementSource::Git {
                url: "https://example.test/repo".into(),
                directory: Some("packages/dep".into()),
            }
        );
        assert_eq!(requirement.only.as_ref().unwrap().len(), 1);
        assert_eq!(requirement.grant.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn format_one_does_not_accept_format_two_requirement_continuations() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 requires acme:dep @ ^1 -> 1.2.0\n\
                     from git \"https://example.test/repo\"\n";
        let output = parse(source.as_bytes());
        assert!(!output.is_syntax_valid());
        assert!(!output.errors.is_empty());
        assert!(output.lexical_errors.is_empty());
    }

    #[test]
    fn header_version_survives_a_multiline_trailing_comment() {
        let source = r#"lock 2 /* trailing comment
 */
root acme:app @ 1.0.0
    targets { wasm32-wasi }
    requires acme:dep @ ^1 -> 1.2.0
        from git "https://example.test/repo"
"#;
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        assert_eq!(output.file.format_version, Some(2));
    }

    #[test]
    fn same_indent_dir_after_git_edge_starts_an_entry_field() {
        let source = r#"lock 2
root acme:root @ 1.0.0
    targets { wasm32-wasi }
    requires acme:parent @ ^1 -> 1.0.0 from git "https://example.test/mono"
package acme:parent @ 1.0.0
    source git "https://example.test/mono"
    commit "0123456789012345678901234567890123456789"
    checksum "sai1:0123456789012345678901234567890123456789012345678901234567890123"
    capabilities { }
    requires acme:child @ ^1 -> 1.0.0 from git "https://example.test/child"
    dir "packages/parent"
package acme:child @ 1.0.0
    source git "https://example.test/child"
    dir "packages/child"
    commit "0123456789012345678901234567890123456789"
    checksum "sai1:1123456789012345678901234567890123456789012345678901234567890123"
    capabilities { }
"#;
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);

        let parent = &output.file.packages[0].value;
        let requirement = parent
            .fields
            .iter()
            .find_map(|field| match &field.value {
                LockPackageField::Requires(requirement) => Some(requirement),
                _ => None,
            })
            .expect("parent edge");
        assert_eq!(
            requirement.source.as_ref().unwrap().value,
            RequirementSource::Git {
                url: "https://example.test/child".into(),
                directory: None,
            }
        );
        assert!(parent.fields.iter().any(|field| {
            matches!(&field.value, LockPackageField::Directory(directory) if directory.as_ref() == "packages/parent")
        }));
    }

    #[test]
    fn accepts_comments_between_requirement_comparators() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 requires acme:root-dep @ >=1.2, /* explanation */ <2.0 -> 1.5.0\n\
             package acme:dep @ 1.5.0\n\
                 source git \"https://example.test/dep\"\n\
                 compiler >=0.1, // toolchain range\n\
                     <0.2\n\
                 requires acme:child @ >=1, /* upper bound */ <2 -> 1.4.0\n\
             package acme:child @ 1.4.0\n\
                 source path \"../child\"\n";
        let output = parse(source.as_bytes());
        assert!(output.is_syntax_valid(), "{:?}", output.errors);
        assert_eq!(output.file.packages.len(), 2);
    }

    #[test]
    fn reports_structural_syntax_errors_and_merge_markers_before_lexing() {
        let missing_root = parse(b"lock 1\n");
        assert!(
            missing_root
                .errors
                .iter()
                .any(|error| error.kind == LockParseErrorKind::MissingRootBlock)
        );

        let unknown_field = parse(b"lock 1\nroot acme:app @ 1.0.0\nbogus { wasm32-wasi }\n");
        assert!(
            unknown_field
                .errors
                .iter()
                .any(|error| matches!(error.kind, LockParseErrorKind::UnknownRootField(_)))
        );

        let conflict = parse(b"\xef\xbb\xbf\xff\n=======\r\n");
        assert_eq!(
            conflict.errors[0].kind,
            LockParseErrorKind::LockMergeConflict
        );
        assert_eq!(conflict.errors[0].span, 5..12);
        assert!(conflict.lexical_errors.is_empty());

        let bom_marker = parse(b"\xef\xbb\xbf<<<<<<< ours\n");
        assert_eq!(
            bom_marker.errors[0].kind,
            LockParseErrorKind::LockMergeConflict
        );
        assert_eq!(bom_marker.errors[0].span, 3..10);
    }

    #[test]
    fn rejects_malformed_format_headers_without_recording_a_version() {
        for source in [
            b"lock 02\n".as_slice(),
            b"lock 2x\n",
            b"lock 2 trailing\n",
            b"lock 2\r",
            b"lock /* \xff */ 2\n",
            b"lock /* \xe2\x80\xae */ 2\n",
        ] {
            let output = parse(source);
            assert_ne!(
                output.file.format_version,
                Some(2),
                "malformed header unexpectedly recorded version 2: {source:?}"
            );
        }
    }

    #[test]
    fn requires_the_header_to_be_on_the_first_line() {
        for source in [
            b"\nlock 1\nroot acme:app @ 1.0.0\ntargets { wasm32-wasi }\n".as_slice(),
            b"// leading comment\nlock 1\nroot acme:app @ 1.0.0\ntargets { wasm32-wasi }\n",
            b"/* leading\ncomment */ lock 1\nroot acme:app @ 1.0.0\ntargets { wasm32-wasi }\n",
        ] {
            let output = parse(source);
            assert!(
                output
                    .errors
                    .iter()
                    .any(|error| { error.kind == LockParseErrorKind::HeaderNotFirstLine }),
                "expected first-line header error for {source:?}, got {:?}",
                output.errors
            );
        }
    }

    #[test]
    fn does_not_detect_future_versions_through_forbidden_header_whitespace() {
        for source in [
            b"lock /* \xc2\xa0 */ 2\n".as_slice(),
            b"lock /* \xe2\x80\x8b */ 2\n",
        ] {
            let output = parse(source);
            assert_eq!(output.file.format_version, None);
            assert!(!output.lexical_errors.is_empty());
        }
    }
}
