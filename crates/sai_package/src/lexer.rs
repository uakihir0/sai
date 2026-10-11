use std::ops::Range;

use crate::version_req::PartialVersion;

/// Token kinds shared by `sai.package` and `sai.lock`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackageTokenKind {
    Word,
    UpperWord,
    PackagePath,
    Integer,
    Version,
    String,
    Punctuator(PackagePunctuator),
    Newline,
    Error,
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackagePunctuator {
    LeftBrace,
    RightBrace,
    Comma,
    At,
    Caret,
    Tilde,
    Equal,
    GreaterEqual,
    Greater,
    LessEqual,
    Less,
    Arrow,
    Slash,
    Semicolon,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageToken {
    pub kind: PackageTokenKind,
    /// Half-open UTF-8 byte offsets into the original input.
    pub span: Range<usize>,
    /// Decoded string value for `String` tokens.
    pub string_value: Option<Box<str>>,
    /// Whether a lexical error was reported inside this token.
    pub has_lexical_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackageTriviaKind {
    ByteOrderMark,
    Whitespace,
    LineBreak,
    LineComment,
    BlockComment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageTrivia {
    pub kind: PackageTriviaKind,
    /// Half-open UTF-8 byte offsets into the original input.
    pub span: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PackageLexErrorKind {
    InvalidUtf8,
    InvalidLineEnding,
    ForbiddenCharacter,
    UnexpectedCharacter,
    UnterminatedComment,
    UnterminatedString,
    InvalidEscape,
    UnsupportedStringForm,
    InvalidVersion,
    BuildMetadataNotAllowed,
    NoSemicolon,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageLexError {
    pub kind: PackageLexErrorKind,
    /// Half-open UTF-8 byte offsets into the original input.
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackageLexOutput {
    pub tokens: Vec<PackageToken>,
    pub trivia: Vec<PackageTrivia>,
    pub errors: Vec<PackageLexError>,
    /// Whether the input could not be decoded as UTF-8.
    pub invalid_utf8: bool,
    /// Total number of lexical errors, including any that a bounded lexer call
    /// did not retain in `errors`.
    pub error_count: usize,
}

/// Lexes either a package contract or lock file.
///
/// The two formats share tokenization. Package contracts and lock files use
/// their own contextual newline continuation sets.
pub fn lex(source: &[u8]) -> PackageLexOutput {
    lex_with_mode(source, LexMode::Package, true, usize::MAX)
}

/// Lexes a package contract while retaining at most `max_errors` diagnostics.
/// The lexer still processes the complete input and reports the exact total.
pub fn lex_with_error_limit(source: &[u8], max_errors: usize) -> PackageLexOutput {
    lex_with_mode(source, LexMode::Package, true, max_errors)
}

/// Lexes a package contract without retaining whitespace and comment trivia.
///
/// This is useful when a parser already owns the trivia stream and a consumer
/// needs a separate token stream for formatting or source analysis.
pub fn lex_tokens(source: &[u8]) -> PackageLexOutput {
    lex_with_mode(source, LexMode::Package, false, usize::MAX)
}

/// Tokenizes a package contract while retaining at most `max_errors`
/// diagnostics. The lexer still processes the complete input and reports the
/// exact total.
pub fn lex_tokens_with_error_limit(source: &[u8], max_errors: usize) -> PackageLexOutput {
    lex_with_mode(source, LexMode::Package, false, max_errors)
}

/// Lexes a lock file using its field-aware newline continuation set.
pub(crate) fn lex_lock(source: &[u8]) -> PackageLexOutput {
    lex_with_mode(source, LexMode::Lock, true, usize::MAX)
}

fn lex_with_mode(
    source: &[u8],
    mode: LexMode,
    retain_trivia: bool,
    max_errors: usize,
) -> PackageLexOutput {
    let text = match std::str::from_utf8(source) {
        Ok(text) => text,
        Err(error) => {
            let start = error.valid_up_to();
            let end = error
                .error_len()
                .map_or(source.len(), |length| start + length);
            let errors = if max_errors == 0 {
                Vec::new()
            } else {
                vec![PackageLexError {
                    kind: PackageLexErrorKind::InvalidUtf8,
                    span: start..end,
                }]
            };
            return PackageLexOutput {
                tokens: vec![PackageToken {
                    kind: PackageTokenKind::Eof,
                    span: source.len()..source.len(),
                    string_value: None,
                    has_lexical_error: false,
                }],
                trivia: Vec::new(),
                errors,
                invalid_utf8: true,
                error_count: 1,
            };
        }
    };
    Lexer::new(text, mode, retain_trivia, max_errors).run()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LexMode {
    Package,
    Lock,
}

struct Lexer<'a> {
    text: &'a str,
    mode: LexMode,
    retain_trivia: bool,
    pos: usize,
    tokens: Vec<PackageToken>,
    trivia: Vec<PackageTrivia>,
    errors: Vec<PackageLexError>,
    error_count: usize,
    max_errors: usize,
    active_token_start: Option<usize>,
    active_token_has_error: bool,
    previous: Option<PackageTokenKind>,
    pending_newline: Option<Range<usize>>,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, mode: LexMode, retain_trivia: bool, max_errors: usize) -> Self {
        Self {
            text,
            mode,
            retain_trivia,
            pos: 0,
            tokens: Vec::new(),
            trivia: Vec::new(),
            errors: Vec::new(),
            error_count: 0,
            max_errors,
            active_token_start: None,
            active_token_has_error: false,
            previous: None,
            pending_newline: None,
        }
    }

    fn run(mut self) -> PackageLexOutput {
        if self.text.starts_with('\u{feff}') {
            self.pos = '\u{feff}'.len_utf8();
            self.push_trivia(PackageTriviaKind::ByteOrderMark, 0..self.pos);
        }

        loop {
            self.lex_trivia();
            if self.at_eof() {
                if self.previous.is_some_and(is_terminator) {
                    let span = self.pending_newline.take().unwrap_or(self.pos..self.pos);
                    self.push_token(PackageTokenKind::Newline, span, None);
                }
                self.push_token(PackageTokenKind::Eof, self.pos..self.pos, None);
                break;
            }

            if let Some(newline) = self.pending_newline.take()
                && self.previous.is_some_and(is_terminator)
                && !self.next_token_continues_line()
            {
                self.push_token(PackageTokenKind::Newline, newline, None);
            }

            self.active_token_start = Some(self.pos);
            self.active_token_has_error = false;
            let mut token = self.lex_token();
            token.has_lexical_error = self.active_token_has_error;
            self.active_token_start = None;
            self.previous = Some(token.kind);
            self.tokens.push(token);
        }

        PackageLexOutput {
            tokens: self.tokens,
            trivia: self.trivia,
            errors: self.errors,
            invalid_utf8: false,
            error_count: self.error_count,
        }
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn starts_with(&self, value: &str) -> bool {
        self.text[self.pos..].starts_with(value)
    }

    fn peek(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    fn push_token(
        &mut self,
        kind: PackageTokenKind,
        span: Range<usize>,
        string_value: Option<Box<str>>,
    ) {
        self.tokens.push(PackageToken {
            kind,
            span,
            string_value,
            has_lexical_error: false,
        });
    }

    fn push_trivia(&mut self, kind: PackageTriviaKind, span: Range<usize>) {
        if self.retain_trivia {
            self.trivia.push(PackageTrivia { kind, span });
        }
    }

    fn error(&mut self, kind: PackageLexErrorKind, span: Range<usize>) {
        self.error_count = self.error_count.saturating_add(1);
        if self
            .active_token_start
            .is_some_and(|token_start| span.start >= token_start)
        {
            self.active_token_has_error = true;
        }
        if self.errors.len() < self.max_errors {
            self.errors.push(PackageLexError { kind, span });
        }
    }

    fn comment_error(&mut self, kind: PackageLexErrorKind, span: Range<usize>) {
        self.error(kind, span);
    }

    fn lex_trivia(&mut self) {
        loop {
            if self.at_eof() {
                return;
            }
            let start = self.pos;
            match self.peek() {
                Some(' ' | '\t') => {
                    while matches!(self.peek(), Some(' ' | '\t')) {
                        self.pos += 1;
                    }
                    self.push_trivia(PackageTriviaKind::Whitespace, start..self.pos);
                }
                Some('\n' | '\r') => self.lex_line_break(),
                Some('/') if self.starts_with("//") => self.lex_line_comment(),
                Some('/') if self.starts_with("/*") => self.lex_block_comment(),
                _ => return,
            }
        }
    }

    fn lex_line_break(&mut self) {
        let start = self.pos;
        if self.peek() == Some('\r') {
            self.pos += 1;
            if self.text.as_bytes().get(self.pos) == Some(&b'\n') {
                self.pos += 1;
            } else {
                self.error(PackageLexErrorKind::InvalidLineEnding, start..self.pos);
            }
        } else {
            self.pos += 1;
        }
        self.pending_newline.get_or_insert(start..self.pos);
        self.push_trivia(PackageTriviaKind::LineBreak, start..self.pos);
    }

    fn lex_line_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        while let Some(character) = self.peek() {
            if matches!(character, '\r' | '\n') {
                break;
            }
            let character_start = self.pos;
            self.pos += character.len_utf8();
            if is_bidi_control(character) || is_forbidden_non_ascii_whitespace(character) {
                self.comment_error(
                    PackageLexErrorKind::ForbiddenCharacter,
                    character_start..self.pos,
                );
            }
        }
        self.push_trivia(PackageTriviaKind::LineComment, start..self.pos);
    }

    fn lex_block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1_usize;
        while !self.at_eof() {
            if self.starts_with("/*") {
                depth += 1;
                self.pos += 2;
            } else if self.starts_with("*/") {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    break;
                }
            } else if matches!(self.peek(), Some('\r' | '\n')) {
                self.lex_comment_line_break();
            } else {
                let character_start = self.pos;
                let character = self.peek().expect("not at end of input");
                self.pos += character.len_utf8();
                if is_bidi_control(character) || is_forbidden_non_ascii_whitespace(character) {
                    self.comment_error(
                        PackageLexErrorKind::ForbiddenCharacter,
                        character_start..self.pos,
                    );
                }
            }
        }
        if depth != 0 {
            self.error(
                PackageLexErrorKind::UnterminatedComment,
                start..self.text.len(),
            );
        }
        self.push_trivia(PackageTriviaKind::BlockComment, start..self.pos);
    }

    fn lex_comment_line_break(&mut self) {
        let start = self.pos;
        if self.peek() == Some('\r') {
            self.pos += 1;
            if self.text.as_bytes().get(self.pos) == Some(&b'\n') {
                self.pos += 1;
            } else {
                self.error(PackageLexErrorKind::InvalidLineEnding, start..self.pos);
            }
        } else {
            self.pos += 1;
        }
        self.pending_newline.get_or_insert(start..self.pos);
    }

    fn next_token_continues_line(&self) -> bool {
        if scan_package_path(self.text, self.pos).is_some() {
            return false;
        }
        let Some(end) = scan_word(self.text, self.pos) else {
            return false;
        };
        match self.mode {
            LexMode::Package => {
                matches!(&self.text[self.pos..end], "from" | "dir" | "only" | "grant")
            }
            LexMode::Lock => &self.text[self.pos..end] == "only",
        }
    }

    fn lex_token(&mut self) -> PackageToken {
        let start = self.pos;
        if let Some((quote, raw_hashes)) = unsupported_string_opener(self.text, self.pos) {
            return self.lex_unsupported_string(start, quote, raw_hashes);
        }
        if self.peek() == Some('"') {
            if self.starts_with("\"\"\"") {
                return self.lex_unsupported_string(start, start, 0);
            }
            return self.lex_string();
        }
        if let Some(end) = scan_package_path(self.text, self.pos) {
            self.pos = end;
            return PackageToken {
                kind: PackageTokenKind::PackagePath,
                span: start..end,
                string_value: None,
                has_lexical_error: false,
            };
        }
        if let Some(end) = scan_word(self.text, self.pos) {
            self.pos = end;
            return PackageToken {
                kind: PackageTokenKind::Word,
                span: start..end,
                string_value: None,
                has_lexical_error: false,
            };
        }
        if let Some(end) = scan_upper_word(self.text, self.pos) {
            self.pos = end;
            return PackageToken {
                kind: PackageTokenKind::UpperWord,
                span: start..end,
                string_value: None,
                has_lexical_error: false,
            };
        }
        if self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            return self.lex_number();
        }
        if let Some((punctuator, length)) = punctuator_at(self.text, self.pos) {
            self.pos += length;
            let kind = PackageTokenKind::Punctuator(punctuator);
            if punctuator == PackagePunctuator::Semicolon {
                self.error(PackageLexErrorKind::NoSemicolon, start..self.pos);
            }
            return PackageToken {
                kind,
                span: start..self.pos,
                string_value: None,
                has_lexical_error: false,
            };
        }

        let character = self.peek().expect("not at end of input");
        self.pos += character.len_utf8();
        let error_kind = if is_forbidden_outside_literal(character) {
            PackageLexErrorKind::ForbiddenCharacter
        } else {
            PackageLexErrorKind::UnexpectedCharacter
        };
        self.error(error_kind, start..self.pos);
        PackageToken {
            kind: PackageTokenKind::Error,
            span: start..self.pos,
            string_value: None,
            has_lexical_error: false,
        }
    }

    fn lex_number(&mut self) -> PackageToken {
        let start = self.pos;
        self.consume_ascii_digits();
        if self.text.as_bytes().get(self.pos) != Some(&b'.') {
            if self
                .peek()
                .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
            {
                self.consume_malformed_number_tail();
                self.error(PackageLexErrorKind::InvalidVersion, start..self.pos);
                let kind = if self.text[start..self.pos].contains('.') {
                    PackageTokenKind::Version
                } else {
                    PackageTokenKind::Integer
                };
                return self.token(kind, start, None);
            }

            let number = &self.text[start..self.pos];
            if (number.starts_with('0') && number.len() > 1) || number.parse::<u64>().is_err() {
                self.error(PackageLexErrorKind::InvalidVersion, start..self.pos);
            }
            return self.token(PackageTokenKind::Integer, start, None);
        }

        let mut components = 1;
        let mut malformed = false;
        while self.text.as_bytes().get(self.pos) == Some(&b'.') {
            if components == 3 {
                malformed = true;
                self.consume_malformed_number_tail();
                break;
            }
            components += 1;
            self.pos += 1;
            if !self
                .peek()
                .is_some_and(|character| character.is_ascii_digit())
            {
                malformed = true;
                self.consume_malformed_number_tail();
                break;
            }
            self.consume_ascii_digits();
        }

        if !malformed && self.text.as_bytes().get(self.pos) == Some(&b'-') {
            if components != 3 {
                malformed = true;
                self.consume_malformed_number_tail();
            } else {
                self.pos += 1;
                while self.peek().is_some_and(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '.')
                }) {
                    self.pos += self.peek().expect("character was checked").len_utf8();
                }
            }
        }
        let mut build_metadata = false;
        if !malformed && self.text.as_bytes().get(self.pos) == Some(&b'+') {
            build_metadata = true;
            self.pos += 1;
            while self.peek().is_some_and(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
            }) {
                self.pos += self.peek().expect("character was checked").len_utf8();
            }
        }
        if !malformed
            && !build_metadata
            && self.peek().is_some_and(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '.')
            })
        {
            malformed = true;
            self.consume_malformed_number_tail();
        }

        if malformed {
            self.error(PackageLexErrorKind::InvalidVersion, start..self.pos);
        } else {
            match PartialVersion::parse(&self.text[start..self.pos]) {
                Ok(_) => {}
                Err(crate::version::VersionParseError::InvalidVersion) => {
                    self.error(PackageLexErrorKind::InvalidVersion, start..self.pos);
                }
                Err(crate::version::VersionParseError::BuildMetadataNotAllowed) => {
                    let plus = self.text[start..self.pos]
                        .find('+')
                        .map_or(start, |offset| start + offset);
                    self.error(PackageLexErrorKind::BuildMetadataNotAllowed, plus..self.pos);
                }
            }
        }
        self.token(PackageTokenKind::Version, start, None)
    }

    fn consume_ascii_digits(&mut self) {
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.pos += 1;
        }
    }

    fn consume_malformed_number_tail(&mut self) {
        while self.peek().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '+' | '-')
        }) {
            self.pos += self.peek().expect("character was checked").len_utf8();
        }
    }

    fn lex_string(&mut self) -> PackageToken {
        let start = self.pos;
        self.pos += 1;
        let mut value = String::new();
        let mut terminated = false;

        while let Some(character) = self.peek() {
            if character == '"' {
                self.pos += 1;
                terminated = true;
                break;
            }
            if matches!(character, '\r' | '\n') {
                self.error(PackageLexErrorKind::UnterminatedString, start..self.pos);
                break;
            }

            let character_start = self.pos;
            if character == '\\' {
                if let Some((end, lone_cr)) = self.string_continuation_end(self.pos) {
                    if lone_cr {
                        self.error(PackageLexErrorKind::InvalidLineEnding, end - 1..end);
                    }
                    self.error(
                        PackageLexErrorKind::UnsupportedStringForm,
                        character_start..end,
                    );
                    self.pos = end;
                    continue;
                }
                let escape = scan_escape(self.text, self.pos);
                self.pos = escape.end;
                match escape.value {
                    Some(decoded) => {
                        if is_bidi_control(decoded) {
                            self.error(
                                PackageLexErrorKind::ForbiddenCharacter,
                                character_start..self.pos,
                            );
                        }
                        value.push(decoded);
                    }
                    None => {
                        self.error(
                            PackageLexErrorKind::InvalidEscape,
                            character_start..self.pos,
                        );
                    }
                }
                continue;
            }

            self.pos += character.len_utf8();
            if is_bidi_control(character) {
                self.error(
                    PackageLexErrorKind::ForbiddenCharacter,
                    character_start..self.pos,
                );
            }
            value.push(character);
        }

        if !terminated && self.at_eof() {
            self.error(PackageLexErrorKind::UnterminatedString, start..self.pos);
        }
        self.token(
            PackageTokenKind::String,
            start,
            Some(value.into_boxed_str()),
        )
    }

    fn string_continuation_end(&self, slash: usize) -> Option<(usize, bool)> {
        let next = slash + 1;
        match self.text.as_bytes().get(next) {
            Some(b'\n') => Some((next + 1, false)),
            Some(b'\r') if self.text.as_bytes().get(next + 1) == Some(&b'\n') => {
                Some((next + 2, false))
            }
            Some(b'\r') => Some((next + 1, true)),
            _ => None,
        }
    }

    fn lex_unsupported_string(
        &mut self,
        start: usize,
        quote: usize,
        raw_hashes: usize,
    ) -> PackageToken {
        let raw = self.text[start..quote].contains('r');
        let triple = self.text[quote..].starts_with("\"\"\"");
        let quote_width = if triple { 3 } else { 1 };
        let content_start = quote + quote_width;
        self.pos = content_start;
        let mut terminated = false;

        while !self.at_eof() {
            if raw {
                if self.starts_raw_string_end(raw_hashes, quote_width) {
                    self.pos += quote_width + raw_hashes;
                    terminated = true;
                    break;
                }
            } else if triple && self.starts_with("\"\"\"") {
                self.pos += 3;
                terminated = true;
                break;
            } else if !triple && self.peek() == Some('"') {
                self.pos += 1;
                terminated = true;
                break;
            }

            let character_start = self.pos;
            let character = self.peek().expect("not at end of input");
            if !raw && character == '\\' {
                self.pos += 1;
                if let Some((end, lone_cr)) = self.string_continuation_end(character_start) {
                    if lone_cr {
                        self.error(PackageLexErrorKind::InvalidLineEnding, end - 1..end);
                    }
                    self.pos = end;
                    continue;
                }
                if !self.at_eof() {
                    self.pos += self.peek().expect("not at end of input").len_utf8();
                }
                continue;
            }
            if matches!(character, '\r' | '\n') {
                self.pos += 1;
                if character == '\r' {
                    if self.text.as_bytes().get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    } else {
                        self.error(
                            PackageLexErrorKind::InvalidLineEnding,
                            character_start..self.pos,
                        );
                    }
                }
                continue;
            }

            self.pos += character.len_utf8();
            if is_bidi_control(character) {
                self.error(
                    PackageLexErrorKind::ForbiddenCharacter,
                    character_start..self.pos,
                );
            }
        }

        if !terminated {
            self.error(PackageLexErrorKind::UnterminatedString, quote..self.pos);
        }
        self.error(PackageLexErrorKind::UnsupportedStringForm, start..self.pos);
        self.token(PackageTokenKind::Error, start, None)
    }

    fn starts_raw_string_end(&self, hashes: usize, quote_width: usize) -> bool {
        if quote_width == 3 && !self.starts_with("\"\"\"") {
            return false;
        }
        if quote_width == 1 && self.peek() != Some('"') {
            return false;
        }
        let after_quote = self.pos + quote_width;
        self.text
            .as_bytes()
            .get(after_quote..after_quote + hashes)
            .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
    }

    fn token(
        &self,
        kind: PackageTokenKind,
        start: usize,
        string_value: Option<Box<str>>,
    ) -> PackageToken {
        PackageToken {
            kind,
            span: start..self.pos,
            string_value,
            has_lexical_error: false,
        }
    }
}

#[derive(Clone, Copy)]
struct ScannedEscape {
    end: usize,
    value: Option<char>,
}

fn scan_escape(text: &str, start: usize) -> ScannedEscape {
    let bytes = text.as_bytes();
    let Some(escaped) = bytes.get(start + 1).copied() else {
        return ScannedEscape {
            end: text.len(),
            value: None,
        };
    };
    let simple = match escaped {
        b'n' => Some('\n'),
        b'r' => Some('\r'),
        b't' => Some('\t'),
        b'\\' => Some('\\'),
        b'"' => Some('"'),
        b'\'' => Some('\''),
        b'0' => Some('\0'),
        _ => None,
    };
    if let Some(value) = simple {
        return ScannedEscape {
            end: start + 2,
            value: Some(value),
        };
    }
    if escaped != b'u' {
        let end = text[start + 1..]
            .chars()
            .next()
            .map_or(text.len(), |character| start + 1 + character.len_utf8());
        return ScannedEscape { end, value: None };
    }

    let brace = start + 2;
    if bytes.get(brace) != Some(&b'{') {
        return ScannedEscape {
            end: brace.min(text.len()),
            value: None,
        };
    }
    let mut end = brace + 1;
    let digits_start = end;
    while bytes.get(end).is_some_and(|byte| byte.is_ascii_hexdigit()) {
        end += 1;
    }
    let digit_count = end - digits_start;
    let has_close = bytes.get(end) == Some(&b'}');
    if has_close {
        end += 1;
    } else {
        while let Some(byte) = bytes.get(end) {
            if *byte == b'}' {
                end += 1;
                break;
            }
            if matches!(byte, b'\r' | b'\n') {
                break;
            }
            end += 1;
        }
    }
    let value = if (1..=6).contains(&digit_count) && has_close {
        u32::from_str_radix(&text[digits_start..digits_start + digit_count], 16)
            .ok()
            .and_then(char::from_u32)
    } else {
        None
    };
    ScannedEscape { end, value }
}

fn is_terminator(kind: PackageTokenKind) -> bool {
    matches!(
        kind,
        PackageTokenKind::Word
            | PackageTokenKind::UpperWord
            | PackageTokenKind::PackagePath
            | PackageTokenKind::Integer
            | PackageTokenKind::Version
            | PackageTokenKind::String
            | PackageTokenKind::Punctuator(PackagePunctuator::RightBrace)
    )
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

pub(crate) fn is_forbidden_non_ascii_whitespace(character: char) -> bool {
    character == '\u{200b}'
        || (character.is_whitespace() && !matches!(character, '\t' | '\n' | '\r' | ' '))
}

fn is_forbidden_outside_literal(character: char) -> bool {
    (character <= '\u{001f}' && !matches!(character, '\t' | '\n' | '\r'))
        || character == '\u{007f}'
        || is_bidi_control(character)
        || is_forbidden_non_ascii_whitespace(character)
}

fn scan_word(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes.get(start).is_some_and(u8::is_ascii_lowercase) {
        return None;
    }
    let mut end = start + 1;
    loop {
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            end += 1;
        }
        if !matches!(bytes.get(end), Some(b'-' | b'_'))
            || !bytes
                .get(end + 1)
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Some(end);
        }
        end += 1;
    }
}

fn scan_upper_word(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes.get(start).is_some_and(u8::is_ascii_uppercase) {
        return None;
    }
    let mut end = start + 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        end += 1;
    }
    Some(end)
}

fn scan_package_path(text: &str, start: usize) -> Option<usize> {
    let namespace_end = scan_package_segment(text, start, false)?;
    if text.as_bytes().get(namespace_end) != Some(&b':') {
        return None;
    }
    let mut end = scan_package_segment(text, namespace_end + 1, false)?;
    while text.as_bytes().get(end) == Some(&b'/') {
        let Some(path_end) = scan_package_segment(text, end + 1, true) else {
            break;
        };
        end = path_end;
    }
    Some(end)
}

fn scan_package_segment(text: &str, start: usize, path_segment: bool) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes.get(start).is_some_and(u8::is_ascii_lowercase) {
        return None;
    }
    let mut end = start + 1;
    while bytes.get(end).is_some_and(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || (path_segment && *byte == b'_')
    }) {
        end += 1;
    }
    while bytes.get(end) == Some(&b'-')
        && bytes
            .get(end + 1)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_lowercase)
            || bytes.get(end).is_some_and(u8::is_ascii_digit)
        {
            end += 1;
        }
    }
    Some(end)
}

fn unsupported_string_opener(text: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let (mut quote, raw) = match bytes.get(start..start + 2) {
        Some(b"br") => (start + 2, true),
        Some(b"b\"") | Some(b"f\"") => return Some((start + 1, 0)),
        Some(b"r#") | Some(b"r\"") => (start + 1, true),
        _ => return None,
    };
    let mut hashes = 0;
    if raw {
        while bytes.get(quote) == Some(&b'#') {
            hashes += 1;
            quote += 1;
        }
        if bytes.get(quote) != Some(&b'"') {
            return None;
        }
    }
    Some((quote, hashes))
}

fn punctuator_at(text: &str, start: usize) -> Option<(PackagePunctuator, usize)> {
    let remaining = &text[start..];
    for (spelling, punctuator) in [
        (">=", PackagePunctuator::GreaterEqual),
        ("<=", PackagePunctuator::LessEqual),
        ("->", PackagePunctuator::Arrow),
    ] {
        if remaining.starts_with(spelling) {
            return Some((punctuator, spelling.len()));
        }
    }
    let punctuator = match remaining.as_bytes().first()? {
        b'{' => PackagePunctuator::LeftBrace,
        b'}' => PackagePunctuator::RightBrace,
        b',' => PackagePunctuator::Comma,
        b'@' => PackagePunctuator::At,
        b'^' => PackagePunctuator::Caret,
        b'~' => PackagePunctuator::Tilde,
        b'=' => PackagePunctuator::Equal,
        b'>' => PackagePunctuator::Greater,
        b'<' => PackagePunctuator::Less,
        b'/' => PackagePunctuator::Slash,
        b';' => PackagePunctuator::Semicolon,
        _ => return None,
    };
    Some((punctuator, 1))
}

#[cfg(test)]
mod tests {
    use super::{
        PackageLexErrorKind as ErrorKind, PackagePunctuator as Punct, PackageTokenKind as Kind,
        lex, lex_lock, lex_tokens, lex_with_error_limit,
    };

    fn token_text(source: &str, kind: Kind, index: usize) -> &str {
        let output = lex(source.as_bytes());
        let token = &output.tokens[index];
        assert_eq!(token.kind, kind);
        &source[token.span.clone()]
    }

    #[test]
    fn tokenizes_words_paths_versions_and_punctuators() {
        let source = "package acme:json/client_v2 @ ^1.2.3";
        let output = lex(source.as_bytes());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::PackagePath,
                Kind::Punctuator(Punct::At),
                Kind::Punctuator(Punct::Caret),
                Kind::Version,
                Kind::Newline,
                Kind::Eof,
            ]
        );
        assert_eq!(token_text(source, Kind::Word, 0), "package");
        assert_eq!(
            token_text(source, Kind::PackagePath, 1),
            "acme:json/client_v2"
        );
        assert_eq!(token_text(source, Kind::Version, 4), "1.2.3");
    }

    #[test]
    fn token_only_lexing_preserves_tokens_and_errors_without_trivia() {
        for source in [
            b"\xef\xbb\xbfpackage acme:app @ 1.0.0 // note\r\ntargets { wasm32-core }\n".as_slice(),
            b"package acme:app @ 1.0.0\n$\n".as_slice(),
            b"package acme:app @ 1.0.0\n\xff".as_slice(),
        ] {
            let with_trivia = lex(source);
            let token_only = lex_tokens(source);

            assert_eq!(token_only.tokens, with_trivia.tokens);
            assert_eq!(token_only.errors, with_trivia.errors);
            assert!(token_only.trivia.is_empty());
        }
    }

    #[test]
    fn lock_newlines_separate_dir_fields_but_continue_only_clauses() {
        let source = "source git \"https://example.test/repo\"\n\
                     dir \"packages/lib\"\n\
                     requires acme:dep @ ^1 -> 1.2.0\n\
                     only { wasm32-wasi }\n";
        let output = lex_lock(source.as_bytes());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let tokens = &output.tokens;
        let dir = tokens
            .iter()
            .position(|token| token.kind == Kind::Word && &source[token.span.clone()] == "dir")
            .unwrap();
        assert_eq!(tokens[dir - 1].kind, Kind::Newline);
        let only = tokens
            .iter()
            .position(|token| token.kind == Kind::Word && &source[token.span.clone()] == "only")
            .unwrap();
        assert_ne!(tokens[only - 1].kind, Kind::Newline);

        let package_tokens = lex(source.as_bytes()).tokens;
        let package_dir = package_tokens
            .iter()
            .position(|token| token.kind == Kind::Word && &source[token.span.clone()] == "dir")
            .unwrap();
        assert_ne!(package_tokens[package_dir - 1].kind, Kind::Newline);
    }

    #[test]
    fn word_and_package_path_suffixes_are_lowercase() {
        let output = lex(b"foo-Bar acme:json/bar-Baz");
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [
                ErrorKind::UnexpectedCharacter,
                ErrorKind::UnexpectedCharacter
            ]
        );
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::Error,
                Kind::UpperWord,
                Kind::PackagePath,
                Kind::Error,
                Kind::UpperWord,
                Kind::Newline,
                Kind::Eof,
            ]
        );
    }

    #[test]
    fn keeps_comments_as_trivia_and_generates_continuation_newlines() {
        let source = "alpha /* outer\n /* nested */ end */ from git \"url\"\nbeta // tail\n";
        let output = lex(source.as_bytes());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::Word,
                Kind::Word,
                Kind::String,
                Kind::Newline,
                Kind::Word,
                Kind::Newline,
                Kind::Eof,
            ]
        );
        assert_eq!(output.trivia.len(), 9);
        assert!(
            output
                .trivia
                .iter()
                .any(|trivia| trivia.kind == super::PackageTriviaKind::BlockComment)
        );
        assert!(
            output
                .trivia
                .iter()
                .any(|trivia| trivia.kind == super::PackageTriviaKind::LineComment)
        );
    }

    #[test]
    fn nested_comment_line_breaks_terminate_declarations() {
        let source = "alpha /* outer\n /* nested */ end */ beta";
        let output = lex(source.as_bytes());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::Newline,
                Kind::Word,
                Kind::Newline,
                Kind::Eof
            ]
        );
        assert_eq!(
            output.tokens[1].span,
            source.find('\n').unwrap()..source.find('\n').unwrap() + 1
        );
    }

    #[test]
    fn recognizes_only_the_documented_line_continuation_words() {
        for continuation in ["from", "dir", "only", "grant"] {
            let source = format!("alpha\n{continuation} beta");
            let output = lex(source.as_bytes());
            assert!(output.errors.is_empty(), "{:?}", output.errors);
            assert_eq!(
                output
                    .tokens
                    .iter()
                    .map(|token| token.kind)
                    .collect::<Vec<_>>(),
                [Kind::Word, Kind::Word, Kind::Word, Kind::Newline, Kind::Eof,],
                "continuation: {continuation}"
            );
        }

        let output = lex(b"alpha\nfrom-more beta");
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::Newline,
                Kind::Word,
                Kind::Word,
                Kind::Newline,
                Kind::Eof,
            ]
        );

        for package_path in ["from:pkg", "dir:pkg", "only:pkg", "grant:pkg"] {
            let source = format!("alpha\n{package_path}");
            let output = lex(source.as_bytes());
            assert_eq!(
                output
                    .tokens
                    .iter()
                    .map(|token| token.kind)
                    .collect::<Vec<_>>(),
                [
                    Kind::Word,
                    Kind::Newline,
                    Kind::PackagePath,
                    Kind::Newline,
                    Kind::Eof,
                ],
                "package path: {package_path}"
            );
        }
    }

    #[test]
    fn checks_bidi_controls_in_comments_and_line_continuation_strings() {
        let comment = lex("// comment\u{202e}\nword".as_bytes());
        assert_eq!(
            comment
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [ErrorKind::ForbiddenCharacter]
        );

        let continued = lex("\"a\\\r\nb\"".as_bytes());
        assert_eq!(
            continued
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [ErrorKind::UnsupportedStringForm]
        );
        assert_eq!(continued.tokens[0].string_value.as_deref(), Some("ab"));
    }

    #[test]
    fn rejects_invisible_whitespace_in_line_and_block_comments() {
        for source in [
            "// comment\u{00a0}\nword",
            "/* comment\u{00a0} */ word",
            "// comment\u{200b}\nword",
            "/* comment\u{200b} */ word",
        ] {
            let output = lex(source.as_bytes());
            assert_eq!(
                output
                    .errors
                    .iter()
                    .map(|error| error.kind)
                    .collect::<Vec<_>>(),
                [ErrorKind::ForbiddenCharacter],
                "unexpected lexical diagnostics for {source:?}"
            );
        }
    }

    #[test]
    fn decodes_simple_string_escapes_without_normalizing_unicode() {
        let source = "\"a\\n\\r\\t\\\\\\\"\\'\\0\\u{1f642}e\\u{301}\"";
        let output = lex(source.as_bytes());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(output.tokens[0].kind, Kind::String);
        assert_eq!(
            output.tokens[0].string_value.as_deref(),
            Some("a\n\r\t\\\"'\0🙂e\u{301}")
        );
    }

    #[test]
    fn reports_unsupported_string_forms_and_invalid_escapes() {
        let source = "\"\"\"multi\nline\"\"\" r#\"raw\"# b\"bytes\" f\"{value}\" \"bad\\q\"";
        let output = lex(source.as_bytes());
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [
                ErrorKind::UnsupportedStringForm,
                ErrorKind::UnsupportedStringForm,
                ErrorKind::UnsupportedStringForm,
                ErrorKind::UnsupportedStringForm,
                ErrorKind::InvalidEscape,
            ]
        );
    }

    #[test]
    fn validates_integer_and_version_lexemes() {
        let source =
            "0 01 18446744073709551616 1.2 1.2-rc 1.2.3.4 1.2.3-01 1.2.3+meta 1.2.3+bad_meta 1x.2";
        let output = lex(source.as_bytes());
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [
                ErrorKind::InvalidVersion,
                ErrorKind::InvalidVersion,
                ErrorKind::InvalidVersion,
                ErrorKind::InvalidVersion,
                ErrorKind::InvalidVersion,
                ErrorKind::BuildMetadataNotAllowed,
                ErrorKind::BuildMetadataNotAllowed,
                ErrorKind::InvalidVersion,
            ]
        );
        assert_eq!(output.tokens[0].kind, Kind::Integer);
        assert_eq!(output.tokens[3].kind, Kind::Version);
        assert_eq!(output.tokens[4].kind, Kind::Version);
        assert!(
            output
                .tokens
                .iter()
                .any(|token| token.kind == Kind::Version && &source[token.span.clone()] == "1x.2")
        );
    }

    #[test]
    fn handles_bom_crlf_lone_cr_and_eof_newlines() {
        let source = "\u{feff}alpha\r\nbeta\rgamma";
        let output = lex(source.as_bytes());
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [ErrorKind::InvalidLineEnding]
        );
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::Newline,
                Kind::Word,
                Kind::Newline,
                Kind::Word,
                Kind::Newline,
                Kind::Eof,
            ]
        );
        assert_eq!(
            output.trivia[0].kind,
            super::PackageTriviaKind::ByteOrderMark
        );
    }

    #[test]
    fn distinguishes_forbidden_characters_from_other_non_ascii() {
        let source = "λ\u{00a0}x\u{202e}";
        let output = lex(source.as_bytes());
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [
                ErrorKind::UnexpectedCharacter,
                ErrorKind::ForbiddenCharacter,
                ErrorKind::ForbiddenCharacter,
            ]
        );
    }

    #[test]
    fn bounded_lexical_errors_keep_the_total_and_first_spans() {
        let source = format!("// {}\n", "\u{00a0}".repeat(100));
        let output = lex_with_error_limit(source.as_bytes(), 3);

        assert_eq!(output.error_count, 100);
        assert_eq!(output.errors.len(), 3);
        assert_eq!(output.errors[0].span, 3..5);
        assert_eq!(output.errors[1].span, 5..7);
        assert_eq!(output.errors[2].span, 7..9);
    }

    #[test]
    fn bounded_lexical_errors_cover_tokens_and_comments_without_stopping_the_scan() {
        let source = format!(";\n;\n//{}\n?", "\u{00a0}".repeat(10));
        let output = lex_with_error_limit(source.as_bytes(), 3);

        assert_eq!(output.error_count, 13);
        assert_eq!(output.errors.len(), 3);
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [
                ErrorKind::NoSemicolon,
                ErrorKind::NoSemicolon,
                ErrorKind::ForbiddenCharacter,
            ]
        );
        assert_eq!(
            output.tokens.last().map(|token| token.kind),
            Some(Kind::Eof)
        );
        assert!(output.tokens.iter().any(|token| token.has_lexical_error));
    }

    #[test]
    fn zero_error_limit_still_marks_invalid_utf8_and_counts_it() {
        let output = super::lex_with_error_limit(b"\xff", 0);

        assert!(output.invalid_utf8);
        assert_eq!(output.error_count, 1);
        assert!(output.errors.is_empty());
    }

    #[test]
    fn lexes_lock_arrow_and_reports_semicolons() {
        let output = lex(b"lock 1 -> 2;");
        assert_eq!(
            output
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                Kind::Word,
                Kind::Integer,
                Kind::Punctuator(Punct::Arrow),
                Kind::Integer,
                Kind::Punctuator(Punct::Semicolon),
                Kind::Eof,
            ]
        );
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            [ErrorKind::NoSemicolon]
        );
    }

    #[test]
    fn invalid_utf8_returns_a_bounded_input_diagnostic() {
        let output = lex(&[b'a', 0xf0, 0x28, 0x8c, 0x28]);
        assert_eq!(
            output.errors,
            [super::PackageLexError {
                kind: ErrorKind::InvalidUtf8,
                span: 1..2,
            }]
        );
        assert_eq!(output.tokens[0].kind, Kind::Eof);
    }
}
