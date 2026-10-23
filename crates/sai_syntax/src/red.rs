use std::cell::{Cell, OnceCell};
use std::fmt::{self, Formatter};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sai_vfs::{FileId, TextRange, TextSize, WorkspaceSnapshot, WorkspaceSnapshotId};

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
    identity: RootIdentity,
    index: RedTreeIndex,
    token_index: OnceCell<RedTokenIndex>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RootIdentity {
    Snapshot {
        file: FileId,
        id: WorkspaceSnapshotId,
    },
    Tree(u64),
    Query(u64),
}

impl RootIdentity {
    fn belongs_to_file(self, file: FileId) -> bool {
        match self {
            Self::Snapshot {
                file: snapshot_file,
                ..
            } => snapshot_file == file,
            Self::Tree(_) | Self::Query(_) => true,
        }
    }
}

static NEXT_TREE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_QUERY_ROOT_ID: AtomicU64 = AtomicU64::new(1);

fn next_tree_id() -> u64 {
    let mut current = NEXT_TREE_ID.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("syntax tree identity space exhausted");
        match NEXT_TREE_ID.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current,
            Err(observed) => current = observed,
        }
    }
}

fn next_query_root_id() -> u64 {
    let mut current = NEXT_QUERY_ROOT_ID.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .expect("query root identity space exhausted");
        match NEXT_QUERY_ROOT_ID.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current,
            Err(observed) => current = observed,
        }
    }
}

struct RedTreeIndex {
    nodes: Box<[NodeIndexEntry]>,
    child_node_ids: Box<[usize]>,
}

struct RedTokenIndex {
    tokens: Box<[TokenIndexEntry]>,
    node_ranges: Box<[(usize, usize)]>,
}

struct NodeIndexEntry {
    green: GreenNode,
    offset: TextSize,
    text_range: TextRange,
    child_ids_start: usize,
    red_data: Option<Rc<NodeData>>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxRootError {
    FileNotInSnapshot,
    SourceMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxNodePtrError {
    FileMismatch,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SyntaxNodePtr {
    file: FileId,
    identity: RootIdentity,
    node_index: usize,
    kind: crate::SyntaxKind,
    range: TextRange,
}

impl SyntaxNodePtr {
    pub fn new(file: FileId, node: &SyntaxNode) -> Result<Self, SyntaxNodePtrError> {
        if !node.belongs_to_file(file) {
            return Err(SyntaxNodePtrError::FileMismatch);
        }

        Ok(Self {
            file,
            identity: node.1.identity,
            node_index: resolve_node_index(&node.0, &node.1.index)
                .expect("a syntax node belongs to its red tree index"),
            kind: node.kind(),
            range: node.text_range(),
        })
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub const fn kind(&self) -> crate::SyntaxKind {
        self.kind
    }

    pub const fn text_range(&self) -> TextRange {
        self.range
    }

    /// Whether two pointers identify the same node within identical source.
    ///
    /// Unlike derived pointer equality, this ignores the red-root identity. Callers must
    /// first establish that both pointers refer to the same unchanged source
    /// file.
    pub fn is_same_source_node(&self, other: &Self) -> bool {
        self.file == other.file
            && self.node_index == other.node_index
            && self.kind == other.kind
            && self.range == other.range
    }

    pub fn resolve(&self, file: FileId, root: &SyntaxNode) -> Option<SyntaxNode> {
        if file != self.file
            || !self.identity.belongs_to_file(file)
            || root.1.identity != self.identity
        {
            return None;
        }

        self.resolve_by_shape(file, root)
    }

    /// Rebuilds this pointer against another red root for the same unchanged
    /// source file. The node index, kind, and range must still match.
    pub fn rebind(&self, file: FileId, root: &SyntaxNode) -> Option<Self> {
        Self::new(file, &self.resolve_by_shape(file, root)?).ok()
    }

    fn resolve_by_shape(&self, file: FileId, root: &SyntaxNode) -> Option<SyntaxNode> {
        if file != self.file || !self.identity.belongs_to_file(file) {
            return None;
        }
        let data = root
            .1
            .index
            .nodes
            .get(self.node_index)?
            .red_data
            .as_ref()?
            .clone();
        let node = SyntaxNode(data, root.1.clone());
        (node.kind() == self.kind && node.text_range() == self.range).then_some(node)
    }
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
        loop {
            if Rc::strong_count(&self.0) != 1 {
                break;
            }

            let parent = Rc::get_mut(&mut self.0).and_then(|data| data.parent.take());
            let Some(parent) = parent else {
                break;
            };
            let node = std::mem::replace(&mut self.0, parent);
            drop(node);
        }
    }
}

impl Drop for RedTreeCache {
    fn drop(&mut self) {
        for entry in self.index.nodes.iter_mut().rev() {
            if let Some(data) = entry.red_data.take() {
                if let Ok(mut data) = Rc::try_unwrap(data) {
                    if let Some(parent) = data.parent.take() {
                        drop(parent);
                    }
                }
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
        Self::new_root_with_identity(green, RootIdentity::Tree(next_tree_id()))
    }

    /// Creates an ephemeral root for query-local AST traversal. Pointers and
    /// IDs from this root must not escape the query computation.
    pub(crate) fn new_root_for_query(green: GreenNode) -> Self {
        Self::new_root_with_identity(green, RootIdentity::Tree(u64::MAX))
    }

    /// Creates a query root with a unique identity so its IDs cannot resolve
    /// against a root produced by another callback invocation.
    pub(crate) fn new_root_for_query_callback(green: GreenNode) -> Self {
        Self::new_root_with_identity(green, RootIdentity::Query(next_query_root_id()))
    }

    pub(crate) fn new_root_in_snapshot(
        green: GreenNode,
        file: FileId,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Self, SyntaxRootError> {
        let source = snapshot
            .file(file)
            .map(|file| file.source())
            .ok_or(SyntaxRootError::FileNotInSnapshot)?;
        if !green_text_matches_source(&green, source.text()) {
            return Err(SyntaxRootError::SourceMismatch);
        }

        Ok(Self::new_root_with_identity(
            green,
            RootIdentity::Snapshot {
                file,
                id: snapshot.id(),
            },
        ))
    }

    fn new_root_with_identity(green: GreenNode, identity: RootIdentity) -> Self {
        let root = Rc::new(NodeData {
            green,
            parent: None,
            index_in_parent: None,
            offset: TextSize::new(0),
            index_id: Cell::new(Some(0)),
        });
        let cache = Rc::new(RedTreeCache {
            identity,
            index: build_red_node_index(root.clone()),
            token_index: OnceCell::new(),
        });
        Self(root, cache)
    }

    pub fn green(&self) -> &GreenNode {
        &self.0.green
    }

    pub(crate) fn root_identity(&self) -> RootIdentity {
        self.1.identity
    }

    pub(crate) fn belongs_to_file(&self, file: FileId) -> bool {
        self.1.identity.belongs_to_file(file)
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
        if let Some(node_id) = resolve_node_index(&self.0, &self.1.index)
            && let Some(entry) = self.1.index.nodes.get(node_id)
        {
            return entry.text_range;
        }

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
        let parent_index_id = self
            .0
            .index_id
            .get()
            .or_else(|| resolve_node_index(&self.0, &self.1.index));
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
                        .nodes
                        .get(parent_id)
                        .and_then(|entry| {
                            cache
                                .index
                                .child_node_ids
                                .get(entry.child_ids_start.checked_add(index)?)
                        })
                        .copied()
                        .filter(|node_id| *node_id != usize::MAX)
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
    /// index. Synthetic zero-width NEWLINE tokens are omitted; EOF and
    /// error-marked zero-width FSTRING_END tokens remain addressable.
    pub fn token_at_offset(&self, offset: TextSize) -> TokenAtOffset {
        let index = self
            .1
            .token_index
            .get_or_init(|| build_red_token_index(&self.1.index));
        let Some(node_id) = resolve_node_index(&self.0, &self.1.index) else {
            return TokenAtOffset::None;
        };
        let (token_start, token_end) = index
            .node_ranges
            .get(node_id)
            .copied()
            .expect("node index is valid");
        let token_entries = &index.tokens[token_start..token_end];
        let first = token_entries.partition_point(|entry| entry.text_range.end() < offset);
        let after_last = token_entries.partition_point(|entry| entry.text_range.start() <= offset);
        let matching_count = after_last.saturating_sub(first);
        let match_start = if offset == self.full_range().end()
            && matching_count > 2
            && token_entries
                .get(after_last.saturating_sub(1))
                .is_some_and(|entry| entry.green.kind() == crate::SyntaxKind::EOF)
        {
            after_last - 2
        } else {
            first
        };
        let mut matches = token_entries[match_start..after_last]
            .iter()
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

fn build_red_node_index(root: Rc<NodeData>) -> RedTreeIndex {
    let root_child_count = root.green.children().len();
    let mut child_node_ids = vec![usize::MAX; root_child_count];
    let mut nodes = vec![NodeIndexEntry {
        green: root.green.clone(),
        offset: root.offset,
        text_range: TextRange::empty(root.offset),
        child_ids_start: 0,
        red_data: Some(root.clone()),
    }];
    let mut stack = vec![(0_usize, 0_usize)];

    while let Some((parent_id, child_index)) = stack.last().copied() {
        if child_index >= nodes[parent_id].green.children().len() {
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
                let parent_child_id_index = nodes[parent_id]
                    .child_ids_start
                    .checked_add(child_index)
                    .expect("child index is within the node");
                child_node_ids[parent_child_id_index] = child_id;
                let child_count = green.children().len();
                let parent_data = nodes[parent_id]
                    .red_data
                    .as_ref()
                    .expect("indexed parent has a red node")
                    .clone();
                let child_data = Rc::new(NodeData {
                    green: green.clone(),
                    parent: Some(parent_data),
                    index_in_parent: Some(index_in_parent),
                    offset: child_offset,
                    index_id: Cell::new(Some(child_id)),
                });
                nodes.push(NodeIndexEntry {
                    green,
                    offset: child_offset,
                    text_range: TextRange::empty(child_offset),
                    child_ids_start: child_node_ids.len(),
                    red_data: Some(child_data),
                });
                child_node_ids.resize(
                    child_node_ids
                        .len()
                        .checked_add(child_count)
                        .expect("child index length fits in memory"),
                    usize::MAX,
                );
                stack.push((child_id, 0));
            }
            GreenElement::Token(_) => {}
        }
    }

    let mut contains_tokens = vec![false; nodes.len()];
    for node_id in (0..nodes.len()).rev() {
        let green = nodes[node_id].green.clone();
        let offset = nodes[node_id].offset;
        let child_ids_start = nodes[node_id].child_ids_start;
        let mut first_token_start = None;
        let mut last_token_end = None;

        for (child_index, child) in green.children().iter().enumerate() {
            let child_range = match child {
                GreenElement::Node(_) => {
                    let child_id = child_node_ids[child_ids_start + child_index];
                    debug_assert_ne!(child_id, usize::MAX);
                    contains_tokens[child_id].then_some(nodes[child_id].text_range)
                }
                GreenElement::Token(token) => {
                    let child_offset = add_size(
                        offset,
                        green
                            .child_offset(child_index)
                            .unwrap_or_else(|| TextSize::new(0)),
                    );
                    Some(token_text_range(token, child_offset))
                }
            };
            if let Some(range) = child_range {
                first_token_start.get_or_insert(range.start());
                last_token_end = Some(range.end());
            }
        }

        let text_range = match (first_token_start, last_token_end) {
            (Some(start), Some(end)) => {
                TextRange::new(start, end).unwrap_or_else(|| TextRange::empty(start))
            }
            _ => TextRange::empty(offset),
        };
        contains_tokens[node_id] = first_token_start.is_some();
        nodes[node_id].text_range = text_range;
    }

    RedTreeIndex {
        nodes: nodes.into_boxed_slice(),
        child_node_ids: child_node_ids.into_boxed_slice(),
    }
}

fn green_text_matches_source(green: &GreenNode, source: &str) -> bool {
    if usize::try_from(green.text_len().raw()).ok() != Some(source.len()) {
        return false;
    }

    let source = source.as_bytes();
    let mut offset = 0;
    let mut stack = green.children().iter().rev().collect::<Vec<_>>();
    while let Some(element) = stack.pop() {
        match element {
            GreenElement::Node(node) => stack.extend(node.children().iter().rev()),
            GreenElement::Token(token) => {
                for trivia in token.leading_trivia() {
                    if !consume_source_bytes(source, &mut offset, trivia.text()) {
                        return false;
                    }
                }
                if !consume_source_bytes(source, &mut offset, token.text()) {
                    return false;
                }
                for trivia in token.trailing_trivia() {
                    if !consume_source_bytes(source, &mut offset, trivia.text()) {
                        return false;
                    }
                }
            }
        }
    }

    offset == source.len()
}

fn consume_source_bytes(source: &[u8], offset: &mut usize, expected: &str) -> bool {
    let Some(end) = (*offset).checked_add(expected.len()) else {
        return false;
    };
    if source.get(*offset..end) != Some(expected.as_bytes()) {
        return false;
    }
    *offset = end;
    true
}

fn build_red_token_index(index: &RedTreeIndex) -> RedTokenIndex {
    let mut tokens = Vec::new();
    let mut node_ranges = vec![(0, 0); index.nodes.len()];
    let mut stack = vec![(0_usize, 0_usize)];

    while let Some((parent_id, child_index)) = stack.last().copied() {
        if child_index == 0 {
            node_ranges[parent_id].0 = tokens.len();
        }
        let parent = &index.nodes[parent_id];
        if child_index >= parent.green.children().len() {
            node_ranges[parent_id].1 = tokens.len();
            stack.pop();
            continue;
        }
        stack.last_mut().expect("stack is non-empty").1 += 1;
        let Some(child) = parent.green.children().get(child_index) else {
            continue;
        };
        let child_offset = add_size(
            parent.offset,
            parent
                .green
                .child_offset(child_index)
                .unwrap_or_else(|| TextSize::new(0)),
        );
        let index_in_parent = u32::try_from(child_index).unwrap_or(u32::MAX);
        match child {
            GreenElement::Node(_) => {
                let child_id = index.child_node_ids[parent.child_ids_start + child_index];
                debug_assert_ne!(child_id, usize::MAX);
                stack.push((child_id, 0));
            }
            GreenElement::Token(green) if green.kind() != crate::SyntaxKind::NEWLINE => {
                tokens.push(TokenIndexEntry {
                    text_range: token_text_range(green, child_offset),
                    green: green.clone(),
                    parent_node_id: parent_id,
                    index_in_parent,
                    offset: child_offset,
                });
            }
            GreenElement::Token(_) => {}
        }
    }

    RedTokenIndex {
        tokens: tokens.into_boxed_slice(),
        node_ranges: node_ranges.into_boxed_slice(),
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
        let child_id_index = parent_entry.child_ids_start.checked_add(child_index)?;
        let node_id = *index.child_node_ids.get(child_id_index)?;
        if node_id == usize::MAX {
            return None;
        }
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
    let data = cache
        .index
        .nodes
        .get(node_id)
        .and_then(|entry| entry.red_data.as_ref())
        .expect("indexed node has a cached red handle")
        .clone();
    SyntaxNode(data, cache)
}

impl SyntaxToken {
    pub fn kind(&self) -> crate::SyntaxKind {
        self.green.kind()
    }

    /// Whether both handles point to the same token occurrence in one red tree.
    pub fn is_same_token(&self, other: &Self) -> bool {
        self.index_in_parent == other.index_in_parent
            && Rc::ptr_eq(&self.parent.0, &other.parent.0)
            && Rc::ptr_eq(&self.parent.1, &other.parent.1)
    }

    pub fn text(&self) -> &str {
        self.green.text()
    }

    pub fn green(&self) -> &GreenToken {
        &self.green
    }

    pub fn has_error(&self) -> bool {
        self.green.has_error()
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
    fn root_construction_materializes_red_handles_for_every_node() {
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
        let index = &root.1.index;

        for node in root.descendants() {
            let expected_range = match (node.first_token(), node.last_token()) {
                (Some(first), Some(last)) => {
                    TextRange::new(first.text_range().start(), last.text_range().end())
                        .unwrap_or_else(|| TextRange::empty(first.text_range().start()))
                }
                _ => TextRange::empty(node.offset()),
            };
            assert_eq!(
                node.text_range(),
                expected_range,
                "cached range for {:?}",
                node.kind()
            );
        }

        assert_eq!(index.nodes.len(), 4);
        assert_eq!(
            index
                .child_node_ids
                .iter()
                .filter(|node_id| **node_id != usize::MAX)
                .count(),
            3
        );
        for (node_id, entry) in index.nodes.iter().enumerate() {
            let data = entry
                .red_data
                .as_ref()
                .cloned()
                .expect("red handle was materialized");
            assert_eq!(data.index_id.get(), Some(node_id));
        }
    }

    #[test]
    fn syntax_tokens_expose_preserved_literal_error_flags() {
        let root = flat_root("1__2 3");
        let literal_error_flags = root
            .children_with_tokens()
            .filter_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::INT => {
                    Some(token.has_error())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(literal_error_flags, [true, false]);
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
    fn token_at_offset_keeps_empty_format_string_end_tokens() {
        let root = flat_root("f\"\nnext");
        let TokenAtOffset::Between(left, right) = root.token_at_offset(TextSize::new(2)) else {
            panic!("expected the format string start and its empty error end");
        };

        assert_eq!(left.kind(), SyntaxKind::FSTRING_START);
        assert_eq!(right.kind(), SyntaxKind::FSTRING_END);
        assert_eq!(right.text(), "");
    }

    #[test]
    fn token_at_eof_keeps_empty_format_string_end_and_eof_addressable() {
        let root = flat_root("f\"");
        let TokenAtOffset::Between(left, right) = root.token_at_offset(TextSize::new(2)) else {
            panic!("expected the empty format string end and EOF");
        };

        assert_eq!(left.kind(), SyntaxKind::FSTRING_END);
        assert_eq!(left.text(), "");
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
    fn red_tree_index_is_shared_and_token_offsets_are_sorted() {
        let root = flat_root("a+b");
        assert!(root.1.token_index.get().is_none());
        assert!(matches!(
            root.token_at_offset(TextSize::new(3)),
            TokenAtOffset::Between(_, _)
        ));
        assert!(matches!(
            root.token_at_offset(TextSize::new(1)),
            TokenAtOffset::Between(_, _)
        ));
        let entries = root
            .1
            .token_index
            .get()
            .expect("offset lookup builds the token index");
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
