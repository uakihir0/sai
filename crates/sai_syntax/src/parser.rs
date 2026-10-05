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
        Ok(Parser {
            source,
            tokens,
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
    fn missing_use_items_close_stops_before_an_unimplemented_item() {
        for source in [
            "use sai:http.{\ntype Thing = U32\n",
            "use sai:http.{Request\ntype Thing = U32\n",
        ] {
            let (snapshot_file, lexed) = lex_file(source);
            let mut parser = Parser::new_for_file(&snapshot_file, &lexed.tokens)
                .expect("valid lexer token stream");

            assert!(matches!(
                parser.parse_source_file(),
                Err(ParserError::UnsupportedItem {
                    kind: SyntaxKind::TYPE_KW,
                    ..
                })
            ));
        }
    }

    #[test]
    fn missing_use_items_close_stops_before_contextual_item_starters() {
        for source in [
            "use sai:http.{Request\ntest \"later\" {}\n",
            "use sai:http.{Request\ndistinct Thing\n",
        ] {
            let (snapshot_file, lexed) = lex_file(source);
            let mut parser = Parser::new_for_file(&snapshot_file, &lexed.tokens)
                .expect("valid lexer token stream");

            assert!(matches!(
                parser.parse_source_file(),
                Err(ParserError::UnsupportedItem {
                    kind: SyntaxKind::LIDENT,
                    ..
                })
            ));
        }
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
    fn unsupported_contract_clauses_leave_no_active_parser_markers() {
        let source = "fn check(value: Bool)\nrequires value\n{}\n";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");

        assert!(matches!(
            parser.parse_source_file(),
            Err(ParserError::UnsupportedSyntax {
                kind: SyntaxKind::LIDENT,
                ..
            })
        ));
        assert!(matches!(
            parser.into_events(),
            Err(ParserError::UnconsumedTokens { .. })
        ));
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
    fn source_file_reports_unimplemented_item_kinds_without_misparsing_them() {
        let source = "type UserId = U32\n";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");
        assert_eq!(
            parser.parse_source_file(),
            Err(ParserError::UnsupportedItem {
                position: 0,
                kind: SyntaxKind::TYPE_KW
            })
        );
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
