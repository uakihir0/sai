#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use sai_diagnostics::{
    Applicability, Child, ChildKind, Code, Diagnostic, Fix, FixOperation, FixOperationName,
    FixOperationPlanner, Location, NodeGroup, NodeId, OperationPlanError, Severity, SourceRef,
    TextEdit, Value, ValueObject,
};
use sai_vfs::{FileId, Span, TextRange, TextSize, WorkspaceSnapshot};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::{UnicodeNormalization, is_nfc};

#[allow(non_camel_case_types)]
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SyntaxKind {
    LIDENT,
    UIDENT,
    PKG_PATH,
    INT,
    FLOAT,
    STRING,
    BYTE_STRING,
    CHAR,
    BYTE,
    LABEL,
    FSTRING_START,
    FSTRING_TEXT,
    FSTRING_INTERP_OPEN,
    FSTRING_SPEC,
    FSTRING_INTERP_CLOSE,
    FSTRING_END,
    DOC_OUTER,
    DOC_INNER,
    AND_KW,
    AS_KW,
    ASSERT_KW,
    ASYNC_KW,
    AWAIT_KW,
    BORROW_KW,
    BREAK_KW,
    CONST_KW,
    CONTINUE_KW,
    ELSE_KW,
    ENUM_KW,
    EXPORT_KW,
    EXTERN_KW,
    FALSE_KW,
    FN_KW,
    FOR_KW,
    IF_KW,
    IMPL_KW,
    IN_KW,
    LET_KW,
    LOOP_KW,
    MATCH_KW,
    MOVE_KW,
    MUT_KW,
    NOT_KW,
    OR_KW,
    OWNED_KW,
    PUB_KW,
    RETURN_KW,
    SELF_KW,
    SELF_TYPE_KW,
    TRAIT_KW,
    TRUE_KW,
    TYPE_KW,
    UNSAFE_KW,
    USE_KW,
    VAR_KW,
    WHERE_KW,
    WHILE_KW,
    L_PAREN,
    R_PAREN,
    L_BRACE,
    R_BRACE,
    L_BRACKET,
    R_BRACKET,
    COMMA,
    DOT,
    DOT2,
    DOT2EQ,
    COLON,
    EQ,
    THIN_ARROW,
    FAT_ARROW,
    AT,
    QUESTION,
    PLUS,
    MINUS,
    STAR,
    SLASH,
    PERCENT,
    PLUS_PERCENT,
    MINUS_PERCENT,
    STAR_PERCENT,
    AMP,
    PIPE,
    CARET,
    TILDE,
    SHL,
    LT,
    LE,
    EQ2,
    NEQ,
    PLUS_EQ,
    MINUS_EQ,
    STAR_EQ,
    SLASH_EQ,
    PERCENT_EQ,
    PLUS_PERCENT_EQ,
    MINUS_PERCENT_EQ,
    STAR_PERCENT_EQ,
    AMP_EQ,
    PIPE_EQ,
    CARET_EQ,
    SHL_EQ,
    GT,
    UNDERSCORE,
    SHR,
    GE,
    SHR_EQ,
    COLON2,
    AMP2,
    PIPE2,
    NEWLINE,
    ERROR,
    EOF,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenFlags(u16);

impl TokenFlags {
    pub const JOINT: Self = Self(1 << 0);
    pub const AFTER_LINE_BREAK: Self = Self(1 << 1);
    pub const RESERVED: Self = Self(1 << 2);
    pub const RAW: Self = Self(1 << 3);
    pub const HAS_ERROR: Self = Self(1 << 4);
    pub const IN_E: Self = Self(1 << 5);
    pub const NON_ASCII: Self = Self(1 << 6);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    fn insert(&mut self, flags: Self) {
        self.0 |= flags.0;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,
    pub flags: TokenFlags,
    pub leading: Range<usize>,
    pub trailing: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriviaKind {
    Whitespace,
    LineBreak,
    LineComment,
    BlockComment,
    Shebang,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NewlineTrigger {
    Trivia(usize),
    Semicolon(usize),
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewlineCause {
    pub newline_token: usize,
    pub previous_token: usize,
    pub trigger: NewlineTrigger,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LexOutput {
    pub tokens: Vec<Token>,
    pub trivia: Vec<Trivia>,
    pub errors: Vec<Diagnostic>,
    pub newline_causes: Vec<NewlineCause>,
    pending_fixes: Vec<PendingFix>,
    file_id: Option<FileId>,
    raw_source: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingFix {
    diagnostic: usize,
    range: TextRange,
    title: &'static str,
    applicability: Applicability,
    operation: FixOperationName,
    replacement: Option<String>,
}

struct LexicalFixTemplate {
    title: &'static str,
    applicability: Applicability,
    operation: FixOperationName,
    replacement: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexError {
    MissingFile,
    InvalidSourceSpan,
    FixPlanningFailed,
}

impl LexOutput {
    pub fn token_text<'a>(&self, token: &Token, source: &'a str) -> &'a str {
        &source[token.range.start().to_usize()..token.range.end().to_usize()]
    }

    /// Binds diagnostics to a snapshot and constructs the lexical fixes that
    /// can be planned directly from their token ranges.
    pub fn attach_lexical_fixes(&mut self, snapshot: &WorkspaceSnapshot) -> Result<(), LexError> {
        let file_id = self.file_id.ok_or(LexError::FixPlanningFailed)?;
        let source_file = snapshot.file(file_id).ok_or(LexError::FixPlanningFailed)?;
        if self.raw_source.as_deref() != Some(source_file.source().raw_bytes()) {
            return Err(LexError::FixPlanningFailed);
        }
        let mut errors = self
            .errors
            .iter()
            .cloned()
            .map(|error| {
                error
                    .with_workspace_snapshot(snapshot)
                    .map_err(|_| LexError::FixPlanningFailed)
            })
            .collect::<Result<Vec<_>, _>>()?;

        for pending in &self.pending_fixes {
            let diagnostic = errors
                .get_mut(pending.diagnostic)
                .ok_or(LexError::FixPlanningFailed)?;
            let primary = diagnostic
                .primary()
                .cloned()
                .ok_or(LexError::FixPlanningFailed)?;
            let token_index = self
                .tokens
                .iter()
                .position(|token| token.range == pending.range)
                .ok_or(LexError::FixPlanningFailed)?;
            let token_index =
                u64::try_from(token_index).map_err(|_| LexError::FixPlanningFailed)?;
            let args = if pending.operation == FixOperationName::ReplaceNode {
                ValueObject::from([(
                    "text".to_owned(),
                    Value::String(
                        pending
                            .replacement
                            .clone()
                            .ok_or(LexError::FixPlanningFailed)?,
                    ),
                )])
            } else {
                ValueObject::new()
            };
            let operation = FixOperation::new(
                pending.operation,
                NodeId::new(NodeGroup::Token, token_index),
                args,
            )
            .map_err(|_| LexError::FixPlanningFailed)?;
            let planner = LexicalFixPlanner {
                token_index,
                decoded_range: pending.range,
                operation: pending.operation,
                replacement: pending.replacement.as_deref().unwrap_or_default(),
            };
            let fix = Fix::new(
                pending.title,
                pending.applicability,
                operation,
                Vec::new(),
                &primary,
                snapshot,
                &planner,
            )
            .map_err(|_| LexError::FixPlanningFailed)?;
            diagnostic
                .push_fix(fix)
                .map_err(|_| LexError::FixPlanningFailed)?;
        }

        self.errors = errors;
        self.pending_fixes.clear();
        Ok(())
    }
}

/// Lexes one source file and binds diagnostics for consumers that need
/// snapshot-validated lexical fixes.
pub fn lex_with_fixes(
    snapshot: &WorkspaceSnapshot,
    file_id: FileId,
) -> Result<LexOutput, LexError> {
    let mut output = lex(snapshot, file_id)?;
    output.attach_lexical_fixes(snapshot)?;
    Ok(output)
}

struct LexicalFixPlanner<'a> {
    token_index: u64,
    decoded_range: TextRange,
    operation: FixOperationName,
    replacement: &'a str,
}

impl LexicalFixPlanner<'_> {
    fn edit(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<TextEdit, OperationPlanError> {
        if operation.name() != self.operation
            || operation.node().group() != NodeGroup::Token
            || operation.node().index() != self.token_index
        {
            return Err(OperationPlanError::NotApplicable);
        }
        let valid_operation = match (operation.name(), operation.args().get("text")) {
            (FixOperationName::ReplaceNode, Some(Value::String(text))) => text == self.replacement,
            (FixOperationName::RemoveToken, None) => {
                self.replacement.is_empty() || self.replacement == "\n"
            }
            _ => false,
        };
        if !valid_operation {
            return Err(OperationPlanError::NotApplicable);
        }

        let file = snapshot
            .file(primary.span.file)
            .ok_or(OperationPlanError::TargetNotFound)?;
        let range = Span::new(
            primary.span.file,
            file.source()
                .to_original_range(self.decoded_range)
                .ok_or(OperationPlanError::NotApplicable)?,
        );
        if range != primary.span {
            return Err(OperationPlanError::NotApplicable);
        }
        Ok(TextEdit {
            file: SourceRef {
                path: file.path().to_owned(),
                package: file.package().map(str::to_owned),
                snapshot: file.document_version(),
            },
            range,
            new_text: self.replacement.to_owned(),
        })
    }
}

impl FixOperationPlanner for LexicalFixPlanner<'_> {
    fn apply_operation(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<std::collections::BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
        let edit = self.edit(operation, primary, snapshot)?;
        let file = snapshot
            .file(edit.range.file)
            .ok_or(OperationPlanError::TargetNotFound)?;
        let mut bytes = file.source().raw_bytes().to_vec();
        let crlf = bytes
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .is_some_and(|index| bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n'));
        let replacement = if crlf {
            edit.new_text.replace('\n', "\r\n").into_bytes()
        } else {
            edit.new_text.into_bytes()
        };
        bytes.splice(
            edit.range.range.start().to_usize()..edit.range.range.end().to_usize(),
            replacement,
        );
        Ok(std::collections::BTreeMap::from([(edit.range.file, bytes)]))
    }

    fn plan_edits(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<TextEdit>, OperationPlanError> {
        Ok(vec![self.edit(operation, primary, snapshot)?])
    }
}

/// Lexes one source file from a consistent workspace snapshot.
///
/// Diagnostics use raw source byte offsets, while token ranges use decoded
/// text offsets, as required by the frontend and diagnostic contracts.
pub fn lex(snapshot: &WorkspaceSnapshot, file_id: FileId) -> Result<LexOutput, LexError> {
    let file = snapshot.file(file_id).ok_or(LexError::MissingFile)?;
    let source = file.source().as_ref();
    let source_ref = SourceRef {
        path: file.path().to_owned(),
        package: file.package().map(str::to_owned),
        snapshot: file.document_version(),
    };
    let mut lexer = Lexer::new(file_id, source, source_ref);

    for sequence in source.invalid_utf8_sequences() {
        lexer.report(
            Code::InvalidUtf8,
            Severity::Error,
            "source contains ill-formed UTF-8",
            sequence.decoded_range(),
        )?;
    }

    lexer.lex_shebang()?;
    loop {
        lexer.lex_trivia()?;
        if lexer.at_eof() {
            lexer.finish_eof()?;
            return Ok(lexer.output);
        }

        let token = lexer.lex_token()?;
        lexer.maybe_emit_newline_before(token)?;
        let is_semicolon =
            token.kind == SyntaxKind::ERROR && lexer.text.get(token.start..token.end) == Some(";");
        let can_arm_semicolon_newline = is_semicolon
            && !lexer.newline_emitted_since_previous
            && lexer.previous.is_some_and(|previous| previous.in_e);
        let index = lexer.push_token(token)?;
        if can_arm_semicolon_newline {
            lexer.arm_newline(NewlineTrigger::Semicolon(index));
        }
    }
}

#[derive(Clone, Copy)]
struct LexedToken {
    kind: SyntaxKind,
    start: usize,
    end: usize,
    flags: TokenFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Bracket {
    Paren,
    Square,
    Brace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForbiddenClass {
    Control,
    Bidi,
    Whitespace,
}

#[derive(Clone, Copy)]
struct PreviousToken {
    index: usize,
    in_e: bool,
}

struct Lexer<'a> {
    file_id: FileId,
    source: &'a sai_vfs::SourceFile,
    source_ref: SourceRef,
    text: &'a str,
    pos: usize,
    output: LexOutput,
    brackets: Vec<Bracket>,
    previous: Option<PreviousToken>,
    trivia_start: usize,
    after_line_break: bool,
    armed_newline: Option<(usize, NewlineTrigger)>,
    newline_emitted_since_previous: bool,
}

impl<'a> Lexer<'a> {
    fn new(file_id: FileId, source: &'a sai_vfs::SourceFile, source_ref: SourceRef) -> Self {
        Self {
            file_id,
            source,
            source_ref,
            text: source.text(),
            pos: 0,
            output: LexOutput {
                file_id: Some(file_id),
                raw_source: Some(source.raw_bytes_arc()),
                ..LexOutput::default()
            },
            brackets: Vec::new(),
            previous: None,
            trivia_start: 0,
            after_line_break: false,
            armed_newline: None,
            newline_emitted_since_previous: false,
        }
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn peek(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    fn starts_with(&self, text: &str) -> bool {
        self.text[self.pos..].starts_with(text)
    }

    fn bump_char(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn text_range(&self, start: usize, end: usize) -> Result<TextRange, LexError> {
        let start = TextSize::from_usize(start).ok_or(LexError::InvalidSourceSpan)?;
        let end = TextSize::from_usize(end).ok_or(LexError::InvalidSourceSpan)?;
        TextRange::new(start, end).ok_or(LexError::InvalidSourceSpan)
    }

    fn report(
        &mut self,
        code: Code,
        severity: Severity,
        message: impl Into<String>,
        decoded_range: TextRange,
    ) -> Result<(), LexError> {
        let span = Span::new(self.file_id, decoded_range);
        let location = Location::from_decoded_source(self.source_ref.clone(), span, self.source)
            .ok_or(LexError::InvalidSourceSpan)?;
        let diagnostic = Diagnostic::new(code, severity, message, Some(location));
        self.output.errors.push(diagnostic);
        Ok(())
    }

    fn report_at(
        &mut self,
        code: Code,
        severity: Severity,
        message: impl Into<String>,
        start: usize,
        end: usize,
    ) -> Result<(), LexError> {
        self.report(code, severity, message, self.text_range(start, end)?)
    }

    fn report_at_with_fix(
        &mut self,
        code: Code,
        severity: Severity,
        message: impl Into<String>,
        range: Range<usize>,
        fix: LexicalFixTemplate,
    ) -> Result<(), LexError> {
        let diagnostic = self.output.errors.len();
        self.report_at(code, severity, message, range.start, range.end)?;
        self.output.pending_fixes.push(PendingFix {
            diagnostic,
            range: self.text_range(range.start, range.end)?,
            title: fix.title,
            applicability: fix.applicability,
            operation: fix.operation,
            replacement: fix.replacement,
        });
        Ok(())
    }

    fn report_forbidden_at(
        &mut self,
        message: impl Into<String>,
        start: usize,
        end: usize,
    ) -> Result<(), LexError> {
        let character = self.text[start..end]
            .chars()
            .next()
            .ok_or(LexError::InvalidSourceSpan)?;
        let diagnostic_index = self.output.errors.len();
        self.report_at(
            Code::ForbiddenCharacter,
            Severity::Error,
            message,
            start,
            end,
        )?;
        self.output
            .errors
            .get_mut(diagnostic_index)
            .ok_or(LexError::InvalidSourceSpan)?
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "first forbidden character is U+{:04X} {}",
                    u32::from(character),
                    unicode_name(character)
                ),
                location: None,
            })
            .map_err(|_| LexError::FixPlanningFailed)?;
        Ok(())
    }

    fn lex_shebang(&mut self) -> Result<(), LexError> {
        if self.pos != 0 || !self.text.starts_with("#!") {
            return Ok(());
        }
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if matches!(ch, '\n' | '\r') {
                break;
            }
            self.bump_char();
        }
        let index = self.push_trivia(TriviaKind::Shebang, start, self.pos)?;
        let range = self.output.trivia[index].range;
        self.check_bidi(range)?;
        Ok(())
    }

    fn lex_trivia(&mut self) -> Result<(), LexError> {
        loop {
            if self.at_eof() || self.at_doc_comment() {
                return Ok(());
            }
            let start = self.pos;
            match self.peek() {
                Some(' ' | '\t') => {
                    self.bump_while(|ch| matches!(ch, ' ' | '\t'));
                    self.push_trivia(TriviaKind::Whitespace, start, self.pos)?;
                }
                Some('\n') => {
                    self.pos += 1;
                    let index = self.push_trivia(TriviaKind::LineBreak, start, self.pos)?;
                    self.after_line_break = true;
                    self.arm_newline(NewlineTrigger::Trivia(index));
                }
                Some('\r') => {
                    self.pos += 1;
                    if self.peek() == Some('\n') {
                        self.pos += 1;
                    } else {
                        self.report_at(
                            Code::InvalidLineEnding,
                            Severity::Error,
                            "lone CR is an invalid line ending",
                            start,
                            self.pos,
                        )?;
                    }
                    let index = self.push_trivia(TriviaKind::LineBreak, start, self.pos)?;
                    self.after_line_break = true;
                    self.arm_newline(NewlineTrigger::Trivia(index));
                }
                Some('/') if self.text[self.pos..].starts_with("//") => {
                    self.lex_line_comment()?;
                }
                Some('/') if self.text[self.pos..].starts_with("/*") => {
                    self.lex_block_comment()?;
                }
                _ => return Ok(()),
            }
        }
    }

    fn at_doc_comment(&self) -> bool {
        self.text[self.pos..].starts_with("//!")
            || (self.text[self.pos..].starts_with("///")
                && !self.text[self.pos..].starts_with("////"))
    }

    fn lex_line_comment(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if matches!(ch, '\n' | '\r') {
                break;
            }
            self.bump_char();
        }
        let index = self.push_trivia(TriviaKind::LineComment, start, self.pos)?;
        let range = self.output.trivia[index].range;
        self.check_bidi(range)?;
        Ok(())
    }

    fn lex_block_comment(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1_u32;
        let mut contains_line_break = false;
        while !self.at_eof() {
            if self.starts_with("/*") {
                self.pos += 2;
                depth = depth.saturating_add(1);
            } else if self.starts_with("*/") {
                self.pos += 2;
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if self.starts_with("\r\n") {
                contains_line_break = true;
                self.pos += 2;
            } else if self.peek() == Some('\r') {
                let line_break = self.pos;
                contains_line_break = true;
                self.pos += 1;
                self.report_at(
                    Code::InvalidLineEnding,
                    Severity::Error,
                    "lone CR is an invalid line ending",
                    line_break,
                    self.pos,
                )?;
            } else if self.peek() == Some('\n') {
                contains_line_break = true;
                self.pos += 1;
            } else {
                self.bump_char();
            }
        }
        let index = self.push_trivia(TriviaKind::BlockComment, start, self.pos)?;
        let range = self.output.trivia[index].range;
        self.check_bidi(range)?;
        if depth != 0 {
            self.report_at(
                Code::UnterminatedComment,
                Severity::Error,
                "unterminated block comment",
                start,
                self.pos,
            )?;
        }
        if contains_line_break {
            self.after_line_break = true;
            self.arm_newline(NewlineTrigger::Trivia(index));
        }
        Ok(())
    }

    fn check_bidi(&mut self, range: TextRange) -> Result<(), LexError> {
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        let mut cursor = start;
        while cursor < end {
            let ch = self.text[cursor..end]
                .chars()
                .next()
                .ok_or(LexError::InvalidSourceSpan)?;
            if forbidden_class(ch) != Some(ForbiddenClass::Bidi) {
                cursor += ch.len_utf8();
                continue;
            }
            let run_start = cursor;
            cursor += ch.len_utf8();
            while cursor < end {
                let next = self.text[cursor..end]
                    .chars()
                    .next()
                    .ok_or(LexError::InvalidSourceSpan)?;
                if forbidden_class(next) != Some(ForbiddenClass::Bidi) {
                    break;
                }
                cursor += next.len_utf8();
            }
            self.report_forbidden_at(
                "bidirectional control characters are forbidden in comments and shebangs",
                run_start,
                cursor,
            )?;
        }
        Ok(())
    }

    fn push_trivia(
        &mut self,
        kind: TriviaKind,
        start: usize,
        end: usize,
    ) -> Result<usize, LexError> {
        let range = self.text_range(start, end)?;
        let index = self.output.trivia.len();
        self.output.trivia.push(Trivia { kind, range });
        Ok(index)
    }

    fn lex_token(&mut self) -> Result<LexedToken, LexError> {
        if self.at_doc_comment() {
            return self.lex_doc_comment();
        }
        let ch = self.peek().ok_or(LexError::InvalidSourceSpan)?;
        if ch == 'r'
            && self.text[self.pos..].starts_with("r#")
            && self
                .text
                .get(self.pos + 2..)
                .and_then(|rest| rest.chars().next())
                .is_some_and(is_ident_start)
        {
            return self.lex_identifier(true);
        }
        if is_ident_start(ch) {
            return self.lex_identifier(false);
        }
        if let Some(class) = forbidden_class(ch) {
            return self.lex_forbidden_run(class);
        }
        if self
            .source
            .invalid_utf8_sequence_at(
                TextSize::from_usize(self.pos).ok_or(LexError::InvalidSourceSpan)?,
            )
            .is_some()
        {
            let start = self.pos;
            self.bump_char();
            return Ok(LexedToken {
                kind: SyntaxKind::ERROR,
                start,
                end: self.pos,
                flags: TokenFlags::empty(),
            });
        }
        if ch.is_ascii_digit() {
            return self.lex_number();
        }
        self.lex_punctuator_or_unexpected()
    }

    fn lex_doc_comment(&mut self) -> Result<LexedToken, LexError> {
        let start = self.pos;
        let kind = if self.text[self.pos..].starts_with("//!") {
            SyntaxKind::DOC_INNER
        } else {
            SyntaxKind::DOC_OUTER
        };
        while let Some(ch) = self.peek() {
            if matches!(ch, '\n' | '\r') {
                break;
            }
            self.bump_char();
        }
        let range = self.text_range(start, self.pos)?;
        self.check_bidi(range)?;
        Ok(LexedToken {
            kind,
            start,
            end: self.pos,
            flags: TokenFlags::empty(),
        })
    }

    fn lex_identifier(&mut self, raw: bool) -> Result<LexedToken, LexError> {
        let start = self.pos;
        if raw {
            self.pos += 2;
        }
        self.bump_char();
        self.bump_while(unicode_ident::is_xid_continue);
        let end = self.pos;
        let raw_name_start = start + usize::from(raw) * 2;
        let name = &self.text[raw_name_start..end];
        let mut flags = TokenFlags::empty();
        if raw {
            flags.insert(TokenFlags::RAW);
        }
        if !name.is_ascii() {
            flags.insert(TokenFlags::NON_ASCII);
        }
        if !is_nfc(name) {
            let normalized = name.nfc().collect::<String>();
            let replacement = if raw {
                format!("r#{normalized}")
            } else {
                normalized.clone()
            };
            self.report_at_with_fix(
                Code::IdentNotNfc,
                Severity::Error,
                format!("identifier is not NFC-normalized; use `{normalized}`"),
                start..end,
                LexicalFixTemplate {
                    title: "Normalize identifier",
                    applicability: Applicability::MachineApplicable,
                    operation: FixOperationName::ReplaceNode,
                    replacement: Some(replacement),
                },
            )?;
        }

        if !raw && name == "_" {
            return Ok(LexedToken {
                kind: SyntaxKind::UNDERSCORE,
                start,
                end,
                flags,
            });
        }

        if raw {
            if matches!(name, "self" | "Self" | "_") {
                self.report_at(
                    Code::InvalidRawIdent,
                    Severity::Error,
                    "r#self, r#Self, and r#_ are not valid raw identifiers",
                    start,
                    end,
                )?;
            } else if keyword_class(name).is_none() {
                self.report_at_with_fix(
                    Code::UnnecessaryRawIdent,
                    Severity::Warning,
                    "raw identifier prefix is unnecessary here",
                    start..end,
                    LexicalFixTemplate {
                        title: "Remove unnecessary raw identifier prefix",
                        applicability: Applicability::MachineApplicable,
                        operation: FixOperationName::ReplaceNode,
                        replacement: Some(name.to_owned()),
                    },
                )?;
            }
            return Ok(LexedToken {
                kind: case_kind(name),
                start,
                end,
                flags,
            });
        }

        let kind = match keyword_class(name) {
            Some(KeywordClass::Strict(kind)) => kind,
            Some(KeywordClass::Contextual) => SyntaxKind::LIDENT,
            Some(KeywordClass::Reserved(code)) => {
                flags.insert(TokenFlags::RESERVED);
                let (message, diagnostic_code) = match code {
                    Code::NoNull => (
                        "null-like values are not part of SAI; use Option and None",
                        code,
                    ),
                    Code::NoExceptions => {
                        ("exceptions are not part of SAI; use Result and ?", code)
                    }
                    _ => ("reserved word cannot be used as an identifier", code),
                };
                self.report_at(diagnostic_code, Severity::Error, message, start, end)?;
                SyntaxKind::LIDENT
            }
            None => case_kind(name),
        };
        Ok(LexedToken {
            kind,
            start,
            end,
            flags,
        })
    }

    fn lex_number(&mut self) -> Result<LexedToken, LexError> {
        let start = self.pos;
        let after_dot = self
            .previous
            .is_some_and(|previous| self.output.tokens[previous.index].kind == SyntaxKind::DOT);
        let mut malformed = false;

        if !after_dot {
            let radix_prefix = if self.starts_with("0x") {
                Some((16, false))
            } else if self.starts_with("0o") {
                Some((8, false))
            } else if self.starts_with("0b") {
                Some((2, false))
            } else if self.starts_with("0X") {
                Some((16, true))
            } else if self.starts_with("0O") {
                Some((8, true))
            } else if self.starts_with("0B") {
                Some((2, true))
            } else {
                None
            };

            if let Some((radix, uppercase_prefix)) = radix_prefix {
                self.pos += 2;
                let digits_start = self.pos;
                self.bump_while(|ch| {
                    ch == '_' || ch.is_ascii_digit() || (radix == 16 && ch.is_ascii_hexdigit())
                });
                let digits = &self.text[digits_start..self.pos];
                malformed |= uppercase_prefix
                    || malformed_digit_run(digits)
                    || digits.bytes().any(|byte| {
                        byte != b'_' && digit_value(byte).is_none_or(|value| value >= radix)
                    });
            } else {
                let integer_start = self.pos;
                self.scan_decimal_digits();
                let integer_digits = &self.text[integer_start..self.pos];
                malformed |= malformed_digit_run(integer_digits);
                let mut kind = SyntaxKind::INT;

                if self.peek() == Some('.')
                    && self
                        .text
                        .get(self.pos + 1..)
                        .and_then(|rest| rest.chars().next())
                        .is_some_and(|ch| ch.is_ascii_digit())
                {
                    self.pos += 1;
                    let fraction_start = self.pos;
                    self.scan_decimal_digits();
                    malformed |= malformed_digit_run(&self.text[fraction_start..self.pos]);
                    kind = SyntaxKind::FLOAT;
                }

                if matches!(self.peek(), Some('e' | 'E')) && self.exponent_follows() {
                    self.bump_char();
                    if matches!(self.peek(), Some('+' | '-')) {
                        self.bump_char();
                    }
                    let exponent_start = self.pos;
                    self.scan_decimal_digits();
                    malformed |= malformed_digit_run(&self.text[exponent_start..self.pos]);
                    kind = SyntaxKind::FLOAT;
                }

                malformed |= kind == SyntaxKind::INT
                    && integer_digits.starts_with('0')
                    && integer_digits.bytes().filter(u8::is_ascii_digit).count() > 1;
                if self.peek().is_some_and(unicode_ident::is_xid_continue) {
                    self.bump_while(unicode_ident::is_xid_continue);
                    malformed = true;
                }
                return self.finish_number(start, kind, malformed);
            }
        } else {
            let integer_start = self.pos;
            self.scan_decimal_digits();
            let integer_digits = &self.text[integer_start..self.pos];
            malformed |= malformed_digit_run(integer_digits)
                || (integer_digits.starts_with('0')
                    && integer_digits.bytes().filter(u8::is_ascii_digit).count() > 1);
        }

        if self.peek().is_some_and(unicode_ident::is_xid_continue) {
            self.bump_while(unicode_ident::is_xid_continue);
            malformed = true;
        }
        self.finish_number(start, SyntaxKind::INT, malformed)
    }

    fn scan_decimal_digits(&mut self) {
        self.bump_while(|ch| ch.is_ascii_digit() || ch == '_');
    }

    fn exponent_follows(&self) -> bool {
        let mut cursor = self.pos + 1;
        if self
            .text
            .get(cursor..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|ch| matches!(ch, '+' | '-'))
        {
            cursor += 1;
        }
        self.text
            .get(cursor..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|ch| ch.is_ascii_digit())
    }

    fn finish_number(
        &mut self,
        start: usize,
        kind: SyntaxKind,
        malformed: bool,
    ) -> Result<LexedToken, LexError> {
        let mut flags = TokenFlags::empty();
        if malformed {
            self.report_at(
                Code::MalformedNumber,
                Severity::Error,
                "malformed numeric literal",
                start,
                self.pos,
            )?;
            flags.insert(TokenFlags::HAS_ERROR);
        }
        Ok(LexedToken {
            kind,
            start,
            end: self.pos,
            flags,
        })
    }

    fn lex_forbidden_run(&mut self, class: ForbiddenClass) -> Result<LexedToken, LexError> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|ch| forbidden_class(ch) == Some(class))
        {
            self.bump_char();
        }
        self.report_forbidden_at(format!("forbidden {class:?} character"), start, self.pos)?;
        Ok(LexedToken {
            kind: SyntaxKind::ERROR,
            start,
            end: self.pos,
            flags: TokenFlags::empty(),
        })
    }

    fn lex_punctuator_or_unexpected(&mut self) -> Result<LexedToken, LexError> {
        const PUNCTUATORS: &[(&str, SyntaxKind)] = &[
            ("..=", SyntaxKind::DOT2EQ),
            ("+%=", SyntaxKind::PLUS_PERCENT_EQ),
            ("-%=", SyntaxKind::MINUS_PERCENT_EQ),
            ("*%=", SyntaxKind::STAR_PERCENT_EQ),
            ("<<=", SyntaxKind::SHL_EQ),
            ("->", SyntaxKind::THIN_ARROW),
            ("=>", SyntaxKind::FAT_ARROW),
            ("..", SyntaxKind::DOT2),
            ("<=", SyntaxKind::LE),
            ("==", SyntaxKind::EQ2),
            ("!=", SyntaxKind::NEQ),
            ("+=", SyntaxKind::PLUS_EQ),
            ("-=", SyntaxKind::MINUS_EQ),
            ("*=", SyntaxKind::STAR_EQ),
            ("/=", SyntaxKind::SLASH_EQ),
            ("%=", SyntaxKind::PERCENT_EQ),
            ("&=", SyntaxKind::AMP_EQ),
            ("|=", SyntaxKind::PIPE_EQ),
            ("^=", SyntaxKind::CARET_EQ),
            ("<<", SyntaxKind::SHL),
            ("+%", SyntaxKind::PLUS_PERCENT),
            ("-%", SyntaxKind::MINUS_PERCENT),
            ("*%", SyntaxKind::STAR_PERCENT),
        ];
        let start = self.pos;
        for (text, kind) in PUNCTUATORS {
            if self.starts_with(text) {
                self.pos += text.len();
                return Ok(self.token(*kind, start, self.pos));
            }
        }

        let ch = self.peek().ok_or(LexError::InvalidSourceSpan)?;
        let kind = match ch {
            '(' => Some(SyntaxKind::L_PAREN),
            ')' => Some(SyntaxKind::R_PAREN),
            '{' => Some(SyntaxKind::L_BRACE),
            '}' => Some(SyntaxKind::R_BRACE),
            '[' => Some(SyntaxKind::L_BRACKET),
            ']' => Some(SyntaxKind::R_BRACKET),
            ',' => Some(SyntaxKind::COMMA),
            '.' => Some(SyntaxKind::DOT),
            ':' => Some(SyntaxKind::COLON),
            '=' => Some(SyntaxKind::EQ),
            '@' => Some(SyntaxKind::AT),
            '?' => Some(SyntaxKind::QUESTION),
            '+' => Some(SyntaxKind::PLUS),
            '-' => Some(SyntaxKind::MINUS),
            '*' => Some(SyntaxKind::STAR),
            '/' => Some(SyntaxKind::SLASH),
            '%' => Some(SyntaxKind::PERCENT),
            '&' => Some(SyntaxKind::AMP),
            '|' => Some(SyntaxKind::PIPE),
            '^' => Some(SyntaxKind::CARET),
            '~' => Some(SyntaxKind::TILDE),
            '<' => Some(SyntaxKind::LT),
            '>' => Some(SyntaxKind::GT),
            '!' => None,
            ';' => None,
            _ => return self.lex_unexpected_run(),
        };
        self.bump_char();
        if let Some(kind) = kind {
            return Ok(self.token(kind, start, self.pos));
        }
        let (code, message, fix) = if ch == '!' {
            let needs_leading_space = self.text[..start]
                .chars()
                .next_back()
                .is_some_and(unicode_ident::is_xid_continue);
            let needs_trailing_space = self.peek().is_some_and(unicode_ident::is_xid_continue);
            let replacement = format!(
                "{}not{}",
                if needs_leading_space { " " } else { "" },
                if needs_trailing_space { " " } else { "" }
            );
            (
                Code::NoBangOperator,
                "a lone ! is not an operator; use not",
                LexicalFixTemplate {
                    title: "Replace ! with not",
                    applicability: Applicability::MaybeIncorrect,
                    operation: FixOperationName::ReplaceNode,
                    replacement: Some(replacement),
                },
            )
        } else {
            let next_token = skip_trivia(self.text, self.pos);
            let has_line_break = self.text[self.pos..next_token]
                .chars()
                .any(|character| matches!(character, '\r' | '\n'));
            (
                Code::NoSemicolon,
                "SAI statements are terminated by newlines",
                LexicalFixTemplate {
                    title: "Remove semicolon",
                    applicability: Applicability::MachineApplicable,
                    operation: FixOperationName::RemoveToken,
                    replacement: (next_token < self.text.len() && !has_line_break)
                        .then(|| "\n".to_owned()),
                },
            )
        };
        self.report_at_with_fix(code, Severity::Error, message, start..self.pos, fix)?;
        Ok(self.token(SyntaxKind::ERROR, start, self.pos))
    }

    fn lex_unexpected_run(&mut self) -> Result<LexedToken, LexError> {
        let start = self.pos;
        self.bump_char();
        while let Some(ch) = self.peek() {
            if is_token_start(ch) || forbidden_class(ch).is_some() {
                break;
            }
            self.bump_char();
        }
        self.report_at(
            Code::UnexpectedCharacter,
            Severity::Error,
            "character cannot start a token",
            start,
            self.pos,
        )?;
        Ok(LexedToken {
            kind: SyntaxKind::ERROR,
            start,
            end: self.pos,
            flags: TokenFlags::empty(),
        })
    }

    fn token(&self, kind: SyntaxKind, start: usize, end: usize) -> LexedToken {
        LexedToken {
            kind,
            start,
            end,
            flags: TokenFlags::empty(),
        }
    }

    fn maybe_emit_newline_before(&mut self, next: LexedToken) -> Result<(), LexError> {
        let Some((previous_token, trigger)) = self.armed_newline.take() else {
            return Ok(());
        };
        if !self.is_continuation(next) {
            self.emit_newline(previous_token, trigger)?;
        }
        Ok(())
    }

    fn is_continuation(&self, next: LexedToken) -> bool {
        match next.kind {
            SyntaxKind::AND_KW | SyntaxKind::OR_KW | SyntaxKind::ELSE_KW => true,
            SyntaxKind::DOT => self.dot_continues(next.end),
            _ => false,
        }
    }

    fn dot_continues(&self, dot_end: usize) -> bool {
        let mut cursor = dot_end;
        cursor = skip_trivia(self.text, cursor);
        let Some(ch) = self.text.get(cursor..).and_then(|rest| rest.chars().next()) else {
            return false;
        };
        if ch.is_ascii_digit() {
            return true;
        }
        let raw = self.text[cursor..].starts_with("r#");
        let start = cursor + usize::from(raw) * 2;
        let Some(first) = self.text.get(start..).and_then(|rest| rest.chars().next()) else {
            return false;
        };
        if !is_ident_start(first) {
            return false;
        }
        let mut end = start + first.len_utf8();
        while let Some(next) = self.text.get(end..).and_then(|rest| rest.chars().next()) {
            if !unicode_ident::is_xid_continue(next) {
                break;
            }
            end += next.len_utf8();
        }
        let name = &self.text[start..end];
        if raw {
            return case_kind(name) == SyntaxKind::LIDENT;
        }
        if name == "_" {
            return false;
        }
        match keyword_class(name) {
            Some(KeywordClass::Strict(SyntaxKind::AWAIT_KW)) => true,
            Some(KeywordClass::Strict(_)) => false,
            _ => case_kind(name) == SyntaxKind::LIDENT,
        }
    }

    fn arm_newline(&mut self, trigger: NewlineTrigger) {
        if self.armed_newline.is_some() || self.newline_emitted_since_previous {
            return;
        }
        let Some(previous) = self.previous else {
            return;
        };
        if previous.in_e && self.bracket_condition() {
            self.armed_newline = Some((previous.index, trigger));
        }
    }

    fn bracket_condition(&self) -> bool {
        matches!(self.brackets.last(), None | Some(Bracket::Brace))
    }

    fn emit_newline(
        &mut self,
        previous_token: usize,
        trigger: NewlineTrigger,
    ) -> Result<(), LexError> {
        let trivia_end = self.output.trivia.len();
        let trailing_end = self.pending_trailing_end(trivia_end);
        let previous = self
            .output
            .tokens
            .get_mut(previous_token)
            .ok_or(LexError::InvalidSourceSpan)?;
        previous.trailing = self.trivia_start..trailing_end;
        let at = if trailing_end > self.trivia_start {
            self.output.trivia[trailing_end - 1].range.end()
        } else {
            previous.range.end()
        };
        self.trivia_start = trailing_end;
        let index = self.output.tokens.len();
        self.output.tokens.push(Token {
            kind: SyntaxKind::NEWLINE,
            range: TextRange::empty(at),
            flags: TokenFlags::empty(),
            leading: self.output.trivia.len()..self.output.trivia.len(),
            trailing: self.output.trivia.len()..self.output.trivia.len(),
        });
        self.output.newline_causes.push(NewlineCause {
            newline_token: index,
            previous_token,
            trigger,
        });
        self.newline_emitted_since_previous = true;
        Ok(())
    }

    fn finish_eof(&mut self) -> Result<(), LexError> {
        if let Some((previous_token, trigger)) = self.armed_newline.take() {
            self.emit_newline(previous_token, trigger)?;
        } else if let Some(previous) = self.previous {
            if previous.in_e && !self.newline_emitted_since_previous {
                self.emit_newline(previous.index, NewlineTrigger::Eof)?;
            }
        }
        let at = self.pos;
        self.push_token(LexedToken {
            kind: SyntaxKind::EOF,
            start: at,
            end: at,
            flags: TokenFlags::empty(),
        })?;
        Ok(())
    }

    fn pending_trailing_end(&self, trivia_end: usize) -> usize {
        self.output.trivia[self.trivia_start..trivia_end]
            .iter()
            .position(|trivia| trivia.kind == TriviaKind::LineBreak)
            .map_or(trivia_end, |offset| self.trivia_start + offset)
    }

    fn push_token(&mut self, mut token: LexedToken) -> Result<usize, LexError> {
        let range = self.text_range(token.start, token.end)?;
        let trivia_end = self.output.trivia.len();
        let leading = if let Some(previous) = self.previous {
            if self.newline_emitted_since_previous {
                self.trivia_start..trivia_end
            } else {
                let trailing_end = self.pending_trailing_end(trivia_end);
                if let Some(previous_token) = self.output.tokens.get_mut(previous.index) {
                    previous_token.trailing = self.trivia_start..trailing_end;
                    if trailing_end == trivia_end
                        && token.kind != SyntaxKind::EOF
                        && previous_token.range.end() == range.start()
                    {
                        previous_token.flags.insert(TokenFlags::JOINT);
                    }
                }
                trailing_end..trivia_end
            }
        } else {
            self.trivia_start..trivia_end
        };
        self.trivia_start = trivia_end;
        if self.after_line_break && token.kind != SyntaxKind::EOF {
            token.flags.insert(TokenFlags::AFTER_LINE_BREAK);
        }
        if is_in_e(token.kind) {
            token.flags.insert(TokenFlags::IN_E);
        }

        let index = self.output.tokens.len();
        self.output.tokens.push(Token {
            kind: token.kind,
            range,
            flags: token.flags,
            leading,
            trailing: trivia_end..trivia_end,
        });

        if token.kind != SyntaxKind::NEWLINE && token.kind != SyntaxKind::EOF {
            self.update_brackets(token.kind);
            self.previous = Some(PreviousToken {
                index,
                in_e: is_in_e(token.kind),
            });
            self.newline_emitted_since_previous = false;
            self.after_line_break = false;
        }
        Ok(index)
    }

    fn update_brackets(&mut self, kind: SyntaxKind) {
        match kind {
            SyntaxKind::L_PAREN => self.brackets.push(Bracket::Paren),
            SyntaxKind::L_BRACKET => self.brackets.push(Bracket::Square),
            SyntaxKind::L_BRACE => self.brackets.push(Bracket::Brace),
            SyntaxKind::R_PAREN => self.pop_bracket(Bracket::Paren),
            SyntaxKind::R_BRACKET => self.pop_bracket(Bracket::Square),
            SyntaxKind::R_BRACE => self.pop_bracket(Bracket::Brace),
            _ => {}
        }
    }

    fn pop_bracket(&mut self, close: Bracket) {
        if let Some(index) = self.brackets.iter().rposition(|bracket| *bracket == close) {
            self.brackets.truncate(index);
        }
    }

    fn bump_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while self.peek().is_some_and(&mut predicate) {
            self.bump_char();
        }
    }
}

#[derive(Clone, Copy)]
enum KeywordClass {
    Strict(SyntaxKind),
    Contextual,
    Reserved(Code),
}

fn keyword_class(name: &str) -> Option<KeywordClass> {
    use SyntaxKind::*;
    Some(match name {
        "and" => KeywordClass::Strict(AND_KW),
        "as" => KeywordClass::Strict(AS_KW),
        "assert" => KeywordClass::Strict(ASSERT_KW),
        "async" => KeywordClass::Strict(ASYNC_KW),
        "await" => KeywordClass::Strict(AWAIT_KW),
        "borrow" => KeywordClass::Strict(BORROW_KW),
        "break" => KeywordClass::Strict(BREAK_KW),
        "const" => KeywordClass::Strict(CONST_KW),
        "continue" => KeywordClass::Strict(CONTINUE_KW),
        "else" => KeywordClass::Strict(ELSE_KW),
        "enum" => KeywordClass::Strict(ENUM_KW),
        "export" => KeywordClass::Strict(EXPORT_KW),
        "extern" => KeywordClass::Strict(EXTERN_KW),
        "false" => KeywordClass::Strict(FALSE_KW),
        "fn" => KeywordClass::Strict(FN_KW),
        "for" => KeywordClass::Strict(FOR_KW),
        "if" => KeywordClass::Strict(IF_KW),
        "impl" => KeywordClass::Strict(IMPL_KW),
        "in" => KeywordClass::Strict(IN_KW),
        "let" => KeywordClass::Strict(LET_KW),
        "loop" => KeywordClass::Strict(LOOP_KW),
        "match" => KeywordClass::Strict(MATCH_KW),
        "move" => KeywordClass::Strict(MOVE_KW),
        "mut" => KeywordClass::Strict(MUT_KW),
        "not" => KeywordClass::Strict(NOT_KW),
        "or" => KeywordClass::Strict(OR_KW),
        "owned" => KeywordClass::Strict(OWNED_KW),
        "pub" => KeywordClass::Strict(PUB_KW),
        "return" => KeywordClass::Strict(RETURN_KW),
        "self" => KeywordClass::Strict(SELF_KW),
        "Self" => KeywordClass::Strict(SELF_TYPE_KW),
        "trait" => KeywordClass::Strict(TRAIT_KW),
        "true" => KeywordClass::Strict(TRUE_KW),
        "type" => KeywordClass::Strict(TYPE_KW),
        "unsafe" => KeywordClass::Strict(UNSAFE_KW),
        "use" => KeywordClass::Strict(USE_KW),
        "var" => KeywordClass::Strict(VAR_KW),
        "where" => KeywordClass::Strict(WHERE_KW),
        "while" => KeywordClass::Strict(WHILE_KW),
        "borrows" | "covers" | "distinct" | "effect" | "effects" | "ensures" | "import"
        | "include" | "interface" | "invariant" | "module" | "old" | "property" | "region"
        | "requires" | "resource" | "result" | "test" | "world" => KeywordClass::Contextual,
        "null" | "nil" | "undefined" => KeywordClass::Reserved(Code::NoNull),
        "try" | "catch" | "throw" => KeywordClass::Reserved(Code::NoExceptions),
        "actor" | "class" | "defer" | "dyn" | "macro" | "static" | "yield" => {
            KeywordClass::Reserved(Code::ReservedWord)
        }
        _ => return None,
    })
}

fn case_kind(name: &str) -> SyntaxKind {
    name.chars()
        .find(|ch| *ch != '_')
        .filter(|ch| {
            matches!(
                get_general_category(*ch),
                GeneralCategory::UppercaseLetter | GeneralCategory::TitlecaseLetter
            )
        })
        .map_or(SyntaxKind::LIDENT, |_| SyntaxKind::UIDENT)
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || unicode_ident::is_xid_start(ch)
}

fn is_token_start(ch: char) -> bool {
    is_ident_start(ch)
        || ch.is_ascii_digit()
        || matches!(
            ch,
            '(' | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | ','
                | '.'
                | ':'
                | '='
                | '@'
                | '?'
                | '+'
                | '-'
                | '*'
                | '/'
                | '%'
                | '&'
                | '|'
                | '^'
                | '~'
                | '<'
                | '>'
                | '!'
                | ';'
                | '"'
                | '\''
        )
}

fn forbidden_class(ch: char) -> Option<ForbiddenClass> {
    if (ch <= '\u{001F}' && !matches!(ch, '\t' | '\n' | '\r')) || ch == '\u{007F}' {
        return Some(ForbiddenClass::Control);
    }
    if matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
        return Some(ForbiddenClass::Bidi);
    }
    if (ch.is_whitespace() && !matches!(ch, '\t' | '\n' | '\r' | ' ')) || ch == '\u{200B}' {
        return Some(ForbiddenClass::Whitespace);
    }
    None
}

fn unicode_name(ch: char) -> &'static str {
    match ch {
        '\u{0000}' => "NULL",
        '\u{0001}' => "START OF HEADING",
        '\u{0002}' => "START OF TEXT",
        '\u{0003}' => "END OF TEXT",
        '\u{0004}' => "END OF TRANSMISSION",
        '\u{0005}' => "ENQUIRY",
        '\u{0006}' => "ACKNOWLEDGE",
        '\u{0007}' => "ALERT",
        '\u{0008}' => "BACKSPACE",
        '\u{000B}' => "LINE TABULATION",
        '\u{000C}' => "FORM FEED",
        '\u{000E}' => "SHIFT OUT",
        '\u{000F}' => "SHIFT IN",
        '\u{0010}' => "DATA LINK ESCAPE",
        '\u{0011}' => "DEVICE CONTROL ONE",
        '\u{0012}' => "DEVICE CONTROL TWO",
        '\u{0013}' => "DEVICE CONTROL THREE",
        '\u{0014}' => "DEVICE CONTROL FOUR",
        '\u{0015}' => "NEGATIVE ACKNOWLEDGE",
        '\u{0016}' => "SYNCHRONOUS IDLE",
        '\u{0017}' => "END OF TRANSMISSION BLOCK",
        '\u{0018}' => "CANCEL",
        '\u{0019}' => "END OF MEDIUM",
        '\u{001A}' => "SUBSTITUTE",
        '\u{001B}' => "ESCAPE",
        '\u{001C}' => "INFORMATION SEPARATOR FOUR",
        '\u{001D}' => "INFORMATION SEPARATOR THREE",
        '\u{001E}' => "INFORMATION SEPARATOR TWO",
        '\u{001F}' => "INFORMATION SEPARATOR ONE",
        '\u{007F}' => "DELETE",
        '\u{0085}' => "NEXT LINE",
        '\u{00A0}' => "NO-BREAK SPACE",
        '\u{1680}' => "OGHAM SPACE MARK",
        '\u{2000}' => "EN QUAD",
        '\u{2001}' => "EM QUAD",
        '\u{2002}' => "EN SPACE",
        '\u{2003}' => "EM SPACE",
        '\u{2004}' => "THREE-PER-EM SPACE",
        '\u{2005}' => "FOUR-PER-EM SPACE",
        '\u{2006}' => "SIX-PER-EM SPACE",
        '\u{2007}' => "FIGURE SPACE",
        '\u{2008}' => "PUNCTUATION SPACE",
        '\u{2009}' => "THIN SPACE",
        '\u{200A}' => "HAIR SPACE",
        '\u{200B}' => "ZERO WIDTH SPACE",
        '\u{2028}' => "LINE SEPARATOR",
        '\u{2029}' => "PARAGRAPH SEPARATOR",
        '\u{202A}' => "LEFT-TO-RIGHT EMBEDDING",
        '\u{202B}' => "RIGHT-TO-LEFT EMBEDDING",
        '\u{202C}' => "POP DIRECTIONAL FORMATTING",
        '\u{202D}' => "LEFT-TO-RIGHT OVERRIDE",
        '\u{202E}' => "RIGHT-TO-LEFT OVERRIDE",
        '\u{202F}' => "NARROW NO-BREAK SPACE",
        '\u{205F}' => "MEDIUM MATHEMATICAL SPACE",
        '\u{2066}' => "LEFT-TO-RIGHT ISOLATE",
        '\u{2067}' => "RIGHT-TO-LEFT ISOLATE",
        '\u{2068}' => "FIRST STRONG ISOLATE",
        '\u{2069}' => "POP DIRECTIONAL ISOLATE",
        '\u{3000}' => "IDEOGRAPHIC SPACE",
        _ => "UNKNOWN FORBIDDEN CHARACTER",
    }
}

fn is_in_e(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LIDENT
            | SyntaxKind::UIDENT
            | SyntaxKind::PKG_PATH
            | SyntaxKind::INT
            | SyntaxKind::FLOAT
            | SyntaxKind::STRING
            | SyntaxKind::BYTE_STRING
            | SyntaxKind::CHAR
            | SyntaxKind::BYTE
            | SyntaxKind::LABEL
            | SyntaxKind::FSTRING_END
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
            | SyntaxKind::SELF_KW
            | SyntaxKind::SELF_TYPE_KW
            | SyntaxKind::RETURN_KW
            | SyntaxKind::BREAK_KW
            | SyntaxKind::CONTINUE_KW
            | SyntaxKind::AWAIT_KW
            | SyntaxKind::R_PAREN
            | SyntaxKind::R_BRACKET
            | SyntaxKind::R_BRACE
            | SyntaxKind::GT
            | SyntaxKind::QUESTION
            | SyntaxKind::UNDERSCORE
            | SyntaxKind::ERROR
    )
}

fn skip_trivia(text: &str, mut cursor: usize) -> usize {
    loop {
        while let Some(ch) = text.get(cursor..).and_then(|rest| rest.chars().next()) {
            if matches!(ch, ' ' | '\t' | '\r' | '\n') {
                cursor += ch.len_utf8();
            } else {
                break;
            }
        }
        if text
            .get(cursor..)
            .is_some_and(|rest| rest.starts_with("//"))
        {
            let rest = &text[cursor..];
            let is_doc =
                rest.starts_with("//!") || (rest.starts_with("///") && !rest.starts_with("////"));
            if is_doc {
                return cursor;
            }
            while let Some(ch) = text.get(cursor..).and_then(|rest| rest.chars().next()) {
                if matches!(ch, '\r' | '\n') {
                    break;
                }
                cursor += ch.len_utf8();
            }
            continue;
        }
        if text
            .get(cursor..)
            .is_some_and(|rest| rest.starts_with("/*"))
        {
            cursor += 2;
            let mut depth = 1_u32;
            while cursor < text.len() && depth != 0 {
                if text[cursor..].starts_with("/*") {
                    depth = depth.saturating_add(1);
                    cursor += 2;
                } else if text[cursor..].starts_with("*/") {
                    depth -= 1;
                    cursor += 2;
                } else {
                    let ch = text[cursor..].chars().next().unwrap_or('\0');
                    cursor += ch.len_utf8();
                }
            }
            continue;
        }
        return cursor;
    }
}

fn malformed_digit_run(digits: &str) -> bool {
    digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.bytes().any(|byte| digit_value(byte).is_some())
}

fn digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'a'..=b'f' => Some(u32::from(byte - b'a' + 10)),
        b'A'..=b'F' => Some(u32::from(byte - b'A' + 10)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_vfs::{SnapshotFile, SourceFile};
    use std::sync::Arc;

    fn lex_text(text: &str) -> LexOutput {
        let source = Arc::new(SourceFile::new(FileId::new(1), text, false).expect("source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            source,
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");
        lex(&snapshot, FileId::new(1)).expect("lex succeeds")
    }

    fn kinds(output: &LexOutput) -> Vec<SyntaxKind> {
        output.tokens.iter().map(|token| token.kind).collect()
    }

    fn assert_every_trivia_is_attached_once(output: &LexOutput) {
        let mut attached = vec![0_u8; output.trivia.len()];
        for token in &output.tokens {
            for trivia_range in [&token.leading, &token.trailing] {
                assert!(trivia_range.start <= trivia_range.end);
                assert!(trivia_range.end <= attached.len());
                for index in trivia_range.clone() {
                    attached[index] += 1;
                }
            }
        }
        assert!(
            attached.into_iter().all(|count| count == 1),
            "every trivia piece must be attached to exactly one token"
        );
    }

    #[test]
    fn lexes_strict_contextual_and_reserved_keywords() {
        let output = lex_text("fn result null try name");

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::FN_KW,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert!(output.tokens[2].flags.contains(TokenFlags::RESERVED));
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [Code::NoNull, Code::NoExceptions]
        );
    }

    #[test]
    fn classifies_case_after_raw_prefix_and_leading_underscores() {
        let output = lex_text("_Foo __init ___ r#type r#self");

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::UIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert!(output.tokens[3].flags.contains(TokenFlags::RAW));
        assert_eq!(output.errors[0].code, Code::InvalidRawIdent);
    }

    #[test]
    fn reports_non_nfc_identifier_and_sets_non_ascii_flag() {
        let output = lex_text("e\u{301}");

        assert_eq!(output.tokens[0].kind, SyntaxKind::LIDENT);
        assert!(output.tokens[0].flags.contains(TokenFlags::NON_ASCII));
        assert_eq!(output.errors[0].code, Code::IdentNotNfc);
        assert!(output.errors[0].message.contains('é'));
    }

    #[test]
    fn lexical_fixes_bind_at_the_snapshot_boundary() {
        let text = "e\u{301} r#name ! ; after";
        let source = Arc::new(SourceFile::new(FileId::new(1), text, false).expect("source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            source,
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");

        let query_output = lex(&snapshot, FileId::new(1)).expect("lex succeeds");
        assert!(
            query_output
                .errors
                .iter()
                .all(|error| error.fixes().is_empty())
        );
        let output = lex_with_fixes(&snapshot, FileId::new(1)).expect("lex with fixes succeeds");

        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [
                Code::IdentNotNfc,
                Code::UnnecessaryRawIdent,
                Code::NoBangOperator,
                Code::NoSemicolon,
            ]
        );
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.fixes().len())
                .collect::<Vec<_>>(),
            [1, 1, 1, 1]
        );
        let fixes = output
            .errors
            .iter()
            .map(|error| &error.fixes()[0])
            .collect::<Vec<_>>();
        assert_eq!(
            fixes
                .iter()
                .map(|fix| fix.operation().name())
                .collect::<Vec<_>>(),
            [
                FixOperationName::ReplaceNode,
                FixOperationName::ReplaceNode,
                FixOperationName::ReplaceNode,
                FixOperationName::RemoveToken,
            ]
        );
        assert_eq!(fixes[0].edits()[0].new_text, "é");
        assert_eq!(fixes[1].edits()[0].new_text, "name");
        assert_eq!(fixes[2].edits()[0].new_text, "not");
        assert_eq!(fixes[3].edits()[0].new_text, "\n");
        assert_eq!(fixes[2].applicability(), Applicability::MaybeIncorrect);
    }

    #[test]
    fn semicolon_fixes_match_crlf_snapshot_application() {
        for (id, bytes, expected_edit) in [
            (1, b"a; b\r\n".as_slice(), "\n"),
            (2, b"a; \r\nb".as_slice(), ""),
        ] {
            let source = Arc::new(SourceFile::decode(FileId::new(id), bytes).expect("source"));
            let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
                source,
                format!("src/main{id}.sai"),
                Some("sai:test@0.1.0".to_owned()),
                None,
            )])
            .expect("snapshot");

            let output = lex_with_fixes(&snapshot, FileId::new(id))
                .expect("the semicolon operation and CRLF edit should agree");
            let semicolon = output
                .errors
                .iter()
                .find(|error| error.code == Code::NoSemicolon)
                .expect("semicolon diagnostic");

            assert_eq!(semicolon.fixes().len(), 1);
            assert_eq!(semicolon.fixes()[0].edits()[0].new_text, expected_edit);
        }
    }

    #[test]
    fn semicolon_newline_requires_a_terminating_token_before_it() {
        for (text, expected_semicolon_newline) in
            [("a; b", true), ("a +; b", false), ("a\n; b", false)]
        {
            let output = lex_text(text);
            let semicolon_trigger_count = output
                .newline_causes
                .iter()
                .filter(|cause| matches!(cause.trigger, NewlineTrigger::Semicolon(_)))
                .count();

            assert_eq!(
                semicolon_trigger_count,
                usize::from(expected_semicolon_newline),
                "unexpected semicolon newline behavior for {text:?}"
            );
            if text == "a\n; b" {
                assert_eq!(
                    output
                        .newline_causes
                        .iter()
                        .filter(|cause| matches!(cause.trigger, NewlineTrigger::Trivia(_)))
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn lexical_fixes_reject_a_snapshot_with_different_source_bytes() {
        let make_snapshot = |text: &str| {
            WorkspaceSnapshot::new([SnapshotFile::new(
                Arc::new(SourceFile::new(FileId::new(1), text, false).expect("source")),
                "src/main.sai",
                Some("sai:test@0.1.0".to_owned()),
                None,
            )])
            .expect("snapshot")
        };
        let original = make_snapshot("r#name");
        let replacement = make_snapshot("foobar");
        let mut output = lex(&original, FileId::new(1)).expect("lex succeeds");

        assert_eq!(
            output.attach_lexical_fixes(&replacement),
            Err(LexError::FixPlanningFailed)
        );
        assert!(output.errors.iter().all(|error| error.fixes().is_empty()));
    }

    #[test]
    fn forbidden_character_notes_name_the_first_code_point() {
        let output = lex_text("x\u{202E}\u{2066}y \u{00A0}z");

        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [Code::ForbiddenCharacter, Code::ForbiddenCharacter]
        );
        assert_eq!(
            output.errors[0].children()[0].message,
            "first forbidden character is U+202E RIGHT-TO-LEFT OVERRIDE"
        );
        assert_eq!(
            output.errors[1].children()[0].message,
            "first forbidden character is U+00A0 NO-BREAK SPACE"
        );
    }

    #[test]
    fn every_forbidden_character_class_has_a_unicode_name() {
        for raw in 0..=0x3000 {
            let Some(character) = char::from_u32(raw) else {
                continue;
            };
            if forbidden_class(character).is_some() {
                assert_ne!(
                    unicode_name(character),
                    "UNKNOWN FORBIDDEN CHARACTER",
                    "missing Unicode name for U+{raw:04X}"
                );
            }
        }
        assert_eq!(unicode_name('\u{0007}'), "ALERT");
        assert_eq!(unicode_name('\u{001C}'), "INFORMATION SEPARATOR FOUR");
    }

    #[test]
    fn lexes_decimal_radix_and_floating_point_numbers() {
        let output = lex_text("0 42 1_000 0xFF 0o77 0b1010 1.0 2.5e-3 4E5");

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::FLOAT,
                SyntaxKind::FLOAT,
                SyntaxKind::FLOAT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert!(
            output.tokens[..9]
                .iter()
                .all(|token| !token.flags.contains(TokenFlags::HAS_ERROR))
        );
    }

    #[test]
    fn malformed_numbers_are_single_error_marked_literal_tokens() {
        let text = "0XFF 0b12 1__2 1_ 0x_1 007 10u8 1.0f32 0x";
        let output = lex_text(text);

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::INT,
                SyntaxKind::FLOAT,
                SyntaxKind::INT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert_eq!(output.errors.len(), 9);
        assert!(
            output
                .errors
                .iter()
                .all(|error| error.code == Code::MalformedNumber)
        );
        assert!(
            output.tokens[..9]
                .iter()
                .all(|token| token.flags.contains(TokenFlags::HAS_ERROR))
        );
        assert_eq!(
            output.tokens[..9]
                .iter()
                .map(|token| output.token_text(token, text))
                .collect::<Vec<_>>(),
            [
                "0XFF", "0b12", "1__2", "1_", "0x_1", "007", "10u8", "1.0f32", "0x"
            ]
        );
    }

    #[test]
    fn numbers_after_dots_follow_tuple_index_lexing_rules() {
        let text = "t.0.1 t.0e1 t.0_1 t.007";
        let output = lex_text(text);

        let lexemes = output
            .tokens
            .iter()
            .filter(|token| !matches!(token.kind, SyntaxKind::NEWLINE | SyntaxKind::EOF))
            .map(|token| (token.kind, output.token_text(token, text)))
            .collect::<Vec<_>>();
        assert_eq!(
            lexemes,
            [
                (SyntaxKind::LIDENT, "t"),
                (SyntaxKind::DOT, "."),
                (SyntaxKind::INT, "0"),
                (SyntaxKind::DOT, "."),
                (SyntaxKind::INT, "1"),
                (SyntaxKind::LIDENT, "t"),
                (SyntaxKind::DOT, "."),
                (SyntaxKind::INT, "0e1"),
                (SyntaxKind::LIDENT, "t"),
                (SyntaxKind::DOT, "."),
                (SyntaxKind::INT, "0_1"),
                (SyntaxKind::LIDENT, "t"),
                (SyntaxKind::DOT, "."),
                (SyntaxKind::INT, "007"),
            ]
        );
        assert_eq!(output.errors.len(), 3);
        assert!(
            output
                .errors
                .iter()
                .all(|error| error.code == Code::MalformedNumber)
        );
    }

    #[test]
    fn keeps_comments_as_trivia_and_doc_comments_as_tokens() {
        let output = lex_text("/// docs\nfn f() { /* outer /* inner */ done */ }");
        assert_every_trivia_is_attached_once(&output);

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::DOC_OUTER,
                SyntaxKind::FN_KW,
                SyntaxKind::LIDENT,
                SyntaxKind::L_PAREN,
                SyntaxKind::R_PAREN,
                SyntaxKind::L_BRACE,
                SyntaxKind::R_BRACE,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert!(
            output
                .trivia
                .iter()
                .any(|trivia| trivia.kind == TriviaKind::BlockComment)
        );
        assert_eq!(output.tokens[0].leading, 0..0);
        assert!(!output.tokens[1].leading.is_empty());
    }

    #[test]
    fn attaches_line_breaks_and_indentation_as_leading_trivia() {
        let output = lex_text("x // trailing\n  // own line\n y");
        assert_every_trivia_is_attached_once(&output);
        let x = &output.tokens[0];
        let y = output
            .tokens
            .iter()
            .find(|token| {
                token.kind == SyntaxKind::LIDENT
                    && output.token_text(token, "x // trailing\n  // own line\n y") == "y"
            })
            .expect("y token");

        assert_eq!(
            output.trivia[x.trailing.clone()]
                .iter()
                .map(|trivia| trivia.kind)
                .collect::<Vec<_>>(),
            [TriviaKind::Whitespace, TriviaKind::LineComment]
        );
        assert_eq!(
            output.trivia[y.leading.clone()]
                .iter()
                .map(|trivia| trivia.kind)
                .collect::<Vec<_>>(),
            [
                TriviaKind::LineBreak,
                TriviaKind::Whitespace,
                TriviaKind::LineComment,
                TriviaKind::LineBreak,
                TriviaKind::Whitespace,
            ]
        );
        let newline = output
            .tokens
            .iter()
            .find(|token| token.kind == SyntaxKind::NEWLINE)
            .expect("generated newline");
        assert_eq!(
            newline.range.start(),
            output.trivia[x.trailing.end - 1].range.end()
        );
    }

    #[test]
    fn attaches_suppressed_line_breaks_to_the_next_token() {
        let text = "f(a\n  b)";
        let output = lex_text(text);
        assert_every_trivia_is_attached_once(&output);
        let a = output
            .tokens
            .iter()
            .find(|token| output.token_text(token, text) == "a")
            .expect("a token");
        let b = output
            .tokens
            .iter()
            .find(|token| output.token_text(token, text) == "b")
            .expect("b token");

        assert_eq!(
            output
                .tokens
                .iter()
                .filter(|token| token.kind == SyntaxKind::NEWLINE)
                .count(),
            1
        );
        assert!(a.trailing.is_empty());
        assert_eq!(
            output.trivia[b.leading.clone()]
                .iter()
                .map(|trivia| trivia.kind)
                .collect::<Vec<_>>(),
            [TriviaKind::LineBreak, TriviaKind::Whitespace]
        );
    }

    #[test]
    fn final_token_is_not_joint_with_eof_and_eof_owns_no_line_ending_trivia() {
        let text = "x // trailing";
        let output = lex_text(text);
        assert_every_trivia_is_attached_once(&output);
        let x = &output.tokens[0];
        let newline = &output.tokens[1];
        let eof = &output.tokens[2];

        assert!(!x.flags.contains(TokenFlags::JOINT));
        assert_eq!(x.trailing, 0..2);
        assert_eq!(newline.range.start().to_usize(), text.len());
        assert!(newline.leading.is_empty());
        assert!(newline.trailing.is_empty());
        assert!(eof.leading.is_empty());
    }

    #[test]
    fn shebang_is_leading_trivia_and_does_not_become_a_token() {
        let output = lex_text("#!/usr/bin/env sai\nfn main() {}");

        assert_eq!(output.tokens[0].kind, SyntaxKind::FN_KW);
        assert!(
            output
                .trivia
                .iter()
                .any(|trivia| trivia.kind == TriviaKind::Shebang)
        );
        assert!(output.tokens[0].leading.end > output.tokens[0].leading.start);
    }

    #[test]
    fn reports_bidi_controls_in_shebangs() {
        let output = lex_text("#!tool\u{202E}name\nfn main() {}");

        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [Code::ForbiddenCharacter]
        );
    }

    #[test]
    fn emits_newlines_at_eof_and_respects_brackets_and_continuations() {
        let output = lex_text("f(a\nb)\nx\n.find()\ny\nand z\nelse {}");

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::LIDENT,
                SyntaxKind::L_PAREN,
                SyntaxKind::LIDENT,
                SyntaxKind::LIDENT,
                SyntaxKind::R_PAREN,
                SyntaxKind::NEWLINE,
                SyntaxKind::LIDENT,
                SyntaxKind::DOT,
                SyntaxKind::LIDENT,
                SyntaxKind::L_PAREN,
                SyntaxKind::R_PAREN,
                SyntaxKind::NEWLINE,
                SyntaxKind::LIDENT,
                SyntaxKind::AND_KW,
                SyntaxKind::LIDENT,
                SyntaxKind::ELSE_KW,
                SyntaxKind::L_BRACE,
                SyntaxKind::R_BRACE,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
    }

    #[test]
    fn continuation_lookahead_skips_only_real_trivia() {
        assert_eq!(
            kinds(&lex_text("x\n./* comment */find")),
            [
                SyntaxKind::LIDENT,
                SyntaxKind::DOT,
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert_eq!(
            kinds(&lex_text("x\n.\u{00A0}find")),
            [
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::DOT,
                SyntaxKind::ERROR,
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        assert_eq!(
            kinds(&lex_text("x\n./// doc\nfind")),
            [
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::DOT,
                SyntaxKind::DOC_OUTER,
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
    }

    #[test]
    fn lexical_diagnostics_do_not_capture_transient_snapshot_identity() {
        let source = Arc::new(SourceFile::new(FileId::new(1), "null", false).expect("source"));
        let make_snapshot = || {
            WorkspaceSnapshot::new([SnapshotFile::new(
                Arc::clone(&source),
                "src/main.sai",
                Some("sai:test@0.1.0".to_owned()),
                None,
            )])
            .expect("snapshot")
        };
        let first = make_snapshot();
        let second = make_snapshot();

        assert_ne!(first.id(), second.id());
        assert_eq!(
            lex(&first, FileId::new(1)).expect("lex"),
            lex(&second, FileId::new(1)).expect("lex")
        );
    }

    #[test]
    fn preserves_joint_flags_and_keeps_gt_as_single_tokens() {
        let output = lex_text("a::b && c >> d");

        assert_eq!(
            kinds(&output),
            [
                SyntaxKind::LIDENT,
                SyntaxKind::COLON,
                SyntaxKind::COLON,
                SyntaxKind::LIDENT,
                SyntaxKind::AMP,
                SyntaxKind::AMP,
                SyntaxKind::LIDENT,
                SyntaxKind::GT,
                SyntaxKind::GT,
                SyntaxKind::LIDENT,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]
        );
        for token in &output.tokens[..3] {
            assert!(token.flags.contains(TokenFlags::JOINT));
        }
        assert!(output.tokens[7].flags.contains(TokenFlags::JOINT));
        assert!(!output.tokens[8].flags.contains(TokenFlags::JOINT));
    }

    #[test]
    fn reports_lone_cr_and_unterminated_nested_comments() {
        let output = lex_text("x\r/* outer /* inner */");

        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [Code::InvalidLineEnding, Code::UnterminatedComment]
        );
        assert_eq!(output.tokens[1].kind, SyntaxKind::NEWLINE);
    }

    #[test]
    fn reports_lone_cr_inside_block_comments() {
        let output = lex_text("x /* first\rsecond */ y");

        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [Code::InvalidLineEnding]
        );
        assert!(
            output
                .tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::NEWLINE)
        );
    }

    #[test]
    fn diagnostics_use_original_byte_offsets_after_a_bom() {
        let source =
            Arc::new(SourceFile::decode(FileId::new(1), b"\xEF\xBB\xBF!").expect("source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            source,
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");
        let output = lex(&snapshot, FileId::new(1)).expect("lex succeeds");

        assert_eq!(output.errors[0].code, Code::NoBangOperator);
        assert_eq!(
            output.errors[0]
                .primary()
                .expect("location")
                .span
                .range
                .start(),
            TextSize::new(3)
        );
    }

    #[test]
    fn malformed_utf8_is_reported_once_and_still_becomes_an_error_token() {
        let source = Arc::new(SourceFile::decode(FileId::new(1), b"\xFF").expect("source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            source,
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");
        let output = lex(&snapshot, FileId::new(1)).expect("lex succeeds");

        assert_eq!(
            kinds(&output),
            [SyntaxKind::ERROR, SyntaxKind::NEWLINE, SyntaxKind::EOF]
        );
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [Code::InvalidUtf8]
        );
    }
}
