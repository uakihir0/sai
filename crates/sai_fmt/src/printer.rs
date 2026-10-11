use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const INDENT_WIDTH: usize = 4;
const MAX_LINE_WIDTH: usize = 100;

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

/// An invalid or not-yet-supported document construct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrintError {
    /// `Text` must not contain a line break; use `Verbatim` for multiline text.
    TextContainsLineBreak,
    /// `IfBroken` must refer to a group that has already been encountered.
    UnknownGroup(GroupId),
    /// A `Choice` is part of the document model but its fit-selection
    /// algorithm has not been integrated yet.
    ChoiceNotImplemented,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Copy)]
struct Command<'a> {
    indent: usize,
    mode: Mode,
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
}

struct GroupFrame {
    id: GroupId,
    parent: Option<Rc<GroupFrame>>,
}

struct FitFrame<'a> {
    commands: Vec<Command<'a>>,
    remaining: usize,
    group_modes: BTreeMap<GroupId, Mode>,
    awaiting_group: Option<PendingFitGroup<'a>>,
}

struct PendingFitGroup<'a> {
    id: GroupId,
    doc: &'a Doc,
    indent: usize,
}

enum FitStep<'a> {
    Continue,
    Done(bool),
    Nested(FitFrame<'a>),
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
}

impl<'a> Renderer<'a> {
    fn new(root: &'a Doc, forced_groups: &[GroupId], width: usize) -> Self {
        let forced = collect_forced_groups(root, forced_groups);
        Self {
            stack: vec![Command {
                indent: 0,
                mode: Mode::Break,
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
        while let Some(command) = self.stack.pop() {
            let doc = match command.action {
                Action::Doc(doc) => doc,
                Action::VerbatimSegment(text) => {
                    self.emit_verbatim_segment(text);
                    continue;
                }
                Action::InternalLineBreak {
                    indent,
                    next_indent,
                } => {
                    self.enqueue_line_break(indent, next_indent);
                    continue;
                }
                Action::LineBreak { next_indent } => {
                    if self.pending_suffixes.is_empty() {
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
                Doc::Text(text) => self.emit_text(text)?,
                Doc::Verbatim(text) => self.push_verbatim_segments(text, command.indent),
                Doc::Concat(docs) => {
                    self.push_docs(docs, command.indent, command.mode);
                }
                Doc::Indent(doc) => self.push_doc(doc, command.indent + INDENT_WIDTH, command.mode),
                Doc::Line | Doc::SoftLine if command.mode == Mode::Flat => {
                    if matches!(doc, Doc::Line) {
                        self.emit_text(" ")?;
                    }
                }
                Doc::Line | Doc::SoftLine | Doc::HardLine => {
                    self.enqueue_line_break(command.indent, Some(command.indent));
                }
                Doc::Group(id, doc) => {
                    let mode = self.group_mode(*id, doc, command)?;
                    self.group_modes.insert(*id, mode);
                    self.push_doc(doc, command.indent, mode);
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
                    );
                }
                Doc::LineSuffix(doc) => {
                    self.pending_suffixes.push(Command {
                        indent: command.indent,
                        mode: command.mode,
                        action: Action::Doc(doc),
                    });
                }
                Doc::Flat(doc) => self.push_doc(doc, command.indent, Mode::Flat),
                Doc::Choice(alternatives) if alternatives.is_empty() => {
                    return Err(PrintError::EmptyChoice);
                }
                Doc::Choice(_) => return Err(PrintError::ChoiceNotImplemented),
            }
        }
        Ok(())
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
            self.width - current_width,
            &self.forced,
            &mut prior_modes,
        )? {
            Ok(Mode::Flat)
        } else {
            Ok(Mode::Break)
        }
    }

    fn push_doc(&mut self, doc: &'a Doc, indent: usize, mode: Mode) {
        self.stack.push(Command {
            indent,
            mode,
            action: Action::Doc(doc),
        });
    }

    fn push_docs(&mut self, docs: &'a [Doc], indent: usize, mode: Mode) {
        self.stack.extend(docs.iter().rev().map(|doc| Command {
            indent,
            mode,
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

    fn push_verbatim_segments(&mut self, text: &'a str, indent: usize) {
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
                action,
            }));
    }

    fn emit_pending_indent(&mut self) {
        if let Some(indent) = self.pending_indent.take() {
            self.output.extend(std::iter::repeat_n(' ', indent));
            self.column = indent;
        }
    }

    fn enqueue_line_break(&mut self, indent: usize, next_indent: Option<usize>) {
        self.stack.push(Command {
            indent,
            mode: Mode::Break,
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

fn fits<'a>(
    doc: &'a Doc,
    rest: &mut Vec<Command<'a>>,
    remaining: usize,
    forced: &BTreeSet<GroupId>,
    group_modes: &mut BTreeMap<GroupId, Mode>,
) -> Result<bool, PrintError> {
    rest.push(Command {
        indent: 0,
        mode: Mode::Flat,
        action: Action::Doc(doc),
    });

    let mut frames = vec![FitFrame {
        commands: std::mem::take(rest),
        remaining,
        group_modes: group_modes.clone(),
        awaiting_group: None,
    }];
    loop {
        let step = {
            let frame = frames.last_mut().expect("fit stack is non-empty");
            fit_step(frame, forced)?
        };
        match step {
            FitStep::Continue => {}
            FitStep::Nested(frame) => frames.push(frame),
            FitStep::Done(result) => {
                frames.pop();
                let Some(parent) = frames.last_mut() else {
                    return Ok(result);
                };
                let pending = parent
                    .awaiting_group
                    .take()
                    .expect("nested fit has a waiting group");
                let mode = if result { Mode::Flat } else { Mode::Break };
                parent.group_modes.insert(pending.id, mode);
                parent.commands.push(Command {
                    indent: pending.indent,
                    mode,
                    action: Action::Doc(pending.doc),
                });
            }
        }
    }
}

fn fit_step<'a>(
    frame: &mut FitFrame<'a>,
    forced: &BTreeSet<GroupId>,
) -> Result<FitStep<'a>, PrintError> {
    let Some(command) = frame.commands.pop() else {
        return Ok(FitStep::Done(true));
    };
    match command.action {
        Action::InternalLineBreak { .. } | Action::LineBreak { .. } => Ok(FitStep::Done(true)),
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
                    } else {
                        Ok(FitStep::Done(true))
                    }
                } else {
                    consume_width(frame, width)
                }
            }
            Doc::Concat(docs) => {
                frame
                    .commands
                    .extend(docs.iter().rev().map(|child| Command {
                        indent: command.indent,
                        mode: command.mode,
                        action: Action::Doc(child),
                    }));
                Ok(FitStep::Continue)
            }
            Doc::Indent(child) => {
                frame.commands.push(Command {
                    indent: command.indent + INDENT_WIDTH,
                    mode: command.mode,
                    action: Action::Doc(child),
                });
                Ok(FitStep::Continue)
            }
            Doc::Line if command.mode == Mode::Flat => consume_width(frame, 1),
            Doc::SoftLine if command.mode == Mode::Flat => Ok(FitStep::Continue),
            Doc::Line | Doc::SoftLine | Doc::HardLine => Ok(FitStep::Done(true)),
            Doc::Group(id, child) if forced.contains(id) => {
                frame.group_modes.insert(*id, Mode::Break);
                frame.commands.push(Command {
                    indent: command.indent,
                    mode: Mode::Break,
                    action: Action::Doc(child),
                });
                Ok(FitStep::Continue)
            }
            Doc::Group(id, child) if command.mode == Mode::Flat => {
                frame.group_modes.insert(*id, Mode::Flat);
                frame.commands.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    action: Action::Doc(child),
                });
                Ok(FitStep::Continue)
            }
            Doc::Group(id, child) => {
                let mut commands = frame.commands.clone();
                commands.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    action: Action::Doc(child),
                });
                let mut group_modes = frame.group_modes.clone();
                group_modes.insert(*id, Mode::Flat);
                frame.awaiting_group = Some(PendingFitGroup {
                    id: *id,
                    doc: child,
                    indent: command.indent,
                });
                Ok(FitStep::Nested(FitFrame {
                    commands,
                    remaining: frame.remaining,
                    group_modes,
                    awaiting_group: None,
                }))
            }
            Doc::IfBroken(id, broken, flat) => {
                let mode = frame
                    .group_modes
                    .get(id)
                    .copied()
                    .ok_or(PrintError::UnknownGroup(*id))?;
                frame.commands.push(Command {
                    indent: command.indent,
                    mode: command.mode,
                    action: Action::Doc(if mode == Mode::Break { broken } else { flat }),
                });
                Ok(FitStep::Continue)
            }
            Doc::LineSuffix(_) => Ok(FitStep::Continue),
            Doc::Flat(child) => {
                frame.commands.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    action: Action::Doc(child),
                });
                Ok(FitStep::Continue)
            }
            Doc::Choice(alternatives) if alternatives.is_empty() => Err(PrintError::EmptyChoice),
            Doc::Choice(_) => Err(PrintError::ChoiceNotImplemented),
        },
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
    use super::{Doc, GroupId, PrintError, Renderer, print, print_with_forced_groups};

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
    fn choice_is_explicitly_rejected_until_fit_selection_is_integrated() {
        assert_eq!(
            print(&Doc::choice([Doc::text("a")])),
            Err(PrintError::ChoiceNotImplemented)
        );
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
