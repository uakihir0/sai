use std::hash::{DefaultHasher, Hash, Hasher};
use std::{collections::HashMap, mem, sync::Arc};
use std::{fmt, fmt::Formatter};

use sai_diagnostics::Diagnostic;
use sai_vfs::{TextRange, TextSize};

use crate::{SyntaxKind, Token, TokenFlags, Trivia, TriviaKind};

/// Events emitted by the parser and consumed by the lossless tree builder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Start {
        kind: SyntaxKind,
        forward_parent: Option<u32>,
    },
    Finish,
    Token {
        kind: SyntaxKind,
        n_raw: u8,
    },
    DemoteToTrivia,
    Error {
        diag: Box<Diagnostic>,
    },
    Tombstone,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GreenTrivia {
    kind: TriviaKind,
    text: Box<str>,
}

impl GreenTrivia {
    pub fn kind(&self) -> TriviaKind {
        self.kind
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GreenTokenData {
    kind: SyntaxKind,
    text: Box<str>,
    leading: Box<[GreenTrivia]>,
    trailing: Box<[GreenTrivia]>,
    full_text_len: TextSize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GreenToken(Arc<GreenTokenData>);

impl GreenToken {
    pub fn kind(&self) -> SyntaxKind {
        self.0.kind
    }

    pub fn text(&self) -> &str {
        &self.0.text
    }

    pub fn leading_trivia(&self) -> &[GreenTrivia] {
        &self.0.leading
    }

    pub fn trailing_trivia(&self) -> &[GreenTrivia] {
        &self.0.trailing
    }

    pub fn full_text_len(&self) -> TextSize {
        self.0.full_text_len
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum GreenElement {
    Node(GreenNode),
    Token(GreenToken),
}

impl GreenElement {
    pub fn text_len(&self) -> TextSize {
        match self {
            Self::Node(node) => node.text_len(),
            Self::Token(token) => token.full_text_len(),
        }
    }

    pub fn as_node(&self) -> Option<&GreenNode> {
        match self {
            Self::Node(node) => Some(node),
            Self::Token(_) => None,
        }
    }

    pub fn as_token(&self) -> Option<&GreenToken> {
        match self {
            Self::Node(_) => None,
            Self::Token(token) => Some(token),
        }
    }
}

struct GreenNodeData {
    kind: SyntaxKind,
    text_len: TextSize,
    structural_hash: u64,
    child_offsets: Box<[TextSize]>,
    children: Box<[GreenElement]>,
}

#[derive(Clone)]
pub struct GreenNode(Arc<GreenNodeData>);

impl GreenNode {
    pub fn kind(&self) -> SyntaxKind {
        self.0.kind
    }

    pub fn text_len(&self) -> TextSize {
        self.0.text_len
    }

    pub fn children(&self) -> &[GreenElement] {
        &self.0.children
    }

    pub(crate) fn child_offset(&self, index: usize) -> Option<TextSize> {
        self.0.child_offsets.get(index).copied()
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn last_token(&self) -> Option<&GreenToken> {
        let mut stack = self.children().iter().collect::<Vec<_>>();
        while let Some(element) = stack.pop() {
            match element {
                GreenElement::Node(node) => stack.extend(node.children().iter()),
                GreenElement::Token(token) => return Some(token),
            }
        }
        None
    }
}

impl fmt::Debug for GreenNode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GreenNode")
            .field("kind", &self.kind())
            .field("text_len", &self.text_len())
            .field("children_len", &self.children().len())
            .finish()
    }
}

impl PartialEq for GreenNode {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        let mut nodes = vec![(self, other)];
        while let Some((left, right)) = nodes.pop() {
            if left.kind() != right.kind()
                || left.text_len() != right.text_len()
                || left.children().len() != right.children().len()
            {
                return false;
            }
            for (left, right) in left.children().iter().zip(right.children()) {
                match (left, right) {
                    (GreenElement::Node(left), GreenElement::Node(right)) => {
                        nodes.push((left, right));
                    }
                    (GreenElement::Token(left), GreenElement::Token(right)) if left == right => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

impl Eq for GreenNode {}

impl Drop for GreenNode {
    fn drop(&mut self) {
        let mut pending = Vec::<GreenElement>::new();
        if let Some(data) = Arc::get_mut(&mut self.0) {
            pending.extend(mem::take(&mut data.children));
        }
        while let Some(element) = pending.pop() {
            if let GreenElement::Node(mut node) = element
                && let Some(data) = Arc::get_mut(&mut node.0)
            {
                pending.extend(mem::take(&mut data.children));
            }
        }
    }
}

impl Hash for GreenNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.structural_hash.hash(state);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SmallNodeKey {
    kind: SyntaxKind,
    children: Box<[GreenElement]>,
}

/// Per-parse structural interning for green tokens and small nodes.
#[derive(Default)]
pub struct NodeCache {
    tokens: HashMap<GreenToken, GreenToken>,
    small_nodes: HashMap<SmallNodeKey, GreenNode>,
}

impl NodeCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn token(
        &mut self,
        kind: SyntaxKind,
        text: &str,
        leading: Vec<GreenTrivia>,
        trailing: Vec<GreenTrivia>,
    ) -> Result<GreenToken, TreeBuildError> {
        let mut full_text_len = TextSize::new(0);
        for trivia in &leading {
            full_text_len = checked_add_text_len(full_text_len, trivia.text.len())?;
        }
        full_text_len = checked_add_text_len(full_text_len, text.len())?;
        for trivia in &trailing {
            full_text_len = checked_add_text_len(full_text_len, trivia.text.len())?;
        }

        let token = GreenToken(Arc::new(GreenTokenData {
            kind,
            text: text.into(),
            leading: leading.into_boxed_slice(),
            trailing: trailing.into_boxed_slice(),
            full_text_len,
        }));
        if let Some(interned) = self.tokens.get(&token) {
            return Ok(interned.clone());
        }
        self.tokens.insert(token.clone(), token.clone());
        Ok(token)
    }

    fn node(
        &mut self,
        kind: SyntaxKind,
        children: Vec<GreenElement>,
    ) -> Result<GreenNode, TreeBuildError> {
        let mut text_len = TextSize::new(0);
        let mut child_offsets = Vec::with_capacity(children.len());
        for child in &children {
            child_offsets.push(text_len);
            text_len = checked_add_text_len(text_len, child.text_len().to_usize())?;
        }
        let structural_hash = green_node_hash(kind, text_len, &children);
        let node = GreenNode(Arc::new(GreenNodeData {
            kind,
            text_len,
            structural_hash,
            child_offsets: child_offsets.into_boxed_slice(),
            children: children.into_boxed_slice(),
        }));
        if node.children().len() > 3 {
            return Ok(node);
        }
        let key = SmallNodeKey {
            kind,
            children: node.children().to_vec().into_boxed_slice(),
        };
        if let Some(interned) = self.small_nodes.get(&key) {
            return Ok(interned.clone());
        }
        self.small_nodes.insert(key, node.clone());
        Ok(node)
    }
}

fn green_node_hash(kind: SyntaxKind, text_len: TextSize, children: &[GreenElement]) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    text_len.hash(&mut hasher);
    children.len().hash(&mut hasher);
    for child in children {
        match child {
            GreenElement::Node(node) => {
                0_u8.hash(&mut hasher);
                node.0.structural_hash.hash(&mut hasher);
            }
            GreenElement::Token(token) => {
                1_u8.hash(&mut hasher);
                token.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeBuildError {
    InvalidEventStream,
    InvalidSourcePartition,
    InvalidCompositeToken,
    EmptyNode(SyntaxKind),
    MissingNodeDelimiter(SyntaxKind),
    InvalidSpan,
    TextTooLarge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeBuildOutput {
    pub root: GreenNode,
    pub errors: Vec<Diagnostic>,
}

struct NodeFrame {
    kind: SyntaxKind,
    children: Vec<GreenElement>,
}

/// Builds a source-file green tree from parser events and lexer output.
pub fn build_green_tree(
    source: &str,
    tokens: &[Token],
    trivia: &[Trivia],
    events: Vec<Event>,
    cache: &mut NodeCache,
) -> Result<TreeBuildOutput, TreeBuildError> {
    validate_source_partition(source, tokens, trivia)?;
    let mut events = events;
    let mut frames = Vec::<NodeFrame>::new();
    let mut root = None;
    let mut errors = Vec::new();
    let mut raw_token = 0_usize;
    let mut pending_demote = false;
    let mut pending_leading = Vec::new();

    for event_index in 0..events.len() {
        let event = mem::replace(&mut events[event_index], Event::Tombstone);
        match event {
            Event::Start {
                kind,
                forward_parent,
            } => {
                if !kind.is_node() {
                    return Err(TreeBuildError::InvalidEventStream);
                }
                let mut kinds = vec![kind];
                let mut parent_index = event_index;
                let mut next_parent = forward_parent;
                while let Some(distance) = next_parent {
                    let distance = usize::try_from(distance)
                        .map_err(|_| TreeBuildError::InvalidEventStream)?;
                    let target = parent_index
                        .checked_add(distance)
                        .filter(|target| *target > parent_index)
                        .ok_or(TreeBuildError::InvalidEventStream)?;
                    let Some(target_event) = events.get_mut(target) else {
                        return Err(TreeBuildError::InvalidEventStream);
                    };
                    let Event::Start {
                        kind,
                        forward_parent,
                    } = mem::replace(target_event, Event::Tombstone)
                    else {
                        return Err(TreeBuildError::InvalidEventStream);
                    };
                    if !kind.is_node() {
                        return Err(TreeBuildError::InvalidEventStream);
                    }
                    kinds.push(kind);
                    parent_index = target;
                    next_parent = forward_parent;
                }
                for kind in kinds.into_iter().rev() {
                    frames.push(NodeFrame {
                        kind,
                        children: Vec::new(),
                    });
                }
            }
            Event::Finish => {
                let frame = frames.pop().ok_or(TreeBuildError::InvalidEventStream)?;
                let is_root = frames.is_empty();
                if frame.kind == SyntaxKind::SOURCE_FILE && !is_root {
                    return Err(TreeBuildError::InvalidEventStream);
                }
                if frame
                    .children
                    .iter()
                    .all(|child| child.text_len().raw() == 0)
                    && !empty_node_allowed(frame.kind, is_root)
                {
                    return Err(TreeBuildError::EmptyNode(frame.kind));
                }
                if let Some((open, close)) = node_delimiters(frame.kind)
                    && !frame.children.iter().any(|child| {
                        child
                            .as_token()
                            .is_some_and(|token| token.kind() == open || token.kind() == close)
                    })
                {
                    return Err(TreeBuildError::MissingNodeDelimiter(frame.kind));
                }
                let node = cache.node(frame.kind, frame.children)?;
                append_element(&mut frames, &mut root, GreenElement::Node(node))?;
            }
            Event::Token { kind, n_raw } => {
                let count = usize::from(n_raw);
                if count == 0 || !kind.is_token() {
                    return Err(TreeBuildError::InvalidEventStream);
                }
                let end = raw_token
                    .checked_add(count)
                    .filter(|end| *end <= tokens.len())
                    .ok_or(TreeBuildError::InvalidEventStream)?;
                let first = tokens
                    .get(raw_token)
                    .ok_or(TreeBuildError::InvalidEventStream)?;
                let last = tokens
                    .get(end - 1)
                    .ok_or(TreeBuildError::InvalidEventStream)?;
                validate_token_mapping(tokens, raw_token, end, kind)?;
                validate_joint_tokens(tokens, raw_token, end)?;

                if pending_demote {
                    if count != 1 || !matches!(kind, SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER)
                    {
                        return Err(TreeBuildError::InvalidEventStream);
                    }
                    pending_leading.extend(copy_trivia(source, trivia, first.leading.clone())?);
                    pending_leading.push(GreenTrivia {
                        kind: TriviaKind::LineComment,
                        text: source_text(source, first.range)?.into(),
                    });
                    pending_leading.extend(copy_trivia(source, trivia, first.trailing.clone())?);
                    pending_demote = false;
                    raw_token = end;
                    continue;
                }

                let mut leading = if kind == SyntaxKind::NEWLINE {
                    Vec::new()
                } else {
                    mem::take(&mut pending_leading)
                };
                leading.extend(copy_trivia(source, trivia, first.leading.clone())?);
                let trailing = copy_trivia(source, trivia, last.trailing.clone())?;
                let text_start = first.range.start();
                let text_end = last.range.end();
                let text = source_text(
                    source,
                    TextRange::new(text_start, text_end).ok_or(TreeBuildError::InvalidSpan)?,
                )?;
                let token = cache.token(kind, text, leading, trailing)?;
                let frame = frames
                    .last_mut()
                    .ok_or(TreeBuildError::InvalidEventStream)?;
                frame.children.push(GreenElement::Token(token));
                raw_token = end;
            }
            Event::DemoteToTrivia => {
                if pending_demote {
                    return Err(TreeBuildError::InvalidEventStream);
                }
                pending_demote = true;
            }
            Event::Error { diag } => errors.push(*diag),
            Event::Tombstone => {}
        }
    }

    if pending_demote
        || !pending_leading.is_empty()
        || raw_token != tokens.len()
        || !frames.is_empty()
    {
        return Err(TreeBuildError::InvalidEventStream);
    }
    let root = root.ok_or(TreeBuildError::InvalidEventStream)?;
    if root.kind() != SyntaxKind::SOURCE_FILE
        || root.last_token().map(GreenToken::kind) != Some(SyntaxKind::EOF)
        || TextSize::from_usize(source.len()) != Some(root.text_len())
    {
        return Err(TreeBuildError::InvalidSourcePartition);
    }
    Ok(TreeBuildOutput { root, errors })
}

fn empty_node_allowed(kind: SyntaxKind, is_root: bool) -> bool {
    kind == SyntaxKind::ERROR || (kind == SyntaxKind::SOURCE_FILE && is_root)
}

fn node_delimiters(kind: SyntaxKind) -> Option<(SyntaxKind, SyntaxKind)> {
    match kind {
        SyntaxKind::ARG_LIST | SyntaxKind::PARAM_LIST | SyntaxKind::CTOR_PAT_ARGS => {
            Some((SyntaxKind::L_PAREN, SyntaxKind::R_PAREN))
        }
        SyntaxKind::FIELD_LIST
        | SyntaxKind::VARIANT_LIST
        | SyntaxKind::ITEM_LIST
        | SyntaxKind::MATCH_ARM_LIST
        | SyntaxKind::EFFECT_SET
        | SyntaxKind::USE_ITEMS
        | SyntaxKind::BLOCK => Some((SyntaxKind::L_BRACE, SyntaxKind::R_BRACE)),
        _ => None,
    }
}

fn append_element(
    frames: &mut [NodeFrame],
    root: &mut Option<GreenNode>,
    element: GreenElement,
) -> Result<(), TreeBuildError> {
    if let Some(frame) = frames.last_mut() {
        frame.children.push(element);
        return Ok(());
    }
    let GreenElement::Node(node) = element else {
        return Err(TreeBuildError::InvalidEventStream);
    };
    if root.replace(node).is_some() {
        return Err(TreeBuildError::InvalidEventStream);
    }
    Ok(())
}

fn validate_token_mapping(
    tokens: &[Token],
    start: usize,
    end: usize,
    kind: SyntaxKind,
) -> Result<(), TreeBuildError> {
    let raw = tokens
        .get(start..end)
        .ok_or(TreeBuildError::InvalidEventStream)?;
    let valid = match kind {
        SyntaxKind::SHR => {
            raw.len() == 2 && raw[0].kind == SyntaxKind::GT && raw[1].kind == SyntaxKind::GT
        }
        SyntaxKind::GE => {
            raw.len() == 2 && raw[0].kind == SyntaxKind::GT && raw[1].kind == SyntaxKind::EQ
        }
        SyntaxKind::SHR_EQ => {
            raw.len() == 3
                && raw[0].kind == SyntaxKind::GT
                && raw[1].kind == SyntaxKind::GT
                && raw[2].kind == SyntaxKind::EQ
        }
        SyntaxKind::COLON2 => {
            raw.len() == 2 && raw[0].kind == SyntaxKind::COLON && raw[1].kind == SyntaxKind::COLON
        }
        SyntaxKind::AMP2 => {
            raw.len() == 2 && raw[0].kind == SyntaxKind::AMP && raw[1].kind == SyntaxKind::AMP
        }
        SyntaxKind::PIPE2 => {
            raw.len() == 2 && raw[0].kind == SyntaxKind::PIPE && raw[1].kind == SyntaxKind::PIPE
        }
        _ => raw.len() == 1 && raw[0].kind == kind,
    };
    if valid {
        Ok(())
    } else {
        Err(TreeBuildError::InvalidCompositeToken)
    }
}

fn validate_joint_tokens(tokens: &[Token], start: usize, end: usize) -> Result<(), TreeBuildError> {
    for index in start..end.saturating_sub(1) {
        let left = tokens
            .get(index)
            .ok_or(TreeBuildError::InvalidEventStream)?;
        let right = tokens
            .get(index + 1)
            .ok_or(TreeBuildError::InvalidEventStream)?;
        if !left.flags.contains(TokenFlags::JOINT)
            || left.range.end() != right.range.start()
            || !left.trailing.is_empty()
            || !right.leading.is_empty()
        {
            return Err(TreeBuildError::InvalidEventStream);
        }
    }
    Ok(())
}

fn validate_source_partition(
    source: &str,
    tokens: &[Token],
    trivia: &[Trivia],
) -> Result<(), TreeBuildError> {
    let mut cursor = 0_usize;
    let mut seen_trivia = vec![false; trivia.len()];
    for token in tokens {
        consume_trivia_range(
            source,
            trivia,
            &mut seen_trivia,
            &mut cursor,
            token.leading.clone(),
        )?;
        consume_source_range(source, token.range, &mut cursor)?;
        consume_trivia_range(
            source,
            trivia,
            &mut seen_trivia,
            &mut cursor,
            token.trailing.clone(),
        )?;
    }
    if cursor != source.len() || seen_trivia.iter().any(|seen| !seen) {
        return Err(TreeBuildError::InvalidSourcePartition);
    }
    Ok(())
}

fn consume_trivia_range(
    source: &str,
    trivia: &[Trivia],
    seen: &mut [bool],
    cursor: &mut usize,
    range: std::ops::Range<usize>,
) -> Result<(), TreeBuildError> {
    if range.start > range.end || range.end > trivia.len() {
        return Err(TreeBuildError::InvalidSourcePartition);
    }
    for index in range {
        let already_seen = seen
            .get_mut(index)
            .ok_or(TreeBuildError::InvalidSourcePartition)?;
        if mem::replace(already_seen, true) {
            return Err(TreeBuildError::InvalidSourcePartition);
        }
        let item = trivia
            .get(index)
            .ok_or(TreeBuildError::InvalidSourcePartition)?;
        consume_source_range(source, item.range, cursor)?;
    }
    Ok(())
}

fn consume_source_range(
    source: &str,
    range: TextRange,
    cursor: &mut usize,
) -> Result<(), TreeBuildError> {
    let start = range.start().to_usize();
    let end = range.end().to_usize();
    if start != *cursor || source.get(start..end).is_none() {
        return Err(TreeBuildError::InvalidSourcePartition);
    }
    *cursor = end;
    Ok(())
}

fn copy_trivia(
    source: &str,
    trivia: &[Trivia],
    range: std::ops::Range<usize>,
) -> Result<Vec<GreenTrivia>, TreeBuildError> {
    if range.start > range.end || range.end > trivia.len() {
        return Err(TreeBuildError::InvalidEventStream);
    }
    range
        .map(|index| {
            let trivia = trivia
                .get(index)
                .ok_or(TreeBuildError::InvalidEventStream)?;
            Ok(GreenTrivia {
                kind: trivia.kind,
                text: source_text(source, trivia.range)?.into(),
            })
        })
        .collect()
}

fn source_text(source: &str, range: TextRange) -> Result<&str, TreeBuildError> {
    source
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or(TreeBuildError::InvalidSpan)
}

fn checked_add_text_len(total: TextSize, additional: usize) -> Result<TextSize, TreeBuildError> {
    let additional = u32::try_from(additional).map_err(|_| TreeBuildError::TextTooLarge)?;
    total
        .raw()
        .checked_add(additional)
        .map(TextSize::new)
        .ok_or(TreeBuildError::TextTooLarge)
}

#[cfg(test)]
mod tests {
    use sai_vfs::{FileId, SnapshotFile, SourceFile, WorkspaceSnapshot};

    use super::*;
    use crate::{LexOutput, lex};

    fn lex_text(text: &str) -> LexOutput {
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

    fn flat_events(tokens: &[Token]) -> Vec<Event> {
        let mut events = vec![Event::Start {
            kind: SyntaxKind::SOURCE_FILE,
            forward_parent: None,
        }];
        events.extend(tokens.iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        events.push(Event::Finish);
        events
    }

    fn append_text(node: &GreenNode, text: &mut String) {
        for child in node.children() {
            match child {
                GreenElement::Node(node) => append_text(node, text),
                GreenElement::Token(token) => {
                    for trivia in token.leading_trivia() {
                        text.push_str(trivia.text());
                    }
                    text.push_str(token.text());
                    for trivia in token.trailing_trivia() {
                        text.push_str(trivia.text());
                    }
                }
            }
        }
    }

    fn all_tokens(node: &GreenNode, tokens: &mut Vec<GreenToken>) {
        for child in node.children() {
            match child {
                GreenElement::Node(node) => all_tokens(node, tokens),
                GreenElement::Token(token) => tokens.push(token.clone()),
            }
        }
    }

    #[test]
    fn green_tree_preserves_all_tokens_and_trivia_losslessly() {
        let source = "#!/usr/bin/sai\n/// docs\nfn main() { let x = 1 // trailing\n}\n";
        let lexed = lex_text(source);
        let mut cache = NodeCache::new();
        let tree = build_green_tree(
            source,
            &lexed.tokens,
            &lexed.trivia,
            flat_events(&lexed.tokens),
            &mut cache,
        )
        .expect("build tree");

        let mut reconstructed = String::new();
        append_text(&tree.root, &mut reconstructed);
        assert_eq!(reconstructed, source);
        assert_eq!(tree.root.kind(), SyntaxKind::SOURCE_FILE);
        assert_eq!(tree.root.text_len().to_usize(), source.len());
        assert_eq!(
            tree.root.last_token().map(GreenToken::kind),
            Some(SyntaxKind::EOF)
        );
    }

    #[test]
    fn forward_parent_events_wrap_the_completed_left_operand() {
        let source = "a + b";
        let lexed = lex_text(source);
        let events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Start {
                kind: SyntaxKind::LITERAL,
                forward_parent: Some(3),
            },
            Event::Token {
                kind: SyntaxKind::LIDENT,
                n_raw: 1,
            },
            Event::Finish,
            Event::Start {
                kind: SyntaxKind::BIN_EXPR,
                forward_parent: None,
            },
            Event::Token {
                kind: SyntaxKind::PLUS,
                n_raw: 1,
            },
            Event::Start {
                kind: SyntaxKind::LITERAL,
                forward_parent: None,
            },
            Event::Token {
                kind: SyntaxKind::LIDENT,
                n_raw: 1,
            },
            Event::Finish,
            Event::Finish,
            Event::Token {
                kind: SyntaxKind::NEWLINE,
                n_raw: 1,
            },
            Event::Token {
                kind: SyntaxKind::EOF,
                n_raw: 1,
            },
            Event::Finish,
        ];
        let mut cache = NodeCache::new();
        let tree = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("build tree");

        let GreenElement::Node(bin_expr) = &tree.root.children()[0] else {
            panic!("expected binary expression node");
        };
        assert_eq!(bin_expr.kind(), SyntaxKind::BIN_EXPR);
        let GreenElement::Node(lhs) = &bin_expr.children()[0] else {
            panic!("expected left operand node");
        };
        assert_eq!(lhs.kind(), SyntaxKind::LITERAL);
    }

    #[test]
    fn composite_tokens_glue_joint_source_tokens() {
        let source = "a::b";
        let lexed = lex_text(source);
        let mut events = vec![Event::Start {
            kind: SyntaxKind::SOURCE_FILE,
            forward_parent: None,
        }];
        events.extend([
            Event::Token {
                kind: SyntaxKind::LIDENT,
                n_raw: 1,
            },
            Event::Token {
                kind: SyntaxKind::COLON2,
                n_raw: 2,
            },
            Event::Token {
                kind: SyntaxKind::LIDENT,
                n_raw: 1,
            },
            Event::Token {
                kind: SyntaxKind::NEWLINE,
                n_raw: 1,
            },
            Event::Token {
                kind: SyntaxKind::EOF,
                n_raw: 1,
            },
            Event::Finish,
        ]);
        let mut cache = NodeCache::new();
        let tree = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("build tree");
        let mut tokens = Vec::new();
        all_tokens(&tree.root, &mut tokens);

        assert_eq!(tokens[1].kind(), SyntaxKind::COLON2);
        assert_eq!(tokens[1].text(), "::");
        let mut reconstructed = String::new();
        append_text(&tree.root, &mut reconstructed);
        assert_eq!(reconstructed, source);
    }

    #[test]
    fn demoted_doc_comments_become_line_comment_trivia_without_text_loss() {
        let source = "/// misplaced\nlet value = 1\n";
        let lexed = lex_text(source);
        let mut events = vec![Event::Start {
            kind: SyntaxKind::SOURCE_FILE,
            forward_parent: None,
        }];
        for token in &lexed.tokens {
            if token.kind == SyntaxKind::DOC_OUTER {
                events.push(Event::DemoteToTrivia);
            }
            events.push(Event::Token {
                kind: token.kind,
                n_raw: 1,
            });
        }
        events.push(Event::Finish);
        let mut cache = NodeCache::new();
        let tree = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("build tree");
        let mut tokens = Vec::new();
        all_tokens(&tree.root, &mut tokens);

        assert!(
            tokens.iter().any(|token| {
                token.leading_trivia().iter().any(|trivia| {
                    trivia.kind() == TriviaKind::LineComment && trivia.text() == "/// misplaced"
                })
            }),
            "demoted comment should lead the next non-NEWLINE token"
        );
        let mut reconstructed = String::new();
        append_text(&tree.root, &mut reconstructed);
        assert_eq!(reconstructed, source);
    }

    #[test]
    fn malformed_event_streams_are_rejected() {
        let source = "x";
        let lexed = lex_text(source);
        let mut cache = NodeCache::new();
        assert_eq!(
            build_green_tree(source, &lexed.tokens, &lexed.trivia, Vec::new(), &mut cache),
            Err(TreeBuildError::InvalidEventStream)
        );
    }

    #[test]
    fn composite_tokens_must_match_their_raw_spellings() {
        let source = "&&";
        let lexed = lex_text(source);
        let events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Token {
                kind: SyntaxKind::SHR,
                n_raw: 2,
            },
            Event::Token {
                kind: SyntaxKind::NEWLINE,
                n_raw: 1,
            },
            Event::Token {
                kind: SyntaxKind::EOF,
                n_raw: 1,
            },
            Event::Finish,
        ];
        let mut cache = NodeCache::new();

        assert_eq!(
            build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache),
            Err(TreeBuildError::InvalidCompositeToken)
        );
    }

    #[test]
    fn empty_nodes_are_restricted_to_the_documented_recovery_and_list_kinds() {
        let source = "";
        let lexed = lex_text(source);
        for kind in [SyntaxKind::FN_DECL, SyntaxKind::ARG_LIST] {
            let events = vec![
                Event::Start {
                    kind: SyntaxKind::SOURCE_FILE,
                    forward_parent: None,
                },
                Event::Start {
                    kind,
                    forward_parent: None,
                },
                Event::Finish,
                Event::Token {
                    kind: SyntaxKind::EOF,
                    n_raw: 1,
                },
                Event::Finish,
            ];
            let mut cache = NodeCache::new();

            assert_eq!(
                build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache),
                Err(TreeBuildError::EmptyNode(kind))
            );
        }
    }

    #[test]
    fn empty_error_markers_and_delimited_empty_lists_remain_valid() {
        let empty_source = "";
        let empty_lexed = lex_text(empty_source);
        let mut error_events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Start {
                kind: SyntaxKind::ERROR,
                forward_parent: None,
            },
            Event::Finish,
        ];
        error_events.extend(empty_lexed.tokens.iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        error_events.push(Event::Finish);
        let mut cache = NodeCache::new();
        assert!(
            build_green_tree(
                empty_source,
                &empty_lexed.tokens,
                &empty_lexed.trivia,
                error_events,
                &mut cache,
            )
            .is_ok()
        );

        let source = "()";
        let lexed = lex_text(source);
        let close = lexed
            .tokens
            .iter()
            .position(|token| token.kind == SyntaxKind::R_PAREN)
            .expect("closing delimiter");
        let mut list_events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Start {
                kind: SyntaxKind::ARG_LIST,
                forward_parent: None,
            },
        ];
        list_events.extend(lexed.tokens[..=close].iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        list_events.push(Event::Finish);
        list_events.extend(lexed.tokens[close + 1..].iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        list_events.push(Event::Finish);
        let mut cache = NodeCache::new();
        assert!(
            build_green_tree(
                source,
                &lexed.tokens,
                &lexed.trivia,
                list_events,
                &mut cache,
            )
            .is_ok()
        );
    }

    #[test]
    fn zero_width_tokens_and_unrelated_text_cannot_stand_in_for_list_delimiters() {
        let source = "";
        let lexed = lex_text(source);
        let mut events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Start {
                kind: SyntaxKind::ARG_LIST,
                forward_parent: None,
            },
        ];
        events.extend(lexed.tokens.iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        events.extend([Event::Finish, Event::Finish]);
        let mut cache = NodeCache::new();
        assert_eq!(
            build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache),
            Err(TreeBuildError::EmptyNode(SyntaxKind::ARG_LIST))
        );

        let source = "x";
        let lexed = lex_text(source);
        let mut events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Start {
                kind: SyntaxKind::ARG_LIST,
                forward_parent: None,
            },
        ];
        events.extend(lexed.tokens.iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        events.extend([Event::Finish, Event::Finish]);
        let mut cache = NodeCache::new();
        assert_eq!(
            build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache),
            Err(TreeBuildError::MissingNodeDelimiter(SyntaxKind::ARG_LIST))
        );
    }

    #[test]
    fn invalid_token_ranges_cannot_produce_a_lossy_green_tree() {
        let source = "x";
        let lexed = lex_text(source);
        let mut tokens = lexed.tokens.clone();
        tokens[0].range = TextRange::empty(TextSize::new(1));
        let mut cache = NodeCache::new();

        assert_eq!(
            build_green_tree(
                source,
                &tokens,
                &lexed.trivia,
                flat_events(&tokens),
                &mut cache,
            ),
            Err(TreeBuildError::InvalidSourcePartition)
        );
    }

    #[test]
    fn small_nodes_are_interned_by_structural_children() {
        let mut cache = NodeCache::new();
        let token = cache
            .token(SyntaxKind::LIDENT, "name", Vec::new(), Vec::new())
            .expect("token");
        let make_large_child = |cache: &mut NodeCache| {
            cache
                .node(
                    SyntaxKind::FN_DECL,
                    vec![
                        GreenElement::Token(token.clone()),
                        GreenElement::Token(token.clone()),
                        GreenElement::Token(token.clone()),
                        GreenElement::Token(token.clone()),
                    ],
                )
                .expect("large child")
        };

        let first_child = make_large_child(&mut cache);
        let second_child = make_large_child(&mut cache);
        assert!(!Arc::ptr_eq(&first_child.0, &second_child.0));

        let first_parent = cache
            .node(
                SyntaxKind::SOURCE_FILE,
                vec![GreenElement::Node(first_child)],
            )
            .expect("first parent");
        let second_parent = cache
            .node(
                SyntaxKind::SOURCE_FILE,
                vec![GreenElement::Node(second_child)],
            )
            .expect("second parent");

        assert!(Arc::ptr_eq(&first_parent.0, &second_parent.0));
    }

    #[test]
    fn deep_left_associative_trees_hash_compare_and_find_their_last_token_iteratively() {
        use std::hash::Hasher;

        let mut left_cache = NodeCache::new();
        let left_plus = left_cache
            .token(SyntaxKind::PLUS, "+", Vec::new(), Vec::new())
            .expect("plus token");
        let left_rhs = left_cache
            .token(SyntaxKind::LIDENT, "rhs", Vec::new(), Vec::new())
            .expect("rhs token");
        let mut left = left_cache
            .node(
                SyntaxKind::LITERAL,
                vec![GreenElement::Token(left_rhs.clone())],
            )
            .expect("left leaf");
        for _ in 0..10_000 {
            left = left_cache
                .node(
                    SyntaxKind::BIN_EXPR,
                    vec![
                        GreenElement::Node(left),
                        GreenElement::Token(left_plus.clone()),
                        GreenElement::Token(left_rhs.clone()),
                    ],
                )
                .expect("left binary node");
        }

        let mut right_cache = NodeCache::new();
        let right_plus = right_cache
            .token(SyntaxKind::PLUS, "+", Vec::new(), Vec::new())
            .expect("plus token");
        let right_rhs = right_cache
            .token(SyntaxKind::LIDENT, "rhs", Vec::new(), Vec::new())
            .expect("rhs token");
        let mut right = right_cache
            .node(
                SyntaxKind::LITERAL,
                vec![GreenElement::Token(right_rhs.clone())],
            )
            .expect("right leaf");
        for _ in 0..10_000 {
            right = right_cache
                .node(
                    SyntaxKind::BIN_EXPR,
                    vec![
                        GreenElement::Node(right),
                        GreenElement::Token(right_plus.clone()),
                        GreenElement::Token(right_rhs.clone()),
                    ],
                )
                .expect("right binary node");
        }

        assert!(left == right);
        assert_eq!(left.last_token().map(GreenToken::text), Some("rhs"));
        let mut left_hash = std::collections::hash_map::DefaultHasher::new();
        let mut right_hash = std::collections::hash_map::DefaultHasher::new();
        left.hash(&mut left_hash);
        right.hash(&mut right_hash);
        assert_eq!(left_hash.finish(), right_hash.finish());
        assert!(format!("{left:?}").len() < 128);
    }
}
