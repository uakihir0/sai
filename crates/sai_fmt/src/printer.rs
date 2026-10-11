#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const INDENT_WIDTH: usize = 4;
const MAX_LINE_WIDTH: usize = 100;
static NEXT_PERSISTENT_STACK_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
thread_local! {
    static FITS_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// A document group referenced by [`Doc::IfBroken`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GroupId(pub u32);

/// A layout-independent description of formatted text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Doc {
    Text(String),
    Line,
    SoftLine,
    HardLine,
    Indent(Box<Doc>),
    Group(GroupId, Box<Doc>),
    IfBroken(GroupId, Box<Doc>, Box<Doc>),
    LineSuffix(Box<Doc>),
    Choice(Vec<Doc>),
    Flat(Box<Doc>),
    Verbatim(String),
    Concat(Vec<Doc>),
}

impl Doc {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn verbatim(text: impl Into<String>) -> Self {
        Self::Verbatim(text.into())
    }

    pub fn concat(docs: impl IntoIterator<Item = Doc>) -> Self {
        Self::Concat(docs.into_iter().collect())
    }

    pub fn group(id: GroupId, doc: Doc) -> Self {
        Self::Group(id, Box::new(doc))
    }

    pub fn if_broken(id: GroupId, broken: Doc, flat: Doc) -> Self {
        Self::IfBroken(id, Box::new(broken), Box::new(flat))
    }

    pub fn indent(doc: Doc) -> Self {
        Self::Indent(Box::new(doc))
    }

    pub fn line_suffix(doc: Doc) -> Self {
        Self::LineSuffix(Box::new(doc))
    }

    pub fn flat(doc: Doc) -> Self {
        Self::Flat(Box::new(doc))
    }

    pub fn choice(alternatives: impl IntoIterator<Item = Doc>) -> Self {
        Self::Choice(alternatives.into_iter().collect())
    }
}

/// An invalid document construct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrintError {
    /// `Text` must not contain a line break; use `Verbatim` for multiline text.
    TextContainsLineBreak,
    /// `IfBroken` must refer to a group that has already been encountered.
    UnknownGroup(GroupId),
    /// `Choice` requires at least one alternative.
    EmptyChoice,
}

/// Prints a document with a 100-scalar line width and a trailing LF.
///
/// An empty document prints as the empty string. All indentation is four
/// spaces, independent of platform and display-width conventions.
pub fn print(doc: &Doc) -> Result<String, PrintError> {
    print_with_forced_groups(doc, &[])
}

/// Prints a document while forcing the listed groups to break.
///
/// A forced group breaks even inside [`Doc::Flat`]. Forcing propagates to
/// containing groups except across [`Doc::Choice`], as specified by
/// `docs/impl/14-formatter.md` §14.3.2.
pub fn print_with_forced_groups(
    doc: &Doc,
    forced_groups: &[GroupId],
) -> Result<String, PrintError> {
    Renderer::new(doc, forced_groups, MAX_LINE_WIDTH).render()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Copy)]
struct Command<'a> {
    indent: usize,
    mode: Mode,
    choice_trial: Option<u64>,
    action: Action<'a>,
}

#[derive(Clone, Copy)]
enum Action<'a> {
    Doc(&'a Doc),
    VerbatimSegment(&'a str),
    InternalLineBreak {
        indent: usize,
        next_indent: Option<usize>,
    },
    LineBreak {
        next_indent: Option<usize>,
    },
    ChoiceEnd(u64),
}

struct GroupFrame {
    id: GroupId,
    parent: Option<Rc<GroupFrame>>,
}

struct FitFrame<'a> {
    commands: PersistentStack<FitCommand<'a>>,
    remaining: usize,
    group_modes_state: u64,
    deferred_suffixes: PersistentStack<FitCommand<'a>>,
    awaiting_group: Option<PendingFitGroup<'a>>,
    awaiting_choice: Option<PendingFitChoice<'a>>,
    choice_leading_break: Option<bool>,
}

#[derive(Clone, Copy)]
struct FitCommand<'a> {
    command: Command<'a>,
    region: FitRegion,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FitRegion {
    Candidate,
    Rest,
    DeferredSuffix,
}

struct PendingFitGroup<'a> {
    id: GroupId,
    doc: &'a Doc,
    indent: usize,
    region: FitRegion,
    cache_key: GroupFitCacheKey,
}

struct PendingFitChoice<'a> {
    command: Command<'a>,
    region: FitRegion,
    first_selectable_alternative: &'a Doc,
}

struct LeadingFrame<'a> {
    paths: Vec<LeadingPath<'a>>,
    rest: Vec<FitCommand<'a>>,
    remaining: usize,
    awaiting_group: Option<PendingLeadingGroup<'a>>,
    group_fit_result: Option<bool>,
    visited: HashSet<LeadingPathKey>,
}

struct LeadingPath<'a> {
    stack: PersistentStack<LeadingCommand<'a>>,
    deferred_suffixes: PersistentStack<FitCommand<'a>>,
    group_modes_state: u64,
}

#[derive(Clone, Copy)]
enum LeadingCommand<'a> {
    Doc(&'a Doc, Mode),
    Verbatim(&'a str),
    LineBreak,
}

#[derive(Clone)]
struct PersistentStack<T: Copy> {
    arena: Rc<PersistentStackArena<T>>,
    top: Option<usize>,
}

struct PersistentStackArena<T: Copy> {
    id: u64,
    nodes: RefCell<Vec<PersistentStackNode<T>>>,
}

struct PersistentStackNode<T: Copy> {
    value: T,
    tail: Option<usize>,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct PersistentStackKey {
    arena_id: u64,
    top: Option<usize>,
}

impl<T: Copy> Default for PersistentStack<T> {
    fn default() -> Self {
        Self {
            arena: new_persistent_stack_arena(),
            top: None,
        }
    }
}

impl<T: Copy> PersistentStackArena<T> {
    fn new() -> Self {
        let id = loop {
            let id = NEXT_PERSISTENT_STACK_ID.load(Ordering::Relaxed);
            let next_id = id
                .checked_add(1)
                .expect("persistent stack arena IDs do not overflow");
            if NEXT_PERSISTENT_STACK_ID
                .compare_exchange_weak(id, next_id, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break id;
            }
        };
        Self {
            id,
            nodes: RefCell::new(Vec::new()),
        }
    }
}

fn new_persistent_stack_arena<T: Copy>() -> Rc<PersistentStackArena<T>> {
    Rc::new(PersistentStackArena::new())
}

impl<T: Copy> PersistentStack<T> {
    fn with_arena(arena: Rc<PersistentStackArena<T>>) -> Self {
        Self { arena, top: None }
    }

    fn key(&self) -> PersistentStackKey {
        PersistentStackKey {
            arena_id: self.arena.id,
            top: self.top,
        }
    }

    fn is_empty(&self) -> bool {
        self.top.is_none()
    }

    fn push(&mut self, value: T) {
        let mut arena = self.arena.nodes.borrow_mut();
        let index = arena.len();
        arena.push(PersistentStackNode {
            value,
            tail: self.top,
        });
        self.top = Some(index);
    }

    fn pop(&mut self) -> Option<T> {
        let index = self.top?;
        let arena = self.arena.nodes.borrow();
        let node = &arena[index];
        self.top = node.tail;
        Some(node.value)
    }

    fn clear(&mut self) {
        self.top = None;
    }

    fn bottom_to_top(&self) -> Vec<T> {
        let mut values = Vec::new();
        let mut stack = self.clone();
        while let Some(value) = stack.pop() {
            values.push(value);
        }
        values.reverse();
        values
    }
}

#[derive(Hash, PartialEq, Eq)]
struct LeadingPathKey {
    stack: PersistentStackKey,
    deferred_suffixes: PersistentStackKey,
    group_modes_state: u64,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct GroupFitCacheKey {
    group: GroupId,
    doc: usize,
    rest: PersistentStackKey,
    remaining: usize,
    indent: usize,
    mode: Mode,
    region: FitRegion,
    deferred_suffixes: PersistentStackKey,
    group_modes_state: u64,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct ConcatExpansionKey {
    doc: usize,
    indent: usize,
    mode: Mode,
    choice_trial: Option<u64>,
    region: FitRegion,
    tail: PersistentStackKey,
}

struct ModeStateInterner {
    branches: HashMap<(u8, u64, u64), u64>,
    nodes: Vec<ModeStateNode>,
    enabled: bool,
}

#[derive(Clone, Copy)]
enum ModeStateNode {
    Empty,
    Flat,
    Break,
    Branch { depth: u8, left: u64, right: u64 },
}

impl Default for ModeStateInterner {
    fn default() -> Self {
        Self {
            branches: HashMap::new(),
            nodes: vec![
                ModeStateNode::Empty,
                ModeStateNode::Flat,
                ModeStateNode::Break,
            ],
            enabled: true,
        }
    }
}

impl ModeStateInterner {
    fn with_tracking(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    fn transition(&mut self, previous: u64, id: GroupId, mode: Mode) -> u64 {
        if !self.enabled {
            return previous;
        }
        if self.mode(previous, id) == Some(mode) {
            return previous;
        }
        let mut path = Vec::with_capacity(u32::BITS as usize);
        let mut node = previous;
        for depth in 0..u32::BITS as u8 {
            let (left, right) = match self.nodes[node as usize] {
                ModeStateNode::Empty => (0, 0),
                ModeStateNode::Branch {
                    depth: node_depth,
                    left,
                    right,
                } => {
                    debug_assert_eq!(node_depth, depth);
                    (left, right)
                }
                ModeStateNode::Flat | ModeStateNode::Break => {
                    unreachable!("group mode leaves only occur below all key bits")
                }
            };
            let right_branch = id.0 & (1 << (u32::BITS - 1 - u32::from(depth))) != 0;
            path.push((depth, left, right, right_branch));
            node = if right_branch { right } else { left };
        }

        let mut updated = match mode {
            Mode::Flat => 1,
            Mode::Break => 2,
        };
        for (depth, mut left, mut right, right_branch) in path.into_iter().rev() {
            if right_branch {
                right = updated;
            } else {
                left = updated;
            }
            let key = (depth, left, right);
            updated = if let Some(node) = self.branches.get(&key) {
                *node
            } else {
                let node = self.nodes.len() as u64;
                self.nodes
                    .push(ModeStateNode::Branch { depth, left, right });
                self.branches.insert(key, node);
                node
            };
        }
        updated
    }

    fn mode(&mut self, mut state: u64, id: GroupId) -> Option<Mode> {
        if !self.enabled {
            return None;
        }
        for depth in 0..u32::BITS as u8 {
            let (left, right) = match self.nodes[state as usize] {
                ModeStateNode::Empty => return None,
                ModeStateNode::Branch {
                    depth: node_depth,
                    left,
                    right,
                } => {
                    debug_assert_eq!(node_depth, depth);
                    (left, right)
                }
                ModeStateNode::Flat | ModeStateNode::Break => {
                    unreachable!("group mode leaves only occur below all key bits")
                }
            };
            let right_branch = id.0 & (1 << (u32::BITS - 1 - u32::from(depth))) != 0;
            state = if right_branch { right } else { left };
        }
        match self.nodes[state as usize] {
            ModeStateNode::Empty => None,
            ModeStateNode::Flat => Some(Mode::Flat),
            ModeStateNode::Break => Some(Mode::Break),
            ModeStateNode::Branch { .. } => {
                unreachable!("group mode branches only occur above all key bits")
            }
        }
    }

    fn record(&mut self, state: &mut u64, id: GroupId, mode: Mode) {
        if !self.enabled {
            return;
        }
        if self.mode(*state, id) == Some(mode) {
            return;
        }
        *state = self.transition(*state, id, mode);
    }
}

struct PendingLeadingGroup<'a> {
    id: GroupId,
    doc: &'a Doc,
}

enum EvalFrame<'a> {
    Fit(FitFrame<'a>),
    Leading(LeadingFrame<'a>),
}

enum FitStep<'a> {
    Continue,
    Done(bool),
    Nested(FitFrame<'a>),
    Leading(LeadingFrame<'a>),
}

enum LeadingStep<'a> {
    Done(bool),
    Nested(Box<FitFrame<'a>>),
}

enum EvalOperation<'a> {
    Continue,
    PushFit(FitFrame<'a>),
    PushLeading(LeadingFrame<'a>),
    DoneFit(bool),
    DoneLeading(bool),
}

#[derive(Clone)]
struct ChoiceTrial {
    id: u64,
    saw_text: bool,
}

struct ChoiceEvalFrame<'a> {
    base: Renderer<'a>,
    choice: &'a Doc,
    alternatives: &'a [Doc],
    command: Command<'a>,
    next_alternative: usize,
    active_alternative: Option<&'a Doc>,
    active_index: Option<usize>,
    last_selections: Option<HashMap<*const Doc, usize>>,
}

struct ChoiceCandidateResult {
    fits: bool,
    selections: HashMap<*const Doc, usize>,
}

enum RenderEvent<'a> {
    Empty,
    Choice {
        choice: &'a Doc,
        alternatives: &'a [Doc],
        command: Command<'a>,
    },
    TrialFinished(bool),
}

struct Renderer<'a> {
    stack: Vec<Command<'a>>,
    pending_suffixes: Vec<Command<'a>>,
    output: String,
    column: usize,
    pending_indent: Option<usize>,
    protected_verbatim_end: Option<usize>,
    emitted_line_break: bool,
    width: usize,
    forced: BTreeSet<GroupId>,
    group_modes: BTreeMap<GroupId, Mode>,
    observed_group_modes: BTreeSet<GroupId>,
    choice_selections: HashMap<*const Doc, usize>,
    choice_trial: Option<ChoiceTrial>,
    next_choice_trial: u64,
}

impl<'a> Renderer<'a> {
    fn new(root: &'a Doc, forced_groups: &[GroupId], width: usize) -> Self {
        let forced = collect_forced_groups(root, forced_groups);
        let observed_group_modes = collect_if_broken_group_ids(root);
        Self {
            stack: vec![Command {
                indent: 0,
                mode: Mode::Break,
                choice_trial: None,
                action: Action::Doc(root),
            }],
            pending_suffixes: Vec::new(),
            output: String::new(),
            column: 0,
            pending_indent: None,
            protected_verbatim_end: None,
            emitted_line_break: false,
            width,
            forced,
            group_modes: BTreeMap::new(),
            observed_group_modes,
            choice_selections: HashMap::new(),
            choice_trial: None,
            next_choice_trial: 0,
        }
    }

    fn clone_for_choice(&self) -> Self {
        let line_start = self.output.rfind('\n').map_or(0, |newline| newline + 1);
        Self {
            stack: self.stack.clone(),
            pending_suffixes: self.pending_suffixes.clone(),
            output: self.output[line_start..].to_owned(),
            column: self.column,
            pending_indent: self.pending_indent,
            protected_verbatim_end: self
                .protected_verbatim_end
                .map(|end| end.saturating_sub(line_start)),
            emitted_line_break: self.emitted_line_break,
            width: self.width,
            forced: self.forced.clone(),
            group_modes: self.group_modes.clone(),
            observed_group_modes: self.observed_group_modes.clone(),
            choice_selections: self.choice_selections.clone(),
            choice_trial: self.choice_trial.clone(),
            next_choice_trial: self.next_choice_trial,
        }
    }

    fn render(mut self) -> Result<String, PrintError> {
        self.run_until_empty()?;
        self.flush_pending_suffixes()?;
        self.trim_trailing_spaces();
        while self.output.ends_with('\n') {
            self.output.pop();
        }
        if !self.output.is_empty() || self.emitted_line_break {
            self.output.push('\n');
        }
        Ok(self.output)
    }

    fn run_until_empty(&mut self) -> Result<(), PrintError> {
        loop {
            match self.run_until_event()? {
                RenderEvent::Empty => return Ok(()),
                RenderEvent::Choice {
                    choice,
                    alternatives,
                    command,
                } => {
                    let alternative = self.select_choice(choice, alternatives, command)?;
                    self.push_doc(
                        alternative,
                        command.indent,
                        command.mode,
                        command.choice_trial,
                    );
                }
                RenderEvent::TrialFinished(_) => {
                    unreachable!("the main renderer has no active choice trial")
                }
            }
        }
    }

    fn run_until_event(&mut self) -> Result<RenderEvent<'a>, PrintError> {
        while let Some(command) = self.stack.pop() {
            let doc = match command.action {
                Action::Doc(doc) => doc,
                Action::VerbatimSegment(text) => {
                    self.record_choice_text(command.choice_trial, text);
                    self.emit_verbatim_segment(text);
                    continue;
                }
                Action::ChoiceEnd(id) => {
                    if self
                        .choice_trial
                        .as_ref()
                        .is_some_and(|trial| trial.id == id)
                    {
                        return Ok(RenderEvent::TrialFinished(self.choice_remainder_fits()?));
                    }
                    continue;
                }
                Action::InternalLineBreak {
                    indent,
                    next_indent,
                } => {
                    self.enqueue_line_break(indent, next_indent, command.choice_trial);
                    continue;
                }
                Action::LineBreak { next_indent } => {
                    if self.pending_suffixes.is_empty() {
                        if let Some(result) = self.choice_fit_at_line_break(command.choice_trial) {
                            return Ok(RenderEvent::TrialFinished(result));
                        }
                        self.emit_line_break(next_indent);
                    } else {
                        self.stack.push(command);
                        for suffix in self.pending_suffixes.drain(..).rev() {
                            self.stack.push(suffix);
                        }
                    }
                    continue;
                }
            };
            match doc {
                Doc::Text(text) => {
                    self.emit_text(text)?;
                    self.record_choice_text(command.choice_trial, text);
                }
                Doc::Verbatim(text) => {
                    self.push_verbatim_segments(text, command.indent, command.choice_trial)
                }
                Doc::Concat(docs) => {
                    self.push_docs(docs, command.indent, command.mode, command.choice_trial);
                }
                Doc::Indent(doc) => self.push_doc(
                    doc,
                    command.indent + INDENT_WIDTH,
                    command.mode,
                    command.choice_trial,
                ),
                Doc::Line | Doc::SoftLine if command.mode == Mode::Flat => {
                    if matches!(doc, Doc::Line) {
                        self.emit_text(" ")?;
                    }
                }
                Doc::Line | Doc::SoftLine | Doc::HardLine => {
                    self.enqueue_line_break(
                        command.indent,
                        Some(command.indent),
                        command.choice_trial,
                    );
                }
                Doc::Group(id, doc) => {
                    let mode = self.group_mode(*id, doc, command)?;
                    self.group_modes.insert(*id, mode);
                    self.push_doc(doc, command.indent, mode, command.choice_trial);
                }
                Doc::IfBroken(id, broken, flat) => {
                    let mode = self
                        .group_modes
                        .get(id)
                        .copied()
                        .ok_or(PrintError::UnknownGroup(*id))?;
                    self.push_doc(
                        if mode == Mode::Break { broken } else { flat },
                        command.indent,
                        command.mode,
                        command.choice_trial,
                    );
                }
                Doc::LineSuffix(doc) => {
                    self.pending_suffixes.push(Command {
                        indent: command.indent,
                        mode: command.mode,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(doc),
                    });
                }
                Doc::Flat(doc) => {
                    self.push_doc(doc, command.indent, Mode::Flat, command.choice_trial)
                }
                Doc::Choice(alternatives) if alternatives.is_empty() => {
                    return Err(PrintError::EmptyChoice);
                }
                Doc::Choice(alternatives) => {
                    return Ok(RenderEvent::Choice {
                        choice: doc,
                        alternatives,
                        command,
                    });
                }
            }
        }
        Ok(RenderEvent::Empty)
    }

    fn group_mode(
        &self,
        id: GroupId,
        doc: &'a Doc,
        command: Command<'a>,
    ) -> Result<Mode, PrintError> {
        if self.forced.contains(&id) {
            return Ok(Mode::Break);
        }
        if command.mode == Mode::Flat {
            return Ok(Mode::Flat);
        }
        if !self.observed_group_modes.contains(&id) && is_trivially_empty(doc) {
            return Ok(Mode::Flat);
        }
        let current_width = self.column + self.pending_indent.unwrap_or_default();
        if current_width > self.width {
            return Ok(Mode::Break);
        }

        let mut rest = self.stack.clone();
        let mut prior_modes = self.group_modes.clone();
        prior_modes.insert(id, Mode::Flat);
        if fits(
            doc,
            &mut rest,
            &self.pending_suffixes,
            self.width - current_width,
            &self.forced,
            &mut prior_modes,
            &self.observed_group_modes,
        )? {
            Ok(Mode::Flat)
        } else {
            Ok(Mode::Break)
        }
    }

    fn select_choice(
        &mut self,
        choice: &'a Doc,
        alternatives: &'a [Doc],
        command: Command<'a>,
    ) -> Result<&'a Doc, PrintError> {
        let last_index = alternatives
            .len()
            .checked_sub(1)
            .ok_or(PrintError::EmptyChoice)?;
        let key = choice_key(choice);
        if let Some(index) = self.choice_selections.get(&key).copied() {
            if let Some(alternative) = alternatives.get(index) {
                return Ok(alternative);
            }
        }

        let mut fallback_selections = None;
        for (index, alternative) in alternatives.iter().enumerate() {
            let has_forced_flat = contains_forced_flat(alternative, &self.forced);
            if has_forced_flat && index != last_index {
                continue;
            }
            let result = self.choice_alternative_fits(alternative, command)?;
            if index == last_index {
                fallback_selections = Some(result.selections.clone());
            }
            if !has_forced_flat && result.fits {
                self.choice_selections.extend(result.selections);
                self.choice_selections.insert(key, index);
                return Ok(alternative);
            }
        }

        if let Some(selections) = fallback_selections {
            self.choice_selections.extend(selections);
        }
        self.choice_selections.insert(key, last_index);
        Ok(&alternatives[last_index])
    }

    fn choice_alternative_fits(
        &self,
        alternative: &'a Doc,
        command: Command<'a>,
    ) -> Result<ChoiceCandidateResult, PrintError> {
        let mut trial = Self::start_trial_from_base(self.clone_for_choice(), alternative, command);
        let mut choices = Vec::<ChoiceEvalFrame<'a>>::new();

        loop {
            match trial.run_until_event()? {
                RenderEvent::Empty => {
                    unreachable!("a choice trial has an end marker below its candidate")
                }
                RenderEvent::TrialFinished(fits) => {
                    let Some(frame) = choices.last_mut() else {
                        return Ok(ChoiceCandidateResult {
                            fits,
                            selections: trial.choice_selections,
                        });
                    };
                    let trial_selections = trial.choice_selections.clone();
                    if frame.active_index == Some(frame.alternatives.len() - 1) {
                        frame.last_selections = Some(trial_selections.clone());
                    }
                    if fits {
                        let frame = choices.pop().expect("choice frame exists");
                        let selected = frame
                            .active_alternative
                            .expect("a successful choice frame has a candidate");
                        let selected_index = frame
                            .active_index
                            .expect("a successful choice frame has an active index");
                        trial = frame.base;
                        trial.choice_selections.extend(trial_selections);
                        trial
                            .choice_selections
                            .insert(choice_key(frame.choice), selected_index);
                        trial.push_doc(
                            selected,
                            frame.command.indent,
                            frame.command.mode,
                            frame.command.choice_trial,
                        );
                    } else if let Some(candidate) = Self::next_choice_trial(frame, &self.forced)? {
                        trial = candidate;
                    } else {
                        let frame = choices.pop().expect("choice frame exists");
                        let selected_index = frame.alternatives.len() - 1;
                        let selected = &frame.alternatives[selected_index];
                        trial = frame.base;
                        if let Some(selections) = frame.last_selections {
                            trial.choice_selections.extend(selections);
                        }
                        trial
                            .choice_selections
                            .insert(choice_key(frame.choice), selected_index);
                        trial.push_doc(
                            selected,
                            frame.command.indent,
                            frame.command.mode,
                            frame.command.choice_trial,
                        );
                    }
                }
                RenderEvent::Choice {
                    choice,
                    alternatives,
                    command,
                } => {
                    let key = choice_key(choice);
                    if let Some(index) = trial
                        .choice_selections
                        .get(&key)
                        .copied()
                        .filter(|index| *index < alternatives.len())
                    {
                        trial.push_doc(
                            &alternatives[index],
                            command.indent,
                            command.mode,
                            command.choice_trial,
                        );
                        continue;
                    }
                    let mut frame = ChoiceEvalFrame {
                        base: trial,
                        choice,
                        alternatives,
                        command,
                        next_alternative: 0,
                        active_alternative: None,
                        active_index: None,
                        last_selections: None,
                    };
                    if let Some(candidate) = Self::next_choice_trial(&mut frame, &self.forced)? {
                        choices.push(frame);
                        trial = candidate;
                    } else {
                        let selected_index = alternatives.len() - 1;
                        let selected = &alternatives[selected_index];
                        trial = frame.base;
                        trial.choice_selections.insert(key, selected_index);
                        trial.push_doc(
                            selected,
                            command.indent,
                            command.mode,
                            command.choice_trial,
                        );
                    }
                }
            }
        }
    }

    fn start_trial_from_base(mut base: Self, alternative: &'a Doc, command: Command<'a>) -> Self {
        let id = base
            .next_choice_trial
            .checked_add(1)
            .expect("choice nesting exceeds the representable trial ID range");
        base.next_choice_trial = id;
        base.choice_trial = Some(ChoiceTrial {
            id,
            saw_text: false,
        });
        base.stack.push(Command {
            indent: command.indent,
            mode: command.mode,
            choice_trial: Some(id),
            action: Action::ChoiceEnd(id),
        });
        base.push_doc(alternative, command.indent, command.mode, Some(id));
        base
    }

    fn next_choice_trial(
        frame: &mut ChoiceEvalFrame<'a>,
        forced: &BTreeSet<GroupId>,
    ) -> Result<Option<Self>, PrintError> {
        while let Some(alternative) = frame.alternatives.get(frame.next_alternative) {
            let index = frame.next_alternative;
            frame.next_alternative += 1;
            let is_last = index + 1 == frame.alternatives.len();
            if contains_forced_flat(alternative, forced) && !is_last {
                continue;
            }
            frame.active_alternative = Some(alternative);
            frame.active_index = Some(index);
            return Ok(Some(Self::start_trial_from_base(
                frame.base.clone_for_choice(),
                alternative,
                frame.command,
            )));
        }
        frame.active_alternative = None;
        frame.active_index = None;
        Ok(None)
    }

    fn choice_remainder_fits(&self) -> Result<bool, PrintError> {
        let current_width = self.column + self.pending_indent.unwrap_or_default();
        if current_width > self.width {
            return Ok(false);
        }
        let mut rest = self.stack.clone();
        let mut group_modes = self.group_modes.clone();
        fits(
            &Doc::Text(String::new()),
            &mut rest,
            &self.pending_suffixes,
            self.width - current_width,
            &self.forced,
            &mut group_modes,
            &self.observed_group_modes,
        )
    }

    fn record_choice_text(&mut self, choice_trial: Option<u64>, text: &str) {
        if text.is_empty() {
            return;
        }
        if let (Some(id), Some(trial)) = (choice_trial, self.choice_trial.as_mut()) {
            if trial.id == id {
                trial.saw_text = true;
            }
        }
    }

    fn choice_fit_at_line_break(&mut self, choice_trial: Option<u64>) -> Option<bool> {
        let id = choice_trial?;
        let trial = self.choice_trial.as_ref()?;
        if trial.id != id || !trial.saw_text {
            return None;
        }
        self.trim_trailing_spaces();
        Some(self.column <= self.width)
    }

    fn push_doc(&mut self, doc: &'a Doc, indent: usize, mode: Mode, choice_trial: Option<u64>) {
        self.stack.push(Command {
            indent,
            mode,
            choice_trial,
            action: Action::Doc(doc),
        });
    }

    fn push_docs(&mut self, docs: &'a [Doc], indent: usize, mode: Mode, choice_trial: Option<u64>) {
        self.stack.extend(docs.iter().rev().map(|doc| Command {
            indent,
            mode,
            choice_trial,
            action: Action::Doc(doc),
        }));
    }

    fn emit_text(&mut self, text: &str) -> Result<(), PrintError> {
        if text.contains(['\n', '\r']) {
            return Err(PrintError::TextContainsLineBreak);
        }
        if text.is_empty() {
            return Ok(());
        }
        self.emit_pending_indent();
        self.output.push_str(text);
        self.column += text.chars().count();
        Ok(())
    }

    fn emit_verbatim_segment(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.emit_pending_indent();
        self.output.push_str(text);
        self.column += text.chars().count();
        self.protected_verbatim_end = Some(self.output.len());
    }

    fn push_verbatim_segments(&mut self, text: &'a str, indent: usize, choice_trial: Option<u64>) {
        let lines = text.split('\n').collect::<Vec<_>>();
        let mut actions = Vec::with_capacity(lines.len().saturating_mul(2));
        for (index, line) in lines.iter().enumerate() {
            if !line.is_empty() {
                actions.push(Action::VerbatimSegment(line));
            }
            if index + 1 < lines.len() {
                actions.push(Action::InternalLineBreak {
                    indent,
                    next_indent: None,
                });
            }
        }
        self.stack
            .extend(actions.into_iter().rev().map(|action| Command {
                indent,
                mode: Mode::Break,
                choice_trial,
                action,
            }));
    }

    fn emit_pending_indent(&mut self) {
        if let Some(indent) = self.pending_indent.take() {
            self.output.extend(std::iter::repeat_n(' ', indent));
            self.column = indent;
        }
    }

    fn enqueue_line_break(
        &mut self,
        indent: usize,
        next_indent: Option<usize>,
        choice_trial: Option<u64>,
    ) {
        self.stack.push(Command {
            indent,
            mode: Mode::Break,
            choice_trial,
            action: Action::LineBreak { next_indent },
        });
    }

    fn emit_line_break(&mut self, next_indent: Option<usize>) {
        self.trim_trailing_spaces();
        self.output.push('\n');
        self.column = 0;
        self.pending_indent = next_indent;
        self.protected_verbatim_end = None;
        self.emitted_line_break = true;
    }

    fn trim_trailing_spaces(&mut self) {
        let protected_end = self.protected_verbatim_end.unwrap_or_default();
        while self.output.len() > protected_end && self.output.ends_with(' ') {
            self.output.pop();
            self.column = self.column.saturating_sub(1);
        }
    }

    fn flush_pending_suffixes(&mut self) -> Result<(), PrintError> {
        while !self.pending_suffixes.is_empty() {
            let suffixes = std::mem::take(&mut self.pending_suffixes);
            self.stack.extend(suffixes.into_iter().rev());
            self.run_until_empty()?;
        }
        Ok(())
    }
}

fn collect_forced_groups(root: &Doc, explicitly_forced: &[GroupId]) -> BTreeSet<GroupId> {
    let mut forced = explicitly_forced.iter().copied().collect::<BTreeSet<_>>();
    let mut stack = vec![(root, None::<Rc<GroupFrame>>)];

    while let Some((doc, ancestors)) = stack.pop() {
        match doc {
            Doc::HardLine => mark_ancestors(ancestors.as_deref(), &mut forced),
            Doc::Verbatim(text) if text.contains(['\n', '\r']) => {
                mark_ancestors(ancestors.as_deref(), &mut forced);
            }
            Doc::LineSuffix(child) => {
                mark_ancestors(ancestors.as_deref(), &mut forced);
                stack.push((child, None));
            }
            Doc::Group(id, child) => {
                let frame = Rc::new(GroupFrame {
                    id: *id,
                    parent: ancestors,
                });
                if forced.contains(id) {
                    mark_ancestors(Some(&frame), &mut forced);
                }
                stack.push((child, Some(frame)));
            }
            Doc::Choice(alternatives) => {
                stack.extend(alternatives.iter().map(|alternative| (alternative, None)));
            }
            Doc::Concat(docs) => {
                stack.extend(docs.iter().map(|child| (child, ancestors.clone())));
            }
            Doc::Indent(child) | Doc::Flat(child) => stack.push((child, ancestors)),
            Doc::IfBroken(_, broken, flat) => {
                stack.push((broken, ancestors.clone()));
                stack.push((flat, ancestors));
            }
            Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::Verbatim(_) => {}
        }
    }

    forced
}

fn mark_ancestors(mut frame: Option<&GroupFrame>, forced: &mut BTreeSet<GroupId>) {
    while let Some(current) = frame {
        forced.insert(current.id);
        frame = current.parent.as_deref();
    }
}

fn contains_forced_flat(root: &Doc, forced: &BTreeSet<GroupId>) -> bool {
    let mut stack = vec![root];
    while let Some(doc) = stack.pop() {
        match doc {
            Doc::Flat(child) => {
                if contains_forced_content_before_choice(child, forced) {
                    return true;
                }
                stack.push(child);
            }
            Doc::Concat(docs) => stack.extend(docs),
            Doc::Group(_, child) | Doc::Indent(child) | Doc::LineSuffix(child) => {
                stack.push(child);
            }
            Doc::IfBroken(_, broken, flat) => {
                stack.push(broken);
                stack.push(flat);
            }
            Doc::Choice(_) => {}
            Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::HardLine | Doc::Verbatim(_) => {}
        }
    }
    false
}

fn contains_forced_content_before_choice(root: &Doc, forced: &BTreeSet<GroupId>) -> bool {
    let mut stack = vec![root];
    while let Some(doc) = stack.pop() {
        match doc {
            Doc::HardLine => return true,
            Doc::Verbatim(text) if text.contains(['\n', '\r']) => return true,
            Doc::Group(id, child) => {
                if forced.contains(id) {
                    return true;
                }
                stack.push(child);
            }
            Doc::Concat(docs) => stack.extend(docs),
            Doc::Indent(child) | Doc::Flat(child) | Doc::LineSuffix(child) => stack.push(child),
            Doc::IfBroken(_, broken, flat) => {
                stack.push(broken);
                stack.push(flat);
            }
            Doc::Choice(_) => {}
            Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::Verbatim(_) => {}
        }
    }
    false
}

fn choice_key(choice: &Doc) -> *const Doc {
    std::ptr::from_ref(choice)
}

fn collect_if_broken_group_ids(root: &Doc) -> BTreeSet<GroupId> {
    let mut group_ids = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(doc) = pending.pop() {
        match doc {
            Doc::IfBroken(id, broken, flat) => {
                group_ids.insert(*id);
                pending.push(broken);
                pending.push(flat);
            }
            Doc::Concat(children) | Doc::Choice(children) => pending.extend(children),
            Doc::Indent(child)
            | Doc::Group(_, child)
            | Doc::LineSuffix(child)
            | Doc::Flat(child) => pending.push(child),
            Doc::Text(_) | Doc::Line | Doc::SoftLine | Doc::HardLine | Doc::Verbatim(_) => {}
        }
    }
    group_ids
}

fn fits<'a>(
    doc: &'a Doc,
    rest: &mut Vec<Command<'a>>,
    pending_suffixes: &[Command<'a>],
    remaining: usize,
    forced: &BTreeSet<GroupId>,
    group_modes: &mut BTreeMap<GroupId, Mode>,
    observed_group_modes: &BTreeSet<GroupId>,
) -> Result<bool, PrintError> {
    #[cfg(test)]
    FITS_CALLS.with(|calls| calls.set(calls.get() + 1));

    let mut mode_states = ModeStateInterner::with_tracking(!observed_group_modes.is_empty());
    let mut group_modes_state = 0;
    for (id, mode) in group_modes.iter() {
        if observed_group_modes.contains(id) {
            group_modes_state = mode_states.transition(group_modes_state, *id, *mode);
        }
    }
    let root = make_fit_frame(
        doc,
        std::mem::take(rest),
        pending_suffixes,
        remaining,
        group_modes_state,
    );
    let mut group_fit_cache: HashMap<GroupFitCacheKey, bool> = HashMap::new();
    let mut concat_expansion_cache = HashMap::new();
    let mut frames = vec![EvalFrame::Fit(root)];
    loop {
        let operation = match frames.last_mut().expect("fit stack is non-empty") {
            EvalFrame::Fit(frame) => {
                match fit_step(
                    frame,
                    forced,
                    &mut group_fit_cache,
                    &mut concat_expansion_cache,
                    observed_group_modes,
                    &mut mode_states,
                )? {
                    FitStep::Continue => EvalOperation::Continue,
                    FitStep::Done(result) => EvalOperation::DoneFit(result),
                    FitStep::Nested(frame) => EvalOperation::PushFit(frame),
                    FitStep::Leading(frame) => EvalOperation::PushLeading(frame),
                }
            }
            EvalFrame::Leading(frame) => {
                match leading_step(frame, forced, observed_group_modes, &mut mode_states)? {
                    LeadingStep::Done(result) => EvalOperation::DoneLeading(result),
                    LeadingStep::Nested(frame) => EvalOperation::PushFit(*frame),
                }
            }
        };
        match operation {
            EvalOperation::Continue => {}
            EvalOperation::PushFit(frame) => frames.push(EvalFrame::Fit(frame)),
            EvalOperation::PushLeading(frame) => frames.push(EvalFrame::Leading(frame)),
            EvalOperation::DoneFit(result) => {
                frames.pop();
                let Some(parent) = frames.last_mut() else {
                    return Ok(result);
                };
                match parent {
                    EvalFrame::Fit(parent) => {
                        let pending = parent
                            .awaiting_group
                            .take()
                            .expect("nested fit has a waiting group");
                        group_fit_cache.insert(pending.cache_key, result);
                        let mode = if result { Mode::Flat } else { Mode::Break };
                        record_observed_group_mode(
                            &mut parent.group_modes_state,
                            pending.id,
                            mode,
                            observed_group_modes,
                            &mut mode_states,
                        );
                        parent.commands.push(FitCommand {
                            command: Command {
                                indent: pending.indent,
                                mode,
                                choice_trial: None,
                                action: Action::Doc(pending.doc),
                            },
                            region: pending.region,
                        });
                    }
                    EvalFrame::Leading(parent) => {
                        assert!(parent.awaiting_group.is_some());
                        parent.group_fit_result = Some(result);
                    }
                }
            }
            EvalOperation::DoneLeading(result) => {
                frames.pop();
                let Some(EvalFrame::Fit(parent)) = frames.last_mut() else {
                    unreachable!("leading-break probes are nested under fit frames");
                };
                assert!(parent.awaiting_choice.is_some());
                parent.choice_leading_break = Some(result);
            }
        }
    }
}

fn make_fit_frame<'a>(
    doc: &'a Doc,
    rest: Vec<Command<'a>>,
    pending_suffixes: &[Command<'a>],
    remaining: usize,
    group_modes_state: u64,
) -> FitFrame<'a> {
    let mut commands = PersistentStack::default();
    for command in rest {
        commands.push(FitCommand {
            command,
            region: FitRegion::Rest,
        });
    }
    commands.push(FitCommand {
        command: Command {
            indent: 0,
            mode: Mode::Flat,
            choice_trial: None,
            action: Action::Doc(doc),
        },
        region: FitRegion::Candidate,
    });
    FitFrame {
        commands,
        remaining,
        group_modes_state,
        deferred_suffixes: {
            let mut suffixes = PersistentStack::default();
            for command in pending_suffixes.iter().copied() {
                suffixes.push(FitCommand {
                    command,
                    region: FitRegion::DeferredSuffix,
                });
            }
            suffixes
        },
        awaiting_group: None,
        awaiting_choice: None,
        choice_leading_break: None,
    }
}

fn fit_step<'a>(
    frame: &mut FitFrame<'a>,
    forced: &BTreeSet<GroupId>,
    group_fit_cache: &mut HashMap<GroupFitCacheKey, bool>,
    concat_expansion_cache: &mut HashMap<ConcatExpansionKey, PersistentStack<FitCommand<'a>>>,
    observed_group_modes: &BTreeSet<GroupId>,
    mode_states: &mut ModeStateInterner,
) -> Result<FitStep<'a>, PrintError> {
    if let Some(has_leading_break) = frame.choice_leading_break.take() {
        let pending = frame
            .awaiting_choice
            .take()
            .expect("fit frame has a waiting choice");
        if has_leading_break {
            let line_break = FitCommand {
                command: Command {
                    action: Action::LineBreak { next_indent: None },
                    ..pending.command
                },
                region: pending.region,
            };
            return Ok(end_fit_at_line_break(frame, line_break));
        }
        let alternative = pending.first_selectable_alternative;
        frame.commands.push(FitCommand {
            command: Command {
                indent: pending.command.indent,
                mode: pending.command.mode,
                choice_trial: pending.command.choice_trial,
                action: Action::Doc(alternative),
            },
            region: pending.region,
        });
        return Ok(FitStep::Continue);
    }

    let Some(FitCommand { command, region }) = frame.commands.pop() else {
        if !frame.deferred_suffixes.is_empty() {
            let suffixes = frame.deferred_suffixes.bottom_to_top();
            frame.deferred_suffixes.clear();
            for suffix in suffixes.into_iter().rev() {
                frame.commands.push(suffix);
            }
            return Ok(FitStep::Continue);
        }
        return Ok(FitStep::Done(true));
    };
    match command.action {
        Action::ChoiceEnd(_) => Ok(FitStep::Continue),
        Action::InternalLineBreak { .. } | Action::LineBreak { .. } => {
            Ok(end_fit_at_line_break(frame, FitCommand { command, region }))
        }
        Action::VerbatimSegment(text) => consume_width(frame, text.chars().count()),
        Action::Doc(doc) => match doc {
            Doc::Text(text) => {
                if text.contains(['\n', '\r']) {
                    return Err(PrintError::TextContainsLineBreak);
                }
                consume_width(frame, text.chars().count())
            }
            Doc::Verbatim(text) => {
                let first_line = text.split(['\n', '\r']).next().unwrap_or_default();
                let width = first_line.chars().count();
                if first_line.len() != text.len() {
                    if width > frame.remaining {
                        Ok(FitStep::Done(false))
                    } else if region != FitRegion::Candidate && !frame.deferred_suffixes.is_empty()
                    {
                        frame.remaining -= width;
                        let line_break = FitCommand {
                            command: Command {
                                action: Action::LineBreak { next_indent: None },
                                ..command
                            },
                            region,
                        };
                        Ok(end_fit_at_line_break(frame, line_break))
                    } else {
                        Ok(FitStep::Done(true))
                    }
                } else {
                    consume_width(frame, width)
                }
            }
            Doc::Concat(docs) => {
                let key = ConcatExpansionKey {
                    doc: std::ptr::from_ref(doc) as usize,
                    indent: command.indent,
                    mode: command.mode,
                    choice_trial: command.choice_trial,
                    region,
                    tail: frame.commands.key(),
                };
                frame.commands = if let Some(expansion) = concat_expansion_cache.get(&key) {
                    expansion.clone()
                } else {
                    let mut expansion = frame.commands.clone();
                    for child in docs.iter().rev() {
                        expansion.push(FitCommand {
                            command: Command {
                                indent: command.indent,
                                mode: command.mode,
                                choice_trial: command.choice_trial,
                                action: Action::Doc(child),
                            },
                            region,
                        });
                    }
                    concat_expansion_cache.insert(key, expansion.clone());
                    expansion
                };
                Ok(FitStep::Continue)
            }
            Doc::Indent(child) => {
                frame.commands.push(FitCommand {
                    command: Command {
                        indent: command.indent + INDENT_WIDTH,
                        mode: command.mode,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(child),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
            Doc::Line if command.mode == Mode::Flat => consume_width(frame, 1),
            Doc::SoftLine if command.mode == Mode::Flat => Ok(FitStep::Continue),
            Doc::Line | Doc::SoftLine | Doc::HardLine => {
                Ok(end_fit_at_line_break(frame, FitCommand { command, region }))
            }
            Doc::Group(id, child)
                if !observed_group_modes.contains(id) && is_trivially_empty(child) =>
            {
                Ok(FitStep::Continue)
            }
            Doc::Group(id, child) if forced.contains(id) => {
                record_observed_group_mode(
                    &mut frame.group_modes_state,
                    *id,
                    Mode::Break,
                    observed_group_modes,
                    mode_states,
                );
                frame.commands.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: Mode::Break,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(child),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
            Doc::Group(id, child) if command.mode == Mode::Flat => {
                record_observed_group_mode(
                    &mut frame.group_modes_state,
                    *id,
                    Mode::Flat,
                    observed_group_modes,
                    mode_states,
                );
                frame.commands.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(child),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
            Doc::Group(id, child) => {
                let rest = frame.commands.clone();
                let cache_key = GroupFitCacheKey {
                    group: *id,
                    doc: std::ptr::from_ref(child.as_ref()) as usize,
                    rest: rest.key(),
                    remaining: frame.remaining,
                    indent: command.indent,
                    mode: command.mode,
                    region,
                    deferred_suffixes: frame.deferred_suffixes.key(),
                    group_modes_state: frame.group_modes_state,
                };
                if let Some(fits) = group_fit_cache.get(&cache_key).copied() {
                    let mode = if fits { Mode::Flat } else { Mode::Break };
                    record_observed_group_mode(
                        &mut frame.group_modes_state,
                        *id,
                        mode,
                        observed_group_modes,
                        mode_states,
                    );
                    frame.commands.push(FitCommand {
                        command: Command {
                            indent: command.indent,
                            mode,
                            choice_trial: command.choice_trial,
                            action: Action::Doc(child),
                        },
                        region,
                    });
                    return Ok(FitStep::Continue);
                }
                let mut commands = rest;
                commands.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(child),
                    },
                    region: FitRegion::Candidate,
                });
                let mut group_modes_state = frame.group_modes_state;
                record_observed_group_mode(
                    &mut group_modes_state,
                    *id,
                    Mode::Flat,
                    observed_group_modes,
                    mode_states,
                );
                frame.awaiting_group = Some(PendingFitGroup {
                    id: *id,
                    doc: child,
                    indent: command.indent,
                    region,
                    cache_key,
                });
                Ok(FitStep::Nested(FitFrame {
                    commands,
                    remaining: frame.remaining,
                    group_modes_state,
                    deferred_suffixes: frame.deferred_suffixes.clone(),
                    awaiting_group: None,
                    awaiting_choice: None,
                    choice_leading_break: None,
                }))
            }
            Doc::IfBroken(id, broken, flat) => {
                let mode = mode_states
                    .mode(frame.group_modes_state, *id)
                    .ok_or(PrintError::UnknownGroup(*id))?;
                frame.commands.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: command.mode,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(if mode == Mode::Break { broken } else { flat }),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
            Doc::LineSuffix(child) if region == FitRegion::DeferredSuffix => {
                frame.deferred_suffixes.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: command.mode,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(child),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
            Doc::LineSuffix(_) => Ok(FitStep::Continue),
            Doc::Flat(child) => {
                frame.commands.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: Mode::Flat,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(child),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
            Doc::Choice(alternatives) if alternatives.is_empty() => Err(PrintError::EmptyChoice),
            Doc::Choice(alternatives) if region != FitRegion::Candidate => {
                let last_index = alternatives.len() - 1;
                let first_selectable_alternative = alternatives
                    .iter()
                    .enumerate()
                    .find(|(index, alternative)| {
                        *index == last_index || !contains_forced_flat(alternative, forced)
                    })
                    .map(|(_, alternative)| alternative)
                    .expect("the last choice alternative is always selectable");
                frame.awaiting_choice = Some(PendingFitChoice {
                    command,
                    region,
                    first_selectable_alternative,
                });
                let deferred_suffixes = frame.deferred_suffixes.clone();
                let stack_arena = new_persistent_stack_arena();
                let group_modes_state = frame.group_modes_state;
                let paths = alternatives
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(index, alternative)| {
                        *index == last_index || !contains_forced_flat(alternative, forced)
                    })
                    .map(|(_, alternative)| {
                        let mut stack = PersistentStack::with_arena(stack_arena.clone());
                        stack.push(LeadingCommand::Doc(alternative, command.mode));
                        LeadingPath {
                            stack,
                            deferred_suffixes: deferred_suffixes.clone(),
                            group_modes_state,
                        }
                    })
                    .collect();
                Ok(FitStep::Leading(LeadingFrame {
                    paths,
                    rest: frame.commands.bottom_to_top(),
                    remaining: frame.remaining,
                    awaiting_group: None,
                    group_fit_result: None,
                    visited: HashSet::new(),
                }))
            }
            Doc::Choice(alternatives) => {
                let alternative = &alternatives[0];
                frame.commands.push(FitCommand {
                    command: Command {
                        indent: command.indent,
                        mode: command.mode,
                        choice_trial: command.choice_trial,
                        action: Action::Doc(alternative),
                    },
                    region,
                });
                Ok(FitStep::Continue)
            }
        },
    }
}

fn leading_path_key(path: &LeadingPath<'_>) -> LeadingPathKey {
    LeadingPathKey {
        stack: path.stack.key(),
        deferred_suffixes: path.deferred_suffixes.key(),
        group_modes_state: path.group_modes_state,
    }
}

fn queue_deferred_suffixes<'a>(path: &mut LeadingPath<'a>, continuation: LeadingCommand<'a>) {
    path.stack.push(continuation);
    let mut suffixes = path.deferred_suffixes.clone();
    path.deferred_suffixes.clear();
    while let Some(suffix) = suffixes.pop() {
        let command = suffix.command;
        match command.action {
            Action::Doc(doc) => path.stack.push(LeadingCommand::Doc(doc, command.mode)),
            Action::VerbatimSegment(text) => {
                path.stack.push(LeadingCommand::Verbatim(text));
            }
            Action::InternalLineBreak { .. } | Action::LineBreak { .. } => {
                path.stack.push(LeadingCommand::LineBreak);
            }
            Action::ChoiceEnd(_) => {}
        }
    }
}

fn record_leading_group_mode(
    path: &mut LeadingPath<'_>,
    id: GroupId,
    mode: Mode,
    observed_group_modes: &BTreeSet<GroupId>,
    mode_states: &mut ModeStateInterner,
) {
    record_observed_group_mode(
        &mut path.group_modes_state,
        id,
        mode,
        observed_group_modes,
        mode_states,
    );
}

fn record_observed_group_mode(
    state: &mut u64,
    id: GroupId,
    mode: Mode,
    observed_group_modes: &BTreeSet<GroupId>,
    mode_states: &mut ModeStateInterner,
) {
    if observed_group_modes.contains(&id) {
        mode_states.record(state, id, mode);
    }
}

fn is_trivially_empty(doc: &Doc) -> bool {
    let mut pending = vec![doc];
    while let Some(doc) = pending.pop() {
        match doc {
            Doc::Text(text) | Doc::Verbatim(text) if text.is_empty() => {}
            Doc::Concat(children) => pending.extend(children),
            Doc::Indent(child) | Doc::Flat(child) | Doc::LineSuffix(child) => pending.push(child),
            _ => return false,
        }
    }
    true
}

fn leading_step<'a>(
    frame: &mut LeadingFrame<'a>,
    forced: &BTreeSet<GroupId>,
    observed_group_modes: &BTreeSet<GroupId>,
    mode_states: &mut ModeStateInterner,
) -> Result<LeadingStep<'a>, PrintError> {
    if let Some(fits) = frame.group_fit_result.take() {
        let pending = frame
            .awaiting_group
            .take()
            .expect("leading-break probe has a waiting group");
        let mode = if fits { Mode::Flat } else { Mode::Break };
        let path = frame
            .paths
            .last_mut()
            .expect("a leading-break probe has an active path");
        record_leading_group_mode(path, pending.id, mode, observed_group_modes, mode_states);
        path.stack.push(LeadingCommand::Doc(pending.doc, mode));
    }

    loop {
        let Some(path) = frame.paths.last_mut() else {
            return Ok(LeadingStep::Done(false));
        };
        if !frame.visited.insert(leading_path_key(path)) {
            frame.paths.pop();
            continue;
        }
        let Some(command) = path.stack.pop() else {
            frame.paths.pop();
            continue;
        };
        match command {
            LeadingCommand::LineBreak => {
                if path.deferred_suffixes.is_empty() {
                    return Ok(LeadingStep::Done(true));
                }
                queue_deferred_suffixes(path, LeadingCommand::LineBreak);
            }
            LeadingCommand::Verbatim(text) => {
                if text.starts_with('\n') || text.starts_with('\r') {
                    if path.deferred_suffixes.is_empty() {
                        return Ok(LeadingStep::Done(true));
                    }
                    queue_deferred_suffixes(path, LeadingCommand::LineBreak);
                } else if !text.is_empty() {
                    frame.paths.pop();
                }
            }
            LeadingCommand::Doc(doc, mode) => match doc {
                Doc::Text(text) if text.is_empty() => {}
                Doc::Text(_) => {
                    frame.paths.pop();
                }
                Doc::Verbatim(text) if text.is_empty() => {}
                Doc::Verbatim(text) => {
                    if text.starts_with('\n') || text.starts_with('\r') {
                        if path.deferred_suffixes.is_empty() {
                            return Ok(LeadingStep::Done(true));
                        }
                        queue_deferred_suffixes(path, LeadingCommand::LineBreak);
                    } else {
                        frame.paths.pop();
                    }
                }
                Doc::HardLine => {
                    if path.deferred_suffixes.is_empty() {
                        return Ok(LeadingStep::Done(true));
                    }
                    queue_deferred_suffixes(path, LeadingCommand::LineBreak);
                }
                Doc::Line | Doc::SoftLine if mode == Mode::Break => {
                    if path.deferred_suffixes.is_empty() {
                        return Ok(LeadingStep::Done(true));
                    }
                    queue_deferred_suffixes(path, LeadingCommand::LineBreak);
                }
                Doc::Line => {
                    frame.paths.pop();
                }
                Doc::SoftLine => {}
                Doc::Concat(docs) => {
                    for child in docs.iter().rev() {
                        path.stack.push(LeadingCommand::Doc(child, mode));
                    }
                }
                Doc::Indent(child) => path.stack.push(LeadingCommand::Doc(child, mode)),
                Doc::Flat(child) => path.stack.push(LeadingCommand::Doc(child, Mode::Flat)),
                Doc::Group(id, child) if forced.contains(id) => {
                    record_leading_group_mode(
                        path,
                        *id,
                        Mode::Break,
                        observed_group_modes,
                        mode_states,
                    );
                    path.stack.push(LeadingCommand::Doc(child, Mode::Break));
                }
                Doc::Group(id, child) if mode == Mode::Flat => {
                    record_leading_group_mode(
                        path,
                        *id,
                        Mode::Flat,
                        observed_group_modes,
                        mode_states,
                    );
                    path.stack.push(LeadingCommand::Doc(child, Mode::Flat));
                }
                Doc::Group(id, child)
                    if !observed_group_modes.contains(id) && is_trivially_empty(child) =>
                {
                    path.stack.push(LeadingCommand::Doc(child, Mode::Flat));
                }
                Doc::Group(id, child) => {
                    let mut group_rest = frame
                        .rest
                        .iter()
                        .map(|command| command.command)
                        .collect::<Vec<_>>();
                    let stack = path.stack.bottom_to_top();
                    group_rest.extend(stack.iter().map(|command| match command {
                        LeadingCommand::Doc(doc, mode) => Command {
                            indent: 0,
                            mode: *mode,
                            choice_trial: None,
                            action: Action::Doc(doc),
                        },
                        LeadingCommand::Verbatim(text) => Command {
                            indent: 0,
                            mode: Mode::Break,
                            choice_trial: None,
                            action: Action::VerbatimSegment(text),
                        },
                        LeadingCommand::LineBreak => Command {
                            indent: 0,
                            mode: Mode::Break,
                            choice_trial: None,
                            action: Action::LineBreak { next_indent: None },
                        },
                    }));
                    let pending_suffixes = path
                        .deferred_suffixes
                        .bottom_to_top()
                        .into_iter()
                        .map(|command| command.command)
                        .collect::<Vec<_>>();
                    let mut trial_modes_state = path.group_modes_state;
                    record_observed_group_mode(
                        &mut trial_modes_state,
                        *id,
                        Mode::Flat,
                        observed_group_modes,
                        mode_states,
                    );
                    frame.awaiting_group = Some(PendingLeadingGroup {
                        id: *id,
                        doc: child,
                    });
                    return Ok(LeadingStep::Nested(Box::new(make_fit_frame(
                        child,
                        group_rest,
                        &pending_suffixes,
                        frame.remaining,
                        trial_modes_state,
                    ))));
                }
                Doc::IfBroken(id, broken, flat) => {
                    let Some(group_mode) = mode_states.mode(path.group_modes_state, *id) else {
                        frame.paths.pop();
                        continue;
                    };
                    path.stack.push(LeadingCommand::Doc(
                        if group_mode == Mode::Break {
                            broken
                        } else {
                            flat
                        },
                        mode,
                    ));
                }
                Doc::LineSuffix(child) => {
                    if !is_trivially_empty(child) {
                        path.deferred_suffixes.push(FitCommand {
                            command: Command {
                                indent: 0,
                                mode,
                                choice_trial: None,
                                action: Action::Doc(child),
                            },
                            region: FitRegion::DeferredSuffix,
                        });
                    }
                }
                Doc::Choice(alternatives) if alternatives.is_empty() => {
                    return Err(PrintError::EmptyChoice);
                }
                Doc::Choice(alternatives) => {
                    let rest = path.stack.clone();
                    let deferred_suffixes = path.deferred_suffixes.clone();
                    let group_modes_state = path.group_modes_state;
                    let last_index = alternatives.len().saturating_sub(1);
                    frame.paths.pop();
                    for (index, alternative) in alternatives.iter().enumerate().rev() {
                        if index != last_index && contains_forced_flat(alternative, forced) {
                            continue;
                        }
                        let mut stack = rest.clone();
                        stack.push(LeadingCommand::Doc(alternative, mode));
                        frame.paths.push(LeadingPath {
                            stack,
                            deferred_suffixes: deferred_suffixes.clone(),
                            group_modes_state,
                        });
                    }
                }
            },
        }
    }
}

fn end_fit_at_line_break<'a>(frame: &mut FitFrame<'a>, line_break: FitCommand<'a>) -> FitStep<'a> {
    if line_break.region != FitRegion::Candidate && !frame.deferred_suffixes.is_empty() {
        push_deferred_suffixes(frame, line_break);
        FitStep::Continue
    } else {
        FitStep::Done(true)
    }
}

fn push_deferred_suffixes<'a>(frame: &mut FitFrame<'a>, line_break: FitCommand<'a>) {
    frame.commands.push(line_break);
    let suffixes = frame.deferred_suffixes.bottom_to_top();
    frame.deferred_suffixes.clear();
    for suffix in suffixes.into_iter().rev() {
        frame.commands.push(suffix);
    }
}

fn consume_width<'a>(frame: &mut FitFrame<'a>, width: usize) -> Result<FitStep<'a>, PrintError> {
    if width > frame.remaining {
        Ok(FitStep::Done(false))
    } else {
        frame.remaining -= width;
        Ok(FitStep::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Doc, GroupId, Mode, ModeStateInterner, PrintError, Renderer, print,
        print_with_forced_groups,
    };
    use std::collections::{BTreeSet, HashSet};

    fn group_content(id: GroupId, text: &str) -> Doc {
        Doc::group(
            id,
            Doc::concat([Doc::text(text), Doc::Line, Doc::text("tail")]),
        )
    }

    #[test]
    fn empty_document_has_no_trailing_newline() {
        assert_eq!(print(&Doc::text("")).unwrap(), "");
        assert_eq!(print(&Doc::concat([])).unwrap(), "");
    }

    #[test]
    fn appends_exactly_one_lf_and_removes_trailing_spaces() {
        assert_eq!(print(&Doc::text("text  ")).unwrap(), "text\n");
        assert_eq!(print(&Doc::HardLine).unwrap(), "\n");
        assert_eq!(
            print(&Doc::concat([Doc::text("a"), Doc::HardLine, Doc::HardLine])).unwrap(),
            "a\n"
        );
    }

    #[test]
    fn group_is_flat_when_its_contents_fit() {
        let doc = Doc::group(
            GroupId(1),
            Doc::concat([Doc::text("name"), Doc::Line, Doc::text("value")]),
        );
        assert_eq!(print(&doc).unwrap(), "name value\n");
    }

    #[test]
    fn groups_use_unicode_scalar_count_and_allow_exactly_100_columns() {
        let fits = Doc::group(
            GroupId(1),
            Doc::concat([Doc::text("界".repeat(98)), Doc::Line, Doc::text("x")]),
        );
        assert_eq!(print(&fits).unwrap(), format!("{} x\n", "界".repeat(98)));

        let breaks = Doc::group(
            GroupId(2),
            Doc::concat([Doc::text("界".repeat(99)), Doc::Line, Doc::text("x")]),
        );
        assert_eq!(print(&breaks).unwrap(), format!("{}\nx\n", "界".repeat(99)));
    }

    #[test]
    fn group_breaks_when_its_contents_exceed_the_width() {
        let doc = group_content(GroupId(1), "1234567");
        let rendered = Renderer::new(&doc, &[], 10).render().unwrap();
        assert_eq!(rendered, "1234567\ntail\n");
    }

    #[test]
    fn lookahead_uses_the_independent_layout_of_later_groups() {
        let later_group = Doc::group(
            GroupId(2),
            Doc::concat([Doc::Line, Doc::text("x".repeat(100))]),
        );
        let doc = Doc::concat([
            Doc::group(
                GroupId(1),
                Doc::concat([Doc::text("a"), Doc::Line, Doc::text("b")]),
            ),
            later_group,
        ]);

        assert_eq!(print(&doc).unwrap(), format!("a b\n{}\n", "x".repeat(100)));
    }

    #[test]
    fn explicit_forcing_overrides_flat_mode() {
        let id = GroupId(9);
        let doc = Doc::flat(group_content(id, "short"));
        assert_eq!(
            print_with_forced_groups(&doc, &[id]).unwrap(),
            "short\ntail\n"
        );
    }

    #[test]
    fn a_hard_line_forces_its_enclosing_group() {
        let doc = Doc::group(
            GroupId(1),
            Doc::concat([Doc::text("head"), Doc::HardLine, Doc::text("tail")]),
        );
        let rendered = Renderer::new(&doc, &[], 100).render().unwrap();
        assert_eq!(rendered, "head\ntail\n");
    }

    #[test]
    fn indentation_is_lazy_and_uses_four_spaces() {
        assert_eq!(print(&Doc::indent(Doc::text("first"))).unwrap(), "first\n");
        let doc = Doc::concat([
            Doc::text("head"),
            Doc::indent(Doc::concat([
                Doc::HardLine,
                Doc::text("item"),
                Doc::HardLine,
                Doc::text("next"),
            ])),
        ]);
        assert_eq!(print(&doc).unwrap(), "head\n    item\n    next\n");
    }

    #[test]
    fn indentation_after_a_line_break_counts_toward_group_width() {
        let doc = Doc::indent(Doc::concat([
            Doc::text("head"),
            Doc::HardLine,
            Doc::group(
                GroupId(1),
                Doc::concat([Doc::text("1234567"), Doc::Line, Doc::text("tail")]),
            ),
        ]));
        let rendered = Renderer::new(&doc, &[], 12).render().unwrap();
        assert_eq!(rendered, "head\n    1234567\n    tail\n");
    }

    #[test]
    fn line_suffixes_flush_in_order_before_a_line_break() {
        let doc = Doc::concat([
            Doc::text("item"),
            Doc::line_suffix(Doc::text(" // first")),
            Doc::line_suffix(Doc::text(" // second")),
            Doc::HardLine,
            Doc::text("next"),
        ]);
        assert_eq!(print(&doc).unwrap(), "item // first // second\nnext\n");
    }

    #[test]
    fn nested_line_suffixes_flush_before_the_same_line_break() {
        let doc = Doc::concat([
            Doc::line_suffix(Doc::concat([
                Doc::text("outer "),
                Doc::line_suffix(Doc::text("inner")),
            ])),
            Doc::HardLine,
            Doc::text("next"),
        ]);
        assert_eq!(print(&doc).unwrap(), "outer inner\nnext\n");
    }

    #[test]
    fn line_suffixes_flush_at_end_of_document() {
        let doc = Doc::concat([
            Doc::text("item"),
            Doc::line_suffix(Doc::text(" // trailing")),
        ]);
        assert_eq!(print(&doc).unwrap(), "item // trailing\n");
    }

    #[test]
    fn verbatim_line_break_flushes_pending_suffix_before_the_break() {
        let doc = Doc::concat([
            Doc::text("x"),
            Doc::line_suffix(Doc::text("// comment")),
            Doc::verbatim("\nnext"),
        ]);
        assert_eq!(print(&doc).unwrap(), "x// comment\nnext\n");
    }

    #[test]
    fn a_line_break_inside_verbatim_is_preserved_at_eof() {
        assert_eq!(print(&Doc::verbatim("\n")).unwrap(), "\n");
    }

    #[test]
    fn verbatim_final_newline_does_not_indent_following_document() {
        let doc = Doc::indent(Doc::concat([Doc::verbatim("raw\n"), Doc::text("next")]));

        assert_eq!(print(&doc).unwrap(), "raw\nnext\n");
    }

    #[test]
    fn trailing_spaces_inside_verbatim_survive_following_line_breaks() {
        let doc = Doc::concat([Doc::verbatim("raw "), Doc::HardLine, Doc::text("next")]);
        assert_eq!(print(&doc).unwrap(), "raw \nnext\n");
    }

    #[test]
    fn generated_trailing_spaces_after_verbatim_are_removed_without_trimming_it() {
        let doc = Doc::concat([
            Doc::verbatim("raw "),
            Doc::text(" "),
            Doc::HardLine,
            Doc::text("next"),
        ]);
        assert_eq!(print(&doc).unwrap(), "raw \nnext\n");
    }

    #[test]
    fn line_suffix_forces_its_containing_group_to_break() {
        let id = GroupId(1);
        let doc = Doc::group(
            id,
            Doc::concat([
                Doc::text("key"),
                Doc::Line,
                Doc::text("value"),
                Doc::line_suffix(Doc::text(" // note")),
            ]),
        );
        assert_eq!(print(&doc).unwrap(), "key\nvalue // note\n");
    }

    #[test]
    fn multiline_verbatim_resets_column_without_adding_internal_indentation() {
        let doc = Doc::indent(Doc::concat([
            Doc::text("before"),
            Doc::HardLine,
            Doc::verbatim("raw\n  text"),
            Doc::HardLine,
            Doc::text("after"),
        ]));
        assert_eq!(print(&doc).unwrap(), "before\n    raw\n  text\n    after\n");
    }

    #[test]
    fn multiline_verbatim_forces_an_enclosing_group() {
        let doc = Doc::group(
            GroupId(4),
            Doc::concat([Doc::text("a"), Doc::Line, Doc::verbatim("b\nc")]),
        );
        assert_eq!(print(&doc).unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn unknown_group_reference_is_rejected() {
        let doc = Doc::if_broken(GroupId(99), Doc::text("broken"), Doc::text("flat"));
        assert_eq!(print(&doc), Err(PrintError::UnknownGroup(GroupId(99))));
    }

    #[test]
    fn text_rejects_embedded_line_breaks() {
        assert_eq!(
            print(&Doc::text("first\nsecond")),
            Err(PrintError::TextContainsLineBreak)
        );
    }

    #[test]
    fn choice_selects_first_alternative_that_fits_with_the_rest_of_the_line() {
        let doc = Doc::concat([
            Doc::choice([Doc::text("x".repeat(99)), Doc::text("fallback")]),
            Doc::text("y"),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("{}y\n", "x".repeat(99)));

        let overflow = Doc::concat([
            Doc::choice([Doc::text("x".repeat(100)), Doc::text("fallback")]),
            Doc::text("y"),
        ]);
        assert_eq!(print(&overflow).unwrap(), "fallbacky\n");
    }

    #[test]
    fn a_rest_choice_with_a_leading_break_ends_group_lookahead() {
        let doc = Doc::concat([
            Doc::group(
                GroupId(7),
                Doc::concat([Doc::text("a"), Doc::Line, Doc::text("b")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::concat([Doc::HardLine, Doc::text("fallback")]),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), "a b\nfallback\n");
    }

    #[test]
    fn rest_choice_leading_break_checks_the_active_if_broken_branch() {
        let group_mode = GroupId(8);
        let list = GroupId(9);
        let fallback = "y".repeat(98);
        let doc = Doc::concat([
            Doc::group(group_mode, Doc::HardLine),
            Doc::group(
                list,
                Doc::concat([Doc::text("b"), Doc::Line, Doc::text("c")]),
            ),
            Doc::choice([
                Doc::if_broken(group_mode, Doc::text("x".repeat(101)), Doc::HardLine),
                Doc::text(fallback.clone()),
            ]),
        ]);

        let output = print(&doc).unwrap();
        assert_eq!(output, format!("\nb\nc{fallback}\n"));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
    }

    #[test]
    fn rest_choice_detects_a_line_break_from_an_unforced_group() {
        let outer = GroupId(11);
        let inner = GroupId(12);
        let later_line = "y".repeat(100);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::group(
                    inner,
                    Doc::concat([Doc::Line, Doc::text(later_line.clone())]),
                ),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p q\n{later_line}\n"));
    }

    #[test]
    fn rest_choice_does_not_treat_a_flat_group_line_as_a_break() {
        let outer = GroupId(13);
        let inner = GroupId(14);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::group(inner, Doc::concat([Doc::Line, Doc::text("y")])),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), "p\nq y\n");
    }

    #[test]
    fn rest_choice_detects_a_break_through_nested_groups() {
        let outer = GroupId(15);
        let inner = GroupId(16);
        let nested = GroupId(17);
        let later_line = "y".repeat(100);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::group(
                    inner,
                    Doc::concat([Doc::Line, Doc::group(nested, Doc::text(later_line.clone()))]),
                ),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p q\n{later_line}\n"));
    }

    #[test]
    fn rest_choice_detects_a_break_through_a_nested_choice_in_a_group() {
        let outer = GroupId(18);
        let inner = GroupId(19);
        let nested = GroupId(20);
        let later_line = "y".repeat(100);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::group(
                    inner,
                    Doc::concat([
                        Doc::Line,
                        Doc::choice([
                            Doc::text(later_line.clone()),
                            Doc::group(nested, Doc::text("fallback")),
                        ]),
                    ]),
                ),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p q\n{later_line}\n"));
    }

    #[test]
    fn rest_choice_detects_a_break_before_a_following_choice() {
        let outer = GroupId(21);
        let inner = GroupId(22);
        let later_line = "x".repeat(100);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::concat([
                    Doc::group(
                        inner,
                        Doc::concat([Doc::Line, Doc::text(later_line.clone())]),
                    ),
                    Doc::choice([Doc::text("")]),
                ]),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p q\n{later_line}\n"));
    }

    #[test]
    fn rest_choice_detects_a_continuation_induced_group_break() {
        let outer = GroupId(23);
        let inner = GroupId(24);
        let tail_a = GroupId(25);
        let tail_b = GroupId(26);
        let later_line = "x".repeat(100);
        let tail = "x".repeat(10);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::concat([
                    Doc::group(inner, Doc::concat([Doc::Line, Doc::text("x".repeat(90))])),
                    Doc::choice([
                        Doc::group(tail_a, Doc::text(tail.clone())),
                        Doc::group(tail_b, Doc::text(tail)),
                    ]),
                ]),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p q\n{later_line}\n"));
    }

    #[test]
    fn rest_choice_detects_a_break_after_a_flattened_group_prefix() {
        let outer = GroupId(27);
        let inner = GroupId(28);
        let tail_group = GroupId(29);
        let left = "a,".repeat(45);
        let tail = format!("{}a", "a,".repeat(4));
        let whole = format!("{left}{tail}");
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text(whole),
                Doc::concat([
                    Doc::group(inner, Doc::concat([Doc::Line, Doc::text(left.clone())])),
                    Doc::choice([Doc::flat(Doc::group(
                        tail_group,
                        Doc::concat([Doc::Line, Doc::text(tail.clone())]),
                    ))]),
                ]),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p q\n{left} {tail}\n"));
    }

    #[test]
    fn choice_falls_back_after_an_overlong_first_line() {
        let doc = Doc::choice([Doc::text("x".repeat(101)), Doc::text("fallback")]);
        assert_eq!(print(&doc).unwrap(), "fallback\n");
    }

    #[test]
    fn choice_accepts_a_leading_break_before_its_first_text() {
        let first_line = "x".repeat(100);
        let doc = Doc::choice([
            Doc::concat([Doc::HardLine, Doc::text(first_line.clone()), Doc::HardLine]),
            Doc::text("fallback"),
        ]);
        assert_eq!(print(&doc).unwrap(), format!("\n{first_line}\n"));
    }

    #[test]
    fn choice_stops_measuring_after_its_first_line_break() {
        let later_line = "x".repeat(150);
        let doc = Doc::choice([
            Doc::concat([
                Doc::text("first"),
                Doc::HardLine,
                Doc::text(later_line.clone()),
            ]),
            Doc::text("fallback"),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("first\n{later_line}\n"));
    }

    #[test]
    fn choice_measures_verbatim_through_its_first_internal_break() {
        let later_line = "x".repeat(150);
        let doc = Doc::choice([
            Doc::verbatim(format!("raw\n{later_line}")),
            Doc::text("fallback"),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("raw\n{later_line}\n"));
    }

    #[test]
    fn choice_flushes_line_suffixes_before_measuring_its_first_line() {
        let doc = Doc::choice([
            Doc::concat([
                Doc::text("x".repeat(98)),
                Doc::line_suffix(Doc::text("yy")),
                Doc::HardLine,
            ]),
            Doc::text("fallback"),
        ]);
        assert_eq!(print(&doc).unwrap(), format!("{}\n", "x".repeat(98) + "yy"));

        let overflow = Doc::choice([
            Doc::concat([
                Doc::text("x".repeat(99)),
                Doc::line_suffix(Doc::text("yy")),
                Doc::HardLine,
            ]),
            Doc::text("fallback"),
        ]);
        assert_eq!(print(&overflow).unwrap(), "fallback\n");
    }

    #[test]
    fn choice_remainder_reserves_space_for_pending_line_suffixes() {
        let fits = Doc::concat([
            Doc::choice([
                Doc::concat([Doc::text("x".repeat(98)), Doc::line_suffix(Doc::text("yy"))]),
                Doc::text("fallback"),
            ]),
            Doc::HardLine,
        ]);
        assert_eq!(print(&fits).unwrap(), format!("{}yy\n", "x".repeat(98)));

        let overflows = Doc::concat([
            Doc::choice([
                Doc::concat([Doc::text("x".repeat(99)), Doc::line_suffix(Doc::text("yy"))]),
                Doc::text("fallback"),
            ]),
            Doc::HardLine,
        ]);
        assert_eq!(print(&overflows).unwrap(), "fallback\n");

        let grouped_suffix = Doc::choice([
            Doc::concat([
                Doc::text("x".repeat(98)),
                Doc::line_suffix(Doc::group(GroupId(10), Doc::text("yy"))),
            ]),
            Doc::text("fallback"),
        ]);
        assert_eq!(
            print(&grouped_suffix).unwrap(),
            format!("{}yy\n", "x".repeat(98))
        );
    }

    #[test]
    fn pending_line_suffixes_are_checked_before_a_rest_choice_break() {
        let doc = Doc::concat([
            Doc::choice([
                Doc::concat([Doc::text("x".repeat(99)), Doc::line_suffix(Doc::text("yy"))]),
                Doc::text("fallback"),
            ]),
            Doc::choice([
                Doc::concat([Doc::HardLine, Doc::text("next")]),
                Doc::text("other"),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), "fallback\nnext\n");
    }

    #[test]
    fn deferred_suffixes_are_printed_before_a_probed_leading_break() {
        let outer = GroupId(30);
        let inner = GroupId(31);
        let comment = "c".repeat(98);
        let body = "x".repeat(97);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::concat([
                    Doc::text(comment.clone()),
                    Doc::HardLine,
                    Doc::text(body.clone()),
                ]),
                Doc::concat([
                    Doc::line_suffix(Doc::text(comment.clone())),
                    Doc::group(inner, Doc::concat([Doc::Line, Doc::text(body.clone())])),
                ]),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("p\nq{comment}\n{body}\n"));
    }

    #[test]
    fn forced_group_mode_is_visible_to_a_following_if_broken() {
        let outer = GroupId(32);
        let inner = GroupId(33);
        let body = "x".repeat(100);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text(body.clone()),
                Doc::group(
                    inner,
                    Doc::if_broken(
                        inner,
                        Doc::concat([Doc::HardLine, Doc::text(body.clone())]),
                        Doc::text(body.clone()),
                    ),
                ),
            ]),
        ]);

        assert_eq!(
            print_with_forced_groups(&doc, &[inner]).unwrap(),
            format!("p q\n{body}\n")
        );
    }

    #[test]
    fn rest_lookahead_ignores_unselectable_forced_flat_alternatives() {
        let outer = GroupId(35);
        let body = "x".repeat(98);
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("a"), Doc::Line, Doc::text("b")]),
            ),
            Doc::choice([
                Doc::flat(Doc::concat([Doc::HardLine, Doc::text(body.clone())])),
                Doc::text(body.clone()),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), format!("a\nb{body}\n"));
    }

    #[test]
    fn inherited_flat_group_mode_is_visible_to_a_following_if_broken() {
        let inner = GroupId(34);
        let doc = Doc::flat(Doc::group(
            inner,
            Doc::if_broken(inner, Doc::text("broken"), Doc::HardLine),
        ));
        let mut stack = super::PersistentStack::default();
        stack.push(super::LeadingCommand::Doc(&doc, super::Mode::Break));
        let mut frame = super::LeadingFrame {
            paths: vec![super::LeadingPath {
                stack,
                deferred_suffixes: super::PersistentStack::default(),
                group_modes_state: 0,
            }],
            rest: Vec::new(),
            remaining: 100,
            awaiting_group: None,
            group_fit_result: None,
            visited: HashSet::new(),
        };

        let observed_group_modes = BTreeSet::from([inner]);
        assert!(matches!(
            super::leading_step(
                &mut frame,
                &BTreeSet::new(),
                &observed_group_modes,
                &mut super::ModeStateInterner::default(),
            )
            .unwrap(),
            super::LeadingStep::Done(true)
        ));
    }

    #[test]
    fn nested_deferred_line_suffixes_contribute_to_choice_fit() {
        let doc = Doc::choice([
            Doc::concat([
                Doc::text("x".repeat(99)),
                Doc::line_suffix(Doc::concat([
                    Doc::text("a"),
                    Doc::line_suffix(Doc::text("b")),
                ])),
            ]),
            Doc::text("fallback"),
        ]);

        assert_eq!(print(&doc).unwrap(), "fallback\n");
    }

    #[test]
    fn choice_falls_back_to_its_last_alternative_when_none_fit() {
        let last = "y".repeat(102);
        let doc = Doc::choice([Doc::text("x".repeat(101)), Doc::text(last.clone())]);
        assert_eq!(print(&doc).unwrap(), format!("{last}\n"));
    }

    #[test]
    fn nested_choices_are_selected_during_speculative_rendering() {
        let doc = Doc::choice([
            Doc::choice([Doc::text("inner"), Doc::text("other")]),
            Doc::text("outer"),
        ]);
        assert_eq!(print(&doc).unwrap(), "inner\n");
    }

    #[test]
    fn deeply_nested_choices_are_resolved_iteratively() {
        let mut doc = Doc::text("deep");
        for _ in 0..1_500 {
            doc = Doc::choice([doc]);
        }
        let doc = Box::leak(Box::new(doc));

        assert_eq!(print(doc).unwrap(), "deep\n");
    }

    #[test]
    fn deeply_nested_group_choice_probes_are_resolved_iteratively() {
        let mut continuation = Doc::choice([Doc::text("x".repeat(100)), Doc::text("")]);
        for index in 0..512 {
            continuation = Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::concat([
                    Doc::group(
                        GroupId(index + 100),
                        Doc::concat([Doc::Line, Doc::text("y")]),
                    ),
                    continuation,
                ]),
            ]);
        }
        let doc = Doc::concat([
            Doc::group(
                GroupId(99),
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            continuation,
        ]);
        let doc = Box::leak(Box::new(doc));

        let output = print(doc).unwrap();
        assert!(output.ends_with('\n'));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
    }

    #[test]
    fn repeated_empty_choice_paths_are_deduplicated_during_lookahead() {
        let outer = GroupId(700);
        let inner = GroupId(701);
        let mut continuation = Doc::text("tail");
        for _ in 0..24 {
            continuation = Doc::concat([Doc::choice([Doc::text(""), Doc::text("")]), continuation]);
        }
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([
                Doc::text("x".repeat(100)),
                Doc::concat([Doc::group(inner, Doc::text("")), continuation]),
            ]),
        ]);

        assert_eq!(print(&doc).unwrap(), "p\nqtail\n");
    }

    #[test]
    fn empty_suffix_paths_converge_during_lookahead() {
        let outer = GroupId(702);
        let empty_suffix_or_text = || Doc::choice([Doc::line_suffix(Doc::text("")), Doc::text("")]);
        let mut continuation = Doc::text("tail");
        for _ in 0..24 {
            continuation = Doc::concat([empty_suffix_or_text(), continuation]);
        }
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([Doc::text("tail"), continuation]),
        ]);

        assert_eq!(print(&doc).unwrap(), "p qtail\n");
    }

    #[test]
    fn long_leading_probe_uses_compact_visited_state() {
        let outer = GroupId(703);
        let leading = Doc::concat(
            (0..16_000)
                .map(|_| Doc::text(""))
                .chain([Doc::HardLine, Doc::text("tail")]),
        );
        let doc = Doc::concat([
            Doc::group(
                outer,
                Doc::concat([Doc::text("a"), Doc::Line, Doc::text("b")]),
            ),
            Doc::choice([leading, Doc::text("fallback")]),
        ]);

        assert_eq!(print(&doc).unwrap(), "a b\ntail\n");
    }

    #[test]
    fn unobserved_empty_groups_do_not_replay_the_rest_suffix() {
        let count = 400;
        let mut continuation = Doc::HardLine;
        for index in (0..count).rev() {
            continuation = Doc::concat([Doc::group(GroupId(index), Doc::text("")), continuation]);
        }
        let marker = GroupId(count + 1);
        let doc = Doc::concat([
            Doc::group(GroupId(count), Doc::text("")),
            continuation,
            Doc::group(marker, Doc::if_broken(marker, Doc::text(""), Doc::text(""))),
        ]);

        super::FITS_CALLS.with(|calls| calls.set(0));
        assert_eq!(print(&doc).unwrap(), "\n");
        assert_eq!(super::FITS_CALLS.with(|calls| calls.get()), 1);
    }

    #[test]
    fn leading_probe_skips_an_unobserved_empty_group_fit() {
        let doc = Doc::group(GroupId(800), Doc::text(""));
        let mut stack = super::PersistentStack::default();
        stack.push(super::LeadingCommand::Doc(&doc, super::Mode::Break));
        let mut frame = super::LeadingFrame {
            paths: vec![super::LeadingPath {
                stack,
                deferred_suffixes: super::PersistentStack::default(),
                group_modes_state: 0,
            }],
            rest: Vec::new(),
            remaining: 100,
            awaiting_group: None,
            group_fit_result: None,
            visited: HashSet::new(),
        };

        assert!(matches!(
            super::leading_step(
                &mut frame,
                &BTreeSet::new(),
                &BTreeSet::new(),
                &mut ModeStateInterner::default(),
            )
            .unwrap(),
            super::LeadingStep::Done(false)
        ));
    }

    #[test]
    fn group_mode_states_merge_independent_of_assignment_order() {
        let mut states = ModeStateInterner::default();
        let first_mode = states.transition(0, GroupId(1), Mode::Flat);
        let first = states.transition(first_mode, GroupId(2), Mode::Break);
        let second_mode = states.transition(0, GroupId(2), Mode::Break);
        let second = states.transition(second_mode, GroupId(1), Mode::Flat);

        assert_eq!(first, second);
        assert_eq!(states.mode(first, GroupId(1)), Some(Mode::Flat));
        assert_eq!(states.mode(first, GroupId(2)), Some(Mode::Break));
        assert_eq!(states.mode(first, GroupId(3)), None);
    }

    #[test]
    fn leading_choice_paths_merge_equivalent_modes_in_different_orders() {
        let outer = GroupId(900);
        let marker = GroupId(901);
        let mut continuation = Doc::text("tail");
        let mut forced = Vec::new();
        let mut observed = Vec::new();
        for index in 0..24 {
            let first = GroupId(1_000 + index * 2);
            let second = GroupId(1_001 + index * 2);
            forced.extend([first, second]);
            observed.extend([first, second]);
            let forward = Doc::concat([
                Doc::group(first, Doc::text("")),
                Doc::group(second, Doc::text("")),
            ]);
            let reverse = Doc::concat([
                Doc::group(second, Doc::text("")),
                Doc::group(first, Doc::text("")),
            ]);
            continuation = Doc::concat([Doc::choice([forward, reverse]), continuation]);
        }
        let mut docs = vec![
            Doc::group(
                outer,
                Doc::concat([Doc::text("p"), Doc::Line, Doc::text("q")]),
            ),
            Doc::choice([continuation, Doc::text("tail")]),
        ];
        docs.extend(
            observed
                .into_iter()
                .map(|id| Doc::if_broken(id, Doc::text(""), Doc::text(""))),
        );
        docs.push(Doc::group(
            marker,
            Doc::if_broken(marker, Doc::text(""), Doc::text("")),
        ));
        let doc = Doc::concat(docs);

        assert_eq!(
            print_with_forced_groups(&doc, &forced).unwrap(),
            "p qtail\n"
        );
    }

    #[test]
    fn mode_state_storage_is_linear_in_distinct_group_ids() {
        let count = 400;
        let mut states = ModeStateInterner::default();
        let mut state = 0;
        for id in 0..count {
            state = states.transition(state, GroupId(id), Mode::Flat);
        }

        assert!(states.nodes.len() <= 3 + count as usize * u32::BITS as usize);
    }

    #[test]
    fn equivalent_group_mode_paths_are_interned() {
        let outer = GroupId(900);
        let mut continuation = Doc::text("tail");
        let mut forced = Vec::new();
        for index in 0..26 {
            let group = GroupId(1_000 + index);
            forced.push(group);
            continuation = Doc::concat([
                Doc::choice([
                    Doc::group(group, Doc::text("")),
                    Doc::group(group, Doc::text("")),
                ]),
                continuation,
            ]);
        }
        let doc = Doc::concat([
            Doc::group(outer, Doc::text("p")),
            Doc::choice([Doc::text("tail"), continuation]),
        ]);

        assert_eq!(print_with_forced_groups(&doc, &forced).unwrap(), "ptail\n");
    }

    #[test]
    fn forced_groups_inside_flat_choice_content_reject_that_alternative() {
        let forced = GroupId(91);
        let doc = Doc::choice([
            Doc::flat(Doc::group(
                forced,
                Doc::concat([Doc::text("first"), Doc::HardLine, Doc::text("rest")]),
            )),
            Doc::text("fallback"),
        ]);
        assert_eq!(
            print_with_forced_groups(&doc, &[forced]).unwrap(),
            "fallback\n"
        );
    }

    #[test]
    fn hard_lines_and_multiline_verbatim_inside_flat_reject_an_alternative() {
        let direct_hard_line = Doc::choice([
            Doc::flat(Doc::concat([
                Doc::text("first"),
                Doc::HardLine,
                Doc::text("rest"),
            ])),
            Doc::text("fallback"),
        ]);
        assert_eq!(print(&direct_hard_line).unwrap(), "fallback\n");

        let multiline_verbatim = Doc::choice([
            Doc::flat(Doc::verbatim("first\nrest")),
            Doc::text("fallback"),
        ]);
        assert_eq!(print(&multiline_verbatim).unwrap(), "fallback\n");

        let line_suffix = Doc::choice([
            Doc::flat(Doc::group(
                GroupId(92),
                Doc::concat([
                    Doc::text("first"),
                    Doc::line_suffix(Doc::text("// comment")),
                ]),
            )),
            Doc::text("fallback"),
        ]);
        assert_eq!(print(&line_suffix).unwrap(), "fallback\n");
    }

    #[test]
    fn empty_choice_is_rejected() {
        assert_eq!(print(&Doc::choice([])), Err(PrintError::EmptyChoice));
    }

    #[test]
    fn a_choice_alternative_does_not_force_its_containing_group() {
        let outer = GroupId(1);
        let inner = GroupId(2);
        let hard_line = Doc::group(outer, Doc::choice([Doc::HardLine]));
        let forced_inner = Doc::group(outer, Doc::choice([Doc::group(inner, Doc::HardLine)]));

        assert!(!super::collect_forced_groups(&hard_line, &[]).contains(&outer));
        let forced = super::collect_forced_groups(&forced_inner, &[inner]);
        assert!(forced.contains(&inner));
        assert!(!forced.contains(&outer));
    }

    #[test]
    fn forcing_an_inner_group_propagates_to_parent_groups() {
        let inner = GroupId(2);
        let outer = GroupId(1);
        let doc = Doc::group(
            outer,
            Doc::concat([
                Doc::text("outer"),
                Doc::Line,
                Doc::group(
                    inner,
                    Doc::concat([Doc::text("inner"), Doc::Line, Doc::text("end")]),
                ),
            ]),
        );
        assert_eq!(
            print_with_forced_groups(&doc, &[inner]).unwrap(),
            "outer\ninner\nend\n"
        );
    }

    #[test]
    fn if_broken_observes_the_selected_group_mode() {
        let id = GroupId(1);
        let doc = Doc::group(
            id,
            Doc::concat([
                Doc::text("long-prefix"),
                Doc::Line,
                Doc::if_broken(id, Doc::text("broken"), Doc::text("flat")),
            ]),
        );
        let rendered = Renderer::new(&doc, &[], 12).render().unwrap();
        assert_eq!(rendered, "long-prefix\nbroken\n");
    }

    #[test]
    fn current_group_mode_is_registered_before_its_contents() {
        let id = GroupId(1);
        let doc = Doc::group(
            id,
            Doc::concat([
                Doc::if_broken(id, Doc::text("broken"), Doc::text("flat")),
                Doc::Line,
                Doc::text("tail"),
            ]),
        );
        let rendered = Renderer::new(&doc, &[], 100).render().unwrap();
        assert_eq!(rendered, "flat tail\n");
    }
}
