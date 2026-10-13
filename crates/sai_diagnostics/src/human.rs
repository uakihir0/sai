//! Human rendering of diagnostics ([impl/11](../../../docs/impl/11-diagnostics.md) §11.10).
//!
//! This module owns the layout: headings, location lines, gutters, child notes, related
//! entries, and summary lines. It does not own source text. Display columns depend on the
//! characters of the file (tab stops, East Asian width, escaped controls, windows of long lines),
//! so the caller supplies them through [`SourceSnippets`].

use std::fmt::Write as _;

use crate::{
    ChainStep, Child, ChildKind, Diagnostic, LevelInfo, LevelSource, LintLevel, Location, Related,
    Relation, Severity, SourceRef, TextRange,
};

/// Most fixes rendered for one diagnostic (§11.10.2 item 5).
const MAX_RENDERED_FIXES: usize = 3;

const BOLD: &str = "\x1b[1m";
const BOLD_RED: &str = "\x1b[1;31m";
const BOLD_YELLOW: &str = "\x1b[1;33m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_BLUE: &str = "\x1b[1;34m";
const BOLD_GREEN: &str = "\x1b[1;32m";
const RESET: &str = "\x1b[0m";

/// Positions and snippet rows for one diagnostic, supplied by the caller.
///
/// An implementation must be deterministic: the same input yields the same rows.
pub trait SourceSnippets {
    /// The one-based line and one-based UTF-8 byte column of `location` (§11.5.2), or `None`
    /// when the caller cannot place it. Without a position the location is printed by path only.
    fn position(&self, location: &Location) -> Option<(usize, usize)>;

    /// The rows that show `annotations` in `file` (§11.10.4), or `None` when the text is not
    /// available. The caller orders the annotation rows of each line as §11.10.4 rule 4 requires.
    fn snippet(&self, file: &SourceRef, annotations: &[SnippetAnnotation]) -> Option<Snippet>;
}

/// One range to draw in a snippet, with its message (empty for none).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetAnnotation {
    pub range: TextRange,
    pub primary: bool,
    pub message: String,
}

/// The rows of one snippet, in display order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snippet {
    pub rows: Vec<SnippetRow>,
}

/// One row of a snippet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnippetRow {
    /// A single `...` row in place of unannotated lines (§11.10.4 rule 1).
    Gap,
    /// A source line with its annotation rows beneath it.
    Line {
        number: usize,
        text: String,
        marks: Vec<SnippetMark>,
    },
}

/// One annotation row below a source line (§11.10.4 rules 4 and 5).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetMark {
    /// Display column of the first mark, counted from the start of the row's text.
    pub column: usize,
    /// Display width of the marks, at least one.
    pub width: usize,
    pub primary: bool,
    pub message: String,
}

/// Counts printed by the summary line (§11.10.8).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SummaryCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    /// Errors that were counted but not rendered because of `--max-errors`.
    pub hidden_errors: usize,
    /// The `--max-errors` limit, where 0 means no limit.
    pub max_errors: usize,
}

/// Renders one diagnostic as §11.10.2 describes. The text ends with one empty line.
pub fn render_diagnostic(
    diagnostic: &Diagnostic,
    source: &dyn SourceSnippets,
    color: bool,
) -> String {
    let renderer = Renderer { source, color };
    renderer.diagnostic(diagnostic)
}

/// Renders the summary line of §11.10.8. The text ends with a newline.
pub fn render_summary(command: &str, counts: SummaryCounts, color: bool) -> String {
    let status = if counts.errors > 0 {
        paint(color, BOLD_RED, "failed")
    } else {
        paint(color, BOLD, "finished")
    };
    let mut parts = Vec::new();
    if counts.errors > 0 {
        let noun = if counts.errors == 1 {
            "error"
        } else {
            "errors"
        };
        parts.push(format!("{} {noun}", counts.errors));
    }
    if counts.warnings > 0 {
        let noun = if counts.warnings == 1 {
            "warning"
        } else {
            "warnings"
        };
        parts.push(format!("{} {noun}", counts.warnings));
    }
    if counts.infos > 0 {
        parts.push(format!("{} info", counts.infos));
    }
    let mut line = format!("{command} {status}: {}", parts.join(", "));
    if counts.hidden_errors > 0 && counts.max_errors != 0 {
        let _ = write!(
            line,
            "; {} more errors not shown (--max-errors {})",
            counts.hidden_errors, counts.max_errors
        );
    }
    line.push('\n');
    line
}

struct Renderer<'a> {
    source: &'a dyn SourceSnippets,
    color: bool,
}

/// The annotations of one file, with the snippet that shows them.
struct Group {
    file: SourceRef,
    /// The location printed on the group's header line.
    header: Location,
    annotations: Vec<SnippetAnnotation>,
    snippet: Option<Snippet>,
}

impl Renderer<'_> {
    fn diagnostic(&self, diagnostic: &Diagnostic) -> String {
        let groups = self.groups(diagnostic);
        let width = self.number_width(&groups);
        let block_gutter = " ".repeat(width);
        // Without a primary location the children print two spaces before `=` (§11.10.2 item 2,
        // and the example of §13.3.5), so their gutter is one space plus the separator.
        let child_gutter = if diagnostic.primary().is_some() {
            block_gutter.clone()
        } else {
            " ".to_owned()
        };

        let mut out = String::new();
        self.heading(&mut out, diagnostic);
        for (index, group) in groups.iter().enumerate() {
            let arrow = if index == 0 && diagnostic.primary().is_some() {
                "-->"
            } else {
                ":::"
            };
            let position = self.location_text(&group.header);
            let _ = writeln!(
                out,
                "{block_gutter}{} {position}",
                paint(self.color, BOLD_BLUE, arrow)
            );
            if let Some(snippet) = &group.snippet {
                self.snippet_block(&mut out, &block_gutter, width, snippet);
            }
        }

        for child in diagnostic.children() {
            self.child(
                &mut out,
                &child_gutter,
                child.kind,
                &child.message,
                child.location.as_ref(),
            );
        }
        if let Some(level) = diagnostic.level() {
            if let Some(message) = level_message(diagnostic, level) {
                self.child(
                    &mut out,
                    &child_gutter,
                    ChildKind::Note,
                    &message,
                    level.location.as_ref(),
                );
            }
        }
        for related in diagnostic.related() {
            if let Some(child) = self.related_note(related) {
                self.child(
                    &mut out,
                    &child_gutter,
                    child.kind,
                    &child.message,
                    child.location.as_ref(),
                );
            }
        }
        for fix in diagnostic.fixes().iter().take(MAX_RENDERED_FIXES) {
            self.child(&mut out, &child_gutter, ChildKind::Help, fix.title(), None);
        }
        out.push('\n');
        out
    }

    fn heading(&self, out: &mut String, diagnostic: &Diagnostic) {
        let (word, sgr) = match diagnostic.severity {
            Severity::Error => ("error", BOLD_RED),
            Severity::Warning => ("warning", BOLD_YELLOW),
            Severity::Info => ("info", BOLD_CYAN),
        };
        let code = format!("{word}[{}]", diagnostic.code.as_str());
        let _ = writeln!(
            out,
            "{}: {}",
            paint(self.color, sgr, &code),
            paint(self.color, BOLD, &diagnostic.message)
        );
    }

    /// The `G |` line, the rows, and the closing `G |` line of one snippet (§11.6.4).
    fn snippet_block(&self, out: &mut String, gutter: &str, width: usize, snippet: &Snippet) {
        let bar = paint(self.color, BOLD_BLUE, "|");
        let _ = writeln!(out, "{gutter} {bar}");
        for row in &snippet.rows {
            match row {
                SnippetRow::Gap => {
                    let _ = writeln!(out, "{}", paint(self.color, BOLD_BLUE, "..."));
                }
                SnippetRow::Line {
                    number,
                    text,
                    marks,
                } => {
                    let number = paint(self.color, BOLD_BLUE, &format!("{number:>width$}"));
                    let _ = writeln!(out, "{number} {bar} {text}");
                    for mark in marks {
                        self.mark_row(out, gutter, &bar, mark);
                    }
                }
            }
        }
        let _ = writeln!(out, "{gutter} {bar}");
    }

    fn mark_row(&self, out: &mut String, gutter: &str, bar: &str, mark: &SnippetMark) {
        let (glyph, sgr) = if mark.primary {
            ("^", BOLD_RED)
        } else {
            ("-", BOLD_BLUE)
        };
        let marks = glyph.repeat(mark.width.max(1));
        let _ = write!(
            out,
            "{gutter} {bar} {}{}",
            " ".repeat(mark.column),
            paint(self.color, sgr, &marks)
        );
        if !mark.message.is_empty() {
            let _ = write!(out, " {}", paint(self.color, sgr, &mark.message));
        }
        out.push('\n');
    }

    /// `G = note: first line at <location>`, then the other lines at `G` plus `kind.len() + 7`
    /// spaces (§11.10.5).
    fn child(
        &self,
        out: &mut String,
        gutter: &str,
        kind: ChildKind,
        message: &str,
        location: Option<&Location>,
    ) {
        let (word, sgr) = match kind {
            ChildKind::Note => ("note", BOLD),
            ChildKind::Help => ("help", BOLD_GREEN),
        };
        let mut lines = message.split('\n');
        let first = lines.next().unwrap_or_default();
        let _ = write!(
            out,
            "{gutter} {} {}: {first}",
            paint(self.color, BOLD_BLUE, "="),
            paint(self.color, sgr, word)
        );
        if let Some(location) = location {
            let _ = write!(out, " at {}", self.location_text(location));
        }
        out.push('\n');
        let indent = " ".repeat(word.len() + 7);
        for line in lines {
            let _ = writeln!(out, "{gutter}{indent}{line}");
        }
    }

    /// The `<path>:<line>:<col>` text of a location, or the path alone without a position.
    fn location_text(&self, location: &Location) -> String {
        match self.source.position(location) {
            Some((line, column)) => format!("{}:{line}:{column}", location.file.path),
            None => location.file.path.clone(),
        }
    }

    /// The text of a chain note (§11.8.4). A chain without steps has no note.
    fn related_note(&self, related: &Related) -> Option<Child> {
        let (heading, steps, effect) = match related {
            Related::EffectChain { effect, steps } => {
                let first = steps.first()?;
                (
                    format!(
                        "{} has effect {} because:",
                        quoted(&first.name),
                        quoted(effect)
                    ),
                    steps,
                    Some(effect.as_str()),
                )
            }
            Related::CapabilityChain {
                capability,
                effect,
                steps,
                ..
            } => (
                format!(
                    "{} is reached through effect {}:",
                    quoted(capability),
                    quoted(effect)
                ),
                steps,
                Some(effect.as_str()),
            ),
            Related::RequirementChain { steps } => {
                if steps.is_empty() {
                    return None;
                }
                ("conflicting requirement:".to_owned(), steps, None)
            }
            Related::DeclaredHere { location, what } => {
                return Some(Child {
                    kind: ChildKind::Note,
                    message: format!("{what} is declared here"),
                    location: Some(location.clone()),
                });
            }
            Related::BorrowConflict { .. } | Related::UseAfterMove { .. } => return None,
        };

        let mut message = heading;
        for (index, step) in steps.iter().enumerate() {
            message.push('\n');
            message.push_str(&self.step_line(step, steps.get(index + 1), effect));
        }
        Some(Child {
            kind: ChildKind::Note,
            message,
            location: None,
        })
    }

    fn step_line(
        &self,
        step: &ChainStep,
        next: Option<&ChainStep>,
        effect: Option<&str>,
    ) -> String {
        let name = quoted(&step.name);
        let body = match (step.relation, next) {
            (Relation::Calls, Some(next)) => format!("{name} calls {}", quoted(&next.name)),
            (Relation::Instantiates, Some(next)) => {
                format!(
                    "{name} instantiates a type parameter with {}",
                    quoted(&next.name)
                )
            }
            (Relation::Awaits, Some(next)) => format!("{name} awaits {}", quoted(&next.name)),
            (Relation::Covers, Some(next)) => format!("{name} covers {}", quoted(&next.name)),
            (Relation::Imports, Some(next)) => {
                format!("{name} is imported from {}", quoted(&next.name))
            }
            (Relation::Requires, Some(next)) => format!("{name} requires {}", quoted(&next.name)),
            (Relation::Declares, _) => match effect {
                Some(effect) => format!("{name} declares {}", quoted(effect)),
                None => name,
            },
            _ => name,
        };
        match &step.location {
            Some(location) => format!("{body} at {}", self.location_text(location)),
            None => body,
        }
    }

    /// Builds one group per file: the primary file first, then the other files in the order of
    /// their first label (§11.10.2 item 3). Related labels are added unless a label with the same
    /// range already exists (§11.8.4).
    fn groups(&self, diagnostic: &Diagnostic) -> Vec<Group> {
        let mut pending: Vec<(Location, SnippetAnnotation)> = Vec::new();
        if let Some(primary) = diagnostic.primary() {
            pending.push((
                primary.clone(),
                SnippetAnnotation {
                    range: primary.span.range,
                    primary: true,
                    message: diagnostic.primary_label.clone(),
                },
            ));
        }
        for label in diagnostic.labels() {
            pending.push((
                label.location.clone(),
                SnippetAnnotation {
                    range: label.location.span.range,
                    primary: false,
                    message: label.message.clone(),
                },
            ));
        }
        for related in diagnostic.related() {
            for (location, message) in related_labels(related) {
                let exists = pending.iter().any(|(existing, _)| {
                    same_file(existing, &location) && existing.span.range == location.span.range
                });
                if !exists {
                    pending.push((
                        location.clone(),
                        SnippetAnnotation {
                            range: location.span.range,
                            primary: false,
                            message,
                        },
                    ));
                }
            }
        }

        let mut groups: Vec<Group> = Vec::new();
        for (location, annotation) in pending {
            match groups
                .iter_mut()
                .find(|group| same_file_ref(&group.file, &location.file))
            {
                Some(group) => group.annotations.push(annotation),
                None => groups.push(Group {
                    file: location.file.clone(),
                    header: location,
                    annotations: vec![annotation],
                    snippet: None,
                }),
            }
        }
        for group in &mut groups {
            group.snippet = self.source.snippet(&group.file, &group.annotations);
        }
        groups
    }

    /// The number of decimal digits of the largest line number displayed in the diagnostic, with
    /// a minimum of one (§11.10.2).
    /// Only line numbers in the gutter count. The `at` positions of children are text, as the
    /// chain notes of §11.10.9 show with a one-digit gutter next to `88:5`.
    fn number_width(&self, groups: &[Group]) -> usize {
        let largest = groups
            .iter()
            .filter_map(|group| group.snippet.as_ref())
            .flat_map(|snippet| &snippet.rows)
            .filter_map(|row| match row {
                SnippetRow::Gap => None,
                SnippetRow::Line { number, .. } => Some(*number),
            })
            .max()
            .unwrap_or(1);
        largest.to_string().len()
    }
}

/// The labels that a related entry adds to the snippet (§11.8.4), with their messages.
fn related_labels(related: &Related) -> Vec<(Location, String)> {
    match related {
        Related::BorrowConflict {
            place,
            loan,
            loan_kind,
            later_use,
            ..
        } => {
            let kind = match loan_kind {
                crate::LoanKind::Shared => "borrowed",
                crate::LoanKind::Mutable => "mutably borrowed",
            };
            let mut labels = vec![(loan.clone(), format!("{} is {kind} here", quoted(place)))];
            if let Some(later_use) = later_use {
                labels.push((later_use.clone(), "borrow later used here".to_owned()));
            }
            labels
        }
        Related::UseAfterMove {
            place, moved_at, ..
        } => vec![(moved_at.clone(), format!("{} moved here", quoted(place)))],
        _ => Vec::new(),
    }
}

/// The note that explains where a lint level was set (§11.3.4), or `None` for the default.
fn level_message(diagnostic: &Diagnostic, level: &LevelInfo) -> Option<String> {
    let code = diagnostic.code.as_str();
    let word = match level.value {
        LintLevel::Allow => "allow",
        LintLevel::Info => "info",
        LintLevel::Warn => "warn",
        LintLevel::Deny => "deny",
    };
    let verb = match level.value {
        LintLevel::Allow => "allowed",
        LintLevel::Info => "set to info",
        LintLevel::Warn => "warned",
        LintLevel::Deny => "denied",
    };
    match level.source {
        LevelSource::Default => None,
        LevelSource::Attribute => Some(format!("{} is {verb} by `@{word}({code})`", quoted(code))),
        LevelSource::Manifest => Some(format!(
            "{} is {verb} by the `lints` block of `sai.package`",
            quoted(code)
        )),
        LevelSource::Policy => Some(format!(
            "{} is {verb} by the `policy` of `sai.package`",
            quoted(code)
        )),
        LevelSource::Cli => Some(format!("{} is {verb} by `--{word} {code}`", quoted(code))),
        LevelSource::DenyWarnings => {
            Some(format!("{} is denied by `--deny-warnings`", quoted(code)))
        }
    }
}

fn quoted(name: &str) -> String {
    if name == "…" {
        return name.to_owned();
    }
    format!("`{name}`")
}

fn same_file(left: &Location, right: &Location) -> bool {
    same_file_ref(&left.file, &right.file)
}

fn same_file_ref(left: &SourceRef, right: &SourceRef) -> bool {
    left.path == right.path && left.package == right.package
}

fn paint(color: bool, sgr: &str, text: &str) -> String {
    if color {
        format!("{sgr}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::{AccessKind, Code, FileId, Label, LoanKind, Span, TextSize};

    /// Positions come from the file texts. Snippets are prepared per file, and every request is
    /// recorded so that tests can check which annotations were asked for.
    #[derive(Default)]
    struct TestSource {
        files: Vec<(&'static str, String)>,
        snippets: Vec<(&'static str, Snippet)>,
        requested: RefCell<Vec<(String, Vec<SnippetAnnotation>)>>,
    }

    impl SourceSnippets for TestSource {
        fn position(&self, location: &Location) -> Option<(usize, usize)> {
            let text = self.text(&location.file.path)?;
            let offset = location.span.range.start().raw() as usize;
            let before = text.get(..offset)?;
            let line_start = before.rfind('\n').map_or(0, |index| index + 1);
            Some((before.matches('\n').count() + 1, offset - line_start + 1))
        }

        fn snippet(&self, file: &SourceRef, annotations: &[SnippetAnnotation]) -> Option<Snippet> {
            self.requested
                .borrow_mut()
                .push((file.path.clone(), annotations.to_vec()));
            self.snippets
                .iter()
                .find(|(path, _)| *path == file.path)
                .map(|(_, snippet)| snippet.clone())
        }
    }

    impl TestSource {
        fn text(&self, path: &str) -> Option<&str> {
            self.files
                .iter()
                .find(|(file, _)| *file == path)
                .map(|(_, text)| text.as_str())
        }
    }

    /// The text with every line terminated by LF, as the expected output is written.
    fn lines(rows: &[&str]) -> String {
        rows.iter().map(|row| format!("{row}\n")).collect()
    }

    /// The byte offset of the 1-based `line` and `column` in ASCII `text`.
    fn offset(text: &str, line: usize, column: usize) -> u32 {
        let line_start: usize = text
            .split_inclusive('\n')
            .take(line - 1)
            .map(str::len)
            .sum();
        u32::try_from(line_start + column - 1).expect("test offsets fit in u32")
    }

    fn location(path: &str, text: &str, line: usize, column: usize, length: u32) -> Location {
        let start = offset(text, line, column);
        let range = TextRange::new(TextSize::new(start), TextSize::new(start + length))
            .expect("ordered test range");
        Location::new(path, Span::new(FileId::new(0), range))
    }

    fn diagnostic(code: &str, message: &str, primary: Option<Location>) -> Diagnostic {
        Diagnostic::new(
            Code::from_name(code).expect("test code is registered"),
            Severity::Error,
            message,
            primary,
        )
    }

    fn help(message: &str) -> Child {
        Child {
            kind: ChildKind::Help,
            message: message.to_owned(),
            location: None,
        }
    }

    fn one_row(
        number: usize,
        text: &str,
        column: usize,
        width: usize,
        primary: bool,
        message: &str,
    ) -> Snippet {
        Snippet {
            rows: vec![SnippetRow::Line {
                number,
                text: text.to_owned(),
                marks: vec![SnippetMark {
                    column,
                    width,
                    primary,
                    message: message.to_owned(),
                }],
            }],
        }
    }

    fn user_text() -> String {
        "\n".repeat(11) + "    let label = user.username\n"
    }

    fn user_diagnostic(user: &str) -> Diagnostic {
        let mut diagnostic = diagnostic(
            "UNKNOWN_FIELD",
            "`User` has no field `username`",
            Some(location("src/user.sai", user, 12, 22, 8)),
        );
        diagnostic.primary_label = "unknown field".to_owned();
        diagnostic
    }

    #[test]
    fn renders_a_primary_snippet_with_a_two_digit_gutter() {
        let user = user_text();
        let mut diagnostic = user_diagnostic(&user);
        diagnostic.push_child(help("replace with `name`")).unwrap();
        let source = TestSource {
            files: vec![("src/user.sai", user.clone())],
            snippets: vec![(
                "src/user.sai",
                one_row(
                    12,
                    "    let label = user.username",
                    21,
                    8,
                    true,
                    "unknown field",
                ),
            )],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[UNKNOWN_FIELD]: `User` has no field `username`",
                "  --> src/user.sai:12:22",
                "   |",
                "12 |     let label = user.username",
                "   |                      ^^^^^^^^ unknown field",
                "   |",
                "   = help: replace with `name`",
                "",
            ])
        );
    }

    #[test]
    fn renders_a_one_digit_gutter_with_one_space_before_the_arrow() {
        let user = lines(&["fn main() {", "    let x = 1", "}"]);
        let diagnostic = diagnostic(
            "UNKNOWN_FIELD",
            "`User` has no field `username`",
            Some(location("src/user.sai", &user, 2, 9, 1)),
        );
        let source = TestSource {
            files: vec![("src/user.sai", user.clone())],
            snippets: vec![("src/user.sai", one_row(2, "    let x = 1", 8, 1, true, ""))],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[UNKNOWN_FIELD]: `User` has no field `username`",
                " --> src/user.sai:2:9",
                "  |",
                "2 |     let x = 1",
                "  |         ^",
                "  |",
                "",
            ])
        );
    }

    #[test]
    fn renders_further_files_and_multi_line_help_as_in_impl_11() {
        let user = user_text();
        let model = lines(&["", "", "type User {"]);
        let mut diagnostic = user_diagnostic(&user);
        diagnostic
            .push_label(Label {
                location: location("src/model.sai", &model, 3, 6, 4),
                message: "`User` is declared here".to_owned(),
            })
            .unwrap();
        diagnostic
            .push_child(help("available fields:\nname\ndisplay_name"))
            .unwrap();
        diagnostic.push_child(help("replace with `name`")).unwrap();
        let source = TestSource {
            files: vec![("src/user.sai", user.clone()), ("src/model.sai", model)],
            snippets: vec![
                (
                    "src/user.sai",
                    one_row(
                        12,
                        "    let label = user.username",
                        21,
                        8,
                        true,
                        "unknown field",
                    ),
                ),
                (
                    "src/model.sai",
                    one_row(3, "type User {", 5, 4, false, "`User` is declared here"),
                ),
            ],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[UNKNOWN_FIELD]: `User` has no field `username`",
                "  --> src/user.sai:12:22",
                "   |",
                "12 |     let label = user.username",
                "   |                      ^^^^^^^^ unknown field",
                "   |",
                "  ::: src/model.sai:3:6",
                "   |",
                " 3 | type User {",
                "   |      ---- `User` is declared here",
                "   |",
                "   = help: available fields:",
                "             name",
                "             display_name",
                "   = help: replace with `name`",
                "",
            ])
        );
    }

    #[test]
    fn renders_an_effect_chain_as_a_note_with_aligned_steps() {
        let lib = lines(&[
            "// rejected: MISSING_EFFECTS_DECLARATION",
            "export fn greet(count: I32) -> I32 {",
            "    let message = build_message(count)",
            "    message.len().trunc_to<I32>()",
            "}",
        ]);
        let message = lines(&["", "", "", "    String.concat(a, b)"]);
        let string = "\n".repeat(87) + "    fn concat\n";
        let dependency = "dep:sai:alloc@0.1.0/src/string.sai";
        let mut diagnostic = diagnostic(
            "MISSING_EFFECTS_DECLARATION",
            "exported function `greet` does not declare its effects",
            Some(location("src/lib.sai", &lib, 2, 11, 5)),
        );
        diagnostic.primary_label = "inferred effects: `{ alloc }`".to_owned();
        let step = |name: &str, location: Location, relation| ChainStep {
            name: name.to_owned(),
            symbol: None,
            location: Some(location),
            relation,
        };
        diagnostic
            .push_related(Related::EffectChain {
                effect: "alloc".to_owned(),
                steps: vec![
                    step(
                        "greet",
                        location("src/lib.sai", &lib, 3, 19, 5),
                        Relation::Calls,
                    ),
                    step(
                        "build_message",
                        location("src/message.sai", &message, 4, 5, 11),
                        Relation::Calls,
                    ),
                    step(
                        "String.concat",
                        location(dependency, &string, 88, 5, 6),
                        Relation::Declares,
                    ),
                ],
            })
            .unwrap();
        diagnostic
            .push_child(help("declare the inferred effects"))
            .unwrap();
        let source = TestSource {
            files: vec![
                ("src/lib.sai", lib.clone()),
                ("src/message.sai", message),
                (dependency, string),
            ],
            snippets: vec![(
                "src/lib.sai",
                one_row(
                    2,
                    "export fn greet(count: I32) -> I32 {",
                    10,
                    5,
                    true,
                    "inferred effects: `{ alloc }`",
                ),
            )],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[MISSING_EFFECTS_DECLARATION]: exported function `greet` does not declare its effects",
                " --> src/lib.sai:2:11",
                "  |",
                "2 | export fn greet(count: I32) -> I32 {",
                "  |           ^^^^^ inferred effects: `{ alloc }`",
                "  |",
                "  = help: declare the inferred effects",
                "  = note: `greet` has effect `alloc` because:",
                "            `greet` calls `build_message` at src/lib.sai:3:19",
                "            `build_message` calls `String.concat` at src/message.sai:4:5",
                "            `String.concat` declares `alloc` at dep:sai:alloc@0.1.0/src/string.sai:88:5",
                "",
            ])
        );
    }

    #[test]
    fn renders_the_level_source_as_the_last_note_before_related_notes() {
        let lib = lines(&["// header", "fn main() {", "    let x = 1", "}"]);
        let mut diagnostic = diagnostic(
            "UNUSED_VARIABLE",
            "unused variable `x`",
            Some(location("src/lib.sai", &lib, 3, 9, 1)),
        );
        diagnostic.primary_label = "unused variable".to_owned();
        diagnostic
            .set_level(Some(LevelInfo {
                value: LintLevel::Deny,
                source: LevelSource::Attribute,
                location: Some(location("src/lib.sai", &lib, 3, 1, 1)),
            }))
            .unwrap();
        diagnostic.push_child(help("prefix it with `_`")).unwrap();
        let source = TestSource {
            files: vec![("src/lib.sai", lib.clone())],
            snippets: vec![(
                "src/lib.sai",
                one_row(3, "    let x = 1", 8, 1, true, "unused variable"),
            )],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[UNUSED_VARIABLE]: unused variable `x`",
                " --> src/lib.sai:3:9",
                "  |",
                "3 |     let x = 1",
                "  |         ^ unused variable",
                "  |",
                "  = help: prefix it with `_`",
                "  = note: `UNUSED_VARIABLE` is denied by `@deny(UNUSED_VARIABLE)` at src/lib.sai:3:1",
                "",
            ])
        );
    }

    #[test]
    fn related_borrow_conflicts_become_labels_unless_a_label_has_their_range() {
        let text = lines(&["let r = v", "v = 2", "use r"]);
        let mut diagnostic = diagnostic(
            "ASSIGN_WHILE_BORROWED",
            "`v` is assigned while borrowed",
            Some(location("src/lib.sai", &text, 2, 1, 1)),
        );
        let loan = location("src/lib.sai", &text, 1, 9, 1);
        let later_use = location("src/lib.sai", &text, 3, 5, 1);
        diagnostic
            .push_related(Related::BorrowConflict {
                place: "v".to_owned(),
                loan: loan.clone(),
                loan_kind: LoanKind::Shared,
                access: location("src/lib.sai", &text, 2, 1, 1),
                access_kind: AccessKind::Write,
                later_use: Some(later_use),
            })
            .unwrap();
        let source = TestSource {
            files: vec![("src/lib.sai", text.clone())],
            snippets: vec![(
                "src/lib.sai",
                Snippet {
                    rows: vec![
                        SnippetRow::Line {
                            number: 1,
                            text: "let r = v".to_owned(),
                            marks: vec![SnippetMark {
                                column: 8,
                                width: 1,
                                primary: false,
                                message: "`v` is borrowed here".to_owned(),
                            }],
                        },
                        SnippetRow::Line {
                            number: 2,
                            text: "v = 2".to_owned(),
                            marks: vec![SnippetMark {
                                column: 0,
                                width: 1,
                                primary: true,
                                message: String::new(),
                            }],
                        },
                        SnippetRow::Line {
                            number: 3,
                            text: "use r".to_owned(),
                            marks: vec![SnippetMark {
                                column: 4,
                                width: 1,
                                primary: false,
                                message: "borrow later used here".to_owned(),
                            }],
                        },
                    ],
                },
            )],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[ASSIGN_WHILE_BORROWED]: `v` is assigned while borrowed",
                " --> src/lib.sai:2:1",
                "  |",
                "1 | let r = v",
                "  |         - `v` is borrowed here",
                "2 | v = 2",
                "  | ^",
                "3 | use r",
                "  |     - borrow later used here",
                "  |",
                "",
            ])
        );
        let requested = source.requested.borrow();
        let (_, annotations) = &requested[0];
        let messages: Vec<&str> = annotations
            .iter()
            .map(|annotation| annotation.message.as_str())
            .collect();
        assert_eq!(
            messages,
            ["", "`v` is borrowed here", "borrow later used here"]
        );
    }

    #[test]
    fn a_label_on_the_loan_range_suppresses_the_related_loan_label() {
        let text = lines(&["let r = v", "v = 2"]);
        let mut diagnostic = diagnostic(
            "ASSIGN_WHILE_BORROWED",
            "`v` is assigned while borrowed",
            Some(location("src/lib.sai", &text, 2, 1, 1)),
        );
        diagnostic
            .push_label(Label {
                location: location("src/lib.sai", &text, 1, 9, 1),
                message: "borrowed by `r`".to_owned(),
            })
            .unwrap();
        diagnostic
            .push_related(Related::BorrowConflict {
                place: "v".to_owned(),
                loan: location("src/lib.sai", &text, 1, 9, 1),
                loan_kind: LoanKind::Mutable,
                access: location("src/lib.sai", &text, 2, 1, 1),
                access_kind: AccessKind::Write,
                later_use: None,
            })
            .unwrap();
        let source = TestSource {
            files: vec![("src/lib.sai", text)],
            ..TestSource::default()
        };

        render_diagnostic(&diagnostic, &source, false);

        let requested = source.requested.borrow();
        let messages: Vec<&str> = requested[0]
            .1
            .iter()
            .map(|annotation| annotation.message.as_str())
            .collect();
        assert_eq!(messages, ["", "borrowed by `r`"]);
    }

    #[test]
    fn a_long_gap_between_snippet_rows_is_one_gutterless_row() {
        let text = lines(&["first", "", "", "", "fifth"]);
        let diagnostic = diagnostic(
            "UNKNOWN_FIELD",
            "`User` has no field `username`",
            Some(location("src/a.sai", &text, 5, 1, 5)),
        );
        let source = TestSource {
            files: vec![("src/a.sai", text)],
            snippets: vec![(
                "src/a.sai",
                Snippet {
                    rows: vec![
                        SnippetRow::Line {
                            number: 1,
                            text: "first".to_owned(),
                            marks: Vec::new(),
                        },
                        SnippetRow::Gap,
                        SnippetRow::Line {
                            number: 5,
                            text: "fifth".to_owned(),
                            marks: Vec::new(),
                        },
                    ],
                },
            )],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, false);

        assert_eq!(
            rendered,
            lines(&[
                "error[UNKNOWN_FIELD]: `User` has no field `username`",
                " --> src/a.sai:5:1",
                "  |",
                "1 | first",
                "...",
                "5 | fifth",
                "  |",
                "",
            ])
        );
    }

    #[test]
    fn a_diagnostic_without_a_primary_location_uses_two_space_children() {
        let mut diagnostic = diagnostic(
            "UNKNOWN_OPTION",
            "unknown option `--tagret` for `sai build`",
            None,
        );
        diagnostic
            .push_child(help("a similar option exists: `--target`"))
            .unwrap();

        let rendered = render_diagnostic(&diagnostic, &TestSource::default(), false);

        assert_eq!(
            rendered,
            lines(&[
                "error[UNKNOWN_OPTION]: unknown option `--tagret` for `sai build`",
                "  = help: a similar option exists: `--target`",
                "",
            ])
        );
    }

    #[test]
    fn color_marks_the_heading_marks_and_help_with_the_documented_sequences() {
        let user = user_text();
        let mut diagnostic = user_diagnostic(&user);
        diagnostic.push_child(help("replace with `name`")).unwrap();
        let source = TestSource {
            files: vec![("src/user.sai", user)],
            snippets: vec![(
                "src/user.sai",
                one_row(
                    12,
                    "    let label = user.username",
                    21,
                    8,
                    true,
                    "unknown field",
                ),
            )],
            ..TestSource::default()
        };

        let rendered = render_diagnostic(&diagnostic, &source, true);

        assert!(rendered.starts_with(
            "\x1b[1;31merror[UNKNOWN_FIELD]\x1b[0m: \x1b[1m`User` has no field `username`\x1b[0m\n"
        ));
        assert!(rendered.contains("  \x1b[1;34m-->\x1b[0m src/user.sai:12:22\n"));
        assert!(rendered.contains("\x1b[1;34m12\x1b[0m \x1b[1;34m|\x1b[0m     let label"));
        assert!(rendered.contains(
            "   \x1b[1;34m|\x1b[0m                      \x1b[1;31m^^^^^^^^\x1b[0m \x1b[1;31munknown field\x1b[0m\n"
        ));
        assert!(
            rendered.contains("   \x1b[1;34m=\x1b[0m \x1b[1;32mhelp\x1b[0m: replace with `name`\n")
        );
        assert!(!render_diagnostic(&diagnostic, &TestSource::default(), false).contains('\x1b'));
    }

    #[test]
    fn summary_lines_follow_the_documented_counts_and_limit_text() {
        let errors = |errors, hidden_errors, max_errors| SummaryCounts {
            errors,
            hidden_errors,
            max_errors,
            ..SummaryCounts::default()
        };

        assert_eq!(
            render_summary("check", errors(1, 0, 50), false),
            "check failed: 1 error\n"
        );
        assert_eq!(
            render_summary("check", errors(2, 1, 1), false),
            "check failed: 2 errors; 1 more errors not shown (--max-errors 1)\n"
        );
        assert_eq!(
            render_summary("check", errors(2, 1, 0), false),
            "check failed: 2 errors\n"
        );
        assert_eq!(
            render_summary(
                "check",
                SummaryCounts {
                    warnings: 1,
                    infos: 2,
                    ..SummaryCounts::default()
                },
                false
            ),
            "check finished: 1 warning, 2 info\n"
        );
        assert_eq!(
            render_summary("check", errors(1, 0, 50), true),
            "check \x1b[1;31mfailed\x1b[0m: 1 error\n"
        );
    }
}
