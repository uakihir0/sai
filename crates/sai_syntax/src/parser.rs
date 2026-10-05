use std::cell::RefCell;
use std::rc::Rc;

use sai_diagnostics::Diagnostic;

use crate::tree::{validate_joint_tokens, validate_token_mapping};
use crate::{Event, SyntaxKind, Token, TokenFlags};

/// The event-producing parser cursor used by grammar routines.
///
/// It deliberately exposes raw NEWLINE tokens. Grammar productions decide
/// where a line break may be consumed with [`Parser::eat_nl`].
pub struct Parser<'src, 'tokens> {
    source: &'src str,
    tokens: &'tokens [Token],
    position: usize,
    events: Vec<Event>,
    marker_owner: Rc<()>,
    active_markers: Rc<RefCell<Vec<u32>>>,
    marker_failures: Rc<RefCell<Vec<ParserError>>>,
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
            marker_owner: Rc::new(()),
            active_markers: Rc::new(RefCell::new(Vec::new())),
            marker_failures: Rc::new(RefCell::new(Vec::new())),
        })
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
        if self.current_token().is_none() || !self.at(kind) {
            return false;
        }
        self.bump();
        true
    }

    pub fn eat_nl(&mut self) -> bool {
        self.eat(SyntaxKind::NEWLINE)
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
        let count = usize::from(n_raw);
        if count == 0 {
            return false;
        }
        let Some(end) = self.position.checked_add(count) else {
            return false;
        };
        if end > self.tokens.len()
            || (count > 1
                && self.tokens[self.position..end]
                    .iter()
                    .any(|token| token.kind == SyntaxKind::EOF))
            || validate_token_mapping(self.tokens, self.position, end, kind).is_err()
            || validate_joint_tokens(self.tokens, self.position, end).is_err()
        {
            return false;
        }
        self.events.push(Event::Token { kind, n_raw });
        self.position = end;
        true
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
        let file_id = FileId::new(1);
        let source = Arc::new(SourceFile::new(file_id, text, false).expect("source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            source,
            "src/main.sai",
            Some("sai:test@0.1.0".to_owned()),
            None,
        )])
        .expect("snapshot");
        lex(&snapshot, file_id).expect("lex")
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
