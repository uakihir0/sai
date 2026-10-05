use std::cell::{Cell, OnceCell, RefCell};
use std::fmt::{self, Formatter};
use std::rc::Rc;

use sai_vfs::{TextRange, TextSize};

use crate::{GreenElement, GreenNode, GreenToken};

#[derive(Clone)]
pub struct SyntaxNode(Rc<NodeData>, Rc<RedTreeCache>);

struct NodeData {
    green: GreenNode,
    parent: Option<Rc<NodeData>>,
    index_in_parent: Option<u32>,
    offset: TextSize,
    index_id: Cell<Option<usize>>,
}

struct RedTreeCache {
    root: Rc<NodeData>,
    index: OnceCell<RedTreeIndex>,
}

struct RedTreeIndex {
    nodes: Box<[NodeIndexEntry]>,
    tokens: Box<[TokenIndexEntry]>,
}

struct NodeIndexEntry {
    green: GreenNode,
    parent: Option<usize>,
    index_in_parent: Option<u32>,
    offset: TextSize,
    child_node_ids: Box<[Option<usize>]>,
    token_start: usize,
    token_end: usize,
    red_data: RefCell<Option<Rc<NodeData>>>,
}

struct TokenIndexEntry {
    green: GreenToken,
    parent_node_id: usize,
    index_in_parent: u32,
    offset: TextSize,
    text_range: TextRange,
}

#[derive(Clone, Debug)]
pub struct SyntaxToken {
    green: GreenToken,
    parent: SyntaxNode,
    index_in_parent: u32,
    offset: TextSize,
}

#[derive(Clone, Debug)]
pub enum SyntaxElement {
    Node(SyntaxNode),
    Token(SyntaxToken),
}

#[derive(Clone, Debug)]
pub enum TokenAtOffset {
    None,
    Single(SyntaxToken),
    Between(SyntaxToken, SyntaxToken),
}

pub struct Descendants {
    stack: Vec<SyntaxNode>,
}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyntaxNode")
            .field("kind", &self.kind())
            .field("offset", &self.offset())
            .field("index_in_parent", &self.index_in_parent())
            .finish()
    }
}

impl Drop for SyntaxNode {
    fn drop(&mut self) {
        let placeholder = Rc::new(NodeData {
            green: self.0.green.clone(),
            parent: None,
            index_in_parent: self.0.index_in_parent,
            offset: self.0.offset,
            index_id: Cell::new(None),
        });
        let mut current = Some(std::mem::replace(&mut self.0, placeholder));
        while let Some(node) = current {
            match Rc::try_unwrap(node) {
                Ok(mut data) => current = data.parent.take(),
                Err(node) => {
                    drop(node);
                    break;
                }
            }
        }
    }
}

impl Drop for RedTreeCache {
    fn drop(&mut self) {
        let Some(mut index) = self.index.take() else {
            return;
        };
        let mut pending = Vec::new();
        for entry in &mut index.nodes {
            if let Some(data) = entry.red_data.get_mut().take() {
                pending.push(data);
            }
        }
        while let Some(node) = pending.pop() {
            match Rc::try_unwrap(node) {
                Ok(mut data) => {
                    if let Some(parent) = data.parent.take() {
                        pending.push(parent);
                    }
                }
                Err(node) => drop(node),
            }
        }
    }
}

impl Iterator for Descendants {
    type Item = SyntaxNode;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        let children = node.children().collect::<Vec<_>>();
        self.stack.extend(children.into_iter().rev());
        Some(node)
    }
}

impl SyntaxNode {
    pub fn new_root(green: GreenNode) -> Self {
        let root = Rc::new(NodeData {
            green,
            parent: None,
            index_in_parent: None,
            offset: TextSize::new(0),
            index_id: Cell::new(Some(0)),
        });
        let cache = Rc::new(RedTreeCache {
            root: root.clone(),
            index: OnceCell::new(),
        });
        Self(root, cache)
    }

    pub fn green(&self) -> &GreenNode {
        &self.0.green
    }

    pub fn kind(&self) -> crate::SyntaxKind {
        self.green().kind()
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        self.0
            .parent
            .as_ref()
            .map(|parent| Self(parent.clone(), self.1.clone()))
    }

    pub fn index_in_parent(&self) -> Option<u32> {
        self.0.index_in_parent
    }

    pub fn offset(&self) -> TextSize {
        self.0.offset
    }

    pub fn full_range(&self) -> TextRange {
        TextRange::new(
            self.offset(),
            add_size(self.offset(), self.green().text_len()),
        )
        .unwrap_or_else(|| TextRange::empty(self.offset()))
    }

    /// The range of source text excluding the first token's leading and last
    /// token's trailing trivia.
    pub fn text_range(&self) -> TextRange {
        match (self.first_token(), self.last_token()) {
            (Some(first), Some(last)) => {
                TextRange::new(first.text_range().start(), last.text_range().end())
                    .unwrap_or_else(|| TextRange::empty(first.text_range().start()))
            }
            _ => TextRange::empty(self.offset()),
        }
    }

    pub fn children_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> + '_ {
        let parent = self.0.clone();
        let cache = self.1.clone();
        let parent_index_id = self.0.index_id.get().or_else(|| {
            self.1
                .index
                .get()
                .and_then(|index| resolve_node_index(&self.0, index))
        });
        self.green()
            .children()
            .iter()
            .enumerate()
            .map(move |(index, child)| {
                let child_offset = add_size(
                    self.offset(),
                    self.green()
                        .child_offset(index)
                        .unwrap_or_else(|| TextSize::new(0)),
                );
                let child_index_id = parent_index_id.and_then(|parent_id| {
                    cache
                        .index
                        .get()
                        .and_then(|tree| tree.nodes.get(parent_id))
                        .and_then(|entry| entry.child_node_ids.get(index))
                        .copied()
                        .flatten()
                });
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                match child {
                    GreenElement::Node(_) => {
                        let node = child_index_id
                            .map(|id| red_node_for_index(cache.clone(), id))
                            .unwrap_or_else(|| {
                                let GreenElement::Node(green) = child else {
                                    unreachable!("the match arm guarantees a node")
                                };
                                SyntaxNode(
                                    Rc::new(NodeData {
                                        green: green.clone(),
                                        parent: Some(parent.clone()),
                                        index_in_parent: Some(index),
                                        offset: child_offset,
                                        index_id: Cell::new(None),
                                    }),
                                    cache.clone(),
                                )
                            });
                        SyntaxElement::Node(node)
                    }
                    GreenElement::Token(green) => SyntaxElement::Token(SyntaxToken {
                        green: green.clone(),
                        parent: SyntaxNode(parent.clone(), cache.clone()),
                        index_in_parent: index,
                        offset: child_offset,
                    }),
                }
            })
    }

    pub fn children(&self) -> impl Iterator<Item = SyntaxNode> + '_ {
        self.children_with_tokens()
            .filter_map(|element| match element {
                SyntaxElement::Node(node) => Some(node),
                SyntaxElement::Token(_) => None,
            })
    }

    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> {
        std::iter::successors(Some(self.clone()), |node| node.parent())
    }

    pub fn descendants(&self) -> Descendants {
        Descendants {
            stack: vec![self.clone()],
        }
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        let mut stack = self
            .children_with_tokens()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        while let Some(element) = stack.pop() {
            match element {
                SyntaxElement::Token(token) => return Some(token),
                SyntaxElement::Node(node) => stack.extend(
                    node.children_with_tokens()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                ),
            }
        }
        None
    }

    pub fn last_token(&self) -> Option<SyntaxToken> {
        let mut stack = self.children_with_tokens().collect::<Vec<_>>();
        while let Some(element) = stack.pop() {
            match element {
                SyntaxElement::Token(token) => return Some(token),
                SyntaxElement::Node(node) => stack.extend(node.children_with_tokens()),
            }
        }
        None
    }

    /// Looks up token text at an absolute source offset in the cached token
    /// index. Synthetic zero-width NEWLINE tokens are omitted; EOF remains
    /// addressable at the end of the file.
    pub fn token_at_offset(&self, offset: TextSize) -> TokenAtOffset {
        let index = self
            .1
            .index
            .get_or_init(|| build_red_tree_index(self.1.root.clone()));
        let Some(node_id) = resolve_node_index(&self.0, index) else {
            return TokenAtOffset::None;
        };
        let node_entry = index.nodes.get(node_id).expect("node index is valid");
        let token_entries = &index.tokens[node_entry.token_start..node_entry.token_end];
        let first = token_entries.partition_point(|entry| entry.text_range.end() < offset);
        let mut matches = token_entries[first..]
            .iter()
            .take_while(|entry| entry.text_range.start() <= offset)
            .filter(|entry| offset <= entry.text_range.end())
            .take(2)
            .map(|entry| SyntaxToken {
                green: entry.green.clone(),
                parent: red_node_for_index(self.1.clone(), entry.parent_node_id),
                index_in_parent: entry.index_in_parent,
                offset: entry.offset,
            });
        let first = matches.next();
        let second = matches.next();
        match (first, second) {
            (None, None) => TokenAtOffset::None,
            (Some(token), None) => TokenAtOffset::Single(token),
            (Some(left), Some(right)) => TokenAtOffset::Between(left, right),
            (None, Some(_)) => TokenAtOffset::None,
        }
    }

    pub fn covering_element(&self, range: TextRange) -> Option<SyntaxElement> {
        if !contains_range(self.full_range(), range) {
            return None;
        }
        let mut current = self.clone();
        loop {
            let child = current
                .children_with_tokens()
                .find(|element| contains_range(element.full_range(), range));
            match child {
                Some(SyntaxElement::Node(node)) => current = node,
                Some(token @ SyntaxElement::Token(_)) => return Some(token),
                None => return Some(SyntaxElement::Node(current)),
            }
        }
    }
}

fn build_red_tree_index(root: Rc<NodeData>) -> RedTreeIndex {
    let root_child_count = root.green.children().len();
    let mut nodes = vec![NodeIndexEntry {
        green: root.green.clone(),
        parent: None,
        index_in_parent: None,
        offset: root.offset,
        child_node_ids: vec![None; root_child_count].into_boxed_slice(),
        token_start: 0,
        token_end: 0,
        red_data: RefCell::new(Some(root.clone())),
    }];
    let mut tokens = Vec::new();
    let mut stack = vec![(0_usize, 0_usize)];

    while let Some((parent_id, child_index)) = stack.last().copied() {
        if child_index >= nodes[parent_id].green.children().len() {
            nodes[parent_id].token_end = tokens.len();
            stack.pop();
            continue;
        }
        stack.last_mut().expect("stack is non-empty").1 += 1;
        let Some(child) = nodes[parent_id].green.children().get(child_index).cloned() else {
            continue;
        };
        let parent_offset = nodes[parent_id].offset;
        let child_offset = add_size(
            parent_offset,
            nodes[parent_id]
                .green
                .child_offset(child_index)
                .unwrap_or_else(|| TextSize::new(0)),
        );
        let index_in_parent = u32::try_from(child_index).unwrap_or(u32::MAX);
        match child {
            GreenElement::Node(green) => {
                let child_id = nodes.len();
                nodes[parent_id].child_node_ids[child_index] = Some(child_id);
                let child_count = green.children().len();
                nodes.push(NodeIndexEntry {
                    green,
                    parent: Some(parent_id),
                    index_in_parent: Some(index_in_parent),
                    offset: child_offset,
                    child_node_ids: vec![None; child_count].into_boxed_slice(),
                    token_start: tokens.len(),
                    token_end: tokens.len(),
                    red_data: RefCell::new(None),
                });
                stack.push((child_id, 0));
            }
            GreenElement::Token(green) if green.kind() != crate::SyntaxKind::NEWLINE => {
                tokens.push(TokenIndexEntry {
                    text_range: token_text_range(&green, child_offset),
                    green,
                    parent_node_id: parent_id,
                    index_in_parent,
                    offset: child_offset,
                });
            }
            GreenElement::Token(_) => {}
        }
    }

    RedTreeIndex {
        nodes: nodes.into_boxed_slice(),
        tokens: tokens.into_boxed_slice(),
    }
}

fn token_text_range(green: &GreenToken, offset: TextSize) -> TextRange {
    let leading_len = green.leading_trivia().iter().fold(0_u32, |len, trivia| {
        len.saturating_add(u32::try_from(trivia.text().len()).unwrap_or(u32::MAX))
    });
    let start = add_size(offset, TextSize::new(leading_len));
    let end = add_size(
        start,
        TextSize::from_usize(green.text().len()).unwrap_or(TextSize::new(u32::MAX)),
    );
    TextRange::new(start, end).unwrap_or_else(|| TextRange::empty(start))
}

fn resolve_node_index(node: &Rc<NodeData>, index: &RedTreeIndex) -> Option<usize> {
    if let Some(node_id) = node.index_id.get() {
        return Some(node_id);
    }

    let mut path = Vec::new();
    let mut current = Some(node.clone());
    let mut parent_id = None;
    while let Some(data) = current {
        if let Some(node_id) = data.index_id.get() {
            parent_id = Some(node_id);
            break;
        }
        current = data.parent.clone();
        path.push(data);
    }

    let mut parent_id = parent_id?;
    while let Some(data) = path.pop() {
        let child_index = usize::try_from(data.index_in_parent?).ok()?;
        let parent_entry = index.nodes.get(parent_id)?;
        let node_id = parent_entry
            .child_node_ids
            .get(child_index)
            .copied()
            .flatten()?;
        let node_entry = index.nodes.get(node_id)?;
        if node_entry.offset != data.offset || !node_entry.green.ptr_eq(&data.green) {
            return None;
        }
        data.index_id.set(Some(node_id));
        parent_id = node_id;
    }
    Some(parent_id)
}

fn red_node_for_index(cache: Rc<RedTreeCache>, node_id: usize) -> SyntaxNode {
    let index = cache
        .index
        .get()
        .expect("red-node index is initialized before resolving a token");
    let mut current_id = node_id;
    let mut path = Vec::<usize>::new();

    loop {
        let entry = index.nodes.get(current_id).expect("node index is valid");
        let existing_data = entry.red_data.borrow().clone();
        if let Some(data) = existing_data {
            let mut parent = data;
            for path_id in path.into_iter().rev() {
                let entry = index.nodes.get(path_id).expect("node path index is valid");
                let data = Rc::new(NodeData {
                    green: entry.green.clone(),
                    parent: Some(parent.clone()),
                    index_in_parent: entry.index_in_parent,
                    offset: entry.offset,
                    index_id: Cell::new(Some(path_id)),
                });
                *entry.red_data.borrow_mut() = Some(data.clone());
                parent = data;
            }
            return SyntaxNode(parent, cache.clone());
        }
        path.push(current_id);
        current_id = entry.parent.expect("the root red node is strongly owned");
    }
}

impl SyntaxToken {
    pub fn kind(&self) -> crate::SyntaxKind {
        self.green.kind()
    }

    pub fn text(&self) -> &str {
        self.green.text()
    }

    pub fn green(&self) -> &GreenToken {
        &self.green
    }

    pub fn parent(&self) -> &SyntaxNode {
        &self.parent
    }

    pub fn index_in_parent(&self) -> u32 {
        self.index_in_parent
    }

    /// Absolute start offset including this token's leading trivia.
    pub fn offset(&self) -> TextSize {
        self.offset
    }

    pub fn full_range(&self) -> TextRange {
        TextRange::new(
            self.offset,
            add_size(self.offset, self.green.full_text_len()),
        )
        .unwrap_or_else(|| TextRange::empty(self.offset))
    }

    pub fn text_range(&self) -> TextRange {
        token_text_range(&self.green, self.offset)
    }
}

impl SyntaxElement {
    pub fn kind(&self) -> crate::SyntaxKind {
        match self {
            Self::Node(node) => node.kind(),
            Self::Token(token) => token.kind(),
        }
    }

    pub fn text_range(&self) -> TextRange {
        match self {
            Self::Node(node) => node.text_range(),
            Self::Token(token) => token.text_range(),
        }
    }

    pub fn full_range(&self) -> TextRange {
        match self {
            Self::Node(node) => node.full_range(),
            Self::Token(token) => token.full_range(),
        }
    }

    pub fn as_node(&self) -> Option<&SyntaxNode> {
        match self {
            Self::Node(node) => Some(node),
            Self::Token(_) => None,
        }
    }

    pub fn as_token(&self) -> Option<&SyntaxToken> {
        match self {
            Self::Node(_) => None,
            Self::Token(token) => Some(token),
        }
    }
}

fn add_size(left: TextSize, right: TextSize) -> TextSize {
    TextSize::new(left.raw().saturating_add(right.raw()))
}

fn contains_range(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sai_vfs::{FileId, SnapshotFile, SourceFile, WorkspaceSnapshot};

    use super::*;
    use crate::{Event, NodeCache, SyntaxKind, build_green_tree, lex};

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

    fn flat_root(text: &str) -> SyntaxNode {
        let lexed = lex_text(text);
        let mut events = vec![Event::Start {
            kind: SyntaxKind::SOURCE_FILE,
            forward_parent: None,
        }];
        events.extend(lexed.tokens.iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        events.push(Event::Finish);
        let mut cache = NodeCache::new();
        let tree = build_green_tree(text, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("build green tree");
        SyntaxNode::new_root(tree.root)
    }

    #[test]
    fn red_tree_offsets_and_ranges_distinguish_full_text_from_token_text() {
        let source = "  a+b";
        let root = flat_root(source);

        assert_eq!(
            root.full_range(),
            TextRange::new(TextSize::new(0), TextSize::new(5)).unwrap()
        );
        assert_eq!(
            root.text_range(),
            TextRange::new(TextSize::new(2), TextSize::new(5)).unwrap()
        );
        assert_eq!(
            root.first_token().as_ref().map(SyntaxToken::text),
            Some("a")
        );
        assert_eq!(
            root.last_token().as_ref().map(SyntaxToken::kind),
            Some(SyntaxKind::EOF)
        );
        assert_eq!(
            root.first_token().unwrap().full_range(),
            TextRange::new(TextSize::new(0), TextSize::new(3)).unwrap()
        );
    }

    #[test]
    fn token_at_offset_and_covering_element_use_text_ranges() {
        let root = flat_root("a+b");
        let TokenAtOffset::Between(left, right) = root.token_at_offset(TextSize::new(1)) else {
            panic!("expected tokens on both sides of the boundary");
        };
        assert_eq!(left.text(), "a");
        assert_eq!(right.text(), "+");
        assert!(matches!(
            root.token_at_offset(TextSize::new(0)),
            TokenAtOffset::Single(_)
        ));
        assert!(matches!(
            root.token_at_offset(TextSize::new(9)),
            TokenAtOffset::None
        ));

        let plus = TextRange::new(TextSize::new(1), TextSize::new(2)).unwrap();
        let element = root.covering_element(plus).expect("covering element");
        assert_eq!(element.kind(), SyntaxKind::PLUS);
        assert_eq!(element.text_range(), plus);
    }

    #[test]
    fn token_at_offset_keeps_eof_when_newline_shares_its_zero_width_offset() {
        let root = flat_root("x");
        let TokenAtOffset::Between(left, right) = root.token_at_offset(TextSize::new(1)) else {
            panic!("expected the final source token and EOF");
        };

        assert_eq!(left.kind(), SyntaxKind::LIDENT);
        assert_eq!(right.kind(), SyntaxKind::EOF);
    }

    #[test]
    fn indexed_tokens_remain_valid_after_the_root_handle_is_dropped() {
        let token = {
            let root = flat_root("x");
            let TokenAtOffset::Single(token) = root.token_at_offset(TextSize::new(0)) else {
                panic!("expected one token at the first character");
            };
            token
        };
        assert_eq!(token.text(), "x");
        drop(token);
    }

    #[test]
    fn token_offset_index_is_shared_and_sorted() {
        let root = flat_root("a+b");
        assert!(matches!(
            root.token_at_offset(TextSize::new(3)),
            TokenAtOffset::Between(_, _)
        ));
        assert!(matches!(
            root.token_at_offset(TextSize::new(1)),
            TokenAtOffset::Between(_, _)
        ));
        let entries = root.1.index.get().expect("offset lookup builds the index");
        assert_eq!(entries.tokens.len(), 4);
        assert!(
            entries
                .tokens
                .windows(2)
                .all(|pair| pair[0].text_range.start() <= pair[1].text_range.start())
        );
    }

    #[test]
    fn deep_red_nodes_debug_and_drop_without_recursion() {
        let source = "";
        let lexed = lex_text(source);
        let mut events = vec![Event::Start {
            kind: SyntaxKind::SOURCE_FILE,
            forward_parent: None,
        }];
        for _ in 0..10_000 {
            events.push(Event::Start {
                kind: SyntaxKind::ERROR,
                forward_parent: None,
            });
        }
        events.extend(lexed.tokens.iter().map(|token| Event::Token {
            kind: token.kind,
            n_raw: 1,
        }));
        for _ in 0..10_000 {
            events.push(Event::Finish);
        }
        events.push(Event::Finish);
        let mut cache = NodeCache::new();
        let tree = build_green_tree(source, &lexed.tokens, &lexed.trivia, events, &mut cache)
            .expect("deep green tree");
        let root = SyntaxNode::new_root(tree.root);
        let TokenAtOffset::Single(token) = root.token_at_offset(TextSize::new(0)) else {
            panic!("expected EOF at the empty file offset");
        };
        let deepest = token.parent().clone();

        let debug = format!("{deepest:?}");
        assert!(debug.contains("ERROR"));
        drop(token);
        drop(deepest);
        drop(root);
        drop(cache);
    }

    #[test]
    fn parent_child_and_descendant_traversals_keep_source_order() {
        let source = "a+b";
        let lexed = lex_text(source);
        let events = vec![
            Event::Start {
                kind: SyntaxKind::SOURCE_FILE,
                forward_parent: None,
            },
            Event::Start {
                kind: SyntaxKind::BIN_EXPR,
                forward_parent: None,
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
            .expect("build green tree");
        let root = SyntaxNode::new_root(tree.root);
        let bin = root.children().next().expect("binary node");

        assert_eq!(bin.kind(), SyntaxKind::BIN_EXPR);
        assert_eq!(
            bin.children().map(|node| node.kind()).collect::<Vec<_>>(),
            [SyntaxKind::LITERAL, SyntaxKind::LITERAL]
        );
        let lhs = bin.children().next().expect("left operand");
        assert_eq!(
            lhs.ancestors().map(|node| node.kind()).collect::<Vec<_>>(),
            [
                SyntaxKind::LITERAL,
                SyntaxKind::BIN_EXPR,
                SyntaxKind::SOURCE_FILE
            ]
        );
        assert_eq!(
            bin.descendants()
                .map(|node| node.kind())
                .collect::<Vec<_>>(),
            [
                SyntaxKind::BIN_EXPR,
                SyntaxKind::LITERAL,
                SyntaxKind::LITERAL,
            ]
        );
        assert_eq!(lhs.index_in_parent(), Some(0));
        assert!(matches!(
            lhs.token_at_offset(TextSize::new(0)),
            TokenAtOffset::Single(_)
        ));
        assert!(matches!(
            lhs.token_at_offset(TextSize::new(2)),
            TokenAtOffset::None
        ));
    }
}
