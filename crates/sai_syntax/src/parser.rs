use std::cell::RefCell;
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
    nesting_depth: usize,
    reported_nesting_limit: bool,
}

struct DiagnosticSource<'src> {
    file: &'src SourceFile,
    source_ref: SourceRef,
}

#[derive(Default)]
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
            if !token.kind.is_token()
                || validate_token_mapping(tokens, position, position + 1, token.kind).is_err()
                || start < previous_end
                || end > source.len()
                || !source.is_char_boundary(start)
                || !source.is_char_boundary(end)
                || (token.range.is_empty()
                    != matches!(token.kind, SyntaxKind::NEWLINE | SyntaxKind::EOF))
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
            nesting_depth: 0,
            reported_nesting_limit: false,
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
        let mut diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            &message,
            Some(location),
        );
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
    fn function_parser_reports_nonempty_bodies_as_unsupported_syntax() {
        let source = "fn main() { return 1 }\n";
        let (snapshot_file, lexed) = lex_file(source);
        let mut parser =
            Parser::new_for_file(&snapshot_file, &lexed.tokens).expect("valid lexer token stream");

        assert_eq!(
            parser.parse_source_file(),
            Err(ParserError::UnsupportedSyntax {
                position: lexed
                    .tokens
                    .iter()
                    .position(|token| token.kind == SyntaxKind::L_BRACE)
                    .expect("function body starts with a brace"),
                kind: SyntaxKind::L_BRACE
            })
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
