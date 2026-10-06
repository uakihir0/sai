use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use sai_diagnostics::{Code, Diagnostic, Location, Severity, SourceRef};
use sai_vfs::{SnapshotFile, SourceFile, Span, TextRange, TextSize};

use crate::tree::{validate_joint_tokens, validate_token_mapping};
use crate::{Event, SyntaxKind, Token, TokenFlags};

mod grammar;

/// The event-producing parser cursor used by grammar routines.
///
/// It deliberately exposes raw NEWLINE tokens. Grammar productions decide
/// where a line break may be consumed with [`Parser::eat_nl`].
pub struct Parser<'src, 'tokens> {
    source: &'src str,
    tokens: &'tokens [Token],
    matching_r_parens: Vec<usize>,
    matching_r_braces: Vec<usize>,
    position: usize,
    events: Vec<Event>,
    diagnostic_source: Option<DiagnosticSource<'src>>,
    expected: ExpectedSet,
    reported_expected_at: Option<usize>,
    reported_expected_event: Option<usize>,
    marker_owner: Rc<()>,
    active_markers: Rc<RefCell<Vec<u32>>>,
    marker_failures: Rc<RefCell<Vec<ParserError>>>,
    allow_assoc_type_bindings: bool,
    no_block_operand: bool,
    stop_expression_at_line_start: bool,
    recovered_match_tail: bool,
    nesting_depth: usize,
    reported_nesting_limit: bool,
    spec_failures: HashSet<usize>,
    variant_call_boundary_cache: RefCell<Vec<Option<bool>>>,
}

fn compute_matching_r_parens(tokens: &[Token]) -> Vec<usize> {
    let mut matching_r_parens = vec![usize::MAX; tokens.len()];
    let mut open_parens = Vec::new();
    for (position, token) in tokens.iter().enumerate() {
        match token.kind {
            SyntaxKind::L_PAREN => open_parens.push(position),
            SyntaxKind::R_PAREN => {
                if let Some(open_position) = open_parens.pop() {
                    matching_r_parens[open_position] = position;
                }
            }
            _ => {}
        }
    }
    matching_r_parens
}

fn compute_matching_r_braces(tokens: &[Token]) -> Vec<usize> {
    let mut matching_r_braces = vec![usize::MAX; tokens.len()];
    let mut open_braces = Vec::new();
    for (position, token) in tokens.iter().enumerate() {
        match token.kind {
            SyntaxKind::L_BRACE => open_braces.push(position),
            SyntaxKind::R_BRACE => {
                if let Some(open_position) = open_braces.pop() {
                    matching_r_braces[open_position] = position;
                }
            }
            _ => {}
        }
    }
    matching_r_braces
}

struct DiagnosticSource<'src> {
    file: &'src SourceFile,
    source_ref: SourceRef,
}

#[derive(Clone, Default)]
struct ExpectedSet {
    position: Option<usize>,
    kinds: Vec<SyntaxKind>,
}

impl ExpectedSet {
    fn record(&mut self, position: usize, kind: SyntaxKind) {
        if self.position != Some(position) {
            self.position = Some(position);
            self.kinds.clear();
        }
        if !self.kinds.contains(&kind) {
            self.kinds.push(kind);
        }
    }

    fn clear(&mut self) {
        self.position = None;
        self.kinds.clear();
    }

    fn message(&self) -> String {
        let expected = self
            .kinds
            .iter()
            .map(|kind| format!("{kind:?}"))
            .collect::<Vec<_>>();
        format!("expected {}", expected.join(" or "))
    }
}

/// A start point in the parser event stream.
pub struct Marker {
    position: u32,
    owner: Rc<()>,
    active_markers: Rc<RefCell<Vec<u32>>>,
    failures: Rc<RefCell<Vec<ParserError>>>,
    forward_child: Option<u32>,
    completed: bool,
}

/// A completed syntax node that can be wrapped by a later parent node.
pub struct CompletedMarker {
    position: u32,
    kind: SyntaxKind,
    owner: Rc<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParserError {
    InvalidTokenStream { position: usize },
    UnconsumedTokens { position: usize },
    UnfinishedMarker { position: u32 },
    InvalidMarkerNesting { position: u32 },
    InvalidNodeKind { position: u32, kind: SyntaxKind },
    InvalidMarkerEvent { position: u32 },
    ForeignMarker,
    MissingDiagnosticSource,
    InvalidDiagnosticSpan,
    EmptyExpectedSet,
    InvalidExpectedKind { kind: SyntaxKind },
    UnsupportedItem { position: usize, kind: SyntaxKind },
    UnsupportedSyntax { position: usize, kind: SyntaxKind },
}

impl<'src, 'tokens> Parser<'src, 'tokens> {
    pub fn new(source: &'src str, tokens: &'tokens [Token]) -> Result<Self, ParserError> {
        if let Some(position) = tokens
            .iter()
            .take(tokens.len().saturating_sub(1))
            .position(|token| token.kind == SyntaxKind::EOF)
        {
            return Err(ParserError::InvalidTokenStream { position });
        }
        let Some(last) = tokens.last() else {
            return Err(ParserError::InvalidTokenStream { position: 0 });
        };
        if last.kind != SyntaxKind::EOF {
            return Err(ParserError::InvalidTokenStream {
                position: tokens.len(),
            });
        }
        let mut previous_end = 0;
        for (position, token) in tokens.iter().enumerate() {
            let start = token.range.start().to_usize();
            let end = token.range.end().to_usize();
            let zero_width_allowed = matches!(token.kind, SyntaxKind::NEWLINE | SyntaxKind::EOF)
                || (token.kind == SyntaxKind::FSTRING_END
                    && token.flags.contains(TokenFlags::HAS_ERROR));
            if !token.kind.is_token()
                || validate_token_mapping(tokens, position, position + 1, token.kind).is_err()
                || start < previous_end
                || end > source.len()
                || !source.is_char_boundary(start)
                || !source.is_char_boundary(end)
                || (token.range.is_empty() && !zero_width_allowed)
                || (!token.range.is_empty()
                    && matches!(token.kind, SyntaxKind::NEWLINE | SyntaxKind::EOF))
                || (token.kind == SyntaxKind::EOF && (start != source.len() || end != start))
            {
                return Err(ParserError::InvalidTokenStream { position });
            }
            previous_end = end;
        }
        let matching_r_parens = compute_matching_r_parens(tokens);
        let matching_r_braces = compute_matching_r_braces(tokens);
        Ok(Parser {
            source,
            tokens,
            matching_r_parens,
            matching_r_braces,
            position: 0,
            events: Vec::new(),
            diagnostic_source: None,
            expected: ExpectedSet::default(),
            reported_expected_at: None,
            reported_expected_event: None,
            marker_owner: Rc::new(()),
            active_markers: Rc::new(RefCell::new(Vec::new())),
            marker_failures: Rc::new(RefCell::new(Vec::new())),
            allow_assoc_type_bindings: false,
            no_block_operand: false,
            stop_expression_at_line_start: false,
            recovered_match_tail: false,
            nesting_depth: 0,
            reported_nesting_limit: false,
            spec_failures: HashSet::new(),
            variant_call_boundary_cache: RefCell::new(vec![None; tokens.len()]),
        })
    }

    /// Creates a parser whose syntax diagnostics are bound to one snapshot file.
    pub fn new_for_file(
        file: &'src SnapshotFile,
        tokens: &'tokens [Token],
    ) -> Result<Self, ParserError> {
        let source_file = file.source().as_ref();
        let mut parser = Self::new(source_file.text(), tokens)?;
        for (position, token) in tokens.iter().enumerate() {
            if source_file.to_original_range(token.range).is_none() {
                return Err(ParserError::InvalidTokenStream { position });
            }
        }
        parser.diagnostic_source = Some(DiagnosticSource {
            file: source_file,
            source_ref: SourceRef {
                path: file.path().to_owned(),
                package: file.package().map(str::to_owned),
                snapshot: file.document_version(),
            },
        });
        Ok(parser)
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub fn nth(&self, lookahead: usize) -> SyntaxKind {
        self.position
            .checked_add(lookahead)
            .and_then(|position| self.tokens.get(position))
            .map_or(SyntaxKind::EOF, |token| token.kind)
    }

    pub fn current_token(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    pub fn current_text(&self) -> &str {
        let Some(token) = self.current_token() else {
            return "";
        };
        self.source
            .get(token.range.start().to_usize()..token.range.end().to_usize())
            .unwrap_or("")
    }

    pub fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    pub fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.contains(&self.current())
    }

    /// Matches a contextual keyword only on a non-raw lowercase identifier.
    pub fn at_contextual(&self, spelling: &str) -> bool {
        self.current_token().is_some_and(|token| {
            token.kind == SyntaxKind::LIDENT
                && !token.flags.contains(TokenFlags::RAW)
                && self.current_text() == spelling
        })
    }

    pub fn eat(&mut self, kind: SyntaxKind) -> bool {
        let Some(n_raw) = raw_token_count(kind) else {
            return false;
        };
        self.bump_as(kind, n_raw)
    }

    pub fn eat_nl(&mut self) -> bool {
        self.eat(SyntaxKind::NEWLINE)
    }

    /// Consumes the expected token, or emits one `EXPECTED_TOKEN` diagnostic.
    ///
    /// The parser must have been created with [`Parser::new_for_file`] before
    /// this method can emit a source-bound diagnostic.
    pub fn expect(&mut self, kind: SyntaxKind) -> Result<bool, ParserError> {
        if !kind.is_token() {
            return Err(ParserError::InvalidExpectedKind { kind });
        }
        if self.eat(kind) {
            return Ok(true);
        }
        self.expected.record(self.position, kind);
        if self.emit_expected_token()? {
            self.emit_missing_error_node()?;
        }
        Ok(false)
    }

    /// Consumes one of the given kinds, or emits a diagnostic listing them.
    pub fn expect_any(&mut self, kinds: &[SyntaxKind]) -> Result<bool, ParserError> {
        if kinds.is_empty() {
            return Err(ParserError::EmptyExpectedSet);
        }
        if let Some(kind) = kinds.iter().find(|kind| !kind.is_token()) {
            return Err(ParserError::InvalidExpectedKind { kind: *kind });
        }
        for kind in kinds {
            if let Some(n_raw) = raw_token_count(*kind)
                && self.can_bump_as(*kind, n_raw)
            {
                self.bump_as(*kind, n_raw);
                return Ok(true);
            }
        }
        for kind in kinds {
            self.expected.record(self.position, *kind);
        }
        if self.emit_expected_token()? {
            self.emit_missing_error_node()?;
        }
        Ok(false)
    }

    /// Emits the current raw token unchanged and advances the cursor.
    pub fn bump(&mut self) -> SyntaxKind {
        let kind = self
            .current_token()
            .expect("cannot bump beyond the end of the token stream")
            .kind;
        assert!(
            self.bump_as(kind, 1),
            "validated raw token could not be emitted"
        );
        kind
    }

    /// Emits one grammar token, optionally gluing several joint raw tokens.
    pub fn bump_as(&mut self, kind: SyntaxKind, n_raw: u8) -> bool {
        if !self.can_bump_as(kind, n_raw) {
            return false;
        }
        let count = usize::from(n_raw);
        let end = self.position + count;
        self.events.push(Event::Token { kind, n_raw });
        self.position = end;
        self.expected.clear();
        self.reported_expected_at = None;
        self.reported_expected_event = None;
        true
    }

    fn can_bump_as(&self, kind: SyntaxKind, n_raw: u8) -> bool {
        let count = usize::from(n_raw);
        let Some(end) = self.position.checked_add(count) else {
            return false;
        };
        count > 0
            && end <= self.tokens.len()
            && !(count > 1
                && self.tokens[self.position..end]
                    .iter()
                    .any(|token| token.kind == SyntaxKind::EOF))
            && validate_token_mapping(self.tokens, self.position, end, kind).is_ok()
            && validate_joint_tokens(self.tokens, self.position, end).is_ok()
    }

    pub fn start(&mut self) -> Marker {
        let position = u32::try_from(self.events.len())
            .expect("parser event position exceeds the supported file size");
        self.events.push(Event::Tombstone);
        self.active_markers.borrow_mut().push(position);
        Marker {
            position,
            owner: self.marker_owner.clone(),
            active_markers: self.active_markers.clone(),
            failures: self.marker_failures.clone(),
            forward_child: None,
            completed: false,
        }
    }

    pub fn demote_to_trivia(&mut self) {
        self.events.push(Event::DemoteToTrivia);
    }

    pub fn emit_error(&mut self, diagnostic: Diagnostic) {
        self.events.push(Event::Error {
            diag: Box::new(diagnostic),
        });
    }

    fn emit_expected_token(&mut self) -> Result<bool, ParserError> {
        self.emit_expected_diagnostic(Code::ExpectedToken)
    }

    pub(super) fn emit_expected_expression(&mut self) -> Result<bool, ParserError> {
        self.emit_expected_diagnostic(Code::ExpectedExpression)
    }

    pub(super) fn add_expected_expression_note(
        &mut self,
        message: &str,
    ) -> Result<(), ParserError> {
        if self.reported_expected_at != Some(self.position) {
            return Err(ParserError::InvalidMarkerEvent {
                position: u32::try_from(self.position).unwrap_or(u32::MAX),
            });
        }
        let event_index = self
            .reported_expected_event
            .ok_or(ParserError::InvalidMarkerEvent {
                position: u32::try_from(self.position).unwrap_or(u32::MAX),
            })?;
        let Some(Event::Error { diag }) = self.events.get_mut(event_index) else {
            return Err(ParserError::InvalidMarkerEvent {
                position: u32::try_from(self.position).unwrap_or(u32::MAX),
            });
        };
        if diag.code != Code::ExpectedExpression {
            return Err(ParserError::InvalidMarkerEvent {
                position: u32::try_from(self.position).unwrap_or(u32::MAX),
            });
        }
        diag.push_child(sai_diagnostics::Child {
            kind: sai_diagnostics::ChildKind::Note,
            message: message.to_owned(),
            location: None,
        })
        .map_err(|_| ParserError::InvalidDiagnosticSpan)
    }

    fn emit_expected_diagnostic(&mut self, code: Code) -> Result<bool, ParserError> {
        if self.reported_expected_at == Some(self.position) {
            let event_index =
                self.reported_expected_event
                    .ok_or(ParserError::InvalidMarkerEvent {
                        position: u32::try_from(self.position).unwrap_or(u32::MAX),
                    })?;
            let message = self.expected.message();
            let Some(Event::Error { diag }) = self.events.get_mut(event_index) else {
                return Err(ParserError::InvalidMarkerEvent {
                    position: u32::try_from(self.position).unwrap_or(u32::MAX),
                });
            };
            if !matches!(
                (diag.code, code),
                (Code::ExpectedExpression, Code::ExpectedToken)
                    | (Code::ExpectedToken, Code::ExpectedExpression)
            ) {
                diag.code = code;
            } else {
                diag.code = Code::ExpectedExpression;
            }
            diag.message.clone_from(&message);
            diag.primary_label = message;
            return Ok(false);
        }
        let Some(context) = &self.diagnostic_source else {
            return Err(ParserError::MissingDiagnosticSource);
        };
        let range = self.current_token().map_or_else(
            || {
                TextSize::from_usize(self.source.len())
                    .map(TextRange::empty)
                    .ok_or(ParserError::InvalidDiagnosticSpan)
            },
            |token| Ok(token.range),
        )?;
        let location = Location::from_decoded_source(
            context.source_ref.clone(),
            Span::new(context.file.id(), range),
            context.file,
        )
        .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let message = self.expected.message();
        let mut diagnostic = Diagnostic::new(code, Severity::Error, &message, Some(location));
        diagnostic.primary_label = message;
        let event_index = self.events.len();
        self.emit_error(diagnostic);
        self.reported_expected_at = Some(self.position);
        self.reported_expected_event = Some(event_index);
        Ok(true)
    }

    fn emit_missing_error_node(&mut self) -> Result<(), ParserError> {
        self.start().complete(self, SyntaxKind::ERROR).map(|_| ())
    }

    pub fn into_events(self) -> Result<Vec<Event>, ParserError> {
        let failure = self.marker_failures.borrow().first().cloned();
        if let Some(failure) = failure {
            return Err(failure);
        }
        if let Some(position) = self.active_markers.borrow().first().copied() {
            return Err(ParserError::UnfinishedMarker { position });
        }
        if self.position != self.tokens.len() {
            return Err(ParserError::UnconsumedTokens {
                position: self.position,
            });
        }
        Ok(self.events)
    }

    fn complete_marker(
        &mut self,
        marker: &mut Marker,
        kind: SyntaxKind,
    ) -> Result<CompletedMarker, ParserError> {
        if !Rc::ptr_eq(&marker.owner, &self.marker_owner) {
            marker
                .failures
                .borrow_mut()
                .push(ParserError::ForeignMarker);
            return Err(ParserError::ForeignMarker);
        }
        if self.active_markers.borrow().last() != Some(&marker.position) {
            let error = ParserError::InvalidMarkerNesting {
                position: marker.position,
            };
            self.marker_failures.borrow_mut().push(error.clone());
            return Err(error);
        }
        if !kind.is_node() {
            let error = ParserError::InvalidNodeKind {
                position: marker.position,
                kind,
            };
            self.marker_failures.borrow_mut().push(error.clone());
            *self
                .events
                .get_mut(marker.position as usize)
                .expect("parser marker position is valid") = Event::Tombstone;
            remove_active_marker(&self.active_markers, marker.position);
            return Err(error);
        }
        let Some(event) = self.events.get_mut(marker.position as usize) else {
            let error = ParserError::InvalidMarkerEvent {
                position: marker.position,
            };
            self.marker_failures.borrow_mut().push(error.clone());
            remove_active_marker(&self.active_markers, marker.position);
            return Err(error);
        };
        if !matches!(event, Event::Tombstone) {
            let error = ParserError::InvalidMarkerEvent {
                position: marker.position,
            };
            self.marker_failures.borrow_mut().push(error.clone());
            remove_active_marker(&self.active_markers, marker.position);
            return Err(error);
        }
        *event = Event::Start {
            kind,
            forward_parent: None,
        };
        self.events.push(Event::Finish);
        remove_active_marker(&self.active_markers, marker.position);
        marker.completed = true;
        Ok(CompletedMarker {
            position: marker.position,
            kind,
            owner: self.marker_owner.clone(),
        })
    }

    fn abandon_marker(&mut self, marker: &mut Marker) -> Result<(), ParserError> {
        if !Rc::ptr_eq(&marker.owner, &self.marker_owner) {
            marker
                .failures
                .borrow_mut()
                .push(ParserError::ForeignMarker);
            return Err(ParserError::ForeignMarker);
        }
        if self.active_markers.borrow().last() != Some(&marker.position) {
            let error = ParserError::InvalidMarkerNesting {
                position: marker.position,
            };
            self.marker_failures.borrow_mut().push(error.clone());
            return Err(error);
        }
        let Some(Event::Tombstone) = self.events.get(marker.position as usize) else {
            let error = ParserError::InvalidMarkerEvent {
                position: marker.position,
            };
            self.marker_failures.borrow_mut().push(error.clone());
            remove_active_marker(&self.active_markers, marker.position);
            return Err(error);
        };
        if let Some(child_position) = marker.forward_child {
            let expected_distance = marker.position - child_position;
            let Some(Event::Start { forward_parent, .. }) =
                self.events.get_mut(child_position as usize)
            else {
                let error = ParserError::InvalidMarkerEvent {
                    position: child_position,
                };
                self.marker_failures.borrow_mut().push(error.clone());
                remove_active_marker(&self.active_markers, marker.position);
                return Err(error);
            };
            if *forward_parent != Some(expected_distance) {
                let error = ParserError::InvalidMarkerEvent {
                    position: child_position,
                };
                self.marker_failures.borrow_mut().push(error.clone());
                remove_active_marker(&self.active_markers, marker.position);
                return Err(error);
            }
            *forward_parent = None;
        }
        remove_active_marker(&self.active_markers, marker.position);
        Ok(())
    }
}

fn raw_token_count(kind: SyntaxKind) -> Option<u8> {
    match kind {
        SyntaxKind::SHR
        | SyntaxKind::GE
        | SyntaxKind::COLON2
        | SyntaxKind::AMP2
        | SyntaxKind::PIPE2 => Some(2),
        SyntaxKind::SHR_EQ => Some(3),
        _ if kind.is_token() => Some(1),
        _ => None,
    }
}

fn remove_active_marker(active_markers: &RefCell<Vec<u32>>, position: u32) {
    active_markers
        .borrow_mut()
        .retain(|active| *active != position);
}

impl Marker {
    pub fn complete(
        mut self,
        parser: &mut Parser<'_, '_>,
        kind: SyntaxKind,
    ) -> Result<CompletedMarker, ParserError> {
        let result = parser.complete_marker(&mut self, kind);
        self.completed = true;
        result
    }

    pub fn abandon(mut self, parser: &mut Parser<'_, '_>) -> Result<(), ParserError> {
        let result = parser.abandon_marker(&mut self);
        self.completed = true;
        result
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if !self.completed {
            remove_active_marker(&self.active_markers, self.position);
            self.failures
                .borrow_mut()
                .push(ParserError::UnfinishedMarker {
                    position: self.position,
                });
        }
        debug_assert!(
            self.completed,
            "parser marker must be completed or abandoned"
        );
    }
}

impl CompletedMarker {
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    /// Starts a parent at the current event position and links this node to it.
    pub fn precede(self, parser: &mut Parser<'_, '_>) -> Marker {
        assert!(
            Rc::ptr_eq(&self.owner, &parser.marker_owner),
            "completed parser marker belongs to a different parser"
        );
        let position = u32::try_from(parser.events.len())
            .expect("parser event position exceeds the supported file size");
        let distance = position
            .checked_sub(self.position)
            .filter(|distance| *distance > 0)
            .expect("a parent marker must follow its completed child");
        let event = parser
            .events
            .get(self.position as usize)
            .expect("completed parser marker position is valid");
        let Event::Start { forward_parent, .. } = event else {
            panic!("completed parser marker no longer points to a start event");
        };
        assert!(
            forward_parent.is_none(),
            "completed parser marker already has a forward parent"
        );
        let marker = parser.start();
        debug_assert_eq!(marker.position, position);
        let event = parser
            .events
            .get_mut(self.position as usize)
            .expect("completed parser marker position is valid");
        let Event::Start { forward_parent, .. } = event else {
            unreachable!("completed marker event was validated before starting its parent");
        };
        *forward_parent = Some(distance);
        let mut marker = marker;
        marker.forward_child = Some(self.position);
        marker
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sai_vfs::{FileId, SnapshotFile, SourceFile, WorkspaceSnapshot};

    use super::*;
    use crate::{GreenElement, NodeCache, build_green_tree, lex};

    fn lex_text(text: &str) -> crate::LexOutput {
        lex_file(text).1
    }

    fn lex_file(text: &str) -> (SnapshotFile, crate::LexOutput) {
        lex_file_with_bom(text, false)
    }

    fn lex_file_with_bom(text: &str, had_bom: bool) -> (SnapshotFile, crate::LexOutput) {
        let file_id = FileId::new(1);
        let source = Arc::new(SourceFile::new(file_id, text, had_bom).expect("source"));
        let snapshot_file = SnapshotFile::new(
            source,
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        );
        let snapshot = WorkspaceSnapshot::new([snapshot_file.clone()]).expect("snapshot");
        (snapshot_file, lex(&snapshot, file_id).expect("lex"))
    }

    fn parse_file(text: &str) -> crate::TreeBuildOutput {
        let (snapshot_file, lexed) = lex_file(text);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        parser
            .parse_source_file()
            .expect("supported source grammar");
        let events = parser.into_events().expect("complete source event stream");
        let mut cache = NodeCache::new();
        build_green_tree(text, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("source tree builds")
    }

    fn assert_doc_comment_is_demoted(source: &str, spelling: &str, kind: SyntaxKind) {
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root.clone());

        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::MisplacedDocComment);
        assert_eq!(
            output.errors[0].severity,
            sai_diagnostics::Severity::Warning
        );
        let range = output.errors[0]
            .primary()
            .expect("misplaced-doc diagnostic has a primary location")
            .span
            .range;
        let start = source.find(spelling).expect("doc comment is in source");
        assert_eq!(range.start().to_usize(), start);
        assert_eq!(range.end().to_usize(), start + spelling.len());
        assert!(
            !root.descendants().any(|node| {
                node.children_with_tokens().any(|element| {
                    matches!(
                        element,
                        crate::SyntaxElement::Token(token) if token.kind() == kind
                    )
                })
            }),
            "misplaced doc comments should be trivia, not syntax tokens"
        );
        assert!(
            !root
                .descendants()
                .any(|node| node.kind() == SyntaxKind::DOCS)
        );
        assert_eq!(
            output.root.text_len(),
            TextSize::from_usize(source.len()).expect("source fits TextSize")
        );
    }

    #[test]
    fn token_cursor_keeps_newlines_visible_and_matches_contextual_names() {
        let source = "effects\n";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");

        assert_eq!(parser.current(), SyntaxKind::LIDENT);
        assert_eq!(parser.current_text(), "effects");
        assert!(parser.at_contextual("effects"));
        assert_eq!(parser.nth(1), SyntaxKind::NEWLINE);
        assert_eq!(parser.nth(2), SyntaxKind::EOF);
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        assert!(parser.eat_nl());
        assert_eq!(parser.current(), SyntaxKind::EOF);
        assert_eq!(parser.position(), 2);
    }

    #[test]
    fn source_file_parses_empty_source_and_omits_an_empty_module_header() {
        let output = parse_file("");
        assert_eq!(output.root.kind(), SyntaxKind::SOURCE_FILE);
        assert_eq!(output.root.children().len(), 1);
        assert!(matches!(
            output.root.children().first(),
            Some(GreenElement::Token(token)) if token.kind() == SyntaxKind::EOF
        ));
        assert!(output.errors.is_empty());
    }

    #[test]
    fn source_file_parses_module_docs_and_use_items() {
        let source = "//! Module docs\nunsafe module\n/// imports\npub use sai:http/client as http.{Request, Response as Reply}\n";
        let output = parse_file(source);

        let children = output.root.children();
        let GreenElement::Node(module_header) = &children[0] else {
            panic!("expected a module header");
        };
        assert_eq!(module_header.kind(), SyntaxKind::MODULE_HEADER);
        assert!(module_header.children().iter().any(
            |child| matches!(child, GreenElement::Node(node) if node.kind() == SyntaxKind::DOCS)
        ));

        let use_decl = children
            .iter()
            .find_map(|child| match child {
                GreenElement::Node(node) if node.kind() == SyntaxKind::USE_DECL => Some(node),
                _ => None,
            })
            .expect("use declaration");
        assert!(use_decl.children().iter().any(
            |child| matches!(child, GreenElement::Node(node) if node.kind() == SyntaxKind::VISIBILITY)
        ));
        assert!(use_decl.children().iter().any(
            |child| matches!(child, GreenElement::Node(node) if node.kind() == SyntaxKind::DOCS)
        ));
        let use_items = use_decl
            .children()
            .iter()
            .find_map(|child| match child {
                GreenElement::Node(node) if node.kind() == SyntaxKind::USE_ITEMS => Some(node),
                _ => None,
            })
            .expect("selective use items");
        assert_eq!(
            use_items
                .children()
                .iter()
                .filter(|child| {
                    matches!(child, GreenElement::Node(node) if node.kind() == SyntaxKind::USE_ITEM)
                })
                .count(),
            2
        );
        assert!(output.errors.is_empty());
    }

    #[test]
    fn source_file_parses_aliases_generic_aliases_and_refinements() {
        let source = concat!(
            "/// Public identifier type.\n",
            "pub type UserId = U32\n",
            "type Pair<T> = (T, T)\n",
            "type Positive = I32\n",
            "where self > 0\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ALIAS_DEF)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::GENERIC_PARAM_LIST)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::REFINEMENT_CLAUSE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_constant_declarations_and_initializers() {
        let source = concat!(
            "/// The answer.\n",
            "@const_attr\n",
            "pub const ANSWER: I32 = 40 + 2\n",
            "const MESSAGE: Str = \"ready\"\n",
            "const MULTILINE: I32 =\n",
            "40 + 2\n",
            "const CLOSURE: fn() -> I32 =\n",
            "fn() { 1 }\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            4
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BIN_EXPR)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn exported_constants_are_diagnosed_but_kept_in_the_tree() {
        let source = "export const VALUE: I32 = 1\npub const PUBLIC: I32 = 2\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::InvalidExportTarget);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_constant_type_preserves_a_following_function() {
        let source = "const VALUE:\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            !output.errors.is_empty(),
            "expected a missing type diagnostic"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_constant_initializer_preserves_a_following_function() {
        let source = "const VALUE: I32 =\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn constant_declaration_in_block_is_recovered_as_an_item() {
        let source = "fn outer() {\nconst LOCAL: I32 = 1\nlet later = LOCAL\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ItemInBlock);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_invariant_predicates_with_braced_separators() {
        let source = concat!(
            "invariant Account\n",
            "{\n",
            "self.balance >= 0,\n",
            "self.enabled\n",
            "self.name != \"\"\n",
            "}\n",
            "fn local() { let invariant = 1 }\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INVARIANT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INVARIANT_EXPR_LIST)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BIN_EXPR)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn invariant_without_a_close_preserves_the_following_function() {
        let source = "invariant Account {\nself.balance >= 0\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INVARIANT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn line_start_closure_is_an_invariant_expression_not_an_item_boundary() {
        let source = "invariant Closure {\nfn() { true }\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INVARIANT_EXPR_LIST)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn invariant_visibility_and_block_placement_are_diagnosed() {
        let source = concat!(
            "pub invariant Account { true }\n",
            "export invariant Other { false }\n",
            "fn outer() {\n",
            "invariant Local { true }\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 3, "{:?}", output.errors);
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidExportTarget)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ItemInBlock)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INVARIANT_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn invariant_requires_at_least_one_predicate() {
        let output = parse_file("invariant Empty {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedExpression);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INVARIANT_EXPR_LIST)
                .count(),
            1
        );
    }

    #[test]
    fn source_file_parses_interfaces_and_resource_members() {
        let source = concat!(
            "pub interface storage\n",
            "{\n",
            "/// A storage entry.\n",
            "type Entry { pub key: Str }\n",
            "type Error = Str\n",
            "enum Status { Ready, Failed }\n",
            "resource Handle\n",
            "resource File {\n",
            "/// Close the file.\n",
            "fn close(self: borrow Self)\n",
            "fn size(self: borrow Self) -> U64\n",
            "}\n",
            "fn open(path: Str) -> File\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INTERFACE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IFACE_RESOURCE)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ITEM_LIST)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn interface_function_bodies_are_reported_and_wrapped_as_errors() {
        let source = concat!(
            "interface storage {\n",
            "fn open() {}\n",
            "resource File {\n",
            "fn close(self: borrow Self) {}\n",
            "}\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::InterfaceFnBody),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn interface_function_heads_reject_generics_clauses_and_async() {
        let source = concat!(
            "interface storage {\n",
            "fn generic<T>()\n",
            "fn precondition() requires true\n",
            "fn effectful() effects { alloc }\n",
            "async fn later()\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let codes = output
            .errors
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert_eq!(
            codes,
            [
                Code::GenericInterfaceItem,
                Code::InterfaceFnClause,
                Code::EffectsOnInterfaceFn,
                Code::FeatureNotImplemented,
            ],
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            4
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_interface_close_preserves_a_following_constant() {
        let source = "interface storage {\nfn open()\nconst READY: Bool = true\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::INTERFACE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_interface_close_keeps_valid_members_inside_the_interface() {
        let source = concat!(
            "interface storage {\n",
            "fn open()\n",
            "type Later = Str\n",
            "enum Status { Ready }\n",
            "resource File\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let interface = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::INTERFACE_DECL)
            .expect("interface declaration");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            interface
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(
            interface
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            1
        );
        assert_eq!(
            interface
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            1
        );
        assert_eq!(
            interface
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::IFACE_RESOURCE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_world_references_and_function_items() {
        let source = concat!(
            "pub world client {\n",
            "/// External handler.\n",
            "import wasi:http/outgoing-handler\n",
            "export storage\n",
            "include common.base\n",
            "import fn clock() -> U64\n",
            "export fn run(input: Str) -> U64\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_IMPORT)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_EXPORT)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_INCLUDE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_REF)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::MOD_PATH)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn world_imports_and_exports_must_name_an_interface() {
        let source = "world client {\nimport wasi:http\nexport wasi:cli\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::WorldImportNeedsInterface),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_REF)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn world_function_bodies_are_reported_and_wrapped_as_errors() {
        let source = "world client {\nimport fn clock() {}\nexport fn run() {}\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::InterfaceFnBody),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn world_function_heads_reject_generics_and_clauses() {
        let source = concat!(
            "world client {\n",
            "import unsafe fn generic<T>()\n",
            "export fn constrained() where T: Copy requires true\n",
            "export fn borrowed() borrows { value }\n",
            "import fn postcondition() ensures true\n",
            "import fn effectful() effects { alloc }\n",
            "export async fn asynchronous()\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let codes = output
            .errors
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert_eq!(
            codes,
            [
                Code::InterfaceFnClause,
                Code::GenericInterfaceItem,
                Code::GenericInterfaceItem,
                Code::InterfaceFnClause,
                Code::InterfaceFnClause,
                Code::InterfaceFnClause,
                Code::EffectsOnInterfaceFn,
                Code::FeatureNotImplemented,
            ],
            "{:?}",
            output.errors
        );
        let async_diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::FeatureNotImplemented)
            .expect("async function diagnostic");
        assert_eq!(
            async_diagnostic.data.get("stage"),
            Some(&sai_diagnostics::Value::Integer(5))
        );
        assert_eq!(
            async_diagnostic.data.get("feature"),
            Some(&sai_diagnostics::Value::String("async-fn".to_owned()))
        );
        let async_span = async_diagnostic
            .primary()
            .expect("async diagnostic location")
            .span
            .range;
        assert_eq!(
            &source[async_span.start().to_usize()..async_span.end().to_usize()],
            "async"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            6
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_world_reference_is_consumed_without_recovery_cascades() {
        let source = "world client {\nimport Invalid\ninclude valid.world\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let world_refs = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::WORLD_REF)
            .collect::<Vec<_>>();

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(world_refs.len(), 2);
        assert_eq!(
            world_refs[0]
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            2
        );
        assert_eq!(world_refs[0].first_token().unwrap().text(), "Invalid");
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_INCLUDE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_world_close_preserves_a_following_constant() {
        let source = "world client {\nimport wasi:io/streams\nconst READY: Bool = true\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_world_close_keeps_an_exported_function_as_a_world_item() {
        let source = "world client {\ninclude wasi:http/proxy\nexport fn run()\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let world = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::WORLD_DECL)
            .expect("world declaration");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            world
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn world_item_attributes_are_rejected_but_preserved() {
        let source = "world client {\n@custom\nimport wasi:http/ping\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::InvalidAttributeTarget);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_IMPORT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn world_keywords_remain_identifiers_in_function_bodies() {
        let source = concat!(
            "fn local() {\n",
            "let world = 1\n",
            "let import = world\n",
            "let include = import\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WORLD_DECL)
                .count(),
            0
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            3
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_wasm_extern_blocks_and_decodes_abi_and_module_strings() {
        let source = concat!(
            "extern \"\\u{77}asm\" module r#\"host\"# {\n",
            "/// Host logging function.\n",
            "@import_name(\"host_log\")\n",
            "fn log(message: Str)\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXTERN_MODULE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn extern_blocks_report_unknown_abi_and_invalid_abi_requirements() {
        let source = concat!(
            "extern \"native\" {\n}\n",
            "extern \"wasm\" {\n}\n",
            "extern \"C\" {\n}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                Code::UnknownAbi,
                Code::InvalidExternBlock,
                Code::InvalidExternBlock,
            ],
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
                .count(),
            3
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn unsafe_c_extern_blocks_report_the_unimplemented_stage() {
        let source = "unsafe extern \"C\" {\nfn puts(value: Ptr<U8>)\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::FeatureNotImplemented)
            .expect("native C extern feature diagnostic");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(
            diagnostic.data.get("stage"),
            Some(&sai_diagnostics::Value::Integer(7))
        );
        assert_eq!(
            diagnostic.data.get("feature"),
            Some(&sai_diagnostics::Value::String("extern-c".to_owned()))
        );
        let span = diagnostic
            .primary()
            .expect("extern feature diagnostic location")
            .span
            .range;
        assert_eq!(
            &source[span.start().to_usize()..span.end().to_usize()],
            "extern"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn extern_function_bodies_are_reported_and_wrapped_as_errors() {
        let source = "extern \"wasm\" module \"env\" {\nfn invalid() {}\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::UnexpectedFnBody);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn invalid_extern_item_recovers_through_nested_braces_without_cascading() {
        let source = concat!(
            "extern \"wasm\" module \"env\" {\n",
            "const INVALID: Bool = { true }\n",
            "fn after()\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let extern_block = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
            .expect("extern block");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());

        let source = concat!(
            "extern \"wasm\" module \"env\" {\n",
            "const INVALID: Bool =\n",
            "fn() { true }\n",
            "fn after()\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let extern_block = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
            .expect("extern block");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());

        let source = concat!(
            "extern \"wasm\" module \"env\" {\n",
            "const INVALID: Bool =\n",
            "fn after()\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let extern_block = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
            .expect("extern block");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn closed_extern_block_keeps_each_invalid_member_inside_the_block() {
        let source = concat!(
            "extern \"wasm\" module \"env\" {\n",
            "const FIRST: Bool = true\n",
            "const SECOND: Bool = false\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let extern_block = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
            .expect("extern block");

        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "{:?}",
            output.errors
        );
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            0
        );
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            4
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn invalid_extern_item_recovery_preserves_a_top_level_item_after_a_missing_close() {
        let source = concat!(
            "extern \"wasm\" module \"env\" {\n",
            "const INVALID: Bool =\n",
            "const READY: Bool = true\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::ExpectedToken, Code::ExpectedToken],
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn async_extern_function_reports_its_stage_and_visibility_markers_are_rejected() {
        let source = concat!(
            "extern \"wasm\" module \"env\" {\n",
            "async fn asynchronous()\n",
            "pub fn visible()\n",
            "}\n",
            "pub extern \"wasm\" module \"outside\" {\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let diagnostics = output
            .errors
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert_eq!(
            diagnostics,
            [
                Code::FeatureNotImplemented,
                Code::ExpectedToken,
                Code::ExpectedToken,
            ],
            "{:?}",
            output.errors
        );
        let async_diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::FeatureNotImplemented)
            .expect("async extern diagnostic");
        assert_eq!(
            async_diagnostic.data.get("stage"),
            Some(&sai_diagnostics::Value::Integer(5))
        );
        assert_eq!(
            async_diagnostic.data.get("feature"),
            Some(&sai_diagnostics::Value::String("async-fn".to_owned()))
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn extern_block_recovery_preserves_following_items_and_keeps_function_members() {
        let missing_close =
            "extern \"wasm\" module \"env\" {\nfn open()\nconst READY: Bool = true\n";
        let output = parse_file(missing_close);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), missing_close.len());

        let ambiguous = "extern \"wasm\" module \"env\" {\nfn first()\nfn second()\n}\n";
        let output = parse_file(ambiguous);
        let root = crate::SyntaxNode::new_root(output.root);
        let extern_block = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
            .expect("extern block");

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            extern_block
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), ambiguous.len());
    }

    #[test]
    fn source_file_parses_test_and_property_declarations() {
        let source = concat!(
            "/// Basic assertion.\n",
            "test \"basic\" {\n",
            "assert true\n",
            "}\n",
            "@target(\"wasm32-wasi\")\n",
            "property \"generated\" (value: U32, var bound: U32, _: Bool,) {\n",
            "assert value < bound\n",
            "}\n",
            "property \"no parameters\" () {}\n",
            "fn identifiers() {\n",
            "let test = 1\n",
            "let property = test\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TEST_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::PROPERTY_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::PARAM_LIST)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::PARAM)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn typed_ast_views_expose_test_property_and_parameter_roles() {
        use crate::ast::{AstNode, Type};

        let source = concat!(
            "test \"basic\" {}\n",
            "property \"generated\" (value: U32, var bound: Bool, _: U32) {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let file = crate::ast::SourceFile::cast(root.clone()).expect("source file view");
        let test = file.test_decls().next().expect("test declaration");
        let property = file.property_decls().next().expect("property declaration");
        let params = property.params().expect("property parameters");
        let params = params.params().collect::<Vec<_>>();

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(test.name().expect("test name").text(), "\"basic\"");
        assert_eq!(
            test.body().expect("test body").syntax().kind(),
            SyntaxKind::BLOCK
        );
        assert_eq!(
            property.name().expect("property name").text(),
            "\"generated\""
        );
        assert_eq!(params.len(), 3);
        assert_eq!(
            params[0]
                .name()
                .and_then(|name| name.token())
                .expect("first parameter name")
                .text(),
            "value"
        );
        assert!(!params[0].is_mutable());
        assert!(matches!(params[0].ty(), Some(Type::PathType(_))));
        assert_eq!(
            params[1]
                .name()
                .and_then(|name| name.token())
                .expect("mutable parameter name")
                .text(),
            "bound"
        );
        assert!(params[1].is_mutable());
        assert!(matches!(params[1].ty(), Some(Type::PathType(_))));
        assert_eq!(
            params[2]
                .name()
                .and_then(|name| name.token())
                .expect("unnamed parameter token")
                .text(),
            "_"
        );
        assert!(!Type::can_cast(SyntaxKind::BLOCK));
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn generated_ast_node_inventory_contains_unique_node_kinds() {
        let mut kinds = std::collections::HashSet::new();
        for &kind in crate::ast::AST_NODE_KINDS {
            assert!(kind.is_node(), "{kind:?} is not a syntax node");
            assert!(kinds.insert(kind), "duplicate generated node kind {kind:?}");
        }
    }

    #[test]
    fn generated_binary_expression_roles_select_distinct_same_type_children() {
        use crate::ast::{AstNode, BinExpr};

        let source = "fn compare() { left + right }\n";
        let output = parse_file(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let root = crate::SyntaxNode::new_root(output.root);
        let binary = root
            .descendants()
            .find_map(BinExpr::cast)
            .expect("binary expression");
        let lhs = binary.lhs().expect("left operand");
        let rhs = binary.rhs().expect("right operand");

        let lhs_range = lhs.syntax().text_range();
        let rhs_range = rhs.syntax().text_range();
        assert_eq!(
            &source[lhs_range.start().to_usize()..lhs_range.end().to_usize()],
            "left"
        );
        assert_eq!(
            &source[rhs_range.start().to_usize()..rhs_range.end().to_usize()],
            "right"
        );
        assert_ne!(lhs_range, rhs_range);
    }

    #[test]
    fn generated_function_and_statement_roles_follow_parsed_children() {
        use crate::ast::{AstNode, Item, Stmt};

        let source = concat!(
            "fn compute(seed: I32) -> I32 {\n",
            "let total: I32 = seed + 1\n",
            "let maybe = seed else { return 0 }\n",
            "var current: I32 = total\n",
            "current += 2\n",
            "assert current > 0, \"positive\"\n",
            "return current\n",
            "}\n",
            "test \"smoke\" {}\n",
            "property \"simple\" (input: I32) {}\n",
        );
        let output = parse_file(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);

        let root = crate::SyntaxNode::new_root(output.root);
        let file = crate::ast::SourceFile::cast(root).expect("source file");
        assert_eq!(file.items().count(), 3);
        let function = match file.items().next().expect("one item") {
            Item::Function(function) => function,
            item => panic!("expected function, got {:?}", item.syntax().kind()),
        };
        let name = function.name().expect("function name");
        assert_eq!(name.token().expect("name token").text(), "compute");
        assert_eq!(
            function.params().expect("parameter list").params().count(),
            1
        );
        assert!(
            function
                .ret_type()
                .and_then(|ret_type| ret_type.ty())
                .is_some()
        );

        let body = function.body().expect("function body");
        let statements = body.statements().collect::<Vec<_>>();
        assert_eq!(statements.len(), 6);
        assert!(matches!(statements[0], Stmt::Let(_)));
        assert!(matches!(statements[1], Stmt::Let(_)));
        assert!(matches!(statements[2], Stmt::Var(_)));
        assert!(matches!(statements[3], Stmt::Assign(_)));
        assert!(matches!(statements[4], Stmt::Assert(_)));
        assert!(matches!(statements[5], Stmt::Expr(_)));

        let Stmt::Let(let_stmt) = &statements[0] else {
            unreachable!();
        };
        assert!(let_stmt.pattern().is_some());
        assert!(let_stmt.ty().is_some());
        assert!(let_stmt.initializer().is_some());

        let Stmt::Let(let_else_stmt) = &statements[1] else {
            unreachable!();
        };
        assert!(let_else_stmt.else_clause().is_some());

        let Stmt::Var(var_stmt) = &statements[2] else {
            unreachable!();
        };
        assert!(var_stmt.name().is_some());
        assert!(var_stmt.ty().is_some());
        assert!(var_stmt.initializer().is_some());

        let Stmt::Assign(assign_stmt) = &statements[3] else {
            unreachable!();
        };
        let lhs = assign_stmt.lhs().expect("assignment left side");
        let rhs = assign_stmt.rhs().expect("assignment right side");
        assert_eq!(
            &source[lhs.syntax().text_range().start().to_usize()
                ..lhs.syntax().text_range().end().to_usize()],
            "current"
        );
        assert_eq!(
            &source[rhs.syntax().text_range().start().to_usize()
                ..rhs.syntax().text_range().end().to_usize()],
            "2"
        );

        let Stmt::Assert(assert_stmt) = &statements[4] else {
            unreachable!();
        };
        assert!(assert_stmt.condition().is_some());
        assert!(assert_stmt.message().is_some());

        let Stmt::Expr(expr_stmt) = &statements[5] else {
            unreachable!();
        };
        assert!(expr_stmt.expression().is_some());

        let malformed = parse_file("fn broken() {\nassert , \"message\"\n}\n");
        assert!(!malformed.errors.is_empty());
        let malformed_root = crate::SyntaxNode::new_root(malformed.root);
        let malformed_assert = malformed_root
            .descendants()
            .find_map(crate::ast::AssertStmt::cast)
            .expect("recovered assert statement");
        assert!(malformed_assert.condition().is_none());
        let message = malformed_assert.message().expect("message expression");
        let range = message.syntax().text_range();
        assert_eq!(
            &"fn broken() {\nassert , \"message\"\n}\n"
                [range.start().to_usize()..range.end().to_usize()],
            "\"message\""
        );
    }

    #[test]
    fn generated_control_flow_roles_follow_parsed_children() {
        use crate::ast::{
            AstNode, AsyncBlockExpr, BreakExpr, ClosureExpr, ForExpr, IfExpr, LoopExpr, MatchExpr,
            PrefixExpr, RegionExpr, ReturnExpr, UnsafeBlockExpr, WhileExpr,
        };

        let source = concat!(
            "fn controls(flag: Bool, values: I32) {\n",
            "let plain = if flag { 1 } else { 2 }\n",
            "let chain = if flag { 1 } else if flag { 2 } else { 3 }\n",
            "let matched_if = if let Some(item) = values { item } else { 0 }\n",
            "let matched = match values {\n",
            "Some(item) if flag => item,\n",
            "_ => 0,\n",
            "}\n",
            "'outer:\n",
            "loop { break 'outer 1 }\n",
            "while let Some(item) = values { break }\n",
            "for item in values { continue }\n",
            "let callback = fn(x: I32) -> I32 { x }\n",
            "let plain_callback = fn(y) { y }\n",
            "let task = async { values }\n",
            "let guarded = unsafe { values }\n",
            "let scoped = region arena { values }\n",
            "let block = { values }\n",
            "let negative = -values\n",
            "return values\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let root = crate::SyntaxNode::new_root(output.root);

        let ifs = root
            .descendants()
            .filter_map(IfExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(ifs.len(), 4);
        let plain_else = ifs[0].else_branch().expect("plain else branch");
        assert!(
            ifs[0]
                .condition()
                .and_then(|condition| condition.expression())
                .is_some()
        );
        assert!(ifs[0].then_branch().is_some());
        assert!(plain_else.block().is_some());
        assert!(plain_else.else_if().is_none());

        let chained_else = ifs[1].else_branch().expect("chained else branch");
        assert!(chained_else.else_if().is_some());
        let if_let_condition = ifs
            .iter()
            .find_map(|if_expr| if_expr.condition()?.let_condition())
            .expect("if-let condition");
        assert!(if_let_condition.pattern().is_some());
        assert!(if_let_condition.initializer().is_some());

        let matched = root
            .descendants()
            .find_map(MatchExpr::cast)
            .expect("match expression");
        assert!(matched.scrutinee().is_some());
        let arms = matched
            .arms()
            .expect("match arm list")
            .arms()
            .collect::<Vec<_>>();
        assert_eq!(arms.len(), 2);
        assert!(arms[0].pattern().is_some());
        let guard = arms[0].guard().expect("guarded arm");
        assert!(guard.expression().is_some());
        assert!(arms[0].expression().is_some());
        assert!(arms[1].guard().is_none());

        let loop_expr = root
            .descendants()
            .find_map(LoopExpr::cast)
            .expect("loop expression");
        assert!(loop_expr.label().is_some());
        assert!(loop_expr.body().is_some());

        let while_expr = root
            .descendants()
            .find_map(WhileExpr::cast)
            .expect("while expression");
        assert!(while_expr.label().is_none());
        let while_condition = while_expr.condition().expect("while condition");
        assert!(while_condition.let_condition().is_some());
        assert!(while_expr.body().is_some());

        let for_expr = root
            .descendants()
            .find_map(ForExpr::cast)
            .expect("for expression");
        assert!(for_expr.label().is_none());
        assert!(for_expr.pattern().is_some());
        assert!(for_expr.iterable().is_some());
        assert!(for_expr.body().is_some());

        let closures = root
            .descendants()
            .filter_map(ClosureExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(closures.len(), 2);
        let closure = &closures[0];
        let params = closure.params().expect("closure parameter list");
        let param = params.params().next().expect("closure parameter");
        assert!(param.name().is_some());
        assert!(param.ty().is_some());
        assert!(
            closure
                .ret_type()
                .and_then(|ret_type| ret_type.ty())
                .is_some()
        );
        assert!(closure.body().is_some());
        assert!(closures[1].ret_type().is_none());

        let async_block = root
            .descendants()
            .find_map(AsyncBlockExpr::cast)
            .expect("async block");
        assert!(async_block.body().is_some());
        let unsafe_block = root
            .descendants()
            .find_map(UnsafeBlockExpr::cast)
            .expect("unsafe block");
        assert!(unsafe_block.body().is_some());
        let region = root
            .descendants()
            .find_map(RegionExpr::cast)
            .expect("region expression");
        assert_eq!(
            region
                .name()
                .and_then(|name| name.token())
                .expect("region binder")
                .text(),
            "arena"
        );
        assert!(region.body().is_some());
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::BLOCK_EXPR)
        );
        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK_EXPR)
                .all(|node| crate::ast::BlockExpr::cast(node).unwrap().block().is_some())
        );

        assert!(
            root.descendants()
                .find_map(BreakExpr::cast)
                .and_then(|expression| expression.value())
                .is_some()
        );
        assert!(
            root.descendants()
                .find_map(ReturnExpr::cast)
                .and_then(|expression| expression.value())
                .is_some()
        );
        assert!(
            root.descendants()
                .find_map(PrefixExpr::cast)
                .and_then(|expression| expression.operand())
                .is_some()
        );
    }

    #[test]
    fn generated_postfix_collection_and_argument_roles_follow_parsed_children() {
        use crate::ast::{
            ArgList, ArrayExpr, AstNode, AwaitExpr, CallExpr, FStringInterp, FieldExpr, IndexExpr,
            ParenExpr, PathExpr, TryExpr, TupleExpr, TupleIndexExpr,
        };

        let source = concat!(
            "fn expressions(values: I32, future: I32, base: I32) {\n",
            "let generic = g<I32>\n",
            "let grouped = (values)\n",
            "let tuple = (values, 2,)\n",
            "let array = [1, values,]\n",
            "let call = invoke(1, 2, named: values, other: 3, ..base)\n",
            "let field = values.member\n",
            "let tuple_index = values.0\n",
            "let indexed = values[0]\n",
            "let propagated = values?\n",
            "let awaited = future.await\n",
            "let formatted = f\"{values:pad(3)}\"\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let root = crate::SyntaxNode::new_root(output.root);

        let generic_path = root
            .descendants()
            .filter_map(PathExpr::cast)
            .find(|path| path.generic_args().is_some())
            .expect("generic path");
        assert!(generic_path.name().is_some());

        let grouped = root
            .descendants()
            .find_map(ParenExpr::cast)
            .expect("parenthesized expression");
        assert!(grouped.expression().is_some());
        let tuple = root
            .descendants()
            .find_map(TupleExpr::cast)
            .expect("tuple expression");
        assert_eq!(tuple.elements().count(), 2);
        let array = root
            .descendants()
            .find_map(ArrayExpr::cast)
            .expect("array expression");
        assert_eq!(array.elements().count(), 2);

        let call = root
            .descendants()
            .find_map(CallExpr::cast)
            .expect("call expression");
        assert!(call.callee().is_some());
        let args = call.args().expect("call argument list");
        let positional_args = args.positional_args().collect::<Vec<_>>();
        assert_eq!(positional_args.len(), 2);
        let named_args = args.named_args().collect::<Vec<_>>();
        assert_eq!(named_args.len(), 2);
        assert_eq!(args.base_args().count(), 1);
        assert!(positional_args[0].expression().is_some());
        assert!(positional_args[1].expression().is_some());
        assert!(named_args.iter().all(|argument| argument.name().is_some()));
        assert!(
            named_args
                .iter()
                .all(|argument| argument.expression().is_some())
        );
        assert!(
            args.base_args()
                .next()
                .expect("base argument")
                .expression()
                .is_some()
        );

        let field = root
            .descendants()
            .find_map(FieldExpr::cast)
            .expect("field expression");
        assert!(field.base().is_some());
        assert!(field.name().is_some());
        let tuple_index = root
            .descendants()
            .find_map(TupleIndexExpr::cast)
            .expect("tuple-index expression");
        assert!(tuple_index.base().is_some());
        assert_eq!(tuple_index.index(), Some(0));
        let index = root
            .descendants()
            .find_map(IndexExpr::cast)
            .expect("index expression");
        assert_eq!(index.base().unwrap().syntax().kind(), SyntaxKind::PATH_EXPR);
        assert_eq!(index.index().unwrap().syntax().kind(), SyntaxKind::LITERAL);
        assert!(
            root.descendants()
                .find_map(TryExpr::cast)
                .and_then(|expression| expression.operand())
                .is_some()
        );
        assert!(
            root.descendants()
                .find_map(AwaitExpr::cast)
                .and_then(|expression| expression.operand())
                .is_some()
        );

        let interpolation = root
            .descendants()
            .find_map(FStringInterp::cast)
            .expect("format-string interpolation");
        assert!(interpolation.expression().is_some());
        assert!(interpolation.spec().is_some());
        let argument_lists = root
            .descendants()
            .filter_map(ArgList::cast)
            .collect::<Vec<_>>();
        assert_eq!(argument_lists.len(), 1);

        let missing_index = parse_file("fn broken() { let indexed = values[] }\n");
        assert!(!missing_index.errors.is_empty());
        let missing_index_root = crate::SyntaxNode::new_root(missing_index.root);
        let missing_index = missing_index_root
            .descendants()
            .find_map(IndexExpr::cast)
            .expect("recovered index expression");
        assert_eq!(
            missing_index.base().unwrap().syntax().kind(),
            SyntaxKind::PATH_EXPR
        );
        assert!(missing_index.index().is_none());

        let malformed_args = parse_file("fn broken() { call(, 1) }\n");
        assert!(!malformed_args.errors.is_empty());
        let malformed_root = crate::SyntaxNode::new_root(malformed_args.root);
        let recovered_args = malformed_root
            .descendants()
            .find_map(ArgList::cast)
            .expect("recovered call argument list");
        let recovered_positional = recovered_args.positional_args().collect::<Vec<_>>();
        assert_eq!(recovered_positional.len(), 1);
        assert!(recovered_positional[0].expression().is_some());

        let repeated_base_args =
            parse_file("fn broken() { Record(1, 2, first: 3, second: 4, ..base, ..other) }\n");
        assert_eq!(
            repeated_base_args
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BaseArgNotLast]
        );
        let repeated_base_root = crate::SyntaxNode::new_root(repeated_base_args.root);
        let repeated_args = repeated_base_root
            .descendants()
            .find_map(ArgList::cast)
            .expect("recovered record argument list");
        assert_eq!(repeated_args.positional_args().count(), 2);
        assert_eq!(repeated_args.named_args().count(), 2);
        assert_eq!(repeated_args.base_args().count(), 2);
    }

    #[test]
    fn typed_ast_views_cast_only_matching_nodes_and_tokens() {
        use crate::ast::{
            AstNode, AstToken, Block, BorrowType, FnPtrType, InferType, Name, OwnedType, Param,
            ParamList, ParenType, PathType, PropertyDecl, SelfType, SourceFile, StringLit,
            TestDecl, TupleType, Type,
        };

        let source = concat!(
            "test \"basic\" {}\n",
            "property \"types\" (\n",
            "path: U32,\n",
            "self_type: Self,\n",
            "borrowed: borrow U32,\n",
            "owned_value: owned U32,\n",
            "tuple: (U32, Bool),\n",
            "parenthesized: (U32),\n",
            "inferred: _,\n",
            "function: fn() -> U32,\n",
            ") {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        for kind in kinds {
            let node = root
                .descendants()
                .find(|node| node.kind() == kind)
                .expect("kind came from this tree");
            assert_eq!(
                SourceFile::can_cast(kind),
                SourceFile::cast(node.clone()).is_some()
            );
            assert_eq!(
                TestDecl::can_cast(kind),
                TestDecl::cast(node.clone()).is_some()
            );
            assert_eq!(
                PropertyDecl::can_cast(kind),
                PropertyDecl::cast(node.clone()).is_some()
            );
            assert_eq!(Block::can_cast(kind), Block::cast(node.clone()).is_some());
            assert_eq!(
                ParamList::can_cast(kind),
                ParamList::cast(node.clone()).is_some()
            );
            assert_eq!(Param::can_cast(kind), Param::cast(node.clone()).is_some());
            assert_eq!(Name::can_cast(kind), Name::cast(node.clone()).is_some());
            assert_eq!(Type::can_cast(kind), Type::cast(node.clone()).is_some());
            assert_eq!(
                PathType::can_cast(kind),
                PathType::cast(node.clone()).is_some()
            );
            assert_eq!(
                SelfType::can_cast(kind),
                SelfType::cast(node.clone()).is_some()
            );
            assert_eq!(
                BorrowType::can_cast(kind),
                BorrowType::cast(node.clone()).is_some()
            );
            assert_eq!(
                OwnedType::can_cast(kind),
                OwnedType::cast(node.clone()).is_some()
            );
            assert_eq!(
                TupleType::can_cast(kind),
                TupleType::cast(node.clone()).is_some()
            );
            assert_eq!(
                ParenType::can_cast(kind),
                ParenType::cast(node.clone()).is_some()
            );
            assert_eq!(
                InferType::can_cast(kind),
                InferType::cast(node.clone()).is_some()
            );
            assert_eq!(
                FnPtrType::can_cast(kind),
                FnPtrType::cast(node.clone()).is_some()
            );
        }

        let file = SourceFile::cast(root.clone()).expect("source file view");
        let test_decl = file.test_decls().next().unwrap();
        assert!(test_decl.body().is_some());
        let string = test_decl.name().unwrap();
        assert!(StringLit::can_cast(string.syntax().kind()));
        assert!(StringLit::cast(string.syntax().clone()).is_some());
        let property = file.property_decls().next().unwrap();
        assert!(property.body().is_some());
        let params = property.params().unwrap();
        let non_string = params
            .params()
            .next()
            .unwrap()
            .name()
            .unwrap()
            .token()
            .unwrap();
        assert!(!StringLit::can_cast(non_string.kind()));
        assert!(StringLit::cast(non_string).is_none());

        let property_types = file
            .property_decls()
            .next()
            .unwrap()
            .params()
            .unwrap()
            .params()
            .filter_map(|param| param.ty())
            .map(|ty| ty.syntax().kind())
            .collect::<Vec<_>>();
        assert_eq!(
            property_types,
            [
                SyntaxKind::PATH_TYPE,
                SyntaxKind::SELF_TYPE,
                SyntaxKind::BORROW_TYPE,
                SyntaxKind::OWNED_TYPE,
                SyntaxKind::TUPLE_TYPE,
                SyntaxKind::PAREN_TYPE,
                SyntaxKind::INFER_TYPE,
                SyntaxKind::FN_PTR_TYPE,
            ]
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn item_statement_and_pattern_unions_cover_their_grammar_nodes() {
        use crate::ast::{AstNode, Item, Pattern, Stmt};

        let source = concat!(
            "use sai:http\n",
            "fn statements(value: U32) {\n",
            "let first = value\n",
            "var second = first\n",
            "assert first, second\n",
            "second = first\n",
            "second\n",
            "}\n",
            "type Record<const N: USize, T> { field: T }\n",
            "enum State { Ready, Value(code: U32) }\n",
            "distinct Count = U32\n",
            "const MAX: U32 = 10\n",
            "invariant Record {\nself.field >= 0\n}\n",
            "effect io\n",
            "resource File\n",
            "trait Convert {\ntype Output\nfn convert()\n}\n",
            "impl Convert for Record {\ntype Output = I32\nfn convert() {}\n}\n",
            "interface storage {\nresource Handle {\nfn close(self: borrow Self)\n}\nfn open() -> Handle\n}\n",
            "world client {\nimport wasi:http/outgoing-handler\nexport fn serve()\ninclude common.base\n}\n",
            "extern \"wasm\" module \"env\" {\nfn read()\n}\n",
            "fn patterns(value: Option<U32>) {\n",
            "match value {\n",
            "_ => 0,\n",
            "0 | 1 => 1,\n",
            "name @ _ => 2,\n",
            "Some(item) => 3,\n",
            ".Closed => 4,\n",
            "(left, right) => 5,\n",
            "(item) => 6,\n",
            "[head, ..tail] => 7,\n",
            "0..=9 => 8\n",
            "}\n",
            "}\n",
            "test \"works\" {}\n",
            "property \"holds\"() {}\n",
        );
        let output = parse_file(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let root = crate::SyntaxNode::new_root(output.root);
        let nodes = root.descendants().collect::<Vec<_>>();

        for node in &nodes {
            let kind = node.kind();
            let item = Item::cast((*node).clone());
            assert_eq!(
                Item::can_cast(kind),
                item.is_some(),
                "Item cast for {kind:?}"
            );
            assert_eq!(
                item.as_ref().map(|item| item.syntax().kind()),
                Item::can_cast(kind).then_some(kind),
                "Item syntax for {kind:?}"
            );
            let stmt = Stmt::cast((*node).clone());
            assert_eq!(
                Stmt::can_cast(kind),
                stmt.is_some(),
                "Stmt cast for {kind:?}"
            );
            assert_eq!(
                stmt.as_ref().map(|stmt| stmt.syntax().kind()),
                Stmt::can_cast(kind).then_some(kind),
                "Stmt syntax for {kind:?}"
            );
            let pattern = Pattern::cast((*node).clone());
            assert_eq!(
                Pattern::can_cast(kind),
                pattern.is_some(),
                "Pattern cast for {kind:?}"
            );
            assert_eq!(
                pattern.as_ref().map(|pattern| pattern.syntax().kind()),
                Pattern::can_cast(kind).then_some(kind),
                "Pattern syntax for {kind:?}"
            );
        }

        for kind in [
            SyntaxKind::USE_DECL,
            SyntaxKind::FN_DECL,
            SyntaxKind::TYPE_DECL,
            SyntaxKind::ENUM_DECL,
            SyntaxKind::DISTINCT_DECL,
            SyntaxKind::RESOURCE_DECL,
            SyntaxKind::TRAIT_DECL,
            SyntaxKind::IMPL_DECL,
            SyntaxKind::CONST_DECL,
            SyntaxKind::INVARIANT_DECL,
            SyntaxKind::EFFECT_DECL,
            SyntaxKind::INTERFACE_DECL,
            SyntaxKind::WORLD_DECL,
            SyntaxKind::EXTERN_BLOCK,
            SyntaxKind::TEST_DECL,
            SyntaxKind::PROPERTY_DECL,
        ] {
            assert!(
                nodes
                    .iter()
                    .any(|node| node.kind() == kind && Item::cast((*node).clone()).is_some()),
                "Item union is missing {kind:?}"
            );
        }
        for kind in [
            SyntaxKind::LET_STMT,
            SyntaxKind::VAR_STMT,
            SyntaxKind::ASSERT_STMT,
            SyntaxKind::ASSIGN_STMT,
            SyntaxKind::EXPR_STMT,
        ] {
            assert!(
                nodes
                    .iter()
                    .any(|node| node.kind() == kind && Stmt::cast((*node).clone()).is_some()),
                "Stmt union is missing {kind:?}"
            );
        }
        for kind in [
            SyntaxKind::WILDCARD_PAT,
            SyntaxKind::OR_PAT,
            SyntaxKind::AT_PAT,
            SyntaxKind::BINDING_PAT,
            SyntaxKind::LITERAL_PAT,
            SyntaxKind::CTOR_PAT,
            SyntaxKind::IMPLICIT_MEMBER_PAT,
            SyntaxKind::TUPLE_PAT,
            SyntaxKind::PAREN_PAT,
            SyntaxKind::SLICE_PAT,
            SyntaxKind::RANGE_PAT,
        ] {
            assert!(
                nodes.iter().any(|node| {
                    node.kind() == kind && Pattern::cast((*node).clone()).is_some()
                }),
                "Pattern union is missing {kind:?}"
            );
        }
    }

    #[test]
    fn typed_ast_accessors_remain_optional_for_incomplete_declarations() {
        use crate::ast::{AstNode, SourceFile};

        let source = concat!(
            "property \"without parameters\" {}\n",
            "property \"without type\" (value:) {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let file = SourceFile::cast(root.clone()).expect("source file view");
        let properties = file.property_decls().collect::<Vec<_>>();

        assert_eq!(properties.len(), 2);
        assert!(properties.iter().all(|property| property.name().is_some()));
        assert!(properties[0].params().is_none());
        assert!(properties[0].body().is_some());
        let missing_type = properties[1]
            .params()
            .and_then(|params| params.params().next())
            .expect("parameter remains available");
        assert!(missing_type.name().is_some());
        assert!(missing_type.ty().is_none());
        assert!(properties[1].body().is_some());
        assert!(!output.errors.is_empty());
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn integer_ast_values_decode_radices_and_respect_error_and_range_flags() {
        use crate::ast::{AstToken, IntLit};

        let source = concat!(
            "fn numbers() {\n",
            "let decimal = 123\n",
            "let hexadecimal = 0xff\n",
            "let binary = 0b10_01\n",
            "let octal = 0o17\n",
            "let maximum = 340282366920938463463374607431768211455\n",
            "let overflow = 340282366920938463463374607431768211456\n",
            "let malformed = 1__2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut values = Vec::new();
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                if let crate::SyntaxElement::Token(token) = element
                    && let Some(literal) = IntLit::cast(token)
                {
                    values.push(literal.value());
                }
            }
        }

        assert_eq!(
            values,
            [
                Some(123),
                Some(255),
                Some(9),
                Some(15),
                Some(u128::MAX),
                None,
                None,
            ]
        );
    }

    #[test]
    fn float_ast_values_remove_separators_and_preserve_overflow() {
        use crate::ast::{AstToken, FloatLit};

        let source = concat!(
            "fn floats() {\n",
            "let decimal = 1_2.5_0e+2\n",
            "let rounded32 = 0.1\n",
            "let double_rounding = 1.000000059604644775390626\n",
            "let infinity = 1e99999\n",
            "let malformed = 1.0__0\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut values = Vec::new();
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                if let crate::SyntaxElement::Token(token) = element
                    && let Some(literal) = FloatLit::cast(token)
                {
                    values.push((literal.value_f64(), literal.value_f32()));
                }
            }
        }

        assert_eq!(values.len(), 5);
        assert_eq!(values[0], (Some(1250.0), Some(1250.0)));
        assert_eq!(values[1], (Some(0.1), Some(0.1_f32)));
        assert_eq!(
            values[2].1.map(f32::to_bits),
            Some(0x3f80_0001),
            "direct f32 parsing must avoid a double-rounding regression"
        );
        assert_eq!(
            values[2].0.map(|value| (value as f32).to_bits()),
            Some(1.0_f32.to_bits())
        );
        assert_eq!(values[3].0, Some(f64::INFINITY));
        assert!(values[3].1.is_some_and(f32::is_infinite));
        assert_eq!(values[4], (None, None));
    }

    #[test]
    fn tuple_index_ast_values_require_plain_decimal_u32() {
        use crate::ast::{AstNode, TupleIndexExpr};

        let source = concat!(
            "fn indices() {\n",
            "let zero = value.0\n",
            "let maximum = value.4294967295\n",
            "let overflow = value.4294967296\n",
            "let leading_zero = value.01\n",
            "let separator = value.1_0\n",
            "let prefix = value.0x1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let values = root
            .descendants()
            .filter_map(TupleIndexExpr::cast)
            .map(|index| index.index())
            .collect::<Vec<_>>();

        assert_eq!(values, [Some(0), Some(u32::MAX), None, None, None, None]);
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidTupleIndex)
                .count(),
            3
        );
    }

    #[test]
    fn character_ast_values_decode_escapes_and_recover_invalid_literals() {
        use crate::ast::{AstToken, ByteLit, CharLit};

        let source = r#"fn literals() {
let basic_char = 'a'
let unicode_char = 'é'
let escaped_char = '\u{1f642}'
let invalid_escape_char = '\q'
let multiscalar_char = 'ab'
let escaped_quote = '\''
let escaped_backslash = '\\'
let null_char = '\0'
let basic_byte = b'A'
let escaped_quote_byte = b'\''
let escaped_backslash_byte = b'\\'
let escaped_byte = b'\n'
let hex_byte = b'\xFF'
let invalid_escape_byte = b'\u{41}'
let non_ascii_byte = b'€'
let null_byte = b'\0'
let unclosed_char = '
let unclosed_byte = b'
}"#;
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut chars = Vec::new();
        let mut bytes = Vec::new();
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if let Some(literal) = CharLit::cast(token.clone()) {
                    chars.push(literal.value());
                }
                if let Some(literal) = ByteLit::cast(token) {
                    bytes.push(literal.value());
                }
            }
        }

        assert_eq!(
            chars,
            [
                'a', 'é', '🙂', '\u{fffd}', '\u{fffd}', '\'', '\\', '\0', '\u{fffd}',
            ]
        );
        assert_eq!(bytes, [b'A', b'\'', b'\\', b'\n', 0xff, 0, 0, 0, 0]);
    }

    #[test]
    fn string_ast_values_decode_all_forms_and_replace_invalid_parts() {
        use crate::ast::{AstToken, ByteStringLit, StringLit};
        use std::borrow::Cow;

        let source = r###"fn strings() {
let plain = "plain"
let escaped = "line\n\u{1f642}"
let quoted = "say \"hi\""
let slashed = "path\\name"
let nul = "a\0b"
let continued = "first\
    second"
let multiline = """
    alpha
    beta
    """
let raw = r#"path\n"#
let invalid = "left\qright"
let bad_indent = """
    good
  bad
    """
let byte_plain = b"plain"
let byte_escaped = b"A\n\xFF"
let byte_quoted = b"say \"hi\""
let byte_slashed = b"path\\name"
let byte_nul = b"a\0b"
let byte_raw = br#"raw\n"#
let byte_invalid = b"left\qright"
let byte_unicode_invalid = b"bad\u{41}"
let byte_non_ascii = b"€"
}"###;
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut strings = Vec::new();
        let mut byte_strings = Vec::new();
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if let Some(literal) = StringLit::cast(token.clone()) {
                    strings.push(literal);
                }
                if let Some(literal) = ByteStringLit::cast(token) {
                    byte_strings.push(literal);
                }
            }
        }

        let string_values = strings
            .iter()
            .map(|literal| literal.value().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            string_values,
            [
                "plain",
                "line\n🙂",
                "say \"hi\"",
                "path\\name",
                "a\0b",
                "firstsecond",
                "alpha\nbeta",
                "path\\n",
                "left\u{fffd}right",
                "good\n\u{fffd}",
            ]
        );
        assert!(matches!(strings[0].value(), Cow::Borrowed("plain")));
        assert!(matches!(strings[7].value(), Cow::Borrowed("path\\n")));

        let byte_values = byte_strings
            .iter()
            .map(|literal| literal.value().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            byte_values,
            [
                b"plain".to_vec(),
                vec![b'A', b'\n', 0xff],
                b"say \"hi\"".to_vec(),
                b"path\\name".to_vec(),
                b"a\0b".to_vec(),
                b"raw\\n".to_vec(),
                b"left"
                    .iter()
                    .copied()
                    .chain("\u{fffd}".as_bytes().iter().copied())
                    .chain(b"right".iter().copied())
                    .collect(),
                b"bad"
                    .iter()
                    .copied()
                    .chain("\u{fffd}".as_bytes().iter().copied())
                    .collect(),
                "\u{fffd}".as_bytes().to_vec(),
            ]
        );
        assert!(matches!(byte_strings[0].value(), Cow::Borrowed(b"plain")));
        assert!(matches!(byte_strings[5].value(), Cow::Borrowed(b"raw\\n")));
    }

    #[test]
    fn raw_and_multiline_string_values_preserve_lone_carriage_returns() {
        use crate::ast::{AstToken, ByteStringLit, StringLit};

        let source = "fn literals() {\nlet raw = r\"left\rright\"\nlet bytes = br\"left\rright\"\nlet multiline = \"\"\"\nleft\rright\n\"\"\"\n}\n";
        let output = parse_file(source);
        let (_, lexed) = lex_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut strings = Vec::new();
        let mut byte_string = None;
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if let Some(literal) = StringLit::cast(token.clone()) {
                    strings.push(literal);
                }
                if byte_string.is_none() {
                    byte_string = ByteStringLit::cast(token);
                }
            }
        }

        assert_eq!(strings.len(), 2);
        let byte_string = byte_string.expect("raw byte string token is retained");
        assert!(strings.iter().all(|literal| literal.syntax().has_error()));
        assert_eq!(strings[0].value(), "left\rright");
        assert_eq!(strings[1].value(), "left\rright");
        assert!(byte_string.syntax().has_error());
        assert_eq!(byte_string.value().as_ref(), b"left\rright");
        assert!(output.errors.is_empty());
        assert_eq!(
            lexed
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidLineEnding)
                .count(),
            3
        );
    }

    #[test]
    fn string_values_preserve_allowed_controls_and_foreign_whitespace() {
        use crate::ast::{AstToken, ByteStringLit, StringLit};

        let control = "\u{0001}";
        let foreign_whitespace = "\u{0085}";
        let source = [
            "fn literals() {\nlet simple = \"a",
            control,
            foreign_whitespace,
            "b\"\nlet raw = r\"c",
            control,
            foreign_whitespace,
            "d\"\nlet bytes = br\"x",
            control,
            "y\"\n}\n",
        ]
        .concat();
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut strings = Vec::new();
        let mut byte_string = None;
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if let Some(literal) = StringLit::cast(token.clone()) {
                    strings.push(literal);
                }
                if byte_string.is_none() {
                    byte_string = ByteStringLit::cast(token);
                }
            }
        }

        assert_eq!(strings.len(), 2);
        assert!(strings.iter().all(|literal| !literal.syntax().has_error()));
        assert_eq!(
            strings[0].value(),
            format!("a{control}{foreign_whitespace}b")
        );
        assert_eq!(
            strings[1].value(),
            format!("c{control}{foreign_whitespace}d")
        );

        let byte_string = byte_string.expect("raw byte string token is retained");
        assert!(!byte_string.syntax().has_error());
        assert_eq!(byte_string.value().as_ref(), b"x\x01y");
    }

    #[test]
    fn multiline_string_values_preserve_internal_crlf() {
        use crate::ast::{AstToken, StringLit};

        let source = "fn literals() {\r\nlet text = \"\"\"\r\n    first\r\n    second\r\n    \"\"\"\r\n}\r\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut string = None;
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if string.is_none() {
                    string = StringLit::cast(token);
                }
            }
        }
        let string = string.expect("multiline string token is retained");

        assert!(!string.syntax().has_error());
        assert_eq!(string.value(), "first\r\nsecond");
    }

    #[test]
    fn unterminated_strings_keep_escaped_terminal_quotes_in_value() {
        use crate::ast::{AstToken, ByteStringLit, StringLit};

        let source = [
            "fn literals() {\nlet string = \"tail",
            "\\",
            "\"\nlet bytes = b\"tail",
            "\\",
            "\"\n}\n",
        ]
        .concat();
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);
        let mut string = None;
        let mut byte_string = None;
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if string.is_none() {
                    string = StringLit::cast(token.clone());
                }
                if byte_string.is_none() {
                    byte_string = ByteStringLit::cast(token);
                }
            }
        }

        let string = string.expect("unterminated string token is retained");
        let byte_string = byte_string.expect("unterminated byte string token is retained");
        assert!(string.syntax().has_error());
        assert_eq!(string.value(), "tail\"");
        assert!(byte_string.syntax().has_error());
        assert_eq!(byte_string.value().as_ref(), b"tail\"");
    }

    #[test]
    fn malformed_utf8_character_literals_use_ast_fallback_values() {
        use crate::ast::{AstToken, ByteLit, ByteStringLit, CharLit, StringLit};

        let file_id = sai_vfs::FileId::new(2);
        let source_file = Arc::new(
            SourceFile::decode(
                file_id,
                b"fn literals() {\nlet invalid_utf8_char = '\xff'\nlet invalid_utf8_byte = b'\xff'\nlet invalid_utf8_string = \"a\xffb\"\nlet invalid_utf8_byte_string = b\"a\xffb\"\n}\n",
            )
            .expect("decoded source"),
        );
        let snapshot_file = SnapshotFile::new(
            source_file.clone(),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        );
        let snapshot = WorkspaceSnapshot::new([snapshot_file.clone()]).expect("snapshot");
        let lexed = crate::lex(&snapshot, file_id).expect("lexed source");
        let source = source_file.text();
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid token stream");
        parser
            .parse_source_file()
            .expect("supported source grammar");
        let events = parser.into_events().expect("complete source event stream");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("source tree builds");
        let root = crate::SyntaxNode::new_root(output.root);
        let mut char_literal = None;
        let mut byte_literal = None;
        let mut string_literal = None;
        let mut byte_string_literal = None;
        for node in root.descendants() {
            for element in node.children_with_tokens() {
                let crate::SyntaxElement::Token(token) = element else {
                    continue;
                };
                if char_literal.is_none() {
                    char_literal = CharLit::cast(token.clone());
                }
                if byte_literal.is_none() {
                    byte_literal = ByteLit::cast(token.clone());
                }
                if string_literal.is_none() {
                    string_literal = StringLit::cast(token.clone());
                }
                if byte_string_literal.is_none() {
                    byte_string_literal = ByteStringLit::cast(token);
                }
            }
        }
        let char_literal = char_literal.expect("character token is retained");
        let byte_literal = byte_literal.expect("byte token is retained");
        let string_literal = string_literal.expect("string token is retained");
        let byte_string_literal = byte_string_literal.expect("byte string token is retained");

        assert!(char_literal.syntax().has_error());
        assert_eq!(char_literal.value(), '\u{fffd}');
        assert!(byte_literal.syntax().has_error());
        assert_eq!(byte_literal.value(), 0);
        assert!(string_literal.syntax().has_error());
        assert_eq!(string_literal.value(), "a\u{fffd}b");
        assert!(byte_string_literal.syntax().has_error());
        assert_eq!(byte_string_literal.value().as_ref(), b"a\xEF\xBF\xBDb");
    }

    #[test]
    fn property_parameter_attributes_report_invalid_target_inside_param() {
        let source = "property \"attributed\" (@allow(unused) value: U32) {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let param = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::PARAM)
            .expect("property parameter");

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::InvalidAttributeTarget);
        assert!(
            param
                .children()
                .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn test_and_property_visibility_and_self_parameters_are_rejected() {
        let source = concat!(
            "pub test \"public\" {}\n",
            "export property \"exported\" () {}\n",
            "property \"receiver\" (self: U32) {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let codes = output
            .errors
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert_eq!(
            codes,
            [
                Code::ExpectedToken,
                Code::InvalidExportTarget,
                Code::ExpectedToken,
            ],
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TEST_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::PROPERTY_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            0
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn test_and_property_declarations_inside_blocks_report_item_in_block() {
        let source = concat!(
            "fn local() {\n",
            "test \"nested\" {}\n",
            "property \"nested property\" () {}\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::ItemInBlock),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TEST_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::PROPERTY_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn test_declaration_without_a_body_preserves_the_following_item() {
        use crate::ast::{AstNode, SourceFile};

        let source = "test \"missing body\"\nconst READY: Bool = true\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TEST_DECL)
                .count(),
            1
        );
        let file = SourceFile::cast(root.clone()).expect("source file view");
        let test = file
            .test_decls()
            .next()
            .expect("recovered test declaration");
        assert!(test.body().is_none());
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CONST_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_effect_declarations_and_covers_sets() {
        let source = concat!(
            "pub effect filesystem\n",
            "covers { io, wasi.http\n",
            "alloc, }\n",
            "effect pure\n",
            "effect safe covers {}\n",
            "fn local() { let effect = 1 }\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::COVERS_CLAUSE)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_SET)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_REF)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn exported_effects_are_diagnosed_but_kept_in_the_tree() {
        let source = "export effect network\npub effect public_io\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::InvalidExportTarget);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_effect_set_close_preserves_the_next_effect_declaration() {
        let source = "effect filesystem covers {\nio\n effect network\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_SET)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_effect_set_close_preserves_the_next_function_declaration() {
        let source = "effect filesystem covers {\nio\nfn next() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_record_types_with_attributes_docs_and_where_clauses() {
        let source = concat!(
            "type User<T>\n",
            "where T: Clone\n",
            "{\n",
            "/// User identifier.\n",
            "@field\n",
            "pub id: T,\n",
            "name: Str\n",
            "}\n",
            "type Empty {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::STRUCT_DEF)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FIELD_LIST)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FIELD_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WHERE_CLAUSE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert!(root.descendants().any(|node| {
            node.kind() == SyntaxKind::FIELD_DECL
                && source
                    .get(node.text_range().start().to_usize()..node.text_range().end().to_usize())
                    .is_some_and(|field| field.contains("pub id"))
        }));
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_rejects_exported_nonfunction_items_and_keeps_public_aliases() {
        let source = concat!(
            "export type UserId = U32\n",
            "pub type PublicId = U32\n",
            "export use sai:http\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidExportTarget)
                .count(),
            2
        );
        let invalid_exports = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::InvalidExportTarget)
            .collect::<Vec<_>>();
        for (diagnostic, expected) in invalid_exports.iter().zip([
            source.find("export").expect("type export keyword"),
            source.rfind("export").expect("use export keyword"),
        ]) {
            assert_eq!(
                diagnostic
                    .primary()
                    .expect("invalid export location")
                    .span
                    .range
                    .start()
                    .to_usize(),
                expected
            );
        }
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ALIAS_DEF)
                .count(),
            2
        );
    }

    #[test]
    fn source_file_parses_empty_generic_and_shaped_enums() {
        let source = concat!(
            "enum Void {}\n",
            "pub enum Result<T, E>\n",
            "where T: Copy\n",
            "{\n",
            "/// A successful result.\n",
            "@cold\n",
            "Ok(T),\n",
            "Err(error: E, code: U32)\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_LIST)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TUPLE_SHAPE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RECORD_SHAPE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_FIELD)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::WHERE_CLAUSE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn enum_variants_accept_nested_types_and_function_pointer_payloads() {
        let source = concat!(
            "enum Callback {\n",
            "Apply(fn(Vec<I32>) -> (I32, U32) effects { io })\n",
            "Borrowed(borrow (owned I32))\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_PTR_TYPE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn unit_enum_discriminants_allow_negative_values() {
        let source = "enum Status { Pending = 0, Failed = -2 }\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISCRIMINANT)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn data_enum_reports_one_error_at_its_first_explicit_discriminant() {
        let source = "enum Mixed { Unit = 1, Value(I32) = 2, Other = 3 }\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let diagnostics = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ExplicitDiscriminantOnDataEnum)
            .collect::<Vec<_>>();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0]
                .primary()
                .expect("explicit discriminant has a primary location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("= 1").expect("first discriminant")
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISCRIMINANT)
                .count(),
            3
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn variant_field_attributes_are_diagnosed_but_preserved() {
        let source = "enum Message { Error(@field code: U32) }\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::InvalidAttributeTarget);
        assert_eq!(
            output.errors[0]
                .primary()
                .expect("invalid attribute has a primary location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("@field").expect("field attribute")
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::VARIANT_FIELD)
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_rejects_exported_enums_and_keeps_public_enums() {
        let source = "export enum Hidden {}\npub enum Visible {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::InvalidExportTarget);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_distinct_types_and_refinements() {
        let source = concat!(
            "distinct UserId = U64\n",
            "pub distinct Money = pub I64\n",
            "distinct Meters<T> = T\n",
            "distinct Age = U8\n",
            "where self <= 150\n",
            "distinct Refined<T> = T where self == self\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            5
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::GENERIC_PARAM_LIST)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::REFINEMENT_CLAUSE)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn distinct_base_type_can_follow_a_suppressed_physical_newline() {
        let source = concat!(
            "distinct UserId =\n",
            "U64\n",
            "distinct Opaque =\n",
            "pub\n",
            "I64\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_rejects_exported_distinct_and_keeps_public_visibility() {
        let source = concat!(
            "export distinct Hidden = U32\n",
            "pub distinct Opaque = U32\n",
            "pub distinct VisibleCtor = pub U64\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::InvalidExportTarget);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            3
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_distinct_base_preserves_a_following_function() {
        let source = "distinct Broken =\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn incomplete_distinct_borrow_base_preserves_a_following_function() {
        let source = "distinct Broken = borrow\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn distinct_declaration_in_block_is_recovered_as_an_item() {
        let source = "fn outer() {\ndistinct Local = U32\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_resources_with_optional_bodies_and_shared_fields() {
        let source = concat!(
            "resource UserId\n",
            "pub resource File\n",
            "{\n",
            "pub name: Str\n",
            "owner: U32\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RESOURCE_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FIELD_LIST)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FIELD_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn exported_resources_are_diagnosed_and_missing_resource_close_preserves_next_item() {
        let exported = "export resource Handle\npub resource Empty {}\n";
        let output = parse_file(exported);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidExportTarget)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RESOURCE_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            2,
            "{:?}",
            root.descendants()
                .map(|node| node.kind())
                .collect::<Vec<_>>()
        );

        let source = "resource File {\nname: Str\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RESOURCE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn resource_declaration_in_block_is_recovered_as_an_item() {
        let source = "fn outer() {\nresource Local { id: U32 }\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RESOURCE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_traits_associated_types_and_optional_method_bodies() {
        let source = concat!(
            "pub trait Container<T>: Clone where T: Copy {\n",
            "type Item: Clone\n",
            "fn next<U>(self: borrow Self) -> Item where U: Clone\n",
            "fn verify(self: borrow Self) -> Bool\n",
            "requires true\n",
            "ensures result\n",
            "fn default_value(value: T) -> T { value }\n",
            "}\n",
            "unsafe trait Marker {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TRAIT_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSOC_TYPE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::REQUIRES_CLAUSE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENSURES_CLAUSE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ITEM_LIST)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_trait_inherent_and_interface_impls() {
        let source = concat!(
            "trait Convert<T> {\n",
            "type Output\n",
            "fn convert(value: T) -> T\n",
            "}\n",
            "unsafe impl<T> Convert<T, Output = T> for Box<T> where T: Copy {\n",
            "/// concrete associated output\n",
            "type Output = T\n",
            "@inline\n",
            "fn convert(value: T) -> T { value }\n",
            "}\n",
            "impl Box<I32> {\n",
            "pub fn identity(value: I32) -> I32 { value }\n",
            "}\n",
            "impl wasi.http {\n",
            "fn get() -> I32 { 1 }\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IMPL_DECL)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSOC_TYPE_DEF)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::MOD_PATH)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            4
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn trait_and_interface_impl_items_reject_public_visibility() {
        let source = concat!(
            "impl Convert for Value {\n",
            "pub fn convert() {}\n",
            "}\n",
            "impl wasi.http {\n",
            "pub fn get() {}\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::VisibilityOnTraitImplItem)
                .count(),
            2,
            "{:?}",
            output.errors
        );
        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IMPL_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn public_impl_marker_is_diagnosed_without_losing_the_impl() {
        let source = "pub impl Value {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IMPL_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VISIBILITY)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn export_markers_on_impls_and_impl_items_are_diagnosed() {
        let source = concat!(
            "export impl Value {}\n",
            "impl Convert for Value {\n",
            "export type Output = Value\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 2, "{:?}", output.errors);
        assert_eq!(
            output.errors[0].code,
            Code::InvalidExportTarget,
            "{:?}",
            output.errors
        );
        assert_eq!(output.errors[1].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IMPL_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSOC_TYPE_DEF)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_impl_close_preserves_following_declarations() {
        let source = "impl Value {\nfn method() {}\nenum Later {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "{:?}",
            output.errors
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IMPL_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_associated_type_rhs_preserves_the_next_impl_method() {
        let source = "impl Value {\ntype Output =\nfn method() {}\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSOC_TYPE_DEF)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn associated_types_cannot_have_visibility_markers() {
        let source = "impl Convert for Value {\npub type Output = Value\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSOC_TYPE_DEF)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn impl_declaration_in_block_is_recovered_as_an_item() {
        let source = "fn outer() {\nimpl Value { fn method() {} }\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ItemInBlock);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IMPL_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn inherent_impl_methods_require_bodies() {
        let source = "impl Value {\nfn missing()\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::MissingFnBody);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn trait_item_outer_docs_attach_to_the_declaration() {
        let source = "trait Documented {\n/// method docs\nfn method()\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DOCS)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn exported_traits_and_visibility_on_trait_items_are_diagnosed() {
        let source = "export trait Hidden {}\npub trait Visible { pub fn method() }\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidExportTarget)
                .count(),
            1
        );
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedToken)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TRAIT_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn trait_declaration_in_block_is_recovered_as_an_item() {
        let source = "fn outer() {\ntrait Local { fn value() }\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::ItemInBlock);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TRAIT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_trait_close_preserves_following_declarations() {
        let source = "trait Missing {\nfn method()\nenum Later {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TRAIT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_following_block_statements() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some(1)\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_tuple_variant_close_preserves_following_block_statements() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32\n",
            "1\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_record_variant_close_preserves_following_block_statements() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(code: I32\n",
            "Some(1)\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_a_named_uppercase_call_statement() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some(value: 1)\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_a_call_with_an_operator_after_a_type_path() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some(Foo + 1)\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_a_call_with_a_parenthesized_type_prefix_expression() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some(borrow (Self) + 1)\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let expressions = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
            .collect::<Vec<_>>();

        assert_eq!(expressions.len(), 1);
        assert_eq!(
            &source[expressions[0].text_range().start().to_usize()
                ..expressions[0].text_range().end().to_usize()],
            "Some(borrow (Self) + 1)"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_calls_with_type_prefix_expression_arguments() {
        let mut source = String::from(concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some(borrow value + 1)\n",
            "Some(Self + 1)\n",
        ));
        for index in 0..128 {
            source.push_str(&format!("Some(borrow value{index} + 1)\n"));
        }
        source.push_str("let later = 2\n}\n");

        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        let expression_texts = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
            .map(|node| {
                &source[node.text_range().start().to_usize()..node.text_range().end().to_usize()]
            })
            .collect::<Vec<_>>();
        assert_eq!(expression_texts.len(), 130);
        assert_eq!(expression_texts[0], "Some(borrow value + 1)");
        assert_eq!(expression_texts[1], "Some(Self + 1)");
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn enum_variant_type_lookahead_handles_deep_prefix_parentheses_iteratively() {
        let nesting = 10_000;
        let mut source = String::from("enum Deep {\nValue(");
        for _ in 0..nesting {
            source.push_str("borrow (");
        }
        source.push_str("U32");
        for _ in 0..nesting {
            source.push(')');
        }
        source.push_str(")\n}\n");

        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn enum_expression_call_boundary_lookahead_caches_chain_results() {
        let call_count = 512;
        let mut source = String::from(concat!("fn f() {\n", "enum Broken {\n", "Value(I32)\n",));
        for index in 0..call_count {
            source.push_str(&format!("Some(borrow value{index} + 1)\n"));
        }
        source.push_str("Last(I32)\nlet later = 2\n}\n");

        let (snapshot_file, lexed) = lex_file(&source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        parser
            .parse_source_file()
            .expect("supported source grammar");
        let cached_boundaries = parser
            .variant_call_boundary_cache
            .borrow()
            .iter()
            .filter(|entry| entry.is_some())
            .count();
        assert!(
            cached_boundaries >= call_count,
            "each lookahead result should be cached; found {cached_boundaries} for {call_count} calls"
        );
        let events = parser.into_events().expect("complete source event stream");
        let mut cache = NodeCache::new();
        let output = build_green_tree(&source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("source tree builds");
        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_tuple_variant_close_preserves_a_closure_statement() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32\n",
            "fn() { 1 }\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_a_closure_argument_call() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some(fn() { 1 })\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_a_nested_tuple_call_statement() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "Some((1))\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_enum_close_preserves_an_unsafe_block_statement() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32)\n",
            "unsafe { 1 }\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::UNSAFE_BLOCK_EXPR)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_tuple_variant_close_preserves_a_parenthesized_statement() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32\n",
            "(1)\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_tuple_variant_close_preserves_an_unsafe_block_statement() {
        let source = concat!(
            "fn f() {\n",
            "enum Broken {\n",
            "Value(I32\n",
            "unsafe { 1 }\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::UNSAFE_BLOCK_EXPR)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_record_variant_close_stops_at_the_enum_brace() {
        let source = "enum E { Broken(field: I32 }\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn record_variant_with_a_missing_field_type_is_still_recognized() {
        let source = "enum E {\nV(field:)\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RECORD_SHAPE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_FIELD)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn tuple_variant_with_a_non_type_payload_is_diagnosed() {
        let source = "enum E {\nV(1)\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn repeated_unclosed_tuple_variants_recover_at_following_statements() {
        let mut source = String::from("fn f() {\nenum Broken {\nValue(I32)\n");
        for index in 0..256 {
            source.push_str(&format!("V{index}(1\n"));
        }
        source.push_str("let later = 2\n}\n}\n");

        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn deeply_parenthesized_enum_payload_lookahead_obeys_the_nesting_limit() {
        let mut source = String::from("enum Deep {\nValue(");
        source.push_str(&"(".repeat(10_000));
        source.push_str("I32");
        source.push_str(&")".repeat(10_000));
        source.push_str(")\n}\n");

        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded)
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_variant_list_keeps_later_variants_and_source_text() {
        let source = "enum E { , First, Second }\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::VARIANT_DECL)
                .count(),
            2
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR)
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_record_field_recovery_consumes_an_unexpected_comma() {
        let source = "type Broken {\n,\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::FIELD_LIST)
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR),
            "unexpected comma should be retained under an error node"
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_record_close_preserves_a_following_expression_statement() {
        let source = concat!(
            "fn f() {\n",
            "type Broken {\n",
            "value: I32\n",
            "consume()\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_alias_rhs_preserves_a_following_function_item() {
        let source = "type Broken =\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_recovers_a_missing_use_items_close_delimiter() {
        let output = parse_file("use sai:http.{Request\n");
        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert!(
            crate::SyntaxNode::new_root(output.root.clone())
                .descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR)
        );
    }

    #[test]
    fn missing_use_items_close_preserves_the_next_top_level_use() {
        let source =
            "use sai:http.{Request,\n/// second import\n@allow(UNUSED_IMPORTS)\nuse sai:other\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root.clone());

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            2
        );
        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(
            output.root.text_len(),
            TextSize::from_usize(source.len()).expect("source fits TextSize")
        );
    }

    #[test]
    fn missing_use_items_close_preserves_a_following_function() {
        let output = parse_file("use sai:http.{\nfn next() {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| {
                    matches!(node.kind(), SyntaxKind::USE_DECL | SyntaxKind::FN_DECL)
                })
                .count(),
            2
        );
        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
    }

    #[test]
    fn missing_use_items_close_preserves_a_following_enum() {
        for source in [
            "use sai:http.{\nenum Thing {}\n",
            "use sai:http.{Request\nenum Thing {}\n",
        ] {
            let output = parse_file(source);
            let root = crate::SyntaxNode::new_root(output.root);

            assert_eq!(
                root.descendants()
                    .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                    .count(),
                1
            );
            assert_eq!(
                root.descendants()
                    .filter(|node| node.kind() == SyntaxKind::ENUM_DECL)
                    .count(),
                1
            );
            assert!(
                output
                    .errors
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
            );
            assert_eq!(root.green().text_len().to_usize(), source.len());
        }
    }

    #[test]
    fn missing_use_items_close_preserves_a_following_test_declaration() {
        let source = "use sai:http.{Request\ntest \"later\" {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TEST_DECL)
                .count(),
            1
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "{:?}",
            output.errors
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn missing_use_items_close_preserves_a_following_distinct_declaration() {
        let source = "use sai:http.{Request\ndistinct Thing = U32\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::DISTINCT_DECL)
                .count(),
            1
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn source_file_parses_generic_attributes_on_use_declarations() {
        let source = "@allow(check(name), option = \"value\", 42)\nuse sai:http\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root.clone());
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == SyntaxKind::ATTRIBUTE)
                .count(),
            1
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == SyntaxKind::ATTR_ARGS)
                .count(),
            2
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == SyntaxKind::ATTR_ARG)
                .count(),
            4
        );
        assert!(output.errors.is_empty());
    }

    #[test]
    fn source_file_parses_function_signatures_and_empty_bodies() {
        let source = concat!(
            "pub fn add(x: I32, y: I32) -> I32 {}\n",
            "@allow(x)\n",
            "fn transform<T: Fn(I32) -> I32>(value: Vec<(T, U32), 4>) -> (T, U32) {}\n",
            "fn apply(callback: fn(I32) -> I32) {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        for (kind, expected) in [
            (SyntaxKind::FN_DECL, 3),
            (SyntaxKind::PARAM_LIST, 3),
            (SyntaxKind::BLOCK, 3),
            (SyntaxKind::RET_TYPE, 2),
            (SyntaxKind::GENERIC_PARAM_LIST, 1),
            (SyntaxKind::TYPE_PARAM, 1),
            (SyntaxKind::FN_SUGAR_BOUND, 1),
            (SyntaxKind::GENERIC_ARG_LIST, 1),
            (SyntaxKind::CONST_ARG, 1),
            (SyntaxKind::FN_PTR_TYPE, 1),
        ] {
            assert_eq!(
                kinds.iter().filter(|found| **found == kind).count(),
                expected,
                "unexpected count for {kind:?}"
            );
        }
        assert!(output.errors.is_empty());
    }

    #[test]
    fn source_file_recovers_redundant_pub_before_exported_function() {
        let source = "pub export fn f() {}\nfn next() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::RedundantPubExport);
        assert_eq!(
            output.errors[0]
                .primary()
                .expect("redundant visibility has a primary location")
                .span
                .range
                .start()
                .to_usize(),
            0
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        let visibility = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::VISIBILITY)
            .expect("function retains its visibility node");
        assert_eq!(
            visibility
                .children_with_tokens()
                .filter_map(|element| match element {
                    crate::SyntaxElement::Token(token) => Some(token.kind()),
                    crate::SyntaxElement::Node(_) => None,
                })
                .collect::<Vec<_>>(),
            [SyntaxKind::PUB_KW, SyntaxKind::EXPORT_KW]
        );
    }

    #[test]
    fn source_file_parses_borrow_owned_self_and_unsafe_function_pointer_types() {
        let source = "unsafe async fn inspect(self: Self, value: borrow mut I32, own: owned Request, callback: unsafe fn(I32) -> I32) -> (I32,) {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        for (kind, expected) in [
            (SyntaxKind::SELF_PARAM, 1),
            (SyntaxKind::SELF_TYPE, 1),
            (SyntaxKind::BORROW_TYPE, 1),
            (SyntaxKind::OWNED_TYPE, 1),
            (SyntaxKind::FN_PTR_TYPE, 1),
            (SyntaxKind::TUPLE_TYPE, 1),
        ] {
            assert_eq!(
                kinds.iter().filter(|found| **found == kind).count(),
                expected,
                "unexpected count for {kind:?}"
            );
        }
        assert!(output.errors.is_empty());
    }

    #[test]
    fn source_file_parses_function_clauses_and_effectful_function_pointer_types() {
        let source = concat!(
            "fn identity<T: Clone effects { alloc }>(value: T) -> T\n",
            "where T: Iterator<Item = U32> + (Clone effects { io }),\n",
            "U32: (Fn(I32) -> I32 effects { alloc })\n",
            "borrows { self, value.item }\n",
            "effects { alloc, io }\n",
            "{}\n",
            "fn accepts(callback: fn(I32) -> I32 effects { alloc }) {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        for (kind, expected) in [
            (SyntaxKind::WHERE_CLAUSE, 1),
            (SyntaxKind::WHERE_PRED, 2),
            (SyntaxKind::ASSOC_TYPE_BINDING, 1),
            (SyntaxKind::BORROWS_CLAUSE, 1),
            (SyntaxKind::PROVENANCE, 2),
            (SyntaxKind::EFFECTS_CLAUSE, 5),
            (SyntaxKind::EFFECT_SET, 5),
            (SyntaxKind::FN_SUGAR_BOUND, 1),
            (SyntaxKind::FN_PTR_TYPE, 1),
        ] {
            assert_eq!(
                kinds.iter().filter(|found| **found == kind).count(),
                expected,
                "unexpected count for {kind:?}"
            );
        }
        assert!(output.errors.is_empty());
    }

    #[test]
    fn effects_after_an_unparenthesized_where_bound_belong_to_the_function() {
        let output = parse_file("fn f<T>() where T: Clone effects { io } {}\n");
        let root = crate::SyntaxNode::new_root(output.root);
        let clauses = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::EFFECTS_CLAUSE)
            .collect::<Vec<_>>();

        assert_eq!(clauses.len(), 1);
        assert!(
            clauses[0]
                .parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::FN_DECL)
        );
        assert!(output.errors.is_empty());
    }

    #[test]
    fn fn_sugar_type_arguments_do_not_allow_associated_type_bindings() {
        let output = parse_file("fn f<T: Fn(Vec<Item = U32>) + Trait<Wrapper<Item = U32>>>() {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert!(
            !root
                .descendants()
                .any(|node| node.kind() == SyntaxKind::ASSOC_TYPE_BINDING)
        );
    }

    #[test]
    fn associated_type_binding_values_do_not_enable_nested_bindings() {
        let output = parse_file("fn f<T: Trait<Item = Wrapper<Assoc = U32>>>() {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSOC_TYPE_BINDING)
                .count(),
            1
        );
    }

    #[test]
    fn function_clause_duplicates_and_order_errors_preserve_the_function() {
        let source = concat!(
            "fn f()\n",
            "effects { io }\n",
            "borrows { self }\n",
            "effects { net }\n",
            "{}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ClauseOrder)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::DuplicateClause)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EFFECTS_CLAUSE)
                .count(),
            2
        );
    }

    #[test]
    fn where_predicate_types_do_not_accept_associated_type_bindings() {
        let output = parse_file("fn f() where Iterator<Item = U32>: Clone {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert!(
            !root
                .descendants()
                .any(|node| node.kind() == SyntaxKind::ASSOC_TYPE_BINDING)
        );
    }

    #[test]
    fn deeply_nested_types_are_recovered_at_the_implementation_limit() {
        let depth = 520;
        let source = format!(
            "fn deep(value: {}I32{}) {{}}\n",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let output = parse_file(&source);
        let diagnostics = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded)
            .collect::<Vec<_>>();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].data.get("limit"),
            Some(&sai_diagnostics::Value::String("nesting depth".to_owned()))
        );
        assert_eq!(
            diagnostics[0].data.get("value"),
            Some(&sai_diagnostics::Value::Integer(513))
        );
        assert!(
            crate::SyntaxNode::new_root(output.root)
                .descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR)
        );
    }

    #[test]
    fn nesting_recovery_preserves_a_parameter_after_a_synchronization_comma() {
        let source = format!("fn f(x: {}, y: I32) {{}}\n", "borrow ".repeat(512));
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);
        let parameters = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::PARAM)
            .collect::<Vec<_>>();

        assert_eq!(parameters.len(), 2);
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| { diagnostic.code == Code::ImplementationLimitExceeded })
                .count(),
            1
        );
        assert!(
            parameters[1]
                .descendants()
                .any(|node| node.kind() == SyntaxKind::PATH_TYPE)
        );
    }

    #[test]
    fn nesting_recovery_preserves_the_function_body() {
        let source = format!("fn f() -> {}I32 {{}}\n", "borrow ".repeat(512));
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::BLOCK)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded)
        );
        assert!(
            !output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::MissingFnBody)
        );
    }

    #[test]
    fn deeply_nested_attribute_arguments_are_recovered_at_the_implementation_limit() {
        let depth = 520;
        let nested = format!("{}value{}", "nested(".repeat(depth), ")".repeat(depth));
        let source = format!("@allow({nested})\nfn main() {{}}\n");
        let output = parse_file(&source);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded)
                .count(),
            1
        );
        assert_eq!(
            crate::SyntaxNode::new_root(output.root)
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
    }

    #[test]
    fn malformed_function_parameter_delimiters_still_build_a_recoverable_tree() {
        for source in ["fn f\n", "fn f(callback: fn\n"] {
            let output = parse_file(source);
            let root = crate::SyntaxNode::new_root(output.root);
            assert_eq!(
                root.descendants()
                    .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                    .count(),
                1
            );
            assert!(
                output
                    .errors
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
            );
            assert!(
                !root.descendants().any(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::FN_PTR_TYPE
                            if node
                                .children_with_tokens()
                                .all(|child| !matches!(child, crate::SyntaxElement::Token(token)
                                    if token.kind() == SyntaxKind::L_PAREN
                                        || token.kind() == SyntaxKind::R_PAREN))
                    )
                }),
                "an incomplete function-pointer type must not be completed"
            );
        }
    }

    #[test]
    fn malformed_generic_bounds_and_clause_lists_still_build_trees() {
        for source in [
            "fn f<T: Copy +>() {}\n",
            "fn f() where T: Copy + {}\n",
            "fn f(value: Vec<>) {}\n",
            "fn f() effects {,} {}\n",
            "fn f() borrows {,} {}\n",
        ] {
            let output = parse_file(source);
            assert!(
                output
                    .errors
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
                "malformed input should retain a syntax diagnostic: {source}"
            );
            assert!(
                crate::SyntaxNode::new_root(output.root)
                    .descendants()
                    .any(|node| node.kind() == SyntaxKind::FN_DECL),
                "malformed signature should keep its function node: {source}"
            );
        }
    }

    #[test]
    fn a_free_function_without_a_body_reports_the_documented_diagnostic() {
        let output = parse_file("fn declaration()\n");
        assert!(output.errors.iter().any(|diagnostic| {
            diagnostic.code == Code::MissingFnBody
                && diagnostic.message == "function requires a body"
        }));
    }

    #[test]
    fn attributes_on_parameters_and_generic_parameters_report_invalid_targets() {
        let source = "fn invalid<@allow(foo) @allow(bar) T>(@allow(foo) @allow(bar) value: T) {}\n";
        let output = parse_file(source);
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
                .count(),
            2
        );
        let diagnostic_starts = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
            .map(|diagnostic| {
                diagnostic
                    .primary()
                    .expect("invalid-target diagnostic has a location")
                    .span
                    .range
                    .start()
                    .to_usize()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            diagnostic_starts,
            [
                source.find("@allow(foo)").expect("generic attribute"),
                source.rfind("@allow(foo)").expect("parameter attribute")
            ]
        );
        assert_eq!(
            crate::SyntaxNode::new_root(output.root)
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            4
        );
    }

    #[test]
    fn function_parser_builds_requires_and_ensures_clauses() {
        let source = "fn check(value: Bool)\nrequires value\nensures result\n{}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::REQUIRES_CLAUSE)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ENSURES_CLAUSE)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn function_parser_builds_format_string_and_interpolation_nodes() {
        let source = concat!(
            "fn format(name: Str, age: I32) {\n",
            "let message = f\"Hello {name}, age {age:pad(3)}\"\n",
            "}\n",
            "fn condition(value: I32) {\n",
            "if f\"{ { value } }\" {}\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(
            output.errors.is_empty(),
            "valid format strings should parse without diagnostics: {:?}",
            output.errors
        );

        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(root.green().text_len().to_usize(), source.len());
        let fstrings = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FSTRING_EXPR)
            .collect::<Vec<_>>();
        let interpolations = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FSTRING_INTERP)
            .collect::<Vec<_>>();
        assert_eq!(fstrings.len(), 2);
        assert_eq!(interpolations.len(), 3);
        assert!(interpolations.iter().all(|node| {
            node.descendants()
                .any(|child| child.kind() == SyntaxKind::PATH_EXPR)
        }));
        assert!(
            interpolations[0]
                .children_with_tokens()
                .all(|element| !matches!(
                    element,
                    crate::SyntaxElement::Token(token)
                        if token.kind() == SyntaxKind::FSTRING_SPEC
                ))
        );
        assert!(
            interpolations[1]
                .children_with_tokens()
                .any(|element| matches!(
                    element,
                    crate::SyntaxElement::Token(token)
                        if token.kind() == SyntaxKind::FSTRING_SPEC
                ))
        );
    }

    #[test]
    fn typed_fstring_views_decode_text_interpolations_and_specs() {
        use crate::ast::{AstNode, Expr, FStringExpr, FStringPiece};
        use std::borrow::Cow;

        let source = r###"fn format(name: Str, age: I32) {
let message = f"Hello {{name}} {name}, age {age:pad(3)}!"
let plain = f"Hi {name}!"
}"###;
        let output = parse_file(source);
        assert!(
            output.errors.is_empty(),
            "valid format strings should parse without diagnostics: {:?}",
            output.errors
        );
        let root = crate::SyntaxNode::new_root(output.root);
        let fstrings = root
            .descendants()
            .filter_map(FStringExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(fstrings.len(), 2);
        assert!(matches!(
            Expr::cast(fstrings[0].syntax().clone()),
            Some(Expr::FormatString(_))
        ));
        let interpolation = fstrings[0]
            .syntax()
            .descendants()
            .find(|node| node.kind() == SyntaxKind::FSTRING_INTERP)
            .expect("f-string should contain an interpolation node");
        assert!(matches!(
            Expr::cast(interpolation),
            Some(Expr::FormatStringInterpolation(_))
        ));

        let pieces = fstrings[0].pieces().collect::<Vec<_>>();
        assert_eq!(pieces.len(), 5);
        assert!(matches!(
            &pieces[0],
            FStringPiece::Text(text) if text == "Hello {name} "
        ));
        assert!(matches!(
            &pieces[1],
            FStringPiece::Interp {
                expr: Some(Expr::Path(_)),
                spec: None,
            }
        ));
        assert!(matches!(
            &pieces[2],
            FStringPiece::Text(text) if text == ", age "
        ));
        match &pieces[3] {
            FStringPiece::Interp {
                expr: Some(Expr::Path(_)),
                spec: Some(spec),
            } => {
                assert_eq!(spec.name(), "pad");
                assert_eq!(spec.arg(), Some(3));
            }
            piece => panic!("expected typed interpolation with format spec, got {piece:?}"),
        }
        assert!(matches!(
            &pieces[4],
            FStringPiece::Text(text) if text == "!"
        ));

        let plain_pieces = fstrings[1].pieces().collect::<Vec<_>>();
        assert!(matches!(
            plain_pieces.as_slice(),
            [
                FStringPiece::Text(Cow::Borrowed("Hi ")),
                FStringPiece::Interp {
                    expr: Some(Expr::Path(_)),
                    spec: None
                },
                FStringPiece::Text(Cow::Borrowed("!"))
            ]
        ));
    }

    #[test]
    fn format_spec_accessors_handle_raw_names_radices_and_invalid_numbers() {
        use crate::ast::{AstNode, AstToken, FStringExpr, FStringPiece, FormatSpec};

        let source = r###"fn format(value: I32) {
let raw = f"{value:r#pad(0xA)}"
let malformed = f"{value:pad(01)}"
}"###;
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let fstrings = root
            .descendants()
            .filter_map(FStringExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(fstrings.len(), 2);

        let first = fstrings[0].pieces().collect::<Vec<_>>();
        let FStringPiece::Interp {
            spec: Some(spec), ..
        } = &first[0]
        else {
            panic!("expected raw-name format spec");
        };
        assert_eq!(spec.name(), "pad");
        assert_eq!(spec.arg(), Some(10));

        let second = fstrings[1].pieces().collect::<Vec<_>>();
        let FStringPiece::Interp {
            spec: Some(spec), ..
        } = &second[0]
        else {
            panic!("expected malformed-number format spec token");
        };
        assert_eq!(spec.name(), "pad");
        assert_eq!(spec.arg(), None);
        assert!(spec.syntax().has_error());

        assert!(FormatSpec::can_cast(SyntaxKind::FSTRING_SPEC));
        assert!(!FormatSpec::can_cast(SyntaxKind::STRING));
    }

    #[test]
    fn fstring_values_recover_invalid_text_missing_expressions_and_large_spec_args() {
        use crate::ast::{AstNode, AstToken, FStringExpr, FStringPiece};

        let source = r###"fn format(value: I32) {
let bad = f"bad\q }"
let empty = f"{}"
let large = f"{value:pad(340282366920938463463374607431768211456)}"
}"###;
        let output = parse_file(source);
        let (_, lexed) = lex_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let fstrings = root
            .descendants()
            .filter_map(FStringExpr::cast)
            .collect::<Vec<_>>();
        assert_eq!(fstrings.len(), 3);
        assert!(lexed.errors.iter().any(|diagnostic| {
            matches!(
                diagnostic.code,
                Code::InvalidEscape | Code::InvalidInterpolation
            )
        }));

        let bad = fstrings[0].pieces().collect::<Vec<_>>();
        assert!(matches!(
            bad.as_slice(),
            [FStringPiece::Text(text)] if text == "bad\u{fffd} \u{fffd}"
        ));

        let empty = fstrings[1].pieces().collect::<Vec<_>>();
        assert!(matches!(
            empty.as_slice(),
            [FStringPiece::Interp {
                expr: None,
                spec: None
            }]
        ));

        let large = fstrings[2].pieces().collect::<Vec<_>>();
        let FStringPiece::Interp {
            spec: Some(spec), ..
        } = &large[0]
        else {
            panic!("expected large format spec argument");
        };
        assert_eq!(spec.name(), "pad");
        assert_eq!(spec.arg(), None);
        assert!(!spec.syntax().has_error());
    }

    #[test]
    fn syntax_node_and_ast_pointers_check_file_and_node_kind() {
        use crate::ast::{AstNode, AstPtr, TestDecl};
        use sai_vfs::{FileId, SnapshotFile, SourceFile, WorkspaceSnapshot};

        let source = "// leading comment\ntest \"works\" {} // trailing comment\n";
        let output = parse_file(source);
        let file = FileId::new(42);
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, source, false).expect("source")),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");
        let parsed = crate::parse(&snapshot, file).expect("parse the snapshot source");
        assert!(parsed.errors.is_empty(), "{}", parsed.errors.len());
        assert!(matches!(
            crate::SyntaxNode::new_root_in_snapshot(
                output.root.clone(),
                FileId::new(43),
                &snapshot
            ),
            Err(crate::SyntaxRootError::FileNotInSnapshot)
        ));
        let root = parsed.root;
        let test = root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration");
        assert!(matches!(
            crate::SyntaxNodePtr::new(FileId::new(43), test.syntax()),
            Err(crate::SyntaxNodePtrError::FileMismatch)
        ));
        assert!(matches!(
            AstPtr::new(FileId::new(43), &test),
            Err(crate::SyntaxNodePtrError::FileMismatch)
        ));

        let raw = crate::SyntaxNodePtr::new(file, test.syntax()).expect("matching file identity");
        assert_eq!(
            raw.resolve(file, &root).map(|node| node.kind()),
            Some(SyntaxKind::TEST_DECL)
        );
        assert!(raw.resolve(FileId::new(43), &root).is_none());

        let typed = AstPtr::new(file, &test).expect("matching file identity");
        assert_eq!(
            typed.resolve(file, &root).map(|node| node.syntax().kind()),
            Some(SyntaxKind::TEST_DECL)
        );
        assert!(typed.resolve(FileId::new(43), &root).is_none());

        let reparsed_same_snapshot =
            crate::SyntaxNode::new_root_in_snapshot(output.root.clone(), file, &snapshot)
                .expect("green tree matches the same file snapshot");
        assert!(typed.resolve(file, &reparsed_same_snapshot).is_some());

        let same_source_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, source, false).expect("same source")),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("new snapshot with unchanged file contents");
        let same_source_root = crate::parse(&same_source_snapshot, file)
            .expect("parse unchanged source in the new snapshot")
            .root;
        assert!(raw.resolve(file, &same_source_root).is_none());
        assert!(typed.resolve(file, &same_source_root).is_none());

        let edited_source = "test \"fails\" {}\n";
        let edited_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, edited_source, false).expect("edited source")),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("edited snapshot");
        assert!(matches!(
            crate::SyntaxNode::new_root_in_snapshot(output.root.clone(), file, &edited_snapshot),
            Err(crate::SyntaxRootError::SourceMismatch)
        ));
        let edited_root = crate::SyntaxNode::new_root_in_snapshot(
            parse_file(edited_source).root,
            file,
            &edited_snapshot,
        )
        .expect("edited tree matches the edited snapshot");
        assert!(typed.resolve(file, &edited_root).is_none());

        let other_file = FileId::new(43);
        let other_file_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(other_file, source, false).expect("other file source")),
            "src/other.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("other file snapshot");
        let other_file_root = crate::SyntaxNode::new_root_in_snapshot(
            parse_file(source).root,
            other_file,
            &other_file_snapshot,
        )
        .expect("tree matches the other file snapshot");
        assert!(typed.resolve(file, &other_file_root).is_none());
    }

    #[test]
    fn ast_id_map_numbers_item_like_nodes_in_preorder() {
        use crate::ast::{
            AstId, AstIdMap, AstIdMapError, AstNode, ConstParam, ErasedAstId, Expr, PropertyDecl,
            TestDecl, TypeParam,
        };
        use sai_vfs::{FileId, SnapshotFile, SourceFile, WorkspaceSnapshot};

        let source = concat!(
            "use sai:http\n",
            "fn before() { let local = 1 }\n",
            "type Record<const N: USize, T> { field: T }\n",
            "enum State { Ready, Value(code: U32) }\n",
            "distinct Count = U32\n",
            "const MAX: U32 = 10\n",
            "invariant Record {\nself.field >= 0\n}\n",
            "effect io\n",
            "resource File\n",
            "trait Convert {\ntype Output\nfn convert()\n}\n",
            "impl Convert for Record {\ntype Output = I32\nfn convert() {}\n}\n",
            "interface storage {\nresource Handle {\nfn close(self: borrow Self)\n}\nfn open() -> Handle\n}\n",
            "world client {\nimport wasi:http/outgoing-handler\nexport fn serve()\ninclude common.base\n}\n",
            "extern \"wasm\" module \"env\" {\nfn read()\n}\n",
            "test \"works\" {}\n",
            "property \"holds\"() {}\n",
        );
        let output = parse_file(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let file = FileId::new(7);
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, source, false).expect("source")),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");
        let root = crate::SyntaxNode::new_root_in_snapshot(output.root, file, &snapshot)
            .expect("green tree matches the file in this snapshot");
        let map = AstIdMap::new(file, &root).expect("item count fits ErasedAstId");
        assert!(matches!(
            AstIdMap::new(FileId::new(8), &root),
            Err(AstIdMapError::FileMismatch)
        ));
        let second_fn = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::FN_DECL)
            .expect("function declaration");
        assert!(matches!(
            AstIdMap::new(file, &second_fn),
            Err(AstIdMapError::NotFileRoot)
        ));
        let unbound_root = crate::SyntaxNode::new_root(parse_file(source).root);
        assert!(matches!(
            AstIdMap::new(file, &unbound_root),
            Err(AstIdMapError::UnboundRoot)
        ));
        let test = root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration");
        let test_id = map.ast_id(&test).expect("current test node has an AST ID");
        let test_ptr = map.get(test_id).expect("current test ID has a pointer");
        let same_source_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, source, false).expect("same source")),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("new snapshot with unchanged file contents");
        let same_source_root = crate::parse(&same_source_snapshot, file)
            .expect("parse unchanged source in the new snapshot")
            .root;
        let same_source_map =
            AstIdMap::new(file, &same_source_root).expect("same-source snapshot map");
        let same_source_test = same_source_root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration in same-source snapshot");
        let same_source_test_id = same_source_map
            .ast_id(&same_source_test)
            .expect("same-source test node has an AST ID");
        assert_eq!(test_id.erase(), same_source_test_id.erase());
        assert!(map.get(same_source_test_id).is_none());
        assert!(same_source_map.get(test_id).is_none());
        assert!(test_ptr.resolve(file, &same_source_root).is_none());

        let other_file = FileId::new(8);
        let other_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(other_file, source, false).expect("other file source")),
            "src/other.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("other file snapshot");
        let other_root = crate::SyntaxNode::new_root_in_snapshot(
            parse_file(source).root,
            other_file,
            &other_snapshot,
        )
        .expect("tree matches the other file snapshot");
        let other_map = AstIdMap::new(other_file, &other_root).expect("other file map");
        let other_test = other_root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration in other file");
        let other_test_id = other_map
            .ast_id(&other_test)
            .expect("other file test node has an AST ID");
        assert!(map.get(other_test_id).is_none());
        let second_version = source.replace("test \"works\"", "test \"other\"");
        let same_file_new_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(
                SourceFile::new(file, second_version.as_str(), false)
                    .expect("second version source"),
            ),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("second snapshot for the same file");
        let same_file_new_root = crate::SyntaxNode::new_root_in_snapshot(
            parse_file(&second_version).root,
            file,
            &same_file_new_snapshot,
        )
        .expect("tree matches the second file snapshot");
        let same_file_new_map =
            AstIdMap::new(file, &same_file_new_root).expect("second map for the same file");
        let same_file_new_test = same_file_new_root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration in second snapshot");
        let same_file_new_test_id = same_file_new_map
            .ast_id(&same_file_new_test)
            .expect("test declaration has an AST ID in second snapshot");
        assert_eq!(test_id.erase(), same_file_new_test_id.erase());
        assert!(map.get(same_file_new_test_id).is_none());
        let item_kinds = [
            SyntaxKind::USE_DECL,
            SyntaxKind::FN_DECL,
            SyntaxKind::CONST_PARAM,
            SyntaxKind::TYPE_PARAM,
            SyntaxKind::TYPE_DECL,
            SyntaxKind::FIELD_DECL,
            SyntaxKind::ENUM_DECL,
            SyntaxKind::VARIANT_DECL,
            SyntaxKind::VARIANT_FIELD,
            SyntaxKind::DISTINCT_DECL,
            SyntaxKind::CONST_DECL,
            SyntaxKind::INVARIANT_DECL,
            SyntaxKind::EFFECT_DECL,
            SyntaxKind::RESOURCE_DECL,
            SyntaxKind::TRAIT_DECL,
            SyntaxKind::ASSOC_TYPE_DECL,
            SyntaxKind::IMPL_DECL,
            SyntaxKind::ASSOC_TYPE_DEF,
            SyntaxKind::INTERFACE_DECL,
            SyntaxKind::IFACE_RESOURCE,
            SyntaxKind::WORLD_DECL,
            SyntaxKind::WORLD_IMPORT,
            SyntaxKind::WORLD_EXPORT,
            SyntaxKind::WORLD_INCLUDE,
            SyntaxKind::EXTERN_BLOCK,
            SyntaxKind::TEST_DECL,
            SyntaxKind::PROPERTY_DECL,
        ];
        let expected = root
            .descendants()
            .filter(|node| item_kinds.contains(&node.kind()))
            .map(|node| crate::SyntaxNodePtr::new(file, &node).expect("matching file identity"))
            .collect::<Vec<_>>();
        assert_eq!(map.len(), expected.len());
        for (index, pointer) in expected.iter().enumerate() {
            assert_eq!(
                map.pointer(ErasedAstId::new(index as u32)),
                Some(pointer.clone()),
                "item-like node {index} should retain pre-order"
            );
        }

        let property = root
            .descendants()
            .find_map(PropertyDecl::cast)
            .expect("property declaration");
        let property_id = map
            .ast_id(&property)
            .expect("current property node has an AST ID");
        assert!(map.get(test_id).is_some());
        assert!(map.get(property_id).is_some());
        assert_eq!(map.get(test_id), map.get(test_id));
        assert_eq!(map.ast_id(&test), Some(test_id));
        assert!(AstId::<TestDecl>::from_erased(&map, ErasedAstId::new(u32::MAX)).is_none());
        let same_snapshot_root =
            crate::SyntaxNode::new_root_in_snapshot(parse_file(source).root, file, &snapshot)
                .expect("tree matches the same file snapshot");
        let same_snapshot_test = same_snapshot_root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration in recreated red tree");
        assert_eq!(
            map.ast_id(&same_snapshot_test).map(AstId::erase),
            Some(test_id.erase())
        );

        let const_param = root
            .descendants()
            .find_map(ConstParam::cast)
            .expect("const generic parameter");
        let type_param = root
            .descendants()
            .find_map(TypeParam::cast)
            .expect("type generic parameter");
        assert!(map.ast_id(&const_param).is_some());
        assert!(map.ast_id(&type_param).is_some());

        let literal = root
            .descendants()
            .find_map(Expr::cast)
            .expect("function body expression");
        assert!(map.ast_id(&literal).is_none());

        let reparsed = crate::SyntaxNode::new_root(parse_file(source).root);
        let reparsed_test = reparsed
            .descendants()
            .find_map(TestDecl::cast)
            .expect("reparsed test declaration");
        assert!(map.ast_id(&reparsed_test).is_none());

        let edited_source = source.replace("let local = 1", "let local = 123456");
        let edited_snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file, edited_source.as_str(), false).expect("edited source")),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("edited snapshot");
        let edited_root = crate::SyntaxNode::new_root_in_snapshot(
            parse_file(&edited_source).root,
            file,
            &edited_snapshot,
        )
        .expect("tree matches the edited file snapshot");
        let edited_map =
            AstIdMap::new(file, &edited_root).expect("edited item count fits ErasedAstId");
        let edited_test = edited_root
            .descendants()
            .find_map(TestDecl::cast)
            .expect("test declaration after body edit");
        assert_eq!(
            edited_map
                .ast_id(&edited_test)
                .expect("edited test node has an AST ID")
                .erase(),
            test_id.erase(),
            "editing a function body must not renumber later item-like nodes"
        );
    }

    #[test]
    fn unterminated_format_string_recovers_at_the_next_block_statement() {
        let source = concat!(
            "fn f(value: I32) {\n",
            "let broken = f\"{value +\n",
            "following()\n",
            "let following = 2\n",
            "}\n",
            "fn after() {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.iter().any(|diagnostic| {
            matches!(
                diagnostic.code,
                Code::ExpectedExpression | Code::ExpectedToken
            )
        }));
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::FSTRING_EXPR)
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::CALL_EXPR),
            "the expression on the line after the malformed interpolation must survive"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2,
            "the statement after the unterminated interpolation must survive"
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::FN_DECL
                    && source
                        .get(
                            node.text_range().start().to_usize()
                                ..node.text_range().end().to_usize()
                        )
                        .is_some_and(|text| text.starts_with("fn after"))),
            "the following function must survive"
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_format_interpolations_recover_into_their_actual_expression_and_close() {
        let source = concat!(
            "fn f() {\n",
            "let first = f\"{;item,}\"\n",
            "let second = f\"{,other}\"\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let trailing_comma = source.find(",}").expect("trailing comma");
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken
                    && diagnostic.primary().is_some_and(|location| {
                        location.span.range.start().to_usize() == trailing_comma
                    }))
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression)
        );
        let interpolations = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FSTRING_INTERP)
            .collect::<Vec<_>>();
        assert_eq!(interpolations.len(), 2);
        let interpolation_texts = interpolations
            .iter()
            .map(|interpolation| {
                let range = interpolation.text_range();
                &source[range.start().to_usize()..range.end().to_usize()]
            })
            .collect::<Vec<_>>();
        assert_eq!(interpolation_texts, ["{;item,}", "{,other}"]);
        assert!(interpolations.iter().all(|interpolation| {
            interpolation
                .descendants()
                .any(|node| node.kind() == SyntaxKind::PATH_EXPR)
                && interpolation.children_with_tokens().any(|element| {
                    matches!(
                        element,
                        crate::SyntaxElement::Token(token)
                            if token.kind() == SyntaxKind::FSTRING_INTERP_CLOSE
                    )
                })
        }));
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn function_parser_builds_loop_expressions_and_jumps() {
        let source = concat!(
            "fn f(flag: Bool, values: I32) {\n",
            "'outer:\n",
            "loop {\n",
            "if flag { break 'outer 1 }\n",
            "continue 'outer\n",
            "}\n",
            "loop {\n",
            "break\n",
            "1\n",
            "}\n",
            "while let Some(item) = values {\n",
            "break\n",
            "}\n",
            "for Some(item) in values {\n",
            "continue\n",
            "}\n",
            "for await item in values {\n",
            "break\n",
            "}\n",
            "let outside_break = break 'missing 1\n",
            "continue 'missing\n",
            "let parenthesized_while = while ({ flag }) {}\n",
            "let parenthesized_for = for item in ({ values }) {}\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(
            output.errors.is_empty(),
            "loop forms and jumps should parse without semantic validation: {:?}",
            output.errors
        );
        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };
        let loops = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::LOOP_EXPR)
            .collect::<Vec<_>>();

        assert_eq!(count(SyntaxKind::LOOP_EXPR), 2);
        assert_eq!(count(SyntaxKind::LOOP_LABEL), 1);
        assert_eq!(count(SyntaxKind::WHILE_EXPR), 2);
        assert_eq!(count(SyntaxKind::FOR_EXPR), 3);
        assert_eq!(count(SyntaxKind::BREAK_EXPR), 5);
        assert_eq!(count(SyntaxKind::CONTINUE_EXPR), 3);
        assert_eq!(count(SyntaxKind::BLOCK_EXPR), 2);
        assert_eq!(
            loops[1]
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            2,
            "a newline ends a valueless break before the following expression statement"
        );
    }

    #[test]
    fn loop_conditions_and_for_iterables_reject_direct_blocks() {
        let output = parse_file(concat!(
            "fn f(values: I32) {\n",
            "let invalid_while = while { true } {}\n",
            "let invalid_for = for item in { values } {}\n",
            "}\n",
        ));

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::BlockInCondition)
                .count(),
            2,
            "while conditions and for iterables apply the no-leading-block restriction"
        );
    }

    #[test]
    fn region_expressions_use_contextual_lookahead_and_keep_their_scope_shape() {
        let output = parse_file(concat!(
            "fn f(value: Bool) {\n",
            "let region = value\n",
            "let allocated = region arena { value }\n",
            "let condition = if region arena { value } {}\n",
            "let name = region\n",
            "let raw_name = r#region\n",
            "}\n",
        ));
        assert!(
            output.errors.is_empty(),
            "region expressions and the ordinary identifier should parse cleanly: {:?}",
            output.errors
        );
        let root = crate::SyntaxNode::new_root(output.root);
        let region_expressions = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::REGION_EXPR)
            .collect::<Vec<_>>();

        assert_eq!(region_expressions.len(), 2);
        assert!(
            region_expressions.iter().all(|region| {
                let children = region.children().collect::<Vec<_>>();
                children.len() == 2
                    && children[0].kind() == SyntaxKind::NAME
                    && children[0]
                        .first_token()
                        .is_some_and(|token| token.text() == "arena")
                    && children[1].kind() == SyntaxKind::BLOCK
            }),
            "each region expression must retain its binder before its body block"
        );
        assert!(
            root.descendants().any(|node| {
                node.kind() == SyntaxKind::PATH_EXPR
                    && node
                        .first_token()
                        .is_some_and(|token| token.text() == "region")
            }),
            "a bare region identifier remains a path expression"
        );
        assert!(
            root.descendants().any(|node| {
                node.kind() == SyntaxKind::PATH_EXPR
                    && node
                        .first_token()
                        .is_some_and(|token| token.text() == "r#region")
            }),
            "a raw `region` identifier remains a path expression"
        );
    }

    #[test]
    fn function_parser_builds_closures_and_async_blocks() {
        let source = concat!(
            "fn f(value: I32) {\n",
            "let handler = fn(x: I32, var state, _: I32) -> I32 { x }\n",
            "let moved = move fn(var state) { state }\n",
            "let task = async { value }\n",
            "let moved_task = async move { value }\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(
            output.errors.is_empty(),
            "closures and async blocks should parse cleanly: {:?}",
            output.errors
        );
        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };

        assert_eq!(count(SyntaxKind::CLOSURE_EXPR), 2);
        assert_eq!(count(SyntaxKind::CLOSURE_PARAM_LIST), 2);
        assert_eq!(count(SyntaxKind::CLOSURE_PARAM), 4);
        assert_eq!(count(SyntaxKind::RET_TYPE), 1);
        assert_eq!(count(SyntaxKind::ASYNC_BLOCK_EXPR), 2);
    }

    #[test]
    fn async_closures_report_the_specific_diagnostic() {
        let output = parse_file(concat!(
            "fn f(value: I32) {\n",
            "let invalid = async fn(x: I32) { x }\n",
            "let also_invalid = async move fn() {}\n",
            "}\n",
        ));

        assert_eq!(
            output
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::NoAsyncClosure, Code::NoAsyncClosure]
        );
        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                .count(),
            2,
            "the rejected closure forms remain available under ERROR nodes"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASYNC_BLOCK_EXPR)
                .count(),
            0
        );
    }

    #[test]
    fn malformed_closure_parameters_recover_and_preserve_following_syntax() {
        let output = parse_file(concat!(
            "fn f() {\n",
            "let callback = fn(x: I32, , y) { x }\n",
            "let after = 1\n",
            "}\n",
            "fn next() {}\n",
        ));
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "an empty closure parameter slot is malformed"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_PARAM)
                .count(),
            2,
            "only the valid parameters become CLOSURE_PARAM nodes"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "parameter-list recovery must preserve the following function"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2,
            "parameter-list recovery must preserve the later block statement"
        );
    }

    #[test]
    fn closure_headers_do_not_accept_newlines_before_the_return_type_or_body() {
        for source in [
            "fn f() {\nlet callback = fn() -> I32\n{ 1 }\n}\n",
            "fn f() {\nlet callback = fn()\n-> I32 { 1 }\n}\n",
        ] {
            let output = parse_file(source);
            assert!(
                output
                    .errors
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
                "a closure header may not continue across a newline: {source:?}"
            );
        }
    }

    #[test]
    fn missing_closure_parameter_close_recovers_at_the_body_brace() {
        let output = parse_file(concat!(
            "fn f() {\n",
            "let callback = fn(x { x }\n",
            "let after = 1\n",
            "}\n",
            "fn next() {}\n",
        ));
        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "the missing parameter-list close must be diagnosed"
        );
        assert_eq!(count(SyntaxKind::CLOSURE_PARAM), 1);
        assert_eq!(count(SyntaxKind::LET_STMT), 2);
        assert_eq!(
            count(SyntaxKind::FN_DECL),
            2,
            "the closure body, following statement, and next item must stay intact"
        );
    }

    #[test]
    fn missing_closure_parameter_comma_keeps_the_following_parameter() {
        let output = parse_file("fn f() { let callback = fn(x y: I32) { x } }\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "the missing comma must be diagnosed"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_PARAM)
                .count(),
            2,
            "both valid parameters should remain available in the recovered CST"
        );
    }

    #[test]
    fn deeply_nested_closures_recover_at_the_body_and_preserve_later_items() {
        let mut source = String::from("fn f() {\nlet nested = ");
        for _ in 0..300 {
            source.push_str("fn() { ");
        }
        source.push('0');
        for _ in 0..300 {
            source.push_str(" }");
        }
        source.push_str("\n}\nfn next() {}\n");

        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded),
            "deep closure bodies must use bounded block recovery"
        );
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code != Code::ExpectedToken),
            "closure recovery must not leave the return type or body to cascade"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "recovery must preserve the following top-level function"
        );
    }

    #[test]
    fn function_parser_builds_if_conditions_and_else_branches() {
        let source = concat!(
            "fn f(condition: Bool, value: Option<I32>) {\n",
            "let plain = if condition { 1 } else { 2 }\n",
            "let chain = if condition { 1 } else if condition { 2 } else { 3 }\n",
            "let matched = if let Some(item) = value { item } else { 0 }\n",
            "let next_line = if condition {} \n",
            "    else {}\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(
            output.errors.is_empty(),
            "valid if and if-let expressions should parse cleanly: {:?}",
            output.errors
        );

        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };
        assert_eq!(count(SyntaxKind::IF_EXPR), 5);
        assert_eq!(count(SyntaxKind::CONDITION), 5);
        assert_eq!(count(SyntaxKind::LET_CONDITION), 1);
        assert_eq!(count(SyntaxKind::ELSE_BRANCH), 5);
    }

    #[test]
    fn condition_block_restriction_resets_in_delimited_expressions() {
        let output = parse_file(concat!(
            "fn f(condition: Bool, index: USize) {\n",
            "if ({ true }) {}\n",
            "if check({ true }) {}\n",
            "if [{}] {}\n",
            "if index.. {}\n",
            "if .. {}\n",
            "if return {}\n",
            "if unsafe {} {}\n",
            "}\n",
        ));
        assert!(
            output.errors.is_empty(),
            "parenthesized, array, block, and optional range operands clear or respect the condition restriction: {:?}",
            output.errors
        );

        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::IF_EXPR)
                .count(),
            7
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::RANGE_EXPR)
                .count(),
            2
        );
    }

    #[test]
    fn a_block_at_the_start_of_a_condition_reports_block_in_condition() {
        let output = parse_file("fn f() { if { true } {} }\n");
        assert_eq!(
            output
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BlockInCondition]
        );
        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK_EXPR)
                .count(),
            1,
            "the invalid condition block must remain available for parenthesizing"
        );
    }

    #[test]
    fn required_condition_operands_still_parse_block_expressions() {
        let output = parse_file("fn f(condition: Bool) { if condition and { true } {} }\n");
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::BlockInCondition)
                .count(),
            1,
            "a required binary operand beginning with a block still reports the restriction"
        );
    }

    #[test]
    fn deeply_nested_else_if_chains_report_the_shared_nesting_limit() {
        let chain = "if condition {} else ".repeat(520) + "if condition {}";
        let source = format!("fn f() {{ {chain} }}\n");
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded),
            "recursive else-if parsing must use the shared nesting limit"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1,
            "over-depth else-if recovery must preserve the containing function"
        );
    }

    #[test]
    fn function_parser_builds_match_arms_guards_and_nested_matches() {
        let source = concat!(
            "fn f(value: Option<I32>) {\n",
            "let simple = match value {\n",
            "Some(item) if item > 0 => item,\n",
            "Some(_) => 0\n",
            "None => { 0 }\n",
            "}\n",
            "let empty = match value {}\n",
            "let nested = match value { .Some(item) => match item {\n",
            "0 => 1,\n",
            "_ => 2,\n",
            "} }\n",
            "}\n",
        );
        let output = parse_file(source);
        assert!(
            output.errors.is_empty(),
            "valid match arms, guards, and nested matches should parse cleanly: {:?}",
            output.errors
        );

        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };
        assert_eq!(count(SyntaxKind::MATCH_EXPR), 4);
        assert_eq!(count(SyntaxKind::MATCH_ARM_LIST), 4);
        assert_eq!(count(SyntaxKind::MATCH_ARM), 6);
        assert_eq!(count(SyntaxKind::MATCH_GUARD), 1);
        assert_eq!(count(SyntaxKind::BLOCK_EXPR), 1);
    }

    #[test]
    fn match_scrutinees_apply_and_clear_the_block_condition_restriction() {
        let valid = parse_file(concat!(
            "fn f(value: Bool) {\n",
            "let parenthesized = match ({ true }) { true => 1, false => 0 }\n",
            "let open_range = match value.. { _ => 0 }\n",
            "}\n",
        ));
        assert!(
            valid.errors.is_empty(),
            "parentheses clear the scrutinee restriction and open ranges stop at the arm list: {:?}",
            valid.errors
        );

        let invalid = parse_file("fn f() { let result = match { true } { true => 1 } }\n");
        assert_eq!(
            invalid
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BlockInCondition]
        );
    }

    #[test]
    fn match_arm_recovery_preserves_a_later_arm_after_a_missing_separator() {
        use crate::ast::AstNode;

        let output =
            parse_file("fn f(value: Option<I32>) { match value { Some(1) => 1 None => 2 } }\n");
        let root = crate::SyntaxNode::new_root(output.root);
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "adjacent same-line arms require a separator"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::MATCH_ARM)
                .count(),
            2,
            "separator recovery should continue at the next pattern"
        );
        let match_expr = root
            .descendants()
            .find_map(crate::ast::MatchExpr::cast)
            .expect("recovered match expression");
        let arms = match_expr
            .arms()
            .expect("match arm list")
            .arms()
            .collect::<Vec<_>>();
        assert_eq!(
            arms.len(),
            2,
            "generated iteration should skip recovery nodes"
        );
        assert!(arms.iter().all(|arm| arm.expression().is_some()));
    }

    #[test]
    fn malformed_match_arm_starts_do_not_create_empty_arm_nodes() {
        let output = parse_file("fn f(value: I32) { match value { . => 1, _ => 2 } }\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "an implicit member pattern requires an uppercase name after the dot"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::MATCH_ARM)
                .count(),
            1,
            "the malformed arm must recover without completing an empty MATCH_ARM"
        );
    }

    #[test]
    fn mismatched_match_arm_delimiters_preserve_following_statements_and_items() {
        let source =
            "fn f(value: I32) { match value { (] => 1\nlet later = 2\n}\n}\nfn next() {}\n";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        let parsed = parser.parse_source_file();
        assert!(
            parsed.is_ok(),
            "malformed match arm should recover: {parsed:?}; cursor {} at {:?}; remaining {:?}",
            parser.position,
            parser.current(),
            parser.tokens[parser.position..]
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>()
        );
        let events = parser.into_events().expect("complete source event stream");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("source tree builds");
        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            0,
            "the statement inside the malformed arm tail remains in the recovery node"
        );
        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .any(|node| {
                    let range = node.text_range();
                    source
                        .get(range.start().to_usize()..range.end().to_usize())
                        .is_some_and(|text| text.contains("let later = 2"))
                }),
            "recovery must retain the later statement's source in the CST"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "a malformed arm must not consume the following function"
        );
    }

    #[test]
    fn malformed_match_tail_with_an_unclosed_block_preserves_later_items() {
        let source = concat!(
            "fn f(value: Bool) { match value { (] => 1\n",
            "let later = if value {\n",
            "2\n",
            "}\n",
            "}\n",
            "fn next() {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let match_arm_list = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::MATCH_ARM_LIST)
            .expect("match arm list remains in the tree");
        let match_arm_list_range = match_arm_list.text_range();
        let match_arm_list_text =
            &source[match_arm_list_range.start().to_usize()..match_arm_list_range.end().to_usize()];

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "an unclosed block in a malformed arm tail must not consume the next function"
        );
        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .any(|node| {
                    let range = node.text_range();
                    source
                        .get(range.start().to_usize()..range.end().to_usize())
                        .is_some_and(|text| text.contains("let later = if value"))
                }),
            "the malformed tail must remain in the recovery tree"
        );
        assert!(
            match_arm_list_text.ends_with("}\n}"),
            "the nested block and match list retain their own closing braces: {match_arm_list_text:?}"
        );
        assert!(
            !match_arm_list_text.contains("fn next"),
            "the following top-level function must remain outside the match list"
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "the missing function block brace must be diagnosed at the next item"
        );
        assert!(
            !output.errors.iter().any(
                |diagnostic| diagnostic.message == "unexpected closing delimiter at file scope"
            ),
            "a properly owned match-list brace must not leak into file scope"
        );
    }

    #[test]
    fn malformed_match_tail_stops_before_a_top_level_item_without_closers() {
        let source = "fn f(value: I32) { match value { (] => 1\nfn next() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let match_expression = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::MATCH_EXPR)
            .expect("the malformed match expression remains in the tree");
        let range = match_expression.text_range();
        let match_text = &source[range.start().to_usize()..range.end().to_usize()];
        let next_item_start = source.find("fn next").expect("next item is present");
        let synchronization_offset = source[..next_item_start]
            .rfind('\n')
            .expect("the top-level item starts on a new line");
        let empty_error_offsets = root
            .descendants()
            .filter(|node| {
                let range = node.text_range();
                node.kind() == SyntaxKind::ERROR && range.is_empty()
            })
            .map(|node| node.text_range().start().to_usize())
            .collect::<Vec<_>>();
        let empty_error_nodes = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ERROR && node.text_range().is_empty())
            .collect::<Vec<_>>();
        let pointers = empty_error_nodes
            .iter()
            .map(|node| {
                crate::SyntaxNodePtr::new(sai_vfs::FileId::new(1), node)
                    .expect("unbound syntax roots accept an explicit file identity")
            })
            .collect::<Vec<_>>();
        assert!(
            pointers.len() >= 3,
            "fixture must include the coincident empty recovery nodes"
        );
        assert_eq!(
            pointers.iter().collect::<HashSet<_>>().len(),
            pointers.len(),
            "pre-order node indices must distinguish nodes with identical kind and range"
        );
        for pointer in &pointers {
            let resolved = pointer
                .resolve(sai_vfs::FileId::new(1), &root)
                .expect("pointer should resolve within its tree");
            assert_eq!(
                &crate::SyntaxNodePtr::new(sai_vfs::FileId::new(1), &resolved)
                    .expect("unbound syntax roots accept an explicit file identity"),
                pointer
            );
        }

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "recovery must leave the following top-level function available to the item parser"
        );
        assert!(
            !match_text.contains("fn next"),
            "the top-level function must remain outside malformed match recovery"
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "both missing closing delimiters must be diagnosed at the synchronization point"
        );
        assert!(
            empty_error_offsets
                .iter()
                .filter(|offset| **offset == synchronization_offset)
                .count()
                >= 3,
            "the malformed arm and both missing braces retain zero-width ERROR nodes at the synchronization boundary; empty error offsets {empty_error_offsets:?}, boundary offset {synchronization_offset}"
        );
        assert!(
            !output.errors.iter().any(
                |diagnostic| diagnostic.message == "unexpected closing delimiter at file scope"
            ),
            "recovery must not leave an unexpected closer at file scope"
        );
    }

    #[test]
    fn malformed_nested_match_tail_propagates_recovery_to_the_file_scope() {
        let source = concat!(
            "fn f(value: Bool) {\n",
            "if value {\n",
            "match value { (] => 1\n",
            "fn next() {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let functions = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FN_DECL)
            .collect::<Vec<_>>();

        assert_eq!(
            functions.len(),
            2,
            "nested blocks must propagate the recovery boundary so the next function stays at file scope"
        );
        assert!(
            functions[1]
                .parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::SOURCE_FILE),
            "the recovered function must be a direct file item"
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "the missing match-list and block closers must be diagnosed"
        );
    }

    #[test]
    fn function_parser_builds_postfix_and_array_expressions() {
        let source = concat!(
            "fn f() {\n",
            "let call = g<I32>(1, name: 2)\n",
            "let method = value.convert<U32>(4)\n",
            "let field = value.field\n",
            "let tuple = value.0\n",
            "let index = values[0]\n",
            "let propagated = result?\n",
            "let variant = .Some(1)\n",
            "let array = [1, 2,]\n",
            "let awaited = future.await\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        for (kind, expected) in [
            (SyntaxKind::CALL_EXPR, 3),
            (SyntaxKind::FIELD_EXPR, 2),
            (SyntaxKind::TUPLE_INDEX_EXPR, 1),
            (SyntaxKind::INDEX_EXPR, 1),
            (SyntaxKind::TRY_EXPR, 1),
            (SyntaxKind::ARRAY_EXPR, 1),
            (SyntaxKind::AWAIT_EXPR, 1),
            (SyntaxKind::IMPLICIT_MEMBER_EXPR, 1),
            (SyntaxKind::GENERIC_ARG_LIST, 2),
            (SyntaxKind::ARG_LIST, 3),
            (SyntaxKind::ARG, 3),
            (SyntaxKind::NAMED_ARG, 1),
        ] {
            assert_eq!(
                kinds.iter().filter(|found| **found == kind).count(),
                expected,
                "unexpected count for {kind:?}"
            );
        }
        assert!(output.errors.is_empty());

        for source in [
            "fn f() { g<I32> }\n",
            "fn f() { g<foo.Bar> }\n",
            "fn f() { g<-3> }\n",
            "fn f() { g<(I32, U32)> }\n",
        ] {
            let output = parse_file(source);
            assert!(
                output.errors.is_empty(),
                "valid generic expression arguments should parse: {source}"
            );
            assert!(
                crate::SyntaxNode::new_root(output.root)
                    .descendants()
                    .any(|node| node.kind() == SyntaxKind::GENERIC_ARG_LIST),
                "generic arguments should be represented in the CST: {source}"
            );
        }

        assert!(parse_file("fn f() { a < b }\n").errors.is_empty());
        let comparison = parse_file("fn f() { a < B > c }\n");
        assert_eq!(
            comparison
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ChainedComparison)
                .count(),
            1
        );

        let (snapshot_file, lexed) = lex_file("fn f() { g<(b)> }\n");
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        parser
            .parse_source_file()
            .expect("failed generic speculation falls back to comparisons");
        parser
            .into_events()
            .expect("fallback consumes the malformed comparison expression");

        let malformed = parse_file("fn f() { g<B + C>(d) }\n");
        assert_eq!(malformed.errors.len(), 1);
        assert_eq!(
            malformed.errors[0].code,
            Code::ChainedComparison,
            "an invalid type argument must fall back to comparison parsing"
        );

        assert!(
            parse_file("fn f() { g<(B > ()) }\n").errors.is_empty(),
            "a comparison operator inside a grouped expression must not close generic lookahead"
        );

        let (snapshot_file, lexed) = lex_file("fn f() { g<I32 U32> }\n");
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        parser
            .parse_source_file()
            .expect("missing generic-argument commas fall back to comparisons");
        parser
            .into_events()
            .expect("fallback consumes the malformed comparison expression");
    }

    #[test]
    fn postfix_syntax_reports_tuple_indices_and_nonfinal_base_arguments() {
        let output = parse_file("fn f() { value.0x10\nRecord(..base, field: value)\n}\n");
        assert_eq!(
            output
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::InvalidTupleIndex, Code::BaseArgNotLast]
        );

        let other_invalid_indices = parse_file("fn f() { value.01\nvalue.1_0\n}\n");
        assert_eq!(
            other_invalid_indices
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::InvalidTupleIndex)
                .count(),
            2,
            "leading zeroes and underscores are invalid tuple-index spellings"
        );

        let final_base = parse_file("fn f() { Record(field: value, ..base) }\n");
        assert!(
            final_base
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code != Code::BaseArgNotLast),
            "a final base argument must be accepted"
        );

        let followed_by_multiple =
            parse_file("fn f() { Record(..base, field: value, other: value) }\n");
        assert_eq!(
            followed_by_multiple
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::BaseArgNotLast)
                .count(),
            1,
            "each non-final base argument should be diagnosed exactly once"
        );

        let followed_by_base = parse_file("fn f() { Record(..first, ..second) }\n");
        assert_eq!(
            followed_by_base
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BaseArgNotLast],
            "only the earlier base argument is non-final"
        );

        let missing_comma = parse_file("fn f() { Record(..base field: value) }\n");
        assert_eq!(
            missing_comma
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BaseArgNotLast, Code::ExpectedToken],
            "a following argument without a comma still makes the base argument non-final"
        );

        let malformed_later_arg = parse_file("fn f() { Record(..base, field: ) }\n");
        assert_eq!(
            malformed_later_arg
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::BaseArgNotLast, Code::ExpectedExpression],
            "diagnostics must remain in source-span order when a later argument is malformed"
        );

        let empty_argument_before_later_arg = parse_file("fn f() { Record(..base, , field: ) }\n");
        assert_eq!(
            empty_argument_before_later_arg
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                Code::BaseArgNotLast,
                Code::ExpectedExpression,
                Code::ExpectedExpression
            ],
            "base-argument diagnostics precede errors in empty and later arguments"
        );

        let malformed_base_expression = parse_file("fn f() { Record(..call(1 + ), field: ) }\n");
        let diagnostic_ranges = malformed_base_expression
            .errors
            .iter()
            .map(|diagnostic| {
                diagnostic
                    .primary()
                    .expect("parser diagnostic has a primary span")
                    .span
                    .range
            })
            .collect::<Vec<_>>();
        assert!(
            diagnostic_ranges.windows(2).all(|pair| pair[0] <= pair[1]),
            "tree-build diagnostics should be sorted by source span"
        );
        assert_eq!(
            malformed_base_expression.errors[0].code,
            Code::BaseArgNotLast,
            "an earlier base-argument diagnostic should precede errors in its expression"
        );
    }

    #[test]
    fn malformed_call_arguments_recover_without_empty_arg_nodes() {
        let output = parse_file("fn f() { call(, 1) }\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression)
        );
        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ARG)
                .all(|node| !node.text_range().is_empty())
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ARG)
                .count(),
            1
        );
    }

    #[test]
    fn malformed_nested_list_recovery_preserves_enclosing_closers() {
        let calls = parse_file("fn f() { call(@( }\nfn g() {}\n");
        let call_root = crate::SyntaxNode::new_root(calls.root);
        assert_eq!(
            call_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "an unmatched parenthesis in a call argument must not consume the block close"
        );

        let mismatched = parse_file("fn f() { call(@([) }\nfn g() {}\n");
        let mismatched_root = crate::SyntaxNode::new_root(mismatched.root);
        assert_eq!(
            mismatched_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "a mismatched inner bracket must not consume the call's closing parenthesis"
        );

        let arrays = parse_file("fn f() { let first = [@( ]\nlet second = 1\n}\n");
        let array_root = crate::SyntaxNode::new_root(arrays.root);
        assert_eq!(
            array_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2,
            "an unmatched parenthesis in an array element must not consume the array close"
        );

        let newline = parse_file("fn f() { call(@(\nlet later = 2\n}\nfn g() {}\n");
        let newline_root = crate::SyntaxNode::new_root(newline.root);
        assert_eq!(
            newline_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1,
            "list recovery must stop at a statement-ending newline even inside an unmatched group: {newline_root:?}; errors={:?}",
            newline.errors
        );
        assert_eq!(
            newline_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );

        let multiline_continuation =
            parse_file("fn f() {\ncall(@(value,\n2), 3)\nlet later = 4\n}\n");
        let continuation_root = crate::SyntaxNode::new_root(multiline_continuation.root);
        assert_eq!(
            continuation_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ARG)
                .count(),
            1,
            "recovery must pass a comma-continued line inside a balanced nested group"
        );
        assert_eq!(
            continuation_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1,
            "the statement after the recovered call must remain parseable"
        );
        assert!(
            multiline_continuation
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::ExpectedExpression),
            "a recoverable malformed argument should not cause cascading delimiter errors"
        );

        let multiline_nested_close =
            parse_file("fn f() {\ncall(@(value\n), 3)\nlet later = 4\n}\n");
        let nested_close_root = crate::SyntaxNode::new_root(multiline_nested_close.root);
        assert_eq!(
            nested_close_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ARG)
                .count(),
            1,
            "a matching nested closer on a new line must not close the outer call"
        );
        assert_eq!(
            nested_close_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert!(
            multiline_nested_close
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::ExpectedExpression),
            "the nested closer should prevent cascading call diagnostics"
        );

        let balanced = parse_file("fn f() { call(@(value), 1)\nlet later = 2\n}\n");
        let balanced_root = crate::SyntaxNode::new_root(balanced.root);
        assert_eq!(
            balanced_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ARG)
                .count(),
            1,
            "balanced inner groups must be skipped before synchronizing on the call comma"
        );
        assert_eq!(
            balanced_root
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn source_file_parses_basic_block_statements_and_expressions() {
        let source = concat!(
            "fn main() {\n",
            "let total: I32 = 1 + 2 * 3\n",
            "var current = total\n",
            "current += 4\n",
            "assert current > 0\n",
            "return current\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let kinds = root
            .descendants()
            .map(|node| node.kind())
            .collect::<Vec<_>>();

        for (kind, expected) in [
            (SyntaxKind::BLOCK, 1),
            (SyntaxKind::LET_STMT, 1),
            (SyntaxKind::VAR_STMT, 1),
            (SyntaxKind::ASSIGN_STMT, 1),
            (SyntaxKind::ASSERT_STMT, 1),
            (SyntaxKind::EXPR_STMT, 1),
            (SyntaxKind::RETURN_EXPR, 1),
            (SyntaxKind::BIN_EXPR, 3),
        ] {
            assert_eq!(
                kinds.iter().filter(|found| **found == kind).count(),
                expected,
                "unexpected count for {kind:?}"
            );
        }
        assert!(output.errors.is_empty());
    }

    #[test]
    fn expression_parser_builds_precedence_and_left_associative_nodes() {
        let output = parse_file("fn f() { a + b * c }\n");
        let root = crate::SyntaxNode::new_root(output.root);
        let statement = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::EXPR_STMT)
            .expect("expression statement");
        let outer = statement
            .children()
            .find(|node| node.kind() == SyntaxKind::BIN_EXPR)
            .expect("outer addition");
        let outer_children = outer.children().map(|node| node.kind()).collect::<Vec<_>>();
        assert_eq!(
            outer_children,
            [SyntaxKind::PATH_EXPR, SyntaxKind::BIN_EXPR]
        );
        let inner = outer
            .children()
            .find(|node| node.kind() == SyntaxKind::BIN_EXPR)
            .expect("multiplication binds more tightly");
        assert_eq!(
            inner.children().map(|node| node.kind()).collect::<Vec<_>>(),
            [SyntaxKind::PATH_EXPR, SyntaxKind::PATH_EXPR]
        );
        assert!(output.errors.is_empty());
    }

    #[test]
    fn expression_parser_enforces_prefix_binding_power() {
        let accepted = parse_file("fn f() { not a == b and c }\n");
        let accepted_root = crate::SyntaxNode::new_root(accepted.root);
        let prefix = accepted_root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::PREFIX_EXPR)
            .expect("logical not expression");
        assert!(
            prefix
                .descendants()
                .any(|node| node.kind() == SyntaxKind::BIN_EXPR),
            "not binds less tightly than comparisons"
        );
        assert!(accepted.errors.is_empty());

        let rejected = parse_file(concat!(
            "fn f() {\n",
            "true == not false\n",
            "x * ..y\n",
            "a.. + b\n",
            "}\n",
        ));
        assert_eq!(
            rejected
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedExpression)
                .count(),
            3,
            "{:#?}",
            rejected.errors
        );
    }

    #[test]
    fn expression_parser_keeps_casts_as_recovery_nodes() {
        let output = parse_file("fn f() { a * b as I32 }\n");
        let root = crate::SyntaxNode::new_root(output.root);
        let multiplication = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::BIN_EXPR)
            .expect("multiplication expression");
        assert_eq!(
            multiplication
                .children()
                .map(|node| node.kind())
                .collect::<Vec<_>>(),
            [SyntaxKind::PATH_EXPR, SyntaxKind::CAST_EXPR]
        );
        assert!(output.errors.is_empty());
    }

    #[test]
    fn expression_parser_glues_composite_operators_and_reports_nonassociative_chains() {
        let source = concat!(
            "fn f() {\n",
            "let shifted = 8 >> 1\n",
            "let compared = shifted >= 4\n",
            "var assigned = shifted\n",
            "assigned >>= 1\n",
            "a < b < c\n",
            "d..e..f\n",
            "a && b\n",
            "}\n",
        );
        let output = parse_file(source);
        let codes = output
            .errors
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            [Code::ChainedComparison, Code::ChainedRange, Code::UseAndOr]
        );
        let root = crate::SyntaxNode::new_root(output.root);
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::RANGE_EXPR)
        );
        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ASSIGN_STMT)
                .count()
                == 1
        );
    }

    #[test]
    fn expression_parser_reports_a_chain_after_a_prefix_range() {
        let output = parse_file("fn f() { ..a..b }\n");

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ChainedRange)
                .count(),
            1
        );
    }

    #[test]
    fn malformed_block_expressions_recover_without_invalid_delimiter_nodes() {
        let output = parse_file("fn f() { let value = (1 + 2 }\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
    }

    #[test]
    fn block_errors_report_missing_expressions_and_initializers() {
        let output = parse_file("fn f() {\nlet missing\nlet value = 1 +\n}\n");

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::MissingInitializer)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression)
        );
    }

    #[test]
    fn block_reports_nested_functions_and_keeps_their_syntax_under_error() {
        let source = concat!(
            "fn outer() {\n",
            "fn inner() {}\n",
            "let after = 1\n",
            "}\n",
            "fn sibling() {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        let item_diagnostics = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
            .collect::<Vec<_>>();
        assert_eq!(item_diagnostics.len(), 1);
        assert!(item_diagnostics[0].children().iter().any(|child| {
            child.kind == sai_diagnostics::ChildKind::Help
                && child.message.contains("let inner = fn(..)")
        }));
        assert_eq!(
            item_diagnostics[0]
                .primary()
                .expect("item-in-block location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("fn inner").expect("nested function")
        );
        assert!(root.descendants().any(|node| {
            node.kind() == SyntaxKind::ERROR
                && node
                    .descendants()
                    .any(|child| child.kind() == SyntaxKind::FN_DECL)
                && source
                    .get(node.text_range().start().to_usize()..node.text_range().end().to_usize())
                    .is_some_and(|text| text.starts_with("fn inner"))
        }));
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            3
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn function_expression_at_block_start_is_not_reported_as_an_item() {
        let output = parse_file("fn outer() { fn() { value() } }\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code != Code::ItemInBlock)
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
        );
    }

    #[test]
    fn statement_attributes_are_rejected_once_and_the_statement_is_parsed() {
        let source = concat!(
            "fn f() {\n",
            "@allow(foo)\n",
            "@deny(bar)\n",
            "let value = 1\n",
            "@warn(lint)\n",
            "value()\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        let invalid_targets = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
            .collect::<Vec<_>>();
        assert_eq!(invalid_targets.len(), 2);
        assert_eq!(
            invalid_targets[0]
                .primary()
                .expect("invalid attribute target location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("@allow").expect("first attribute")
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn attributed_item_in_a_block_uses_item_diagnostic_and_recovers_later_statements() {
        let source = concat!(
            "fn outer() {\n",
            "@allow(foo)\n",
            "@deny(bar)\n",
            "fn inner() {}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        let item_diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::ItemInBlock)
            .expect("nested attributed item diagnostic");
        assert_eq!(
            item_diagnostic
                .primary()
                .expect("item-in-block location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("@allow").expect("first item attribute")
        );
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code != Code::InvalidAttributeTarget)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn unsupported_block_items_are_recovered_as_balanced_error_nodes() {
        let source = concat!(
            "fn outer() {\n",
            "type Alias<T> = (T, T)\n",
            "let first = 1\n",
            "type Item<T>\n",
            "where T: Clone\n",
            "{\n",
            "value: T\n",
            "}\n",
            "unsafe trait Marker\n",
            "{\n",
            "}\n",
            "test \"later\" {\n",
            "assert true\n",
            "}\n",
            "let last = 2\n",
            "}\n",
            "fn following() {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            4
        );
        let error_texts = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ERROR)
            .map(|node| {
                source
                    .get(node.text_range().start().to_usize()..node.text_range().end().to_usize())
                    .expect("error node range is within source")
            })
            .collect::<Vec<_>>();
        assert_eq!(error_texts.len(), 4, "{error_texts:?}");
        assert!(
            error_texts
                .iter()
                .any(|text| { text.starts_with("type Alias<T> = (T, T)") })
        );
        assert!(
            error_texts
                .iter()
                .any(|text| { *text == "type Item<T>\nwhere T: Clone\n{\nvalue: T\n}" })
        );
        assert!(
            error_texts
                .iter()
                .any(|text| { *text == "unsafe trait Marker\n{\n}" })
        );
        assert!(
            error_texts
                .iter()
                .any(|text| { text.starts_with("test \"later\" {") })
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn effect_covers_recovery_keeps_a_following_block_expression() {
        let source = concat!(
            "fn outer() {\n",
            "effect marker\n",
            "covers {io}\n",
            "{\n",
            "assert true\n",
            "}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        let error_text = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .map(|node| {
                &source[node.text_range().start().to_usize()..node.text_range().end().to_usize()]
            })
            .expect("effect item error node");
        assert_eq!(error_text, "effect marker\ncovers {io}");
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK_EXPR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn attributes_before_expression_list_items_report_invalid_targets() {
        let source = concat!(
            "fn outer() {\n",
            "consume(@first value, named: @named value, ..@base value)\n",
            "let array = [@array value, 2]\n",
            "let tuple = (@tuple value, 2)\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        let invalid_targets = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
            .collect::<Vec<_>>();
        assert_eq!(invalid_targets.len(), 5);
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
                .count(),
            5
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::NAMED_ARG)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BASE_ARG)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn attributed_argument_does_not_hide_a_nonfinal_base_argument() {
        let output = parse_file("fn f() { value(..base, @allow(foo) field: value) }\n");

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::BaseArgNotLast)
        );
    }

    #[test]
    fn unclosed_generic_item_recovery_preserves_later_statements() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken<T\n",
            "where T: Clone\n",
            "{\n",
            "value: T\n",
            "}\n",
            "let after = 1\n",
            "}\n",
            "fn following() {}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("malformed item error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Broken<T\nwhere T: Clone\n{\nvalue: T\n}"
        );
    }

    #[test]
    fn unclosed_generic_item_stops_before_an_attribute_on_a_later_statement() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken<T\n",
            "@allow(UNUSED_IMPORTS)\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        let invalid_attribute = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::InvalidAttributeTarget)
            .expect("attribute before a statement is rejected");
        assert_eq!(
            invalid_attribute
                .primary()
                .expect("invalid attribute location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("@allow").expect("attribute source position")
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| {
                node.kind() == SyntaxKind::ERROR
                    && &source
                        [node.text_range().start().to_usize()..node.text_range().end().to_usize()]
                        == "type Broken<T"
            })
            .expect("malformed item recovery stops before the attributed statement");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Broken<T"
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn unclosed_generic_item_stops_before_a_later_statement() {
        let source = "fn outer() {\ntype Broken<T\nlet after = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("malformed item error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Broken<T"
        );
    }

    #[test]
    fn malformed_generic_parameter_list_preserves_a_function_after_a_comma() {
        let source = "type Broken<T,\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_argument_list_preserves_a_function_after_a_comma() {
        let source = "type Broken = Box<T,\nfn later() {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::TYPE_DECL)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_parameter_list_preserves_a_block_statement_after_a_comma() {
        let source = "fn outer() {\ntype Broken<T,\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_argument_list_preserves_a_block_statement_after_a_comma() {
        let source = "fn outer() {\ntype Broken = Box<T,\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_parameter_list_preserves_a_const_item_after_a_comma() {
        let source = "fn outer() {\ntype Broken<T,\nconst NEXT: USize = 1\nlet later = 2\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_parameter_list_preserves_a_call_statement_after_a_comma() {
        let source = "fn outer() {\ntype Broken<T,\nSome()\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_argument_list_preserves_a_call_statement_after_a_comma() {
        let source = "fn outer() {\ntype Broken = Box<T,\nSome()\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_argument_list_preserves_a_method_call_after_a_comma() {
        let source = "fn outer() {\ntype Broken = Box<T,\nvalue.method()\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn generic_argument_recovery_keeps_a_multiline_function_pointer_type() {
        let source = concat!(
            "type Callback = Pair<T,\n",
            "fn(I32),\n",
            "fn() -> I32 effects { io },\n",
            "pkg.Type>\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_PTR_TYPE)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_argument_list_preserves_closure_statements_after_a_comma() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken = Box<T,\n",
            "fn() {}\n",
            "fn() -> I32 { 1 }\n",
            "let later = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_closure_return_type_keeps_its_body_after_generic_list_recovery() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken = Box<T,\n",
            "fn() -> { let lost = 1 }\n",
            "let later = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|error| error.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::CLOSURE_EXPR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_generic_argument_list_preserves_an_unsafe_block_statement() {
        let source = "fn outer() {\ntype Broken = Box<T,\nunsafe {}\nlet later = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::UNSAFE_BLOCK_EXPR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn multiline_const_generic_item_is_not_split_at_const_parameter() {
        let source = concat!(
            "fn outer() {\n",
            "type Item<\n",
            "const N: USize,\n",
            "T\n",
            ">\n",
            "where T: Clone\n",
            "{\n",
            "value: [T; N]\n",
            "}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("unsupported type declaration error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Item<\nconst N: USize,\nT\n>\nwhere T: Clone\n{\nvalue: [T; N]\n}"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn multiline_const_generic_impl_is_kept_as_one_item() {
        let source = concat!(
            "fn outer() {\n",
            "impl<\n",
            "const N: USize,\n",
            "T\n",
            "> Trait for Box<T, N> {}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("unsupported impl error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "impl<\nconst N: USize,\nT\n> Trait for Box<T, N> {}"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn generic_bound_effect_set_does_not_end_item_recovery() {
        let source = concat!(
            "fn outer() {\n",
            "type Item<\n",
            "T: Fn() effects {io}\n",
            ">\n",
            "{\n",
            "callback: T\n",
            "}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("unsupported type declaration error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Item<\nT: Fn() effects {io}\n>\n{\ncallback: T\n}"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn associated_type_binding_keeps_nested_generic_depth_during_item_recovery() {
        let source = concat!(
            "fn outer() {\n",
            "type Item<\n",
            "T: Trait<Assoc = Vec<I32>>\n",
            ">\n",
            "{\n",
            "value: T\n",
            "}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("unsupported type declaration error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Item<\nT: Trait<Assoc = Vec<I32>>\n>\n{\nvalue: T\n}"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn unclosed_generic_item_stops_before_a_later_literal_expression() {
        let source = "fn outer() {\ntype Broken<T\n42\nlet after = 1\n}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("malformed item error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Broken<T"
        );
    }

    #[test]
    fn unclosed_nested_generic_item_stops_before_a_later_literal_expression() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken<T: Trait<U\n",
            "42\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("malformed item error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Broken<T: Trait<U"
        );
    }

    #[test]
    fn unclosed_generic_item_does_not_absorb_a_following_const_item() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken<T\n",
            "const NEXT: I32 = 1\n",
            "let after = 2\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let error_texts = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ERROR)
            .map(|node| {
                &source[node.text_range().start().to_usize()..node.text_range().end().to_usize()]
            })
            .collect::<Vec<_>>();

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            2
        );
        assert!(error_texts.contains(&"type Broken<T"));
        assert!(error_texts.contains(&"const NEXT: I32 = 1"));
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn unclosed_generic_item_stops_before_a_constructor_call() {
        let source = concat!(
            "fn outer() {\n",
            "type Broken<T\n",
            "Some()\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            1
        );
        let item_error = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::ERROR)
            .expect("malformed item error node");
        assert_eq!(
            &source[item_error.text_range().start().to_usize()
                ..item_error.text_range().end().to_usize()],
            "type Broken<T"
        );
    }

    #[test]
    fn generic_nested_function_help_does_not_suggest_a_closure() {
        let source = concat!(
            "fn outer() {\n",
            "fn identity<T>(value: T) -> T { value }\n",
            "}\n",
        );
        let output = parse_file(source);
        let diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::ItemInBlock)
            .expect("nested generic function diagnostic");

        let help = diagnostic
            .children()
            .iter()
            .find(|child| child.kind == sai_diagnostics::ChildKind::Help)
            .expect("generic function guidance");
        assert!(help.message.contains("generic function `identity`"));
        assert!(help.message.contains("move it outside this block"));
        assert!(!help.message.contains("fn(..)"));
    }

    #[test]
    fn unsafe_and_async_nested_functions_do_not_suggest_plain_closures() {
        let source = concat!(
            "fn outer() {\n",
            "unsafe fn dangerous() {}\n",
            "async fn future() {}\n",
            "}\n",
        );
        let output = parse_file(source);
        let diagnostics = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
            .collect::<Vec<_>>();

        assert_eq!(diagnostics.len(), 2);
        let help_for = |name: &str| {
            diagnostics
                .iter()
                .find_map(|diagnostic| {
                    diagnostic
                        .children()
                        .iter()
                        .find(|child| {
                            child.kind == sai_diagnostics::ChildKind::Help
                                && child.message.contains(name)
                        })
                        .map(|child| child.message.as_str())
                })
                .expect("qualified function guidance")
        };
        for (name, qualifier) in [("dangerous", "unsafe"), ("future", "async")] {
            let help = help_for(name);
            assert!(help.contains(qualifier));
            assert!(help.contains("outside this block"));
            assert!(!help.contains("fn(..)"));
        }
    }

    #[test]
    fn visibility_qualified_nested_functions_do_not_suggest_local_closures() {
        let source = concat!(
            "fn outer() {\n",
            "export fn entry() {}\n",
            "pub fn visible() {}\n",
            "pub export fn both() {}\n",
            "}\n",
        );
        let output = parse_file(source);
        let diagnostics = output
            .errors
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
            .collect::<Vec<_>>();

        assert_eq!(diagnostics.len(), 3);
        let help_for = |name: &str| {
            diagnostics
                .iter()
                .find_map(|diagnostic| {
                    diagnostic
                        .children()
                        .iter()
                        .find(|child| {
                            child.kind == sai_diagnostics::ChildKind::Help
                                && child.message.contains(name)
                        })
                        .map(|child| child.message.as_str())
                })
                .expect("visibility-qualified function guidance")
        };
        for (name, qualifier) in [
            ("entry", "exported"),
            ("visible", "public"),
            ("both", "exported"),
        ] {
            let help = help_for(name);
            assert!(help.contains(qualifier));
            if name == "both" {
                assert!(help.contains("public"));
            }
            assert!(help.contains("outside this block"));
            assert!(!help.contains("fn(..)"));
        }
    }

    #[test]
    fn unsupported_item_recovery_keeps_a_newline_after_equals_with_its_rhs() {
        let source = concat!(
            "fn outer() {\n",
            "type Alias =\n",
            "I32\n",
            "const VALUE: I32 =\n",
            "42\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let error_texts = root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::ERROR)
            .map(|node| {
                &source[node.text_range().start().to_usize()..node.text_range().end().to_usize()]
            })
            .collect::<Vec<_>>();

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ItemInBlock)
                .count(),
            2
        );
        assert!(error_texts.contains(&"type Alias =\nI32"));
        assert!(error_texts.contains(&"const VALUE: I32 =\n42"));
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .count(),
            0
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
    }

    #[test]
    fn unclosed_attribute_before_an_item_reports_item_in_block() {
        let source = concat!(
            "fn outer() {\n",
            "@allow(foo\n",
            "fn inner() {}\n",
            "let after = 1\n",
            "}\n",
        );
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);
        let item_diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::ItemInBlock)
            .expect("item following malformed attributes");

        assert_eq!(
            item_diagnostic
                .primary()
                .expect("item-in-block location")
                .span
                .range
                .start()
                .to_usize(),
            source.find("@allow").expect("attribute start")
        );
        assert!(
            output
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code != Code::InvalidAttributeTarget)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn malformed_let_patterns_recover_as_error_nodes() {
        let output = parse_file("fn f() { let = 1\nlet later = 2\n}\nfn next() {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
    }

    #[test]
    fn pattern_parser_builds_binding_tuple_constructor_and_alternation_nodes() {
        let output = parse_file(
            "fn f() {\n\
             let (a, borrow mut b, var c,) = tuple\n\
             let named @ User(id: var value, ..) = user\n\
             let | .Some(x) | pkg.http.Status.Ok = result\n\
             let (grouped) = value\n\
             let () = unit\n\
             }\n",
        );
        assert!(
            output.errors.is_empty(),
            "valid binding and constructor patterns should parse cleanly: {:?}",
            output.errors
        );
        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };
        assert_eq!(count(SyntaxKind::BINDING_PAT), 7);
        assert_eq!(count(SyntaxKind::BINDING_MODE), 3);
        assert_eq!(count(SyntaxKind::TUPLE_PAT), 2);
        assert_eq!(count(SyntaxKind::PAREN_PAT), 1);
        assert_eq!(count(SyntaxKind::AT_PAT), 1);
        assert_eq!(count(SyntaxKind::OR_PAT), 1);
        assert_eq!(count(SyntaxKind::CTOR_PAT), 2);
        assert_eq!(count(SyntaxKind::CTOR_PAT_ARGS), 2);
        assert_eq!(count(SyntaxKind::NAMED_PAT_ARG), 1);
        assert_eq!(count(SyntaxKind::REST_PAT), 1);
        assert_eq!(count(SyntaxKind::IMPLICIT_MEMBER_PAT), 1);
        assert_eq!(count(SyntaxKind::CONST_PATH), 2);
    }

    #[test]
    fn pattern_parser_builds_range_and_slice_patterns() {
        let output = parse_file(
            "fn f() {\n\
             let 1..=4 = value\n\
             let ..=8 = value\n\
             let [first, ..borrow rest, last] = values\n\
             }\n",
        );
        assert!(
            output.errors.is_empty(),
            "valid ranges and slice rest patterns should parse cleanly: {:?}",
            output.errors
        );
        let root = crate::SyntaxNode::new_root(output.root);
        let count = |kind| {
            root.descendants()
                .filter(|node| node.kind() == kind)
                .count()
        };
        assert_eq!(count(SyntaxKind::RANGE_PAT), 2);
        assert_eq!(count(SyntaxKind::SLICE_PAT), 1);
        assert_eq!(count(SyntaxKind::REST_PAT), 1);
        assert_eq!(count(SyntaxKind::BINDING_MODE), 1);
    }

    #[test]
    fn pattern_parser_reports_float_and_multiple_slice_rest_patterns() {
        let output =
            parse_file("fn f() { let 1.25 = value\nlet -2.5 = value\nlet [.., ..] = values\n}\n");
        assert_eq!(
            output
                .errors
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                Code::FloatPattern,
                Code::FloatPattern,
                Code::MultipleRestPatterns
            ]
        );
        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LITERAL_PAT)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::SLICE_PAT)
                .count(),
            1
        );
    }

    #[test]
    fn malformed_range_and_constructor_patterns_report_expected_tokens() {
        let missing_inclusive_end = parse_file("fn f() { let 1..= = value }\n");
        assert_eq!(
            missing_inclusive_end.errors[0].code,
            Code::ExpectedToken,
            "an inclusive range requires an endpoint"
        );

        let nonfinal_ctor_rest = parse_file("fn f() { let Some(.., value) = result }\n");
        assert!(
            nonfinal_ctor_rest
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "constructor rest patterns must be final"
        );

        let invalid_lowercase_range_end = parse_file("fn f() { let 1..value = result }\n");
        assert!(
            invalid_lowercase_range_end
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "a bare lowercase binding is not a constant range endpoint"
        );

        let trailing_ctor_rest =
            parse_file("fn f() { let Some(..,) = value\nlet Other(a, ..,) = value }\n");
        assert_eq!(
            trailing_ctor_rest
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedToken)
                .count(),
            2,
            "constructor rest arguments cannot be followed by a comma"
        );

        for invalid in ["-'a'", "-b'x'", "-name", "-Upper"] {
            let invalid_signed_literal =
                parse_file(&format!("fn f() {{ let {invalid} = value }}\n"));
            assert!(
                invalid_signed_literal
                    .errors
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
                "a minus sign in a pattern must be followed by an integer literal: {invalid}"
            );
        }

        let float_range_ends = parse_file(
            "fn f() { let 1..=2.5 = value\nlet ..=3.5 = value\nlet 1..=-4.5 = value }\n",
        );
        assert_eq!(
            float_range_ends
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::FloatPattern)
                .count(),
            3,
            "floats are diagnosed and retained as invalid range endpoints"
        );
    }

    #[test]
    fn deeply_nested_patterns_report_the_shared_nesting_limit() {
        let depth = 520;
        let source = format!(
            "fn f() {{ let {}value{} = value\nlet later = 1\n}}\nfn next() {{}}\n",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded),
            "pattern nesting must use the shared parser depth limit"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2,
            "balanced over-depth recovery must preserve the later statement"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
    }

    #[test]
    fn overdeep_unclosed_pattern_recovery_preserves_later_statements() {
        let depth = 520;
        let source = format!(
            "fn f() {{ let {}value = 0\nlet later = 1\n}}\n",
            "(".repeat(depth)
        );
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded),
            "over-deep recovery must report the shared parser nesting limit"
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            2,
            "an unclosed pattern must stop recovery before the later line-start let"
        );
    }

    #[test]
    fn malformed_tuple_pattern_recovery_preserves_later_items() {
        let output = parse_file("fn f() { let (a, b, ]\n}\nfn next() {}\n");
        let root = crate::SyntaxNode::new_root(output.root);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2,
            "a mismatched tuple-pattern closer must not stall or consume the next item"
        );
    }

    #[test]
    fn block_separator_recovery_preserves_a_same_line_closing_brace() {
        let output = parse_file("fn f() { a b }\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedToken)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            1
        );
    }

    #[test]
    fn block_separator_recovery_balances_nested_delimiters() {
        let output = parse_file("fn f() { a { b\nc } }\nfn next() {}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ExpectedToken)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::BLOCK)
                .count(),
            2
        );
    }

    #[test]
    fn block_recovers_binary_operators_moved_to_the_next_line() {
        let output = parse_file("fn f() {\na\n+ b\n}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::UnexpectedNewline);
        assert!(
            output.errors[0]
                .children()
                .iter()
                .any(|child| child.kind == sai_diagnostics::ChildKind::Note)
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            1
        );

        let unary = parse_file("fn f() {\na\n- b\n}\n");
        assert!(
            unary
                .errors
                .iter()
                .all(|diagnostic| diagnostic.code != Code::UnexpectedNewline),
            "a leading minus can begin a prefix expression"
        );

        let remainder = parse_file("fn f() {\na\n-% b\n}\n");
        assert_eq!(remainder.errors.len(), 1);
        assert_eq!(remainder.errors[0].code, Code::UnexpectedNewline);
        assert_eq!(
            crate::SyntaxNode::new_root(remainder.root)
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ERROR)
                .count(),
            1
        );
    }

    #[test]
    fn invalid_expression_starts_do_not_create_empty_expression_statements() {
        let output = parse_file("fn f() { +\n}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXPR_STMT)
                .all(|node| !node.text_range().is_empty()),
            "a zero-width missing expression must remain a bare ERROR node"
        );
        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR)
        );
        assert!(
            output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedExpression)
        );
        let expected_expression = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::ExpectedExpression)
            .expect("invalid expression start diagnostic");
        assert!(
            !expected_expression.message.contains("NEWLINE"),
            "statement recovery must not add a separator expectation at the expression start"
        );
    }

    #[test]
    fn missing_binary_operands_at_newlines_include_guidance() {
        let output = parse_file("fn f() {\na >\nb\n}\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert!(
            root.descendants()
                .any(|node| node.kind() == SyntaxKind::BIN_EXPR)
        );
        let diagnostic = output
            .errors
            .iter()
            .find(|diagnostic| diagnostic.code == Code::ExpectedExpression)
            .expect("missing right operand is diagnosed");
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.kind == sai_diagnostics::ChildKind::Note),
            "the expected-expression diagnostic should explain how to continue the expression"
        );
    }

    #[test]
    fn deeply_nested_expression_recovery_preserves_later_statements_and_items() {
        let depth = 520;
        let source = format!(
            "fn deep() {{ {}{{ 1 }}{} \nlet after = 2\n}}\nfn next() {{}}\n",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let output = parse_file(&source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            output
                .errors
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::ImplementationLimitExceeded)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::LET_STMT)
                .count(),
            1
        );
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::FN_DECL)
                .count(),
            2
        );
        assert!(
            !output
                .errors
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ExpectedToken),
            "block expressions inside overdeep expressions must not cause delimiter errors"
        );
    }

    #[test]
    fn source_file_allows_an_attribute_without_a_following_newline() {
        let output = parse_file("@allow use sai:http\n");
        assert!(output.errors.is_empty());
        assert!(
            crate::SyntaxNode::new_root(output.root)
                .descendants()
                .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
        );
    }

    #[test]
    fn source_file_demotes_trailing_outer_doc_comments() {
        assert_doc_comment_is_demoted(
            "use sai:http\n/// misplaced",
            "/// misplaced",
            SyntaxKind::DOC_OUTER,
        );
    }

    #[test]
    fn source_file_demotes_inner_doc_comments_after_the_first_item() {
        let source = "use sai:http\n//! misplaced\nuse sai:other\n";
        assert_doc_comment_is_demoted(source, "//! misplaced", SyntaxKind::DOC_INNER);
        let output = parse_file(source);
        assert_eq!(
            crate::SyntaxNode::new_root(output.root)
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            2
        );
    }

    #[test]
    fn source_file_demotes_doc_comments_after_attributes() {
        assert_doc_comment_is_demoted(
            "@allow(UNUSED_IMPORTS)\n/// misplaced\nuse sai:http\n",
            "/// misplaced",
            SyntaxKind::DOC_OUTER,
        );
    }

    #[test]
    fn source_file_recovers_an_unclosed_attribute_before_the_next_use() {
        let source = "@allow(UNUSED_IMPORTS\nuse sai:other\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            1
        );
        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
    }

    #[test]
    fn source_file_demotes_an_inline_doc_and_keeps_the_following_use() {
        let source = "use sai:http /// misplaced\nuse sai:other\n";
        assert_doc_comment_is_demoted(source, "/// misplaced", SyntaxKind::DOC_OUTER);
        let output = parse_file(source);
        assert_eq!(
            crate::SyntaxNode::new_root(output.root)
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            2
        );
    }

    #[test]
    fn missing_use_items_close_demotes_inner_docs_before_the_next_use() {
        let output = parse_file("use sai:http.{Request\n//! misplaced\nuse sai:other\n");
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::USE_DECL)
                .count(),
            2
        );
        assert_eq!(output.errors.len(), 2);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert_eq!(output.errors[1].code, Code::MisplacedDocComment);
    }

    #[test]
    fn use_items_demotes_doc_comments_between_items() {
        let output = parse_file("use sai:http.{Request,\n/// misplaced\nResponse}\n");
        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::MisplacedDocComment);
        assert_eq!(
            output.errors[0].severity,
            sai_diagnostics::Severity::Warning
        );
    }

    #[test]
    fn source_file_recovers_a_missing_item_separator() {
        let output = parse_file("use sai:http use sai:other\n");
        assert_eq!(output.errors.len(), 1);
        assert_eq!(output.errors[0].code, Code::ExpectedToken);
        assert!(
            crate::SyntaxNode::new_root(output.root.clone())
                .descendants()
                .any(|node| node.kind() == SyntaxKind::ERROR)
        );
    }

    #[test]
    fn source_file_parses_extern_items_instead_of_treating_them_as_unsupported() {
        let source = "extern \"wasm\" {}\n";
        let output = parse_file(source);
        let root = crate::SyntaxNode::new_root(output.root);

        assert_eq!(output.errors.len(), 1, "{:?}", output.errors);
        assert_eq!(output.errors[0].code, Code::InvalidExternBlock);
        assert_eq!(
            root.descendants()
                .filter(|node| node.kind() == SyntaxKind::EXTERN_BLOCK)
                .count(),
            1
        );
        assert_eq!(root.green().text_len().to_usize(), source.len());
    }

    #[test]
    fn expect_emits_a_snapshot_bound_diagnostic_without_consuming_the_token() {
        let source = "name";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();

        assert!(
            !parser
                .expect(SyntaxKind::L_PAREN)
                .expect("diagnostic source is available")
        );
        assert_eq!(parser.position(), 0);
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all input was consumed");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("tree builds with a parser diagnostic");
        assert_eq!(output.errors.len(), 1);
        let GreenElement::Node(error_node) = &output.root.children()[0] else {
            panic!("expected a missing-token error node");
        };
        assert_eq!(error_node.kind(), SyntaxKind::ERROR);
        assert_eq!(error_node.text_len().raw(), 0);
        let diagnostic = &output.errors[0];
        assert_eq!(diagnostic.code, Code::ExpectedToken);
        let location = diagnostic.primary().expect("primary source location");
        assert_eq!(location.file.path, "src/main.sai");
        assert_eq!(
            location.span.range,
            TextRange::new(TextSize::new(0), TextSize::new(4)).unwrap()
        );
    }

    #[test]
    fn expect_diagnostic_locations_restore_a_leading_bom_offset() {
        let source = "name";
        let (snapshot_file, lexed) = lex_file_with_bom(source, true);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();

        assert!(!parser.expect(SyntaxKind::L_PAREN).expect("file context"));
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all input was consumed");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("tree builds with a parser diagnostic");
        assert_eq!(
            output.errors[0]
                .primary()
                .expect("primary source location")
                .span
                .range,
            TextRange::new(TextSize::new(3), TextSize::new(7)).unwrap()
        );
    }

    #[test]
    fn expect_diagnostic_locations_map_past_malformed_utf8_bytes() {
        let file_id = FileId::new(2);
        let source_file =
            Arc::new(SourceFile::decode(file_id, b"\xEF\xBB\xBF\xFFname").expect("decoded source"));
        let snapshot_file = SnapshotFile::new(
            source_file.clone(),
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        );
        let snapshot = WorkspaceSnapshot::new([snapshot_file.clone()]).expect("snapshot");
        let lexed = lex(&snapshot, file_id).expect("lex");
        let source = source_file.text();
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();

        assert_eq!(parser.bump(), SyntaxKind::ERROR);
        assert!(!parser.expect(SyntaxKind::L_PAREN).expect("file context"));
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all input was consumed");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("tree builds with a parser diagnostic");
        assert_eq!(
            output.errors[0]
                .primary()
                .expect("primary source location")
                .span
                .range,
            TextRange::new(TextSize::new(4), TextSize::new(8)).unwrap()
        );
    }

    #[test]
    fn expect_requires_snapshot_context_to_emit_a_diagnostic() {
        let source = "name";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");

        assert_eq!(
            parser.expect(SyntaxKind::L_PAREN),
            Err(ParserError::MissingDiagnosticSource)
        );
        assert_eq!(parser.position(), 0);
    }

    #[test]
    fn expect_any_reports_all_alternatives_once() {
        let source = "name";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();

        assert!(
            !parser
                .expect_any(&[SyntaxKind::L_PAREN, SyntaxKind::L_BRACKET])
                .expect("diagnostic source is available")
        );
        assert!(
            !parser
                .expect(SyntaxKind::R_PAREN)
                .expect("same-position diagnostic is suppressed")
        );
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all input was consumed");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("tree builds with a parser diagnostic");
        assert_eq!(output.errors.len(), 1);
        assert!(matches!(
            &output.root.children()[0],
            GreenElement::Node(node) if node.kind() == SyntaxKind::ERROR
        ));
        assert_eq!(
            output.errors[0].message,
            "expected L_PAREN or L_BRACKET or R_PAREN"
        );
    }

    #[test]
    fn expected_token_apis_reject_empty_sets_and_node_kinds() {
        let source = "name";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");

        assert_eq!(parser.expect_any(&[]), Err(ParserError::EmptyExpectedSet));
        assert_eq!(
            parser.expect(SyntaxKind::SOURCE_FILE),
            Err(ParserError::InvalidExpectedKind {
                kind: SyntaxKind::SOURCE_FILE
            })
        );
        assert_eq!(
            parser.expect_any(&[SyntaxKind::L_PAREN, SyntaxKind::SOURCE_FILE]),
            Err(ParserError::InvalidExpectedKind {
                kind: SyntaxKind::SOURCE_FILE
            })
        );
    }

    #[test]
    fn completed_markers_can_be_wrapped_into_left_associative_nodes() {
        let source = "a+b";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();
        let lhs = parser.start();
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        let lhs = lhs
            .complete(&mut parser, SyntaxKind::LITERAL)
            .expect("valid node marker");

        let expression = lhs.precede(&mut parser);
        assert_eq!(parser.bump(), SyntaxKind::PLUS);
        let rhs = parser.start();
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        rhs.complete(&mut parser, SyntaxKind::LITERAL)
            .expect("valid node marker");
        expression
            .complete(&mut parser, SyntaxKind::BIN_EXPR)
            .expect("valid node marker");

        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");
        let events = parser.into_events().expect("all parser markers completed");
        assert!(matches!(
            events.get(1),
            Some(Event::Start {
                kind: SyntaxKind::LITERAL,
                forward_parent: Some(3)
            })
        ));

        let mut cache = NodeCache::new();
        let tree = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("tree built from parser events");
        let GreenElement::Node(expression) = &tree.root.children()[0] else {
            panic!("expected an expression node");
        };
        assert_eq!(expression.kind(), SyntaxKind::BIN_EXPR);
        assert_eq!(expression.text_len().to_usize(), source.len());
    }

    #[test]
    fn parser_glues_only_valid_joint_raw_tokens() {
        let source = ">>";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();

        assert!(!parser.bump_as(SyntaxKind::SHR, 3));
        assert_eq!(parser.position(), 0);
        assert!(parser.bump_as(SyntaxKind::SHR, 2));
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all parser markers completed");
        let mut cache = NodeCache::new();
        let tree = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("joint tokens form a composite");
        let GreenElement::Token(token) = &tree.root.children()[0] else {
            panic!("expected the glued operator token");
        };
        assert_eq!(token.kind(), SyntaxKind::SHR);
        assert_eq!(token.text(), ">>");
    }

    #[test]
    fn expect_consumes_a_composite_from_its_joint_raw_tokens() {
        let source = ">>";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();

        assert!(parser.expect(SyntaxKind::SHR).expect("file context"));
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all input was consumed");
        let mut cache = NodeCache::new();
        let output = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("joint raw tokens build one composite");
        let GreenElement::Token(token) = &output.root.children()[0] else {
            panic!("expected the composite token");
        };
        assert_eq!(token.kind(), SyntaxKind::SHR);
        assert!(output.errors.is_empty());
    }

    #[test]
    fn abandoned_markers_leave_tombstones_without_consuming_source_text() {
        let source = "";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let marker = parser.start();
        marker
            .abandon(&mut parser)
            .expect("valid marker abandonment");
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        assert!(matches!(
            parser
                .into_events()
                .expect("abandoned markers are resolved")
                .as_slice(),
            [
                Event::Tombstone,
                Event::Token {
                    kind: SyntaxKind::EOF,
                    n_raw: 1
                }
            ]
        ));
    }

    #[test]
    fn rejects_composite_kinds_in_raw_token_streams() {
        let source = ">>";
        let mut tokens = lex_text(source).tokens;
        tokens[0].kind = SyntaxKind::SHR;
        assert!(matches!(
            Parser::new(source, &tokens),
            Err(ParserError::InvalidTokenStream { position: 0 })
        ));
    }

    #[test]
    fn rejects_zero_width_non_synthetic_tokens() {
        let source = "a";
        let mut tokens = lex_text(source).tokens;
        let mut phantom = tokens[0].clone();
        phantom.range = crate::TextRange::empty(crate::TextSize::new(0));
        tokens.insert(0, phantom);

        assert!(matches!(
            Parser::new(source, &tokens),
            Err(ParserError::InvalidTokenStream { position: 0 })
        ));
    }

    #[test]
    fn accepts_only_error_marked_zero_width_format_string_ends() {
        let source = "f\"\n";
        let tokens = lex_text(source).tokens;
        let end_position = tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::FSTRING_END)
            .expect("unterminated format string has an end token");
        assert!(tokens[end_position].range.is_empty());
        assert!(
            tokens[end_position]
                .flags
                .contains(crate::TokenFlags::HAS_ERROR)
        );
        Parser::new(source, &tokens).expect("lexer format-string recovery is a valid token stream");

        let mut unmarked_tokens = tokens;
        unmarked_tokens[end_position].flags = crate::TokenFlags::IN_E;
        assert!(matches!(
            Parser::new(source, &unmarked_tokens),
            Err(ParserError::InvalidTokenStream { position }) if position == end_position
        ));
    }

    #[test]
    fn rejects_nonempty_synthetic_newline_ranges() {
        let source = "a\n";
        let mut tokens = lex_text(source).tokens;
        let newline_position = tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::NEWLINE)
            .expect("lexer emits a statement newline");
        tokens[newline_position].range =
            crate::TextRange::new(crate::TextSize::new(1), crate::TextSize::new(2))
                .expect("valid test range");

        assert!(matches!(
            Parser::new(source, &tokens),
            Err(ParserError::InvalidTokenStream { position }) if position == newline_position
        ));
    }

    #[test]
    fn eof_is_consumed_once_and_cannot_appear_after_the_stream() {
        let source = "";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        assert!(parser.eat(SyntaxKind::EOF));
        assert!(!parser.eat(SyntaxKind::EOF));
        assert!(parser.into_events().is_ok());

        assert!(matches!(
            Parser::new(source, &[]),
            Err(ParserError::InvalidTokenStream { position: 0 })
        ));
        let source = "a";
        let mut misplaced_eof = lex_text(source).tokens;
        let eof = misplaced_eof.pop().expect("EOF token");
        misplaced_eof.insert(0, eof);
        assert!(matches!(
            Parser::new(source, &misplaced_eof),
            Err(ParserError::InvalidTokenStream { position: 0 })
        ));

        let mut nonempty_eof = lexed.tokens.clone();
        nonempty_eof[0].range =
            crate::TextRange::new(crate::TextSize::new(0), crate::TextSize::new(1))
                .expect("valid test range");
        assert!(matches!(
            Parser::new("", &nonempty_eof),
            Err(ParserError::InvalidTokenStream { position: 0 })
        ));

        let source = "a";
        let mut early_eof = lex_text(source).tokens;
        let eof_position = early_eof.len() - 1;
        early_eof[eof_position].range = crate::TextRange::empty(crate::TextSize::new(0));
        assert!(matches!(
            Parser::new(source, &early_eof),
            Err(ParserError::InvalidTokenStream { position }) if position == eof_position
        ));
    }

    #[test]
    fn rejects_markers_completed_out_of_nesting_order() {
        let source = "a";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let outer = parser.start();
        let inner = parser.start();
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);

        assert!(matches!(
            outer.complete(&mut parser, SyntaxKind::LITERAL),
            Err(ParserError::InvalidMarkerNesting { position: 0 })
        ));
        inner
            .complete(&mut parser, SyntaxKind::LITERAL)
            .expect("innermost marker still completes");
        assert_eq!(
            parser.into_events(),
            Err(ParserError::InvalidMarkerNesting { position: 0 })
        );
    }

    #[test]
    fn abandoning_a_preceding_marker_unlinks_its_child() {
        let source = "a";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let file = parser.start();
        let child = parser.start();
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        let child = child
            .complete(&mut parser, SyntaxKind::LITERAL)
            .expect("valid node marker");
        child
            .precede(&mut parser)
            .abandon(&mut parser)
            .expect("valid marker abandonment");
        assert!(parser.eat_nl());
        assert_eq!(parser.bump(), SyntaxKind::EOF);
        file.complete(&mut parser, SyntaxKind::SOURCE_FILE)
            .expect("valid node marker");

        let events = parser.into_events().expect("all markers resolved");
        assert!(matches!(
            events.get(1),
            Some(Event::Start {
                kind: SyntaxKind::LITERAL,
                forward_parent: None
            })
        ));
        let mut cache = NodeCache::new();
        build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("abandoned parent leaves a valid tree");
    }

    #[test]
    fn marker_completion_rejects_token_kinds() {
        let source = "a";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let marker = parser.start();
        assert_eq!(parser.bump(), SyntaxKind::LIDENT);
        assert!(matches!(
            marker.complete(&mut parser, SyntaxKind::PLUS),
            Err(ParserError::InvalidNodeKind {
                position: 0,
                kind: SyntaxKind::PLUS
            })
        ));
        assert_eq!(
            parser.into_events(),
            Err(ParserError::InvalidNodeKind {
                position: 0,
                kind: SyntaxKind::PLUS
            })
        );
    }

    #[test]
    fn dropped_unfinished_markers_are_reported_in_all_builds() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let source = "";
        let lexed = lex_text(source);
        let mut parser = Parser::new(source, &lexed.tokens).expect("valid lexer token stream");
        let marker = parser.start();
        let drop_result = catch_unwind(AssertUnwindSafe(|| drop(marker)));
        if cfg!(debug_assertions) {
            assert!(drop_result.is_err());
        }
        assert_eq!(
            parser.into_events(),
            Err(ParserError::UnfinishedMarker { position: 0 })
        );
    }
}
