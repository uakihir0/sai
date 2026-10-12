# 14. Formatter

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 0 (`sai.package` formatting: Stage 3)
**Related:** [decisions.md](../design/decisions.md) D-TOOL-04, D-LEX-01, D-SYN-06, D-SYN-07; [spec/01-introduction.md](../spec/01-introduction.md) §1.4.4; [spec/02-lexical-structure.md](../spec/02-lexical-structure.md); [spec/03-grammar.md](../spec/03-grammar.md); [spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.3.1; [spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.2.1; [package/01-package-contract.md](../package/01-package-contract.md) §1.2, §1.3; [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.5, §2.6; [11-diagnostics.md](11-diagnostics.md); [12-semantic-tooling.md](12-semantic-tooling.md); [13-cli.md](13-cli.md) §13.6.7

This chapter defines the canonical form of SAI source files and of `sai.package` files, and the algorithm that produces it. The canonical form has no settings (D-TOOL-04): 4-space indentation, LF line endings, a maximum line width of 100, and exactly one trailing newline. Every conforming formatter produces byte-identical output for the same input (spec/01 §1.4.4). The chapter covers the guarantees of the formatter (§14.1), which inputs it refuses (§14.2), the document model and printer (§14.3), tokens and spacing (§14.4), line breaking under the NEWLINE rules of spec/02 §2.10 (§14.5), comments and blank lines (§14.6), and the layout of items (§14.7), statements (§14.8), expressions (§14.9), and patterns and types (§14.10). §14.11 defines verification, idempotency, and `sai fmt --check`, §14.12 the layout of `sai.package`, and §14.13 the canonical one-line text used in messages. The command line of `sai fmt` is owned by [13-cli.md](13-cli.md) §13.6.7. The syntax tree and its trivia are owned by [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6.

---

## 14.1 Overview and guarantees

### 14.1.1 Inputs and outputs

The formatter is the library crate `sai_fmt`. It has two entry points.

```rust
pub fn format_source(text: &[u8]) -> Result<String, SourceFormatError>;
pub fn format_package(text: &[u8]) -> Result<String, PackageFormatError>;

// Package-format refusal. Its fields are package-grammar diagnostics.
pub struct Refusal {
    pub lexical_errors: Vec<sai_package::lexer::PackageLexError>,
    pub syntax_errors: Vec<sai_package::manifest::PackageParseError>,
}

// Source-format refusal. `SourceRefusal` and `SourceFormatError` are distinct
// from the package API because they carry source-file diagnostics.
pub struct SourceRefusal {
    pub input_errors: Vec<Diagnostic>,
    pub lexical_errors: Vec<Diagnostic>,
    pub syntax_errors: Vec<Diagnostic>,
}

pub enum SourceFormatError {
    Refused(SourceRefusal),
    UnsupportedShape,
    InternalError,
    Print(PrintError),
    VerificationFailed(SourceVerificationFailure),
}

pub enum SourceVerificationCheck {
    OutputSyntax,
    SyntaxTreePreservation,
    CommentPreservation,
    Idempotency,
}

pub enum PackageFormatError {
    Refused(Refusal),
    InternalError,
    Print(PrintError),
    VerificationFailed(PackageVerificationFailure),
}

pub struct PackageVerificationFailure {
    pub check: PackageVerificationCheck,
    pub input_byte_offset: Option<usize>,
}

pub struct SourceVerificationFailure {
    pub check: SourceVerificationCheck,
    pub input_byte_offset: Option<usize>,
}
```

- `PackageFormatError::Refused` carries only lexer errors and parser errors that cause refusal; structural and value diagnostics remain formatable (§14.12.1).
- `SourceFormatError::Refused` carries input-limit, lexer, and parser diagnostics as defined in §14.2.2. Its `SourceRefusal` is separate from the package-specific `Refusal`.
- The current implementation can return `SourceFormatError::UnsupportedShape` for valid syntax that its document builder does not yet support. This is an implementation gap, not an additional normative refusal condition; a conforming implementation formats every input that passes §14.2.2.
- If formatted output exceeds the source-size limit, the current implementation reports an internal formatting failure; lexical or parser errors in output report `SourceVerificationCheck::OutputSyntax`.
- `PackageFormatError::VerificationFailed` carries the failed check and, when available, the first differing byte offset in the original input (§14.11.2).
- The package entry point receives bytes without a file identity, so its refusal returns the parser's byte-spanned error records. The CLI maps those records to file diagnostics after it supplies the source location.
- `format_package` is implemented. The source input-preparation helper `parse_source` now decodes, lexes, parses, and applies the source refusal rules. The production document builder covers functions with supported outer docs and attributes, named or `_` parameters optionally prefixed by `var`, type and const generic parameters with trait or function bounds and optional bound effects, and recursive path, `Self` path, borrow, owned, tuple, grouped, inferred, and generic types, including associated-type bindings, scalar constant generic arguments, and function-pointer types with parameter lists, return types, and effect sets. Generic parameter attributes remain fail-closed because the grammar accepts them only to report `INVALID_ATTRIBUTE_TARGET`. Function declarations format `pub`/`export`, `unsafe`/`async`, optional bodies, and `where`, `borrows`, `effects`, `requires`, and `ensures` clauses in canonical order. Contract expressions currently cover literals, names, format strings, all six prefix operators, unit, tuple and array expressions, implicit members, field and tuple-index access, generic calls with positional, named, and record-update arguments, indexing, `?`, `.await`, member chains, comparisons, non-range binary operator chains at all precedence levels, and range expressions. Function-body expressions additionally support plain blocks, `if`/`else if` with expression or pattern let conditions, `match` with pattern arms, guards, and block-like bodies, `while` with expression or pattern let conditions, and `loop` with optional labels, `for` with patterns, `async`/`async move` blocks, `region` blocks, and `unsafe` blocks containing exactly one expression when the flat form fits; `for await` syntax is also formatted. Pattern formatting covers the forms in §14.10.1, including destructuring in `if let`/`while let` conditions, `for` loops, match arms, and `let` statements, including `let … else` statements with formatted else blocks. Closure expressions support `fn`/`move fn`, name, typed-name, `var`-name, and `_` parameters; single-expression bodies are flat when they fit, and a final closure argument uses the hugged call layout when its first line fits. Prefix operators use the spacing of §14.9.1 and nested prefix chains are traversed iteratively. Range operators have no surrounding spaces, and their endpoints follow the ordinary expression rules; explicit parentheses protect nested operator chains without being treated as though those chains fill the parentheses. Tuple and array lists use the documented width breaks and trailing commas, including the singleton-tuple comma; index expressions break only inside their brackets. Call arguments preserve source order; a final record-update argument has no trailing comma when its list breaks. Operator chains follow §§14.9.2–14.9.3, preserve source order, and break before each operator with the required context-dependent parentheses. Member chains follow N-H-50: chains with at least two call-bearing segments break before each segment; `.await` and `.INT` start segments, while `?` and index operations attach to the preceding segment or to the head before the first segment; `.UIDENT` operations remain joined to the previous segment. An optional first `self` receiver supports `Self`, `borrow Self`, or `borrow mut Self`. Receiver formatting is tested on isolated function-declaration nodes and inside formatted inherent, trait, and interface impl declarations; impl targets support types, trait references with `for` self types, and lowercase module paths, and member bodies support methods and associated type definitions. Trait wrappers now also format `pub` visibility, optional `unsafe`, generic parameters, supertrait bounds, `where` clauses, associated-type declarations, bodyless or default methods, and empty or nonempty member bodies. Top-level type aliases, records, enums, and plain distinct types format with visibility and generic parameters; distinct types support an optional `pub` base and movable right-hand sides. Top-level resources format optional visibility and omitted, empty, or populated field lists. Enums support unit, tuple, and record variants, signed discriminants, and optional `where` clauses. Record and resource fields retain optional `pub` and source blank-line gaps; nonempty record, resource, and enum lists always break, and empty bodies use `{}`. Outer docs and attributes on supported declarations, record/resource fields, enum variants, and supported impl/trait/interface members are formatted; trailing doc whitespace is removed, and attribute arguments support positional, named, nested, string, and integer forms. Refined aliases and distinct types format their predicates, keeping `where` on the type line when it fits and otherwise on an indented continuation line. Interface wrappers format `pub` visibility, aliases, records, enums, function heads, and interface resources with absent, empty, or populated method bodies. Function bodies are always broken and support pattern `let` statements, including `let … else`, and simple-binding `var` statements, assignments, `assert`, and expression statements; they preserve at most one blank line between statements. Expression statements also support `return`, `break`, and `continue`, with optional labels and values kept on the keyword line. Supported right-hand sides use the documented flat, moved, and broken alternatives; values after `return` and `break` cannot move to another line. Delimited parameter and generic argument lists break by width and receive trailing commas only when required by the documented layout. Effect sets and `borrows` lists use the flat or broken brace-list rules and retain their source order. The public `format_source` entry point now verifies output syntax, normalized syntax-tree preservation, comment multiset preservation, and idempotency. `sai fmt -` and explicitly selected or discovered `.sai` files format through this entry point. The CLI maps decoded diagnostic spans to original input byte offsets, drains oversized input without retaining excess bytes to report its full size, and keeps valid-but-unsupported syntax out of the formatter `refused` list. Detached file-header comments and EOF-leading comments (including demoted docs in those positions) are supported with trailing-whitespace trimming and blank-line preservation. Own-line comments attached to top-level declarations and trailing line or block comments at top-level item boundaries are preserved, and attached comments move with declarations when uses are sorted. Detached inter-item comments, nested comments, shebangs, generic-parameter attributes, and other unsupported shapes remain fail-closed; module-header docs, unsafe module declarations, and top-level use declarations are supported. Implicit workspace discovery now resolves the root package from the current or requested directory, follows transitive local path requirements, and formats each in-workspace package manifest and every `.sai` file below its declared source directory. External path packages are read only for transitive discovery; Git packages are not fetched.
- The current source formatter supports module-header `//!` docs and `unsafe module`, plus top-level `use` declarations with stable visibility grouping, canonical path sorting, selective-item sorting, aliases, outer docs and attributes, and width-broken `UseItems`. Their placement and blank-line boundaries follow §§14.6.4 and 14.7.2. Detached file-header comments and EOF-leading comments, including demoted docs in those positions, preserve their text and blank-line placement. Own-line comments attached to top-level declarations and trailing line or block comments at top-level item boundaries are preserved; attached comments move with declarations when `use` declarations are sorted. Detached inter-item comments, nested comments, shebangs, demoted docs in other positions, and other unsupported syntax still fail closed.
- Match-arm tests cover value-preserving formatting for simple-string continuations, triple-quoted strings, and raw strings and byte strings with line breaks. Raw literal content, including its original internal indentation, is emitted exactly.
- `parse_source` returns a lossless `ParsedSource` on success. Its `SourceInputError::Refused(SourceRefusal)` contains input-limit, lexer, and parser diagnostics. A source larger than 16 MiB produces `IMPLEMENTATION_LIMIT_EXCEEDED`. Parser diagnostics are selected by origin from impl/02 §2.5.12, except `CLAUSE_ORDER`; validation diagnostics and warnings remain available from `ParsedSource::diagnostics()` without refusing formatting. Decode and snapshot failures return `InternalError`. A hard parse failure without a lexer error also returns `InternalError`; if lexical errors were already observed, an unrecoverable parse returns `Refused` with those lexer diagnostics and no parser diagnostics.
- Because `parse_source` accepts bytes without a caller-supplied file identity, it binds diagnostics to the VFS `<stdin>` source sentinel. A caller that formats a named file must replace that sentinel with the selected file's source reference before presenting diagnostics.
- The input is the raw file content. The output is UTF-8 text without a BOM.
- Neither function reads anything other than its argument: no configuration file, no environment variable, no locale, and no clock. The output is a function of the input bytes and of the toolchain version.
- `sai fmt` ([13-cli.md](13-cli.md) §13.6.7), the language server ([12-semantic-tooling.md](12-semantic-tooling.md) §12.10), structured fixes ([11-diagnostics.md](11-diagnostics.md) §11.7.3), and every tool that writes SAI text use this crate. A tool never produces source text by any other means, so generated text is canonical by construction.

### 14.1.2 Guarantees

For every input that is not refused (§14.2.2), the output satisfies the following. They are normative, and the reference implementation checks F3 to F5 on every run (§14.11.1).

| ID | Guarantee | Meaning |
|---|---|---|
| F1 | Determinism | The output depends only on the input bytes. Two conforming formatters produce byte-identical output (spec/01 §1.4.4). |
| F2 | Idempotency | `format(format(x)) = format(x)` for every input `x` that is not refused. The output of the formatter is never refused. |
| F3 | Token preservation | The output has the same tokens as the input, in the same order, except for the changes listed in §14.1.3. |
| F4 | Comment preservation | Every comment of the input appears in the output with the same text, except that trailing whitespace is removed from each of its lines. Comments keep their order relative to each other and relative to the tokens they are attached to (§14.6.1). |
| F5 | Meaning preservation | The output parses to the same syntax tree as the input after the normalization of §14.11.1, which removes trivia, NEWLINE tokens, and the permitted changes of §14.1.3. |
| F6 | Layout independence | The output does not depend on the line breaks, indentation, and spacing of the input, with two exceptions: the position of comments, and the presence of blank lines in the places listed in §14.6.4. Moving a token to another line, or changing the spaces between two tokens, does not change the output. |
| F7 | Width | No output line is longer than 100 columns (§14.3.3), unless the line contains a token, a comment, or an unbreakable sequence that cannot be laid out within the width (§14.5.6). |

F6 is the strongest property and the one that makes the format canonical: there is one way to write a program (P9). The formatter does not preserve the author's choice of breaking a list, a call chain, or an expression. It decides every line break itself, from the width and the rules of this chapter (N-H-44).

### 14.1.3 Permitted token changes

The formatter changes the token sequence in exactly the following ways. No other change is permitted.

1. **Commas in lists** (§14.5.4).
   - In a list inside `{ }` whose separator is `Sep` (spec/03 §3.1.2): commas are inserted between the elements of a list printed on one line, and removed from a list printed one element per line (D-SYN-07). A trailing comma is removed in both forms.
   - In a list inside `( )`, `[ ]`, or `< >`: a trailing comma is added when the list is printed one element per line, and removed when it is printed on one line. The comma of a one-element tuple (`(x,)` in expressions, patterns, and types) is never removed. No trailing comma is added after a `..` rest element of a constructor pattern, where the grammar does not allow one (spec/03 §3.14), or after the `..base` argument of a record construction (§14.9.4).
2. **Parentheses inserted** around a binary expression that the formatter breaks across lines (§14.9.3), and around an open-ended range expression that would end a line inside a broken `{ }` list (§14.5.4 rule 5). The formatter never removes parentheses that the author wrote.
3. **The leading `|` of a pattern** (spec/03 §3.14 `Pattern ::= '|'? …`) is removed.
4. **`use` declarations** are moved to the top of the file and sorted, and the items of every `UseItems` list are sorted (§14.7.2).
5. **Signature clauses** are reordered into the order of D-SYN-06 (§14.7.5). This is the only syntax error that the formatter corrects (§14.2.2).
6. **Token text normalization** that never changes a value:
   - hexadecimal digits of integer literals are printed in uppercase (spec/02 §2.6.1 rule 3), and the exponent marker of a float literal is printed as `e` (N-H-57);
   - the indentation inside multiline string literals and after the line continuations of simple string literals is recomputed (§14.4.3).

Whitespace, line breaks, and the zero-width NEWLINE tokens that follow from them are trivia or layout, not tokens, for the purpose of F3. A BOM is removed and CR LF becomes LF (spec/02 §2.1 rules 2 and 3).

spec/01 §1.4.4 item 2 requires that "the formatted file parses to the same syntax tree as the input once trivia and these canonicalizations are normalized away". That normalization is the one of §14.11.1, which undoes exactly the changes above (F5).

---

## 14.2 Input and refusal

### 14.2.1 Reading the input

The input is decoded and lexed exactly as by the compiler ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.3, §2.4), and parsed into the lossless CST (§2.5, §2.6). The formatter works on the CST only. It never builds HIR and never resolves names, so formatting a file never depends on other files.

1. A leading BOM is dropped (spec/02 §2.1 rule 2). The output never starts with a BOM.
2. CR LF line breaks are read as line breaks. The output uses LF only (spec/02 §2.1 rule 3).
3. TAB in whitespace trivia is not reproduced: all whitespace between tokens is generated by the printer (§14.3), so indentation and spacing never contain TAB (spec/02 §2.1 rule 5). TAB inside a comment or a string literal is part of its text and is kept.
4. A `SHEBANG` trivia piece (impl/02 §2.4) is printed verbatim as the first line, with trailing whitespace removed (§14.6.5).

### 14.2.2 Refusal

The formatter **refuses** a file, and produces no output for it, if and only if one of the following holds (N-H-43):

1. The source file is larger than the 16 MiB implementation limit and produces `IMPLEMENTATION_LIMIT_EXCEEDED` before lexing (impl/02 §2.2.2).
2. The lexer reported a diagnostic of severity `error` (spec/02 §2.12, impl/02 §2.4).
3. The parser reported a diagnostic of severity `error` other than `CLAUSE_ORDER`. "The parser" means the rows marked `parser` in the table of impl/02 §2.5.12, which include every recovery of §2.5.11 (`EXPECTED_TOKEN`, `EXPECTED_EXPRESSION`, `UNEXPECTED_NEWLINE`) and every check made while parsing (for example `CHAINED_COMPARISON`, `DUPLICATE_CLAUSE`, `BLOCK_IN_CONDITION`, `NO_DOUBLE_COLON`, `ITEM_IN_BLOCK`).

Consequences:

- A refused file is never changed. `sai fmt` reports the input-limit, lexer, and parser diagnostics of the refusal and continues with the other files ([13-cli.md](13-cli.md) §13.6.7). No additional generic refusal diagnostic is created.
- Errors of the syntactic validation pass (the rows marked `validation` in impl/02 §2.5.12, such as `MISSING_FN_BODY`, `UNKNOWN_ATTRIBUTE`, or `INFER_IN_SIGNATURE`) and every error found later (name resolution, types, effects) do not refuse. Their trees are complete, so they can be formatted, and a user can format a file while it still has semantic errors.
- Warnings never refuse. A doc comment demoted by `MISPLACED_DOC_COMMENT` is formatted as an ordinary line comment (impl/02 §2.6.4).
- `CLAUSE_ORDER` does not refuse. The tree is complete, and the formatter puts the clauses in the order of D-SYN-06 (§14.7.5), which is the fix of that diagnostic.

Rationale: a tree with recovery errors (missing tokens, `ERROR` nodes, demoted tokens) has no single meaning, so any layout of it could change what the author meant. Every other tree has exactly one meaning, which the formatter preserves (F5).

### 14.2.3 Implementation limits

The formatter has no limits beyond those of the parser (`IMPLEMENTATION_LIMIT_EXCEEDED`, spec/01 §1.5.4, impl/02 §2.5.3): a file that parses is formatted. The printer (§14.3.4) uses an explicit stack and no recursion, so deeply nested input cannot exhaust the native stack. Its running time is linear in the size of the document, except for `Choice` (§14.3.2), which renders each alternative at most once; the total cost is bounded by the input size times the nesting depth of `Choice` nodes.

---

## 14.3 Document model and printer

### 14.3.1 Pipeline

```text
bytes ─► decode + lex + parse (impl/02) ─► CST
      ─► refusal check (§14.2.2)
      ─► normalization of the tree (clause order §14.7.5, use order §14.7.2)
      ─► document builder (§14.4 – §14.10) ─► Doc
      ─► printer (§14.3.4) ─► text
      ─► verification (§14.11.1) ─► output
```

The document builder walks the CST and produces a **document** (`Doc`), a tree that describes the possible layouts of the file. The printer chooses one layout, the canonical one. The builder decides *where* a line break may occur; the printer decides *whether* it occurs. Every break opportunity that the builder creates is a position where a line break is safe under the NEWLINE rules (§14.5.1), so no decision of the printer can change the meaning of the program.

### 14.3.2 Document primitives

```rust
pub enum Doc {
    Text(SmolStr),          // literal text without a line break; width = number of scalar values
    Line,                   // a space if its group is flat, a line break if it is broken
    SoftLine,               // nothing if its group is flat, a line break if it is broken
    HardLine,               // always a line break; forces every enclosing group to break
    Indent(Box<Doc>),       // increases the indentation of line breaks inside it by 4
    Group(GroupId, Box<Doc>),        // printed flat if it fits (§14.3.4), broken otherwise
    IfBroken(GroupId, Box<Doc>, Box<Doc>),  // first doc if the group is broken, second if flat
    LineSuffix(Box<Doc>),   // deferred text, printed just before the next line break (§14.6.2)
    Choice(Vec<Doc>),       // the first alternative whose first line fits (§14.3.4)
    Flat(Box<Doc>),         // its content printed flat, whatever its width (§14.3.4)
    Verbatim(SmolStr),      // text that may contain line breaks; printed unchanged (§14.4.3, §14.6)
    Concat(Vec<Doc>),
}
```

1. A `Group` is **flat** or **broken**. In a flat group every `Line` is a space and every `SoftLine` is empty. In a broken group they are line breaks followed by the current indentation. A `Line` belongs to the innermost group that contains it.
2. A group is **forced**, and always broken, if the builder marks it as forced, or if it contains a `HardLine`, a `Verbatim` with a line break, or a `LineSuffix`, directly or in a nested group. Forcing propagates to every enclosing group up to the nearest enclosing `Choice`, and never across a `Choice`: what an alternative contains does not force the groups around the `Choice`.
3. `IfBroken(g, a, b)` refers to a group `g` that has already been printed or contains it. It is used for separators whose presence depends on the layout: commas (§14.5.4) and inserted parentheses (§14.9.3).
4. `Indent` changes only the indentation used after line breaks inside it. Text on the current line is not moved.
5. `Verbatim` text is emitted exactly. After a line break inside it, the printer does not add indentation; the text carries its own.
6. `Choice` alternatives are complete documents for the same tokens. They differ only in where line breaks and inserted parentheses go.
7. `Flat(d)` prints `d` with every group flat. It is used only inside a `Choice` alternative, to express "this layout, if it fits on one line". An alternative that contains a `Flat` whose content is forced (rule 2) is never accepted, and the last alternative of a `Choice` never contains a `Flat`.

### 14.3.3 Width

1. The **width** of a text is its number of Unicode scalar values. Every scalar value counts as one column, including wide East Asian characters, combining marks, and TAB inside comments and strings (N-H-45).
2. The **column** of a position is the width of the text before it on its line, plus one. The maximum line width is 100: a line fits if its width is at most 100.
3. Indentation consists of spaces only, 4 per level (D-TOOL-04).

Rationale for rule 1: display width depends on the Unicode version, the terminal, and the font. A count of scalar values is the same everywhere, which F1 requires. Identifiers in public names are ASCII anyway (D-LEX-03), so the difference only matters in comments, strings, and private names.

### 14.3.4 Printer

The printer is the algorithm of Wadler's "prettier printer", in the iterative form used by Prettier, with the extensions of §14.3.2. It is normative because F1 requires byte-identical output. The state is an output buffer, the current column, and a stack of commands `(indent, mode, doc)`, where `mode` is `Flat` or `Break`. The stack starts with `(0, Break, root)`.

1. Pop a command. Depending on the doc:
   - `Text(s)`: append `s`; add its width to the column.
   - `Verbatim(s)`: append `s`; the column becomes the width of the text after the last line break of `s` (or increases by the width of `s` if it has none). A line break in `s` flushes pending line suffixes first (rule 3).
   - `Concat(ds)`: push the elements in reverse order with the same indent and mode.
   - `Indent(d)`: push `(indent + 4, mode, d)`.
   - `Group(g, d)`: if `g` is forced, push `(indent, Break, d)` and record `g` as broken, whatever the mode: a forced group breaks even inside a flat one. Otherwise, if `mode` is `Flat`, push `(indent, Flat, d)` and record `g` as flat. Otherwise, if `fits(d, rest, 101 - column)` (rule 2), push `(indent, Flat, d)` and record `g` as flat; else push `(indent, Break, d)` and record `g` as broken.
   - `IfBroken(g, a, b)`: push `a` if `g` is recorded as broken, otherwise `b`, with the same indent and mode.
   - `Line` / `SoftLine` in `Flat` mode: append a space (`Line`) or nothing (`SoftLine`).
   - `Line` / `SoftLine` in `Break` mode, and `HardLine` in any mode: emit a line break (rule 3).
   - `LineSuffix(d)`: append `(indent, mode, d)` to the pending line suffixes.
   - `Flat(d)`: push `(indent, Flat, d)`. Forced groups inside `d` still break (see `Group`), which is why an alternative with a forced `Flat` is rejected.
   - `Choice(alts)`: for each alternative in order, render it speculatively from the current state (a copy of the buffer position, column, stack, and pending suffixes). The **first line** of the alternative is the output line that contains the first `Text` or `Verbatim` of the alternative; line breaks emitted before it (a leading `HardLine`) do not end it. Rendering stops at the first line break emitted after that text, or at the end of the alternative; in the second case the rest of the line is measured as in `fits` over the remaining commands. The alternative **fits** if its first line, so measured, is at most 100 columns wide and it contains no `Flat` with forced content. Accept the first alternative that fits; if none fits, accept the last. Nested choices inside the speculative rendering are decided by the same rule. Then continue with the accepted alternative as if it had been pushed.
2. `fits(d, rest, w)` decides whether `d` printed flat, followed by the text of `rest`, fits in `w` columns before the next line break. It walks `d` in `Flat` mode and then the commands of `rest` in their own modes, subtracting widths from `w`. It returns false as soon as `w` becomes negative. It returns true when it reaches a `Line` or `SoftLine` of `rest` in `Break` mode, a `HardLine`, a `Verbatim` with a line break, or the end of the document. A `Choice`, in `d` or in `rest`, is measured by its first alternative, with one exception: a `Choice` in `rest` that has an alternative starting with a line break (the right-hand sides of §14.9.7) ends the measure there, like a `Line` in `Break` mode, because the printer can always move what follows to the next line. A `LineSuffix` counts zero.
3. **Line break.** Before the line break, the pending line suffixes are pushed back onto the stack (in the order they were added), followed by the line break itself, so that they are printed first. Then trailing spaces are removed from the buffer, LF is appended, and the column becomes 1. Indentation (`indent` spaces) is not written immediately: it is written before the next `Text` or `Verbatim` on the new line. A line that receives no text therefore stays empty, and no line ever has trailing whitespace outside string literals (§14.4.3).
4. **Blank lines.** A `HardLine` that directly follows another line break, with no text in between, produces an empty line. The builder emits at most one such extra `HardLine` (§14.6.4), so the output never has two consecutive empty lines.
5. **End of file.** After the root is printed, trailing line breaks are removed from the buffer and exactly one LF is appended (D-TOOL-04). An input that contains no tokens and no comments produces the empty output (zero bytes); that is the only canonical file without a trailing newline.

The measure in `fits` is the classical one: a group is flat if its flat form, together with whatever must follow it on the same line, fits. This is why `fn f(a: I32) {` breaks its parameter list when ` {` would overflow, and why a call stays flat only if its closing `)` and any following `?` fit too.

### 14.3.5 Groups decide outermost first

A broken group does not force its children to break. Each child group is decided independently when the printer reaches it, against the column at that point. Outer groups are therefore decided before inner ones: the printer breaks the outermost group that does not fit, and keeps every inner group flat if it then fits. All layout rules of §14.7 to §14.10 rely on this order. For example, a call whose arguments do not fit is broken one argument per line before any argument is broken internally, and an argument that then fits on its own line stays flat.

### 14.3.6 Format units

Some tools format only part of a file: structured fixes ([11-diagnostics.md](11-diagnostics.md) §11.7.3, N-H-10), structured edits ([12-semantic-tooling.md](12-semantic-tooling.md) §12.6.2), and range formatting in the language server ([12-semantic-tooling.md](12-semantic-tooling.md) §12.10). They format a **format unit**.

1. The format units are the nodes of the following kinds (impl/02 §2.6.5):
   - **items**: every item and every member item, at any depth: `USE_DECL`, `FN_DECL`, `TYPE_DECL`, `ENUM_DECL`, `DISTINCT_DECL`, `RESOURCE_DECL`, `TRAIT_DECL`, `IMPL_DECL`, `CONST_DECL`, `INVARIANT_DECL`, `EFFECT_DECL`, `INTERFACE_DECL`, `IFACE_RESOURCE`, `WORLD_DECL`, `WORLD_IMPORT`, `WORLD_EXPORT`, `WORLD_INCLUDE`, `EXTERN_BLOCK`, `TEST_DECL`, `PROPERTY_DECL`, `ASSOC_TYPE_DECL`, and `ASSOC_TYPE_DEF`;
   - **fields** (`FIELD_DECL`) and **variants** (`VARIANT_DECL`);
   - **statements** (`LET_STMT`, `VAR_STMT`, `ASSIGN_STMT`, `ASSERT_STMT`, `EXPR_STMT`);
   - **match arms** (`MATCH_ARM`).
2. A unit is **line-isolated** in a text if its first token is the first token of its line and its last token (with its trailing comments) is the last of its line. Items and the statements of a broken block are always line-isolated. Fields, variants, and match arms are line-isolated if and only if their list is broken. A statement of a flat closure body (§14.8.1) is not.
3. **Unit formatting** of a range `R` of a file `F`:
   1. Let `U` be the smallest format unit that contains `R`. If there is none, `U` is the whole file.
   2. Format `U` with the indentation that the canonical layout gives its position: 4 columns per enclosing broken block, broken list, or item body.
   3. If `U` is not line-isolated in the canonical form of `F`, or if the formatted `U` changes a layout decision of an enclosing node (a list that was flat would break, a list that was broken would fit on one line, or a `Choice` would choose differently), replace `U` by the next enclosing format unit and repeat from step 2.
   4. The result replaces the full range of `U`, including the line breaks and blank lines that separate it from its neighbors, recomputed by §14.6.4.
4. **Guarantee.** If `F` is canonical outside `R`, unit formatting produces exactly the text that formatting the whole file would produce (N-H-60). A fix applied to a canonical file therefore leaves it canonical, and changes no text outside `U`.

---

## 14.4 Tokens and spacing

### 14.4.1 Token text

Every token is printed with its source text, except:

1. **Integer literals.** The hexadecimal digits `a`–`f` of a `0x` literal are printed in uppercase (spec/02 §2.6.1 rule 3): `0xff_ff` becomes `0xFF_FF`. Prefixes are lowercase already (uppercase prefixes are lexical errors). Digit separators are kept as written.
2. **Float literals.** The exponent marker `E` is printed as `e` (N-H-57): `1.5E-3` becomes `1.5e-3`. Everything else is kept, including an explicit `+` in the exponent.
3. **String literals**: see §14.4.3.
4. Raw identifiers are kept as written, including an unnecessary `r#` (the lint `UNNECESSARY_RAW_IDENT` has its own fix).
5. `owned` is kept (spec/03 §3.15).
6. Format strings (`FSTRING`) are printed verbatim. The expressions inside their interpolations are not reformatted (they cannot contain line breaks, spec/02 §2.6.5).
7. Composite operators built from `GT` tokens (`>=`, `>>`, `>>=`, spec/02 §2.8) are printed as one operator without inner spaces. Two `GT` tokens that close two generic lists are printed adjacent (`Vec<Vec<I32>>`). A `GT` that closes a generic list is followed by the spacing of the next token, never joined to a following `=`: `let v: Vec<I32> = x`.

### 14.4.2 Spacing

Between two tokens printed on the same line, the printer writes either nothing or exactly one space. The rules are applied in order; the first rule that matches decides.

| # | Context | Spacing | Example |
|---|---|---|---|
| 1 | after `(` or `[`, before `)` or `]` | none | `f(a)`, `[1, 2]`, `()` |
| 2 | after `{` / before `}` of a flat non-empty `{ }` list or flat closure body | one space | `{ a, b }`, `fn(x) { x + 1 }` |
| 3 | empty `{ }` | none | `{}` |
| 4 | before `,`; before `?`; before `:` | none | `a, b`, `f()?`, `x: I32` |
| 5 | after `,`; after `:` (except in rule 13) | one space | `(a, b)`, `x: I32` |
| 6 | around `.` of member access, paths, and `use … .{` | none | `a.b`, `http.Status.Ok`, `client.{A}` |
| 7 | the `.` of an implicit member (`.Pending`, `.Err(e)`) | one space before it, unless rule 1 or 9 applies | `x == .Pending`, `Some(.Ok)` |
| 8 | before `(` of a call, a constructor, a parameter list (other than a property's, rule 15), a tuple or record variant shape, a function pointer type, a closure, an attribute argument list, `FnSugar`, or a constructor pattern | none | `f(x)`, `fn add(a: I32)`, `fn(I32)`, `@derive(Copy)`, `Some(x)` |
| 9 | before `[` of an index; before `<` of generic parameters or arguments; after that `<`; before its `>` | none | `a[i]`, `Vec<I32>`, `fn f<T>(` |
| 10 | after a prefix operator `-`, `~`, `*`; after the `@` of an attribute; after `'label` before `:` | none | `-x`, `*p`, `@inline`, `'outer: loop` |
| 11 | around a range operator `..` or `..=`, and after a prefix `..` (rest, record update, open range) | none | `0..n`, `a..=b`, `..base`, `[first, ..]` |
| 12 | around a binary operator (§14.9.2), an assignment operator, `=`, `->`, `=>`, `+` of bounds, `|` of patterns, the `@` of a binding pattern, and `as` | one space | `a + b`, `x = 1`, `-> I32`, `T: A + B`, `n @ 1..=9` |
| 13 | after `UseItems`' `{` and before its `}` | none (N-H-53) | `use sai:core/fmt.{FormatSpec, Writer as Sink}` |
| 14 | after a keyword or contextual keyword, before the next token on the line | one space | `let x`, `borrow mut T`, `not done`, `effects { io }` |
| 15 | between a `STRING` and the `(` of a property's parameter list | one space | `property "reverse twice" (xs: Vec<I32>)` |
| 16 | before `{` that opens a block, a body, or a `{ }` list (except rule 6) | one space | `if x {`, `type P { x: F64 }`, `effects {}` |
| 17 | every other pair | one space | `match x`, `pub fn`, `x as y` |

Rules 10 and 11 make `- x` and `a .. b` canonical as `-x` and `a..b`. A prefix operator followed by another prefix operator is written without a space (`--x`, `-*p`), except that a keyword prefix (`not`, `borrow`, `borrow mut`) is always followed by one space. The sequence `- -` after a prefix `-` is printed `--`; `--` is not a token of SAI, so this does not change the token sequence.

### 14.4.3 String literals

1. **Simple strings** without line continuations, raw strings, byte strings, character and byte literals are printed verbatim.
2. **Line continuations.** In a simple string, `\` followed by a line break removes the line break and the leading whitespace of the next line (spec/02 §2.6.4). The formatter prints the string with each continuation line indented to `I + 4` columns, where `I` is the indentation of the line on which the string starts. The rest of each line is kept. The value is unchanged, because the removed whitespace never was part of it (N-H-56).
3. **Multiline strings** (`"""`). Let `B` be the base indentation of the input (the whitespace before the closing `"""`, spec/02 §2.6.4 rule 2) and `I` the indentation of the output line on which the opening `"""` appears. The formatter prints:
   - the opening `"""` and the rest of its line as written;
   - each content line: if it is empty, or consists of exactly `B`, as an empty line; otherwise as `I + 4` spaces followed by the line with its `B` prefix removed;
   - the closing line as `I + 4` spaces followed by `"""`.
   Every non-empty content line starts with `B` (otherwise the lexer reports `INCONSISTENT_INDENT` and the file is refused), so the value is unchanged (N-H-56). Whitespace after the base indentation, including trailing whitespace, is part of the value and is kept.
4. Raw strings that contain line breaks are printed verbatim (a `Verbatim` doc): every character of a raw string is part of its value. Their lines are exempt from the trailing-whitespace rule (§14.3.4 rule 3) and from F7.
5. A string literal that does not fit within the width is not split. It overflows (§14.5.6).

Example: the multiline string below is reindented when its statement moves into a block, without changing its value.

```sai
fn usage() -> Str {
    let text = """
        usage: tool <file>

          -v  verbose
        """
    text
}
```

---

## 14.5 Line breaking

### 14.5.1 Break safety

A line break between two tokens is **safe** if the lexer and parser read the program the same way with and without it. The builder creates break opportunities (`Line`, `SoftLine`, `HardLine`) only at safe positions. With the sets E and C of spec/02 §2.10 (D-LEX-01), a line break between a token `a` and the next token `b` is safe if at least one of the following holds:

| # | Condition | Typical use |
|---|---|---|
| S1 | the innermost open bracket at `a` is `(` or `[`, so no NEWLINE is generated | argument, parameter, tuple, and array lists; inserted parentheses (§14.9.3) |
| S2 | `a` is not in E, so no NEWLINE is generated | after `,`, `=`, `->`, `=>`, a binary operator, `<`, `{` |
| S3 | `b` is in C (`.` followed by a lower identifier, `await`, or an integer; `and`; `or`; `else`), so no NEWLINE is generated | method chains, `} else {` across lines |
| S4 | a NEWLINE is generated and the grammar skips it at this position (`nl` in spec/03) | before a signature clause, before a function body `{`, before `->`, before a closing `}` or `>`, before `else` |
| S5 | a NEWLINE is generated and the grammar requires it here as a separator (`NL` or `Sep` in spec/03) | between statements, items, and the elements of a broken `{ }` list |

Every other position is unsafe. In particular, a break is unsafe:

- before a binary operator other than `and` and `or` outside `(` and `[` (`a` ends an operand, which is in E). This is why the formatter adds parentheses before it breaks such an expression (D-LEX-01, §14.9.3);
- before `.` followed by an upper identifier (`http.Status` or `Shape.Circle`): `.` + `UIDENT` is not in C;
- before the `{` of an `if`, `while`, `for`, `loop`, closure, `region`, `unsafe`, or `async` block, which has no `nl` (spec/03 §3.13.5);
- before `?`, before a call's `(`, and before an index's `[`.

Lemma (used in §14.6). If the input has a line break between `a` and `b`, the same break is safe in the output, provided the tokens before and after it are the same. The break was accepted in the input under one of S1 to S5, and none of the permitted changes of §14.1.3 removes a bracket, so the same condition holds. Comments therefore never force a break at an unsafe position (§14.6.2).

### 14.5.2 Continuation indentation

A **continuation line** is a line that continues the same statement, item header, or list element as the line before it. Every continuation line is indented one level (4 columns) deeper than the line on which its statement, header, or element starts. Nested constructs add their own levels. Alignment to a column of the previous line (for example under an opening parenthesis) is never used: every indentation is a multiple of 4.

### 14.5.3 Delimited lists

A **delimited list** is a comma-separated list inside `( )`, `[ ]`, or `< >`: arguments, parameters, generic parameters and arguments, tuple and array elements, attribute arguments, variant shapes, closure parameters, constructor and slice patterns, tuple types, function pointer parameters, and `FnSugar` parameters. Its document is:

```text
Group(g, [ open, Indent([SoftLine, e1, ",", Line, e2, ",", Line, …, en, IfBroken(g, ",", "")]), SoftLine, close ])
```

- **Flat**: `f(a, b, c)`. No space inside the brackets and no trailing comma.
- **Broken**: one element per line, each indented one level, each followed by a comma, and the closing bracket on its own line at the indentation of the line that holds the opening bracket.

```sai
fn connect(
    host: borrow Str,
    port: U16,
    timeout: Duration,
    retries: U32,
    on_error: fn(borrow ConnectError) -> Action,
) -> Result<Connection, ConnectError> {
    open(host, port, timeout, retries, on_error)
}
```

- Breaks in a `( )` or `[ ]` list are safe by S1. In a `< >` list (which is not on the bracket stack, spec/03 §3.1.2) the break after `<` and after `,` is safe by S2, and the break before `>` by S4 (`nl '>'`).
- An empty list is printed `()`, `[]`. An empty generic list does not exist in the grammar.
- A list is never partly broken: either all elements share one line, or each has its own line (no "fill" layout). Exception: the last-argument hugging of §14.9.6.
- A one-element tuple keeps its comma in both forms: `(x,)`.
- In a constructor pattern whose last element is `..`, no comma follows `..` (spec/03 §3.14).

### 14.5.4 Brace lists

A **brace list** is a list inside `{ }` whose separator is `Sep` (a comma or a line break, spec/03 §3.1.2, D-SYN-07): struct and resource fields, enum variants, match arms, effect sets, `borrows` lists, `covers` sets, invariant predicates, and `UseItems`. Its document is:

```text
Group(g, [ "{", Indent([Line, e1, IfBroken(g, "", ","), Line, e2, …, en]), Line, "}" ])
```

1. **Flat**: `{ a, b, c }`, with one space inside the braces and `, ` between elements, and no trailing comma (D-SYN-07). `UseItems` are printed without the inner spaces: `.{A, B}` (§14.7.2, N-H-53).
2. **Broken**: one element per line, indented one level, **without commas**, and `}` on its own line (D-SYN-07). The line breaks are safe by S5 and, after `{` and before `}`, by S4.
3. **Empty**: `{}`. Comments inside an empty list force it to break (§14.6.3).
4. A brace list is broken if it does not fit, or if any element contains a forced break: a doc comment, an attribute, a line comment, a block that is always broken (§14.8.1), or a multiline string. The field list of a record or resource declaration and the variant list of an enum declaration are always broken when they are not empty, even when they fit (§14.7.3, N-H-81).
5. **Open-ended ranges.** In a broken brace list, an element whose last token is not in E would not be terminated by its line break. The only expression that can end with such a token is a range with an omitted end (`a..`, `..`). Such an element, other than the last, is wrapped in parentheses: `.Tail => (start..)` (§14.1.3 item 2, N-H-59). Every other element ends with a token in E.

```sai
type Point {
    x: F64
    y: F64
}

enum Status {
    Pending
    Active
    Disabled(reason: Str)
}

fn describe(status: Status) -> Str {
    match status { .Pending => "pending", .Active => "active", .Disabled(..) => "disabled" }
}
```

`Point` and `Status` are broken although they fit, by rule 4. The `match` fits and is flat. With longer content, the `match` breaks one element per line:

```sai
/// A user record.
type User {
    /// Stable identifier.
    id: UserId
    name: String
    email: Option<Email>
}

fn label(status: Status) -> Str {
    match status {
        .Pending => "waiting for the first review by a maintainer"
        .Active => "active and visible to every member of the team"
        .Disabled(..) => "disabled"
    }
}
```

`type User` is broken as every non-empty record is, and the `match` is broken because it does not fit on one line.

### 14.5.5 Bodies

A **body** is a `{ }` list whose separator is `NL` only: the item lists of `trait`, `impl`, `interface`, interface `resource`, `world`, and `extern` declarations, and the statements of a block. Bodies have no comma form, so they cannot be printed flat with more than one element.

1. An item body (trait, impl, interface, interface resource, world, extern) is always broken, even with a single element. An empty one is `{}`.
2. A block is always broken, except the body of a closure and an `unsafe` block that holds a single expression (§14.8.1). An empty block is `{}`.
3. The elements of a broken body are separated by line breaks, with blank lines as in §14.6.4.

### 14.5.6 Overflow

A line is allowed to exceed 100 columns only if the printer had no way to avoid it: the line contains no break opportunity whose group is flat, or every such group is already broken. Typical causes are a long string literal, a long comment, a long path or identifier, a long `PKG_PATH`, and a long sequence without safe break positions, such as `a.B.C.D` (S3 does not apply to `.` + `UIDENT`). The formatter never splits a token, a comment, or a string to make a line fit. Overflowing lines are not errors; F7 is satisfied because no layout of the same tokens fits.

---

## 14.6 Comments and blank lines

### 14.6.1 Attachment

The formatter uses the trivia attachment of the CST without changing it (impl/02 §2.6.3, N-E-16). Every comment belongs to exactly one token:

- A **trailing comment** is in the trailing trivia of a token: it starts on the same line as the end of the token.
- A **leading comment** is in the leading trivia of a token: it starts on a line of its own (an **own-line comment**), after the previous token's line.
- Comments after the last token are leading comments of `EOF`.

A comment is printed next to its token: a trailing comment on the line where its token is printed (§14.6.2), a leading comment on its own line before the line of its token (§14.6.3). A comment moves when its token moves. In particular, the comments of a `use` declaration, a signature clause, an attribute, or a list element move with it when the formatter reorders (§14.7.2, §14.7.5).

**Tokens that disappear.** When the formatter removes a token (§14.1.3 items 1 and 3), its comments are reattached before layout:

1. The trailing comments of a removed comma become trailing comments of the last token of the element before the comma. Its leading comments become leading comments of the first token of the element after it, or of the closing bracket if there is none.
2. The comments of a removed leading `|` become leading comments of the first token of the pattern.

**Detached comments.** In the leading trivia of the first token of a format unit (§14.3.6), the comments that are separated from the unit by at least one blank line are **detached**. A detached comment stays at its position in the sequence of units when the formatter reorders units, and is never moved together with the unit that follows it. Comments directly above a unit, with no blank line in between, are **attached** to it (N-H-55). This rule keeps a license header or a section heading in place when the `use` declarations below it are sorted (§14.7.2).

### 14.6.2 Trailing comments

1. A trailing **line comment** is printed at the end of the output line on which its token is printed, separated from the last token of that line by one space. It is emitted as `LineSuffix`, which forces the enclosing group to break (§14.3.2). When the token is not the last token of its line in the chosen layout, the comment follows the last token of the line instead: in the output it ends the same line as its token, after the tokens that the layout put on that line.
2. A trailing **block comment** without a line break inside it is printed in place, separated by one space from the token before it and by one space from the token after it. There is no space between the comment and a following `,`, `)`, `]`, `}` of a flat list, or `>`. Example: `f(a /* old */, b)`.
3. A trailing block comment that contains a line break is printed in place as `Verbatim`, followed by a forced line break, which is safe by the lemma of §14.5.1. Its interior lines are not reindented.
4. Several trailing comments of one token keep their order and are separated by one space.

### 14.6.3 Own-line comments

1. The leading comments of a token `t` are printed before `t`, each comment on its own line, and `t` starts a new line. The line break before `t` is safe by the lemma of §14.5.1, because the input had a line break there. The group that contains this break is forced.
2. **Indentation.** Each comment line is indented like the line that starts with `t`. If `t` is a closing bracket (`)`, `]`, `}`, `>`), the comments are indented like the elements of the list or body it closes, one level deeper than the bracket: they are printed after the last element and before the closing line. If `t` is `EOF`, the comments are at column 1.
3. A block comment on its own line is printed as `Verbatim`. Its first line gets the indentation of rule 2. Its other lines are printed unchanged.
4. Between two consecutive own-line comments, and between the last of them and `t`, one blank line is printed if the input had at least one blank line there, and none otherwise.
5. A body, list, or block that contains only comments is broken: `{`, the comments indented one level, `}`.

```sai
fn total(items: borrow Vec<Item>) -> I64 {
    // Sum in a fixed order so that the result is reproducible.
    var sum: I64 = 0
    for item in items.iter() {
        sum += item.price // prices are in cents
    }

    // An empty list costs nothing.
    sum
}
```

### 14.6.4 Blank lines

Blank lines are the only part of the input layout, besides comments, that the output keeps (F6). They are kept only between format units, never inside one (N-H-54).

1. **Between units.** Between two consecutive units of the same body, block, broken list, or file, the output has one blank line if the input had at least one blank line between them (in the leading trivia of the second unit's first token, before its attached comments), and none otherwise. Two or more blank lines become one.
2. **Never at the edges.** No blank line follows an opening `{`, precedes a closing `}`, starts the file, or ends the file. Blank lines inside a unit (between clauses, between the lines of a broken argument list, inside an expression) are removed.
3. **Top-level items.** In the item list of a file, rule 1 applies, and in addition exactly one blank line separates two consecutive items if either of them is printed on more than one line, including its docs and attributes, unless both are `use` declarations (rule 5). Single-line items, such as a group of `const` declarations or `distinct` types, may stay together.
4. **Module header.** The parts of the file start are printed in this order, each separated from the next by exactly one blank line: the shebang line, the file header comments (detached comments before the first token, §14.6.1), the module docs (`//!`), the `unsafe module` declaration, the `use` block (§14.7.2), and the other items. A part that is absent is skipped together with its blank line. As an exception, a shebang line is followed directly by the next line if the input had no blank line after it.
5. **Use block.** No blank line separates two declarations of the same group of the `use` block, and exactly one blank line separates the `use` group from the `pub use` group (§14.7.2).
6. **Docs.** Blank lines between the lines of a `DOCS` node, and between docs, attributes, and the declaration they belong to, are removed.
7. **End of file.** The comments of `EOF` follow the last item. They are separated from it by one blank line if the input had one, and the file ends with exactly one LF (§14.3.4 rule 5).

Blank lines never force a break. A list that is printed flat loses the blank lines between its elements.

### 14.6.5 Doc comments, module header, and shebang

1. **Outer docs** (`///`, a `DOCS` node, impl/02 §2.6.4) are printed one per line, at the indentation of the declaration they document, directly before its attributes. Their text is kept, with trailing whitespace removed. Ordinary comments inside a `DOCS` node keep their place among the doc lines.
2. A declaration with docs is never printed on the same line as another element: a field or variant with docs forces its brace list to break (§14.5.4 rule 4).
3. **Module docs** (`//!`, the `DOCS` child of `MODULE_HEADER`) are printed at column 1, one per line, in the position given by §14.6.4 rule 4.
4. `unsafe module` is printed on its own line at column 1.
5. **Demoted doc comments** (`MISPLACED_DOC_COMMENT`, impl/02 §2.6.4) are ordinary line comments for the formatter. Their text, including the `///` or `//!`, is kept.
6. **Shebang.** The shebang line is printed first, verbatim, with trailing whitespace removed.
7. **Trailing whitespace** is removed from every line of every comment, including the inner lines of block comments, and from the shebang line. No other change is made to comment text: the formatter does not add a space after `//`, rewrap text, or change the style of a comment.

---

## 14.7 Items

### 14.7.1 Common structure

Every item is printed as:

```text
docs            one line per doc comment (§14.6.5)
attributes      one line per attribute (spec/15 §15.2.1 rule 1)
header body     the item itself, starting with its visibility and keywords
```

1. **Attributes.** Each attribute is printed on its own line at the item's indentation, in the order written. Its arguments are a delimited list (§14.5.3): `@derive(Copy, Clone, PartialEq)`. An attribute argument `name = value` has one space on each side of `=`.
2. **Visibility and modifiers** are printed in the order of the grammar, separated by one space: `pub unsafe async fn`, `export fn`, `pub distinct`, `unsafe impl`, `unsafe extern "C"`.
3. An item that the grammar allows to end without a body (a trait function, an interface function, an extern function, a resource without fields, an effect without `covers`) is printed without one.
4. Units inside item bodies (trait, impl, interface, resource, world, and extern members) follow §14.6.4 rule 1: their blank lines are kept, at most one.

### 14.7.2 Use declarations

`use` declarations are items whose order has no meaning (spec/04 §4.3.1). The formatter moves them to the top of the file and sorts them (spec/04 §4.3.1, N-H-53).

1. **Collection.** Every `USE_DECL` of the file's item list is removed from its position, together with its docs, attributes, and attached comments (§14.6.1). Detached comments stay where they were.
2. **Groups.** The declarations form two groups: those without `pub` first, then the `pub use` re-exports.
3. **Order.** Within a group, declarations are sorted by their **sort key**: the canonical one-line text of the declaration (§14.13) without `pub`, docs, attributes, and comments, compared as byte strings. Declarations with equal keys keep their input order (a stable sort). Duplicate declarations are not merged: duplicates are an error of name resolution, not of formatting.
4. **Items.** The elements of every `UseItems` list are sorted by the byte string of the original name, then by the alias (an element without an alias first). Their comments move with them. Sorting happens before the sort keys of rule 3 are computed.
5. **Placement.** The use block is printed after the module header (§14.6.4 rule 4) and before every other item. Detached comments that were among the `use` declarations are printed, in their input order, before the use block.
6. **Layout.** A declaration is printed on one line if it fits. Otherwise its `UseItems` list is broken, one item per line, without commas (§14.5.4). `pub use` keeps its `UseItems` list (spec/03 §3.4).
7. `UseItems` are printed without spaces inside the braces when flat: `.{FormatSpec, Writer as Sink}` (N-H-53). This follows the uniform usage of spec/03, spec/04, and spec/08. The `{ a, b }` spacing of D-SYN-07 applies to all other brace lists.

Because the byte order of ASCII puts uppercase letters before lowercase letters, `.{Order, order}` and `.{User, fetch_user}` are already sorted.

Input:

```sai noncanonical
// Copyright 2026 ACME Corp.

use sai:http/client.{HttpError, HttpClient}

fn helper() {}

pub use acme:app/model/user.{UserId, User}
use acme:image/model.{Image}
```

Output:

```sai
// Copyright 2026 ACME Corp.

use acme:image/model.{Image}
use sai:http/client.{HttpClient, HttpError}

pub use acme:app/model/user.{User, UserId}

fn helper() {}
```

### 14.7.3 Type declarations

1. **Records**: `type Name<G> { fields }`, where the field list is a brace list (§14.5.4) that is always broken unless it is empty (`type Marker {}`, §14.5.4 rule 4). A field is `docs attributes pub? name: Type`. A where clause, if present, is printed on its own line indented one level and laid out as in §14.7.5 rule 4 (safe by S4, `nl WhereClause`). The `{` then starts its own line at the indentation of the item (`nl '{'`), and the field list is broken. Enums, traits, and impls with a where clause are printed the same way.
2. **Aliases and refined types**: `type Name<G> = Type` and `type Name = Type where predicate`. The right-hand side follows the right-hand-side rule of §14.9.7. If the refined type does not fit on one line, the refinement clause is printed on a continuation line (safe by S4, `nl RefinementClause`):

   ```sai
   type Percentage = I32 where self >= 0 and self <= 100

   type TrimmedLabel = Str
       where self.len() > 0 and not self.starts_with(" ") and not self.ends_with(" ")
   ```

3. **Enums**: `enum Name<G> { variants }` with a brace list of variants, always broken unless it is empty. A variant is `docs attributes Name`, `Name(T1, T2)`, `Name(field: T)`, optionally followed by ` = discriminant`. Variant shapes are delimited lists.
4. **Distinct types**: `distinct Name = Type` or `distinct Name = pub Type`, optionally followed by a refinement clause as in rule 2. The right-hand side follows §14.9.7 as for aliases; the grammar has no `nl` after this `=`, but a break after `=` is safe by S2.
5. **Resources**: `resource Name` without fields, or `resource Name { fields }` with a brace list, always broken.

### 14.7.4 Function signatures

A function is printed as **head**, **clauses**, and **body**.

1. **Head**: `visibility unsafe async fn name<generics>(parameters) -> Ret`. The generic parameter list and the parameter list are delimited lists (§14.5.3), and `-> Ret` follows the closing `)` on the same line. The head is one group: if it does not fit, the parameter list breaks first (§14.3.5); the generic parameter list breaks only if the line `fn name<…>(` alone does not fit.
2. **Parameters**: `name: Type`, `_: Type`, `var name: Type`, `var _: Type`, `self: Type`, or `var self: Type`. A receiver's `Type` is `Self`, `borrow Self`, or `borrow mut Self` (spec/05 §5.8.2). Parameters have no other forms (D-SYN-06).
3. **Return type**: printed after `) -> `. A return type that does not fit is broken inside its own generic arguments. The `->` never starts a line, although the grammar would allow it (`nl '->'`), so that the return type is always found on the line of the closing `)`.
4. **Body**: if the function has no clauses, the body's `{` follows the head on the same line, after one space. If it has at least one clause, the `{` is printed on its own line at the indentation of the item (safe by S4, `FnBody ::= nl Block`), so that the clauses are visually separated from the first statement (N-H-48).
5. A function without a body (trait, interface, and extern functions) ends after its head or its last clause.

```sai
pub fn width(image: borrow Image) -> U32 {
    image.width
}

fn withdraw(balance: Money, amount: Money) -> Money
    requires amount <= balance
    ensures result >= Money(0)
{
    balance - amount
}
```

### 14.7.5 Signature clauses

1. **Order.** Clauses are printed in the order of D-SYN-06: `where`, `borrows`, `effects`, `requires`*, `ensures`*. If the input has them in another order (`CLAUSE_ORDER`, §14.2.2), the formatter moves each clause, with its comments, into that order. Several `requires` clauses keep their relative order, and so do several `ensures` clauses.
2. **One clause per line.** Each clause starts its own line, indented one level relative to the item (safe by S4, `nl` before every clause in `FnClauses`). This holds even when the whole signature would fit on one line (N-H-48): clauses are part of the contract of a function and are read one by one.
3. **Effect sets and `borrows` lists** are brace lists (§14.5.4): `effects { alloc, net.http }` when it fits, otherwise one effect per line. The formatter **never reorders effects** or provenances (N-H-11 of [11-diagnostics.md](11-diagnostics.md)): their written order carries no meaning (spec/08), so reordering them is not formatting. An empty set is `effects {}`.
4. **Where clauses.** `where T: Format, U: Clone + Hash` on one line if it fits. Otherwise `where` stays on the clause line and each predicate is printed on its own continuation line, indented one level deeper than `where`, followed by `,` except the last (the grammar has no trailing comma after a where predicate, spec/03 §3.5). The breaks after `where` and after `,` are safe by S2.
5. **`requires` and `ensures`.** `requires expr` with the expression laid out by §14.9. The expression always starts on the clause line: `requires` and `ensures` are contextual keywords, lexed as `LIDENT` (in E), so a line break after them would end the clause. A predicate that does not fit is broken by the rules for its expression, for example wrapped in parentheses (§14.9.3).
6. **Bounds.** A bound list `A + B + C` that does not fit is broken after a `+` (safe by S2), with the continuation indented one level. Bound lists are never wrapped in parentheses: the grammar allows parentheses only around a single where bound (spec/03 §3.5 `WhereBound`).

```sai
pub async fn fetch_user(http: borrow HttpClient, id: UserId) -> Result<User, UserError>
    effects { alloc, net.http }
{
    let path = "/users/" + id.value.to_string().as_str()
    let response = http.get(path.as_str()).await.map_err(fn(_) { UserError.Network })?
    if response.status == 404 {
        return Err(.NotFound)
    }
    response.json<User>().map_err(fn(_) { UserError.InvalidResponse })
}

fn merge<K, V>(left: Map<K, V>, right: Map<K, V>) -> Map<K, V>
    where
        K: Hash + Eq + Clone + Format + Default + PartialOrd + Ord + ToString + Arbitrary,
        V: Clone + Format
    effects { alloc }
{
    left.union(right)
}
```

The second example breaks its where clause because `where K: … , V: Clone + Format` is longer than 96 columns.

### 14.7.6 Other items

| Item | Canonical form |
|---|---|
| trait | `pub? unsafe? trait Name<G>: Bounds {` + member body (§14.5.5) + `}`. A where clause goes on its own continuation line after the header, and `{` then starts its own line, as in §14.7.4 rule 4. |
| impl | `unsafe? impl<G> Trait for Type {`, `impl<G> Type {`, `impl iface {` + member body + `}`. A where clause is handled as for traits. |
| associated type | `type Item: Bounds` (declaration), `type Item = Type` (definition). |
| const | `const NAME: Type = expr`. The initializer follows §14.9.7. |
| invariant | `invariant Name { p1, p2 }`, a brace list of predicates (§14.5.4). |
| effect | `effect name` or `effect name covers { a, b }`. The `covers` set is a brace list. |
| interface | `pub? interface name {` + member body + `}`. Members are type declarations, enums, interface resources, and function heads. |
| interface resource | `resource Name` or `resource Name {` + member body of function heads + `}`. |
| world | `world name {` + one world item per line + `}`: `import wasi:http/outgoing-handler`, `export users`, `include base`, `import fn now() -> U64`. |
| extern block | `unsafe? extern "abi" module "name" {` + member body of function heads + `}`. |
| test | `test "name" {` + block + `}`. |
| property | `property "name" (params) {` + block + `}`, with one space between the name and `(` (§14.4.2 rule 15). |

Member bodies are always broken, even when they hold one member (§14.5.5 rule 1). An empty member body is `{}`. Members are separated by line breaks, with the blank lines kept as in §14.6.4 rule 1.

```sai
use sai:core/fmt.{FormatSpec, Writer}

interface processor {
    enum ImageError {
        TooLarge
        Unsupported
    }

    fn resize(image: Bytes, width: U32, height: U32) -> Result<Bytes, ImageError>
}

world client {
    import wasi:http/outgoing-handler
    export users
}

impl Format for UserId {
    fn format<W: Writer>(self: borrow Self, out: borrow mut W, spec: FormatSpec) {
        self.value.format(out, spec)
    }
}

property "reverse twice" (xs: Vec<I32>) {
    reverse(reverse(xs.clone())) == xs
}
```

---

## 14.8 Statements and blocks

### 14.8.1 Blocks

1. A block is printed broken: `{`, one statement per line indented one level, `}` on its own line (§14.5.5 rule 2). This applies to function, test, and property bodies and to the blocks of `if`, `else`, `loop`, `while`, `for`, `match` arm bodies, `unsafe`, `async`, `region`, and block expressions, whatever their length (N-H-47), with the exceptions of rules 2 and 3.
2. **Closure bodies** are the exception. A closure body is printed flat, `fn(x) { x + 1 }`, if it consists of exactly one expression statement, contains no comments, and fits. Otherwise it is broken like any other block.
3. **Single-expression `unsafe` blocks** follow the closure rule: `unsafe { slot.assume_init() }` is printed flat if the block consists of exactly one expression statement, contains no comments, and fits (N-H-47). An `unsafe` block with statements is broken.
4. An empty block is `{}`, also for closures: `fn() {}`.

Rationale: a statement ends at a line break (D-LEX-01), so statement-oriented code reads one statement per line. A closure with a single expression is an expression, and a flat closure keeps chains like `items.map(fn(x) { x.id })` on one line. A single-expression `unsafe` block marks one operation, and a flat block keeps the unsafe region as narrow as that operation, on the line that uses it ([spec/12](../spec/12-unsafe-and-ffi.md) §12.2).

### 14.8.2 Statement forms

| Statement | Canonical form |
|---|---|
| `let` | `let pattern: Type = expr`; with an else block, `let pattern = expr else {` + block + `}` |
| `var` | `var name: Type = expr` |
| assignment | `place op expr`, with one space around the operator (`x += 1`) |
| `assert` | `assert expr` or `assert expr, message` |
| expression | the expression (§14.9) |

The right-hand side of `=` in `let`, `var`, and assignments follows §14.9.7. A `let … else` keeps `else {` on the line where the initializer ends; the else block is a block (§14.8.1).

```sai
fn first_word(text: Str) -> Str {
    let Some(end) = text.find(" ") else {
        return text
    }
    text.slice(0, end)
}
```

### 14.8.3 Statement separation

Statements of a broken block are separated by exactly one line break, plus at most one blank line where the input has one (§14.6.4 rule 1). The line break after a statement is the `NL` separator of `Block` (spec/03 §3.12); it is a NEWLINE because every statement ends with a token in E, with one exception: a statement that ends with an open range (`let r = start..`). Such a statement is always the last of its block, because a line break after `..` emits no NEWLINE: a statement after it would either become the end of the range or be a syntax error. Its line break is followed by `}`, which is safe (S4, `nl '}'`). No statement starts with a token of C: an expression cannot start with `.` followed by a `LIDENT`, `await`, or `INT` (an implicit member is `.` followed by a `UIDENT`), nor with `and`, `or`, or `else`.

---

## 14.9 Expressions

### 14.9.1 General rules

1. An expression is printed flat if it fits. Otherwise the outermost breakable construct breaks first (§14.3.5), and its parts are decided again at their new positions.
2. The breakable constructs are: delimited lists (§14.5.3: call arguments, tuples, arrays, generic arguments, closure parameters), brace lists (§14.5.4: match arms), operator chains (§14.9.3), member chains (§14.9.5), the right-hand sides of §14.9.7, and blocks (§14.8.1).
3. Prefix operators (`-`, `~`, `*`, `borrow`, `borrow mut`, `not`), postfix operators (`?`, `.await`, `.0`, indexing), names, literals, and paths are never broken internally. `borrow mut x` is printed with single spaces, `not x` with one space, and `-x`, `~x`, `*x` with none (§14.4.2).
4. Parentheses written by the author are kept, even when they are redundant (§14.1.3 item 2). The formatter inserts parentheses only around broken operator chains (§14.9.3) and open ranges in broken brace lists (§14.5.4 rule 5).

### 14.9.2 Operator chains

An **operator chain** is a maximal sequence `e0 op1 e1 … opn en` (n ≥ 1) of operands joined by binary operators of one precedence level of spec/03 §3.13.1:

| Level | Operators | Operands |
|---|---|---|
| or | `or` | any number |
| and | `and` | any number |
| comparison | `==` `!=` `<` `<=` `>` `>=` | exactly two (non-associative) |
| range | `..` `..=` | at most two (non-associative) |
| bit or | `\|` | any number |
| bit xor | `^` | any number |
| bit and | `&` | any number |
| shift | `<<` `>>` | any number |
| additive | `+` `-` `+%` `-%` | any number |
| multiplicative | `*` `/` `%` `*%` | any number |

An operand is any expression of a tighter level, including a nested chain. `a * b + c * d` is an additive chain with two operands, each a multiplicative chain. Because chains are left-associative and the levels are fixed by the grammar, the chain structure is a property of the tree and does not depend on the layout.

**Ranges** are not break points. A range is printed without spaces (`start..end`, `..=limit`), and only its operands can break.

### 14.9.3 Breaking operator chains

A chain that does not fit is broken **before every operator** of the chain: each operator starts a new line, followed by one space and its right operand. A chain is broken as a whole or not at all, like a list (§14.5.3). The operands are then decided independently (§14.3.5).

1. **Protected chains.** The line break before an operator such as `+` is safe only inside `( )` or `[ ]` (S1): in a block, the operand before the break ends with a token in E, and the break would end the statement. The breaks before `and` and `or` are safe everywhere (S3), but they are treated the same way, so that conditions read alike. A chain is **protected** if its innermost enclosing bracket in the output is `(` or `[`. This includes call arguments, tuple and array elements, index expressions, and the content of written or inserted parentheses.
2. **Inserted parentheses.** A chain that is not protected is wrapped in parentheses when, and only when, it breaks. The parentheses are part of the chain's group: `IfBroken(g, "(", "")` before the chain and `IfBroken(g, ")", "")` after it (§14.1.3 item 2). They are never inserted around a chain that is printed flat.
3. **Parenthesized layout.** A broken chain whose parentheses are inserted, or that is the whole content of a parenthesized expression written by the author, is printed as `(` at the end of the current line, the operands one level deeper with each operator at the start of its line, and `)` on its own line at the indentation of the line where the chain started:

   ```text
   Group(g, [ "(", Indent([SoftLine, e0, Line, op1, " ", e1, …, Line, opn, " ", en]), SoftLine, ")" ])
   ```

4. **Indented layout.** Any other broken chain, that is, a protected chain that is not the whole content of parentheses, is printed with its continuation lines one level deeper than the line where the chain starts:

   ```text
   Group(g, [ e0, Indent([Line, op1, " ", e1, …, Line, opn, " ", en]) ])
   ```

5. A chain nested as an operand inside a broken chain is protected by the outer chain's parentheses or brackets, so it uses the indented layout.
6. The closing `)` of rule 3 is followed by whatever follows the chain: ` {` after a condition, `?`, `.method()`, or the end of the statement.

These rules are N-H-49. Breaking before the operator puts the operator where a reader scanning the left edge sees it, and it is the only placement that is safe for `and`, `or`, and `else` without parentheses (D-LEX-01). Inserting parentheses is the only token change that makes the other breaks safe without a line continuation `\`, which the formatter never inserts.

```sai
fn can_publish(user: borrow User, post: borrow Post) -> Bool {
    if (
        user.is_active()
        and user.email_verified
        and not post.is_locked()
        and (user.id == post.author or user.role == Role.Moderator)
    ) {
        return true
    }
    false
}

fn total_price(order: borrow Order) -> I64 {
    let subtotal =
        order.base_price * order.quantity + order.shipping_cost + order.handling_fee + order.tax
    let discounted = apply_discount(
        subtotal,
        order.coupon_value * order.coupon_multiplier
            + order.loyalty_points * LOYALTY_RATE
            + order.seasonal_bonus,
    )
    discounted
}
```

In the first function, the condition does not fit on the `if` line, so it is wrapped in inserted parentheses. Its last operand is a parenthesized chain written by the author, which fits and stays flat. In the second, the initializer of `subtotal` fits on the next line and moves there (§14.9.7); the second argument of `apply_discount` is a protected chain and uses the indented layout.

### 14.9.4 Postfix and primary expressions

1. **Calls**: `f(args)`, `x.f(args)`, `f<T>(args)`, `.Variant(args)`. The argument list is a delimited list (§14.5.3). A named argument is `name: value`. A record update argument `..base` is last, and it gets no trailing comma when the list is broken, for the same reason as the `..` of a constructor pattern: it closes the list rather than adding an element (§14.1.3 item 1).
2. **Generic arguments** in expressions: `json<User>()`, `Vec<I32>.new()`. The list is a delimited list. Composite `>` tokens are joined (§14.4.1): `collect<Vec<String>>()`.
3. **Indexing**: `a[i]`. The index expression is protected (§14.9.3 rule 1) and may break internally, but no line break is ever placed directly after `[` or before `]`.
4. **`?`, `.await`, `.0`** attach to the expression before them without spaces.
5. **Tuples and arrays** are delimited lists: `(a, b)`, `(x,)`, `()`, `[1, 2, 3]`, `[]`.
6. **Implicit members**: `.Variant`, `.Variant(payload)`, with a space before the `.` when it follows another token on the line (§14.4.2).
7. **Blocks as expressions**: `unsafe {`, `async {`, `async move {`, `region arena {`, and a plain `{` are blocks (§14.8.1), always broken, except a single-expression `unsafe` block (§14.8.1 rule 3).

### 14.9.5 Member chains

A **member chain** is a postfix expression whose postfix operations include at least two calls. It consists of:

- the **head**: the primary expression and every following `.name` operation that is not followed by call arguments, up to the first call. For `self.items.iter().map(f)`, the head is `self.items`;
- the **segments**: from the first call on, each `.lident` operation with its generic arguments and call arguments, each `.await`, and each `.INT`, together with the `?` and index operations that follow it. A `.UIDENT` operation is not a segment; it joins the segment before it. For a head that is a call, such as `f(x).g().h()`, the head is `f(x)`.

1. A member chain is one group. Flat, it is printed on one line. Broken, the head is printed on the current line and **every segment** starts a new line, indented one level relative to the line where the chain starts. The line break before `.lident`, `.await`, and `.INT` is safe in every context (S3), so chains never need parentheses.
2. The chain breaks if its flat form does not fit, measured as in `fits` (§14.3.4 rule 2). A closure in the last segment whose body is forced is measured only up to its `{`, because `fits` stops at the first forced line break.
3. The builder marks the chain as forced if its head or a segment other than the last contains a forced break, for example a closure argument whose body is always broken. This includes a closure inside a parenthesized head. A forced break in the last segment does not force the chain: that segment's arguments are a `Choice` (§14.9.6), across which forcing does not propagate (§14.3.2 rule 2).
4. An expression with fewer than two calls is not a member chain. `response.json<User>()` and `user.name.len()` never break before a `.`; only their argument lists break.

These rules are N-H-50. Breaking every segment, rather than only those that do not fit, keeps one operation per line, which makes each line of a pipeline a unit for reading and for diffs. `.UIDENT` joins its segment because it names a type, variant, or module item, not an operation, and because a break before `.` + `UIDENT` would not be safe (S3 does not cover it).

```sai
fn active_names(users: borrow Vec<User>) -> Vec<String> {
    users
        .iter()
        .filter(fn(user) { user.is_active() and user.email_verified })
        .map(fn(user) { user.name.clone() })
        .collect<Vec<String>>()
}
```

### 14.9.6 Last-argument hugging

A call whose **last argument is a closure** can keep the closure on the line of the call and break only its body. The argument list is then a `Choice` of three alternatives:

1. **Flat**: `Flat(list)`, the whole argument list on one line. It is rejected if the closure body is forced (§14.8.1 rule 2).
2. **Hugged**: `(a, b, fn(x) {`, the closure body indented one level relative to the line of the call, and `})` on its own line. In this alternative, the preceding arguments and the closure's parameter list and return type are `Flat` (§14.3.2 rule 7), and the closure body is always broken.
3. **Broken**: the ordinary broken argument list (§14.5.3), with the closure as its last element.

The printer takes the first alternative that fits (§14.3.4): the flat call if it fits as a whole, otherwise the hugged form if its first line, up to and including the `{`, fits and the preceding arguments contain no forced break, otherwise the broken list.

Hugging applies to calls and to the calls of member-chain segments. It does not apply when the closure is not the last argument, or when more than one argument is a closure with a block that does not fit on one line; in that case the list is broken. These rules are N-H-51: they keep the common callback form readable without a separate layout for each kind of callback.

```sai
fn log_all(entries: borrow Vec<Entry>, sink: borrow mut Sink) {
    entries.for_each(fn(entry) {
        let line = entry.render()
        sink.write(line.as_str())
    })
}
```

### 14.9.7 Right-hand sides

A **right-hand side** is the expression or type after `=` in `let`, `var`, assignments, `const`, type aliases, refined and distinct types, and the arm body after `=>`. The break after `=` and `=>` is safe (S2; the grammar writes `nl` after `=` in `let`, `var`, assignments, `const`, and aliases, and after `=>`). A right-hand side `r` is printed as a `Choice` of these alternatives, in order:

1. `" "`, `Flat(r)`: the whole right-hand side on the current line;
2. `Indent([HardLine, Flat(r)])`: the whole right-hand side on the next line, indented one level;
3. `" "`, `r`: on the current line, broken by its own rules, if its first line fits;
4. `Indent([HardLine, r])`: on the next line, broken by its own rules, if its first line fits;
5. `" "`, `r`: if nothing fits, on the current line (overflow, §14.5.6).

If `r` is block-like, alternatives 2 and 4 are omitted, so that it always starts on the line of `=` or `=>`. The block-like expressions are blocks, `unsafe`, `async`, and `region` blocks, `if`, `match`, `loop`, `while`, `for`, and closures. A `match` arm whose body is block-like therefore keeps `=> {` or `=> if …` on the arm line.

These rules are N-H-52. Moving a right-hand side to the next line when it then fits on one line avoids breaking an expression internally only because of the length of its left-hand side. Block-like expressions never move, because their own layout already starts a new line after `{`.

```sai
fn message(error: ImageError) -> Str {
    match error {
        .TooLarge =>
            "the image is larger than the maximum size accepted by the processor for this account"
        .Unsupported => "unsupported format"
    }
}
```

### 14.9.8 Control expressions

1. **`if`**: `if condition {`, the block, `}`. An `else` follows the closing `}` on the same line: `} else {`, `} else if condition {`. The line break before `else` would also be safe (`else` is in C), but the canonical form keeps it on the line of `}`. `if let pattern = expr {` is printed the same way.
2. **Conditions.** The condition of `if` and `while`, the scrutinee of `match`, and the iterated expression of `for` are printed on the line of their keyword, and the `{` that follows them stays on the line of their last token: the grammar has no `nl` before these blocks, and a line break there would end the statement (the last token of the condition is in E). A condition that does not fit is broken by its own rules; a broken operator chain is not protected there, so it is wrapped in parentheses (§14.9.3), and `) {` ends the condition.
3. **`match`**: `match scrutinee {`, the arms as a brace list (§14.5.4), `}`. An arm is `pattern => body` or `pattern if guard => body`; the body is a right-hand side (§14.9.7). A match whose arms fit on one line is flat (D-SYN-07). An arm whose body is a block forces the list to break, and the arms that follow it are separated by the line break after its `}`.
4. **Loops**: `loop {`, `while condition {`, `while let pattern = expr {`, `for pattern in expr {`, `for await pattern in expr {`. A label is printed on the line of its loop: `'outer: for row in rows {` (the `nl` that the grammar allows after `LABEL ':'` is never used).
5. **Closures**: `fn(params) {`, `move fn(params) -> Type {`, with the parameter list as a delimited list and the body as in §14.8.1. A closure parameter is `name`, `name: Type`, `var name`, or `_`.
6. **`return`, `break`, `continue`**: `return`, `return expr`, `break`, `break 'label`, `break 'label expr`, `continue`, `continue 'label`. The expression after `return` or `break` always starts on the line of the keyword (spec/03 §3.13.4: these keywords are in E). If it does not fit, it is broken by its own rules, as a right-hand side that may not move (alternatives 1, 3, and 5 of §14.9.7).
7. **`unsafe`, `async`, `region`**: `unsafe {`, `async move {`, `region arena {`, followed by the block.

```sai
fn classify(values: Vec<I32>) -> Summary {
    var summary = Summary.empty()
    'scan: for value in values {
        if value < 0 {
            summary.negative += 1
        } else if value == 0 {
            continue 'scan
        } else {
            summary.positive += 1
        }
    }
    summary
}
```

---

## 14.10 Patterns and types

### 14.10.1 Patterns

1. **Leading `|`.** The optional leading `|` of a pattern is removed (§14.1.3 item 3, N-H-58).
2. **Or-patterns** are printed `p1 | p2 | p3`, one space around each `|`. An or-pattern that does not fit is broken **after** every `|`, with the alternatives after the first indented one level relative to the line where the pattern starts. A line break after `|` is safe in every context (S2), while a line break before it would end the line after a token in E (a variant name, a binding, or `)`) and is not safe inside a `{ }` list.
3. **Constructor patterns** `Some(x)`, `User(id, name: n, ..)`, `.Disabled(..)`, and **tuple patterns** `(a, b)`, `(x,)` use delimited lists (§14.5.3). No comma follows a final `..`.
4. **Slice patterns** `[first, .., last]`, `[head, ..rest]` use delimited lists.
5. **Range patterns** are printed without spaces: `0..=9`, `'a'..='z'`, `..=MAX`, `10..`.
6. **Bindings**: `x`, `var x`, `borrow x`, `borrow mut x`, `name @ pattern`, with one space around `@`.
7. Literal patterns follow the token rules of §14.4.1; a negative literal is `-1`.

```sai
fn is_open(status: OrderStatus) -> Bool {
    match status {
        .Pending |
            .Active |
            .AwaitingPayment |
            .AwaitingShipment |
            .AwaitingConfirmation |
            .PartiallyRefunded => true
        .Disabled(..) | .Closed => false
    }
}
```

Rationale (N-H-58): the leading `|` is a convenience for hand-broken or-patterns, which the formatter lays out itself, so it is redundant. Breaking after `|` is the only placement that needs no parentheses in a match arm.

### 14.10.2 Types

1. **Paths and generic arguments**: `Vec<I32>`, `Map<String, Vec<User>>`, `http.Status`, `Self.Item`, `T.Output`. Generic argument lists are delimited lists (§14.5.3), and composite `>` tokens are joined (`Vec<Vec<I32>>`). An associated type binding is `Item = I32`.
2. **Borrows**: `borrow T`, `borrow mut T`. `owned T` is kept as written (spec/03 §3.15, D-OWN-01): the formatter does not remove a synonym, because removing a token is not among the permitted changes (§14.1.3).
3. **Tuples**: `()`, `(T,)`, `(A, B)`. Grouping parentheses written by the author are kept: `(fn(I32) -> I32 effects { alloc })`.
4. **Function pointer types**: `fn(I32, I32) -> I32`, `unsafe fn(Ptr<U8>) -> I32`, and `fn(Str) -> Bool effects { io }`. The parameter list is a delimited list, and the effect set is a brace list (§14.5.4).
5. **Bounds**: `T: Format + Clone`, `F: Fn(I32) -> I32 effects { alloc }`. A bound list breaks after `+` (§14.7.5 rule 6).
6. `_` in a type annotation inside an expression is printed as written.

---

## 14.11 Verification, idempotency, and `--check`

### 14.11.1 Normalization and verification

The **normal form** `N(t)` of a CST `t` is the tree obtained by applying the following steps, in order. It undoes exactly the permitted changes of §14.1.3, so that a correct formatter output and its input have the same normal form (F5).

1. Remove all trivia (whitespace, line breaks, comments, line continuations) and all `NEWLINE` tokens. Remove the BOM flag.
2. Remove every comma that separates or ends the elements of a list, except the comma of a one-element tuple expression, tuple pattern, or tuple type.
3. Replace every parenthesized expression whose content is a binary expression or a range expression by its content. (This removes parentheses written by the author too. The tree shape, and therefore the precedence, is still compared: `(a + b) * c` and `a + b * c` have different normal forms.)
4. Remove the leading `|` of every pattern.
5. Sort the `use` declarations of the file as in §14.7.2, and the items of every `UseItems` list. Reorder the clauses of every function into the order of D-SYN-06, keeping the relative order of several `requires` and of several `ensures` clauses.
6. Replace the text of each token by its **normal text**: for an integer literal, the text with hexadecimal digits in uppercase; for a float literal, the text with the exponent marker `e`; for a simple string or a multiline string, its value after escape processing, line-continuation removal, and indentation stripping (spec/02 §2.6.4); for every other token, its text.

**Verification.** After printing, the formatter parses its own output and checks:

1. the output has no lexical or syntax error;
2. `N(parse(output)) = N(parse(input))`, comparing node kinds, children, and normal token texts;
3. the multiset of comment texts of the output equals that of the input, where the text of a comment is taken with trailing whitespace removed from each of its lines (F4).

Check 3 compares multisets, not sequences, because the formatter legitimately moves comments together with the `use` declarations and clauses that it reorders. The position of each comment relative to its token is guaranteed by construction (§14.6.1) and tested by the conformance suite (§14.11.3), not checked at run time. This choice is part of N-H-46.

The package formatter uses the same normalization principle over its lossless token stream: it compares token kinds and source spellings in order after dropping `NEWLINE`, EOF, and only the commas indexed as brace-list separators. It retains comparator commas in version requirements. It also compares parser error and warning kinds and the ordered comment texts after line-ending normalization and removal of trailing whitespace. Package declarations and comments are not reordered, so this comment-sequence check is stricter than the multiset check above. The public package entry point formats the result again and requires a byte-identical fixed point on every call.

### 14.11.2 Verification failure

If any check of §14.11.1 fails, the formatter has a bug. The library retains the failed check and the first differing byte offset in the input when one exists. The command reports the hard error `INTERNAL_COMPILER_ERROR` ([11-diagnostics.md](11-diagnostics.md) §11.4.6) with `component` `"sai_fmt"`, a `panic_message` that names the failed check (`"formatter verification failed: output does not parse"`, `"…: syntax tree changed"`, or `"…: comments changed"`), and a primary location at the first token, in the input, where the normal forms differ (or `null` for check 3). The file is left unchanged, and the command stops with exit code 101 ([13-cli.md](13-cli.md) §13.3.4). Files already written by the same `sai fmt` run keep their new content: each of them passed its own verification (N-H-46).

Rationale: a formatter that silently changes a program is worse than one that fails. Verification costs one extra parse of each formatted file, which is small compared with the formatting itself (P2, P9).

### 14.11.3 Idempotency and conformance

1. **Idempotency (F2)** follows from F6: the output of the formatter is a layout of the same tokens as its input, with blank lines and comments in the same places, so formatting it again makes the same decisions. The canonical form is a fixed point by construction.
2. The conformance suite of the formatter consists of:
   - pairs of an input file and its expected output, one pair per rule of §14.4 to §14.10 and §14.12, compared byte for byte;
   - a fixed-point check: every expected output, formatted again, is unchanged;
   - a layout-independence check (F6): every input is re-laid out at random safe positions (§14.5.1), with random spaces and without changing comments or blank lines, and must produce the same output;
   - the `sai` and `sai-package` code blocks of this specification that are not marked as rejected or `noncanonical` ([15](15-conformance-and-testing.md) §15.8.3 item 4): each must be a fixed point.
3. The reference implementation runs the fixed-point check of item 2 on every file of its own test corpus, and in debug builds it formats every output a second time and treats a difference as a verification failure (§14.11.2).

### 14.11.4 `--check` and `UNFORMATTED_FILE`

`sai fmt --check` ([13-cli.md](13-cli.md) §13.6.7) formats each file without writing it, and reports every file whose bytes differ from its canonical form with the error **`UNFORMATTED_FILE`**:

- **Message**: `file is not formatted`.
- **Primary location**: the first differing line. The input and the canonical output are split into lines after each LF; a line includes its LF, a CR before the LF, and a BOM at the start of the file. Let `n` be the smallest line number at which the input line differs from the output line, where a missing line counts as empty. The primary span is the content of input line `n` without its line terminator; if the input has no line `n`, it is the empty span at the end of the file.
- **Data**: `{"line": n, "expected": "<output line n without its LF, or null if the output has no line n>"}`.
- **Help**: ``run `sai fmt <path>` to format this file``.
- **Fix**: none. The repair is `sai fmt`, which rewrites the whole file. A structured fix would have to describe the same rewrite as text edits, and the edits rule for CR LF files ([11-diagnostics.md](11-diagnostics.md) §11.7.4 rule 3) would undo part of it.

A refused file is not reported with `UNFORMATTED_FILE`: its lexical and syntax errors are reported instead (§14.2.2). A file that contains only a BOM, CR LF line breaks, or a missing final LF differs from its canonical form and is reported.

```text
error[UNFORMATTED_FILE]: file is not formatted
  --> src/user.sai:12:1
   |
12 | pub fn find(id: UserId)->Option<User> {
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = help: run `sai fmt src/user.sai` to format this file
```

---

## 14.12 `sai.package` formatting

`format_package` formats a package contract into the canonical layout of [package/01](../package/01-package-contract.md) §1.3.1 rule 6. This section makes that rule precise. Stage 3, like `sai.package` itself.

### 14.12.1 Input, refusal, and comments

1. The input is read as in §14.2.1: a BOM is removed, CR LF becomes LF, and TAB whitespace becomes spaces. The package lexer and parser of package/01 §1.2 and §1.3 produce the tokens, the NEWLINE tokens of package/01 §1.2.6, and the tree.
2. **Refusal.** The file is refused if and only if the package lexer reports an error (package/01 §1.2: `INVALID_UTF8`, `INVALID_LINE_ENDING`, `FORBIDDEN_CHARACTER`, `UNEXPECTED_CHARACTER`, `UNTERMINATED_COMMENT`, `UNTERMINATED_STRING`, `INVALID_ESCAPE`, `UNSUPPORTED_STRING_FORM`, `INVALID_VERSION`, `BUILD_METADATA_NOT_ALLOWED`, `NO_SEMICOLON`), or the parser reports a syntax error against the grammar of package/01 §1.3 (`UNEXPECTED_NEWLINE`, `EXPECTED_TOKEN`, `UNKNOWN_DECLARATION`, `UNKNOWN_ARTIFACT_KIND`). The structural rules of package/01 §1.3.1 rules 1 to 5 (`MISSING_PACKAGE_DECL`, `PACKAGE_DECL_NOT_FIRST`, `DUPLICATE_DECLARATION`, `DUPLICATE_LIST_ITEM`) and every later validation do not refuse a file. A manifest rejected by the fixed 16 MiB parser limit is also refused under §14.2.3 and package/04 §4.4.4, before lexing. This is N-H-43 applied to package contracts.
3. **Comments** are attached and printed as in §14.6.1 to §14.6.3. A package contract has no doc comments (package/01 §1.2.2), so `///` and `//!` are ordinary comments.
4. **No reordering.** Declarations, list elements, requirements, and their clauses keep their order. Although the order of declarations has no meaning (package/01 §1.3.1 rule 2), tools that edit the file must preserve the order of unrelated declarations ([package/02](../package/02-resolver.md) §2.10.4 rule 4), and the formatter does not impose an order that those tools would then have to reproduce (N-H-62).

### 14.12.2 Layout

1. **Declarations** start at column 1, one per line. Nested entries are indented 4 columns per level (D-TOOL-04).
2. **Spacing.** Tokens on one line are separated by one space, with these exceptions: no space between a version operator and its version (`^1.4`, `>=0.7.1`, `<0.8`); no space before `,` and one after it; no space around the `/` of a world name (`export world api/server`); no space inside an empty list (`{}`). The `@` of `package` and of a requirement has one space on each side, and so has the `<=` of a budget item (`wasm <= 64 KiB`).
3. **Brace lists** with `Sep` separators (`targets`, `only`, `grant`, `allow`, `deny`, `policy`, `lints`, `budget`, `authors`) are printed as in §14.5.4: `{ a, b }` on one line when the list fits, otherwise one element per line without commas, except that a comma is retained before an element whose first token is the contextual continuation word `from`, `dir`, `only`, or `grant`. The retained comma prevents the package lexer from joining that element to the preceding line. A list that contains a comment on its own line or a trailing line comment is broken.
4. **Entry lists** with `NL` separators only (`requires`, `requires test`, `overrides`, `artifact component`, and the body of `profile`) are always broken, one entry per line, even when they contain one entry. An empty one is printed `{}`.
5. **Requirements.** Every clause that a requirement has among `from`, `only`, and `grant` starts its own continuation line, indented 8 columns (one level deeper than the requirement), even when the requirement would fit on one line; `dir` stays on the line of its `from` unless a comment forces a break before `dir`, in which case it stays at 8 columns. A requirement without these clauses is one line. These breaks are safe because `from`, `only`, and `grant` are in the continuation set C of package/01 §1.2.6. A comment may also force a physical continuation before `dir`; `dir` is in C, so this does not insert a `NEWLINE` token. If a continuation line with `only { … }` or `grant { … }` still does not fit, its list breaks, with its elements indented 12 columns and `}` at 8 columns.
6. **Version requirements** are not broken to satisfy the width limit; a requirement whose first line does not fit overflows (§14.5.6). Comments may force a physical continuation after `@`, a comparator, or `,`; those preceding tokens do not terminate a package line, so the lexer does not insert a `NEWLINE` and the version requirement remains one entry. Keeping ordinary version requirements on one line keeps them searchable.
7. **Overrides** (`acme:image use artifact`), artifact entries (`wasm32-wasi "dist/photo.wasm"`), and the other one-line declarations are never broken. A `policy` or `lints` entry keeps its words on one line, except a comment may force a continuation before a policy item's value when that `WORD` is in C; indent the continued value by 8 columns. A lint code cannot continue this way because `UWORD` is not in C.
8. **Blank lines** are kept between declarations and between the entries of a list, at most one, as in §14.6.4 rules 1 and 2. Unlike source files (§14.6.4 rule 3), no blank line is ever inserted.
9. **Safety.** Every line break that the layout uses is safe under package/01 §1.2.6: after `{` and before `}` (the parser skips a NEWLINE there); between list elements, where `NL` is a separator when the lexer emits it; and before `from`, `only`, `grant`, and comment-forced `dir` (C). Because the lexer suppresses `NEWLINE` before a word in C, a broken brace list retains a comma before any element beginning with one of those words, making the separator explicit. A comment-forced physical continuation after `@`, a version comparator, or a version comma does not insert a `NEWLINE` because those tokens are not in E.

Input:

```sai-package noncanonical
package acme:photo-service@1.3.0
targets {
    wasm32-core,
    wasm32-wasi,
}
requires {
    sai:http @ ^ 1.4
        from git "https://github.com/sai-lang/http"
    sai:wasi @ ^0.2 from git "https://github.com/sai-lang/wasi-bindings-for-the-sai-language-runtime" only { wasm32-wasi }
    acme:exif @ >=0.7.1 , <0.8 from git "https://github.com/acme/exif" grant { }
}

profile tiny { budget { wasm <= 64 KiB } }
```

Output:

```sai-package
package acme:photo-service @ 1.3.0
targets { wasm32-core, wasm32-wasi }
requires {
    sai:http @ ^1.4
        from git "https://github.com/sai-lang/http"
    sai:wasi @ ^0.2
        from git "https://github.com/sai-lang/wasi-bindings-for-the-sai-language-runtime"
        only { wasm32-wasi }
    acme:exif @ >=0.7.1, <0.8
        from git "https://github.com/acme/exif"
        grant {}
}

profile tiny {
    budget { wasm <= 64 KiB }
}
```

---

## 14.13 Canonical one-line text

Several places of the specification quote source text on a single line: the panic message of a failed contract, which contains "the source text of the predicate in canonical formatting" ([spec/09](../spec/09-contracts-and-refinements.md) §9.7.1); the sort key of `use` declarations (§14.7.2); the text renderings of the semantic diff and of query results ([12-semantic-tooling.md](12-semantic-tooling.md) §12.8.6); and diagnostic messages that quote code ([11-diagnostics.md](11-diagnostics.md) §11.6.1). They all use the **canonical one-line text** of a node, defined here (N-H-61).

```rust
pub fn one_line(node: &SyntaxNode) -> String;
```

1. The node is printed by the document builder of this chapter with an unlimited width and with every group flat: `Line` is a space, `SoftLine` is empty, `IfBroken` takes its flat branch, and a `Choice` takes the alternative that keeps everything on the current line (alternative 1 of §14.9.7, the flat alternative of §14.9.6). Inserted parentheses (§14.9.3, §14.5.4 rule 5) therefore never appear. Parentheses written by the author do.
2. Comments, including doc comments, are omitted. Attributes are kept, separated from the rest by one space.
3. **Blocks and bodies** cannot be flat in general (§14.8.1). An empty block or body is `{}`. A block with exactly one statement is printed `{ stmt }`, with the statement's own one-line text. A block or item body with more than one element is printed `{ … }`, with U+2026 HORIZONTAL ELLIPSIS. This form is not SAI source; it occurs only in messages and renderings.
4. **String literals** that contain a line break (multiline strings, raw strings with line breaks, and simple strings with line continuations) are printed as the simple string literal with the same value: `\` becomes `\\`, `"` becomes `\"`, LF `\n`, CR `\r`, TAB `\t`, and every other C0 control or DEL `\u{X}` with uppercase hexadecimal digits. Every other literal is printed with its normalized token text (§14.4.1).
5. The result never contains a line break, never has leading or trailing spaces, and depends only on the tree (F1, F6). The one-line text of a canonical node and of any other layout of it are identical.

Examples:

| Source | One-line text |
|---|---|
| a broken `requires` predicate (§14.9.3) | `user.is_active() and (user.id == post.author or user.role == Role.Moderator)` |
| `effects {` / `net.http` / `alloc` / `}` | `effects { net.http, alloc }` |
| `use sai:core/fmt.{Writer, FormatSpec}` | `use sai:core/fmt.{FormatSpec, Writer}` |
| `fn(x) { let y = x + 1` / `y * 2 }` | `fn(x) { … }` |
| `match s { .A => 1, .B => 2 }` | `match s { .A => 1, .B => 2 }` |


Line comments end at the end of their line, so they cannot appear in one-line text. Omitting every comment keeps the rule simple and makes the text independent of where comments were placed.

---

## 14.14 Decisions introduced by this chapter

These decisions refine D-TOOL-04 and D-SYN-07. They do not change either decision. The decisions N-H-10 and N-H-11, which this chapter also applies, are introduced by [11-diagnostics.md](11-diagnostics.md).

| ID | Rule | Rationale |
|---|---|---|
| N-H-43 | The formatter refuses a source file if and only if the lexer reported an error, or the parser reported an error other than `CLAUSE_ORDER` (the `parser` rows of impl/02 §2.5.12). Errors of the validation pass and of all later phases, and all warnings, never refuse. `CLAUSE_ORDER` is corrected by reordering. A package contract is refused only on package lexer errors and grammar errors (§14.12.1) (§14.2.2). | Only recovery trees are ambiguous. Every other tree has one meaning, and refusing it would stop users from formatting files with ordinary semantic errors. |
| N-H-44 | The output layout does not depend on the input layout, except for comments and blank lines between format units. The formatter decides every line break from the width and the rules of this chapter; there is no "keep the author's break" rule (§14.1.2). | A canonical format must map every layout of a tree to one text (F6, P9). Preserved breaks make the output depend on editing history. |
| N-H-45 | The width of a line is its number of Unicode scalar values. Every scalar value counts as one column (§14.3.3). | It is deterministic, independent of fonts, terminals, and Unicode width tables, and cheap to compute. |
| N-H-46 | After formatting, the output is re-parsed, its normal form (§14.11.1) is compared with that of the input, and the multisets of comment texts are compared. A failure is `INTERNAL_COMPILER_ERROR` with `component` `"sai_fmt"`; the file is not changed and `sai fmt` exits with 101. Files already written by the same run are kept (§14.11.1, §14.11.2). | A formatter bug must never change the meaning of a program or lose a comment. The check is linear in the file size. Comments are compared as multisets because use declarations and clauses legitimately move. |
| N-H-47 | Every block and body is printed broken, one statement per line, whatever its length. The only exceptions are a closure body and an `unsafe` block with exactly one expression statement and no comments, which are printed flat when they fit; an empty block is `{}` (§14.8.1). | Blocks are where statements start and NEWLINE terminates them (D-LEX-01). A fixed layout makes diffs line-oriented. Short closures are the common case of `map_err(fn(_) { E.X })`. A flat `unsafe { op() }` keeps the unsafe region as narrow as the operation it marks. |
| N-H-48 | Each signature clause (`where`, `borrows`, `effects`, `requires`, `ensures`) is printed on its own line, one level deeper than the item, even if the signature fits on one line. A function body's `{` goes on its own line at the item's indentation if and only if the function has at least one clause (§14.7.4, §14.7.5). | Clauses are the contract of a function and are reviewed one by one (P2). The separate `{` keeps the contract visually apart from the first statement. |
| N-H-49 | A broken operator chain breaks before every operator of its precedence level, with the operands at +4. If the chain is not inside parentheses or brackets (protected), the formatter inserts parentheses around it, but only when it is broken; in the parenthesized layout, `(` ends the current line and `)` is on its own line; other broken chains use the indented layout (§14.9.3). | Breaking before the operator is the only break that is safe for `and`, `or`, and `else` (D-LEX-01), and the operator stays visible at the left edge. Inserted parentheses are the only safe way to break other operators without `\`. |
| N-H-50 | A member chain is a head followed by at least two segments with calls. When it does not fit, every segment is printed on its own line at +4; a `.UIDENT` access joins the preceding segment (§14.9.5). A forced break in the head or a non-final segment forces the chain; a forced break in the final segment does not. | One operation per line makes pipelines readable and line diffs precise. A break before `.` followed by `UIDENT` is not safe. |
| N-H-51 | A call whose last argument is a closure with a block body that does not fit is printed with the other arguments on the call line and the closure body broken (hugging). The alternatives, in order, are the flat list, the hugged form, and the broken list. Hugging does not apply if the closure is not the last argument or if more than one argument is a closure whose block does not fit on one line (§14.9.6). | Callbacks are common, and a fully broken argument list for every callback wastes lines and indents the body twice. |
| N-H-52 | The right-hand side after `=` (in `let`, `var`, assignments, `const`, aliases, refined and distinct types) and after `=>` is a `Choice` of five alternatives, in order: flat on the same line; flat on the next line at +4; broken starting on the same line; broken starting on the next line; broken on the same line (overflow). Block-like right-hand sides (blocks, `unsafe`, `async`, `region`, `if`, `match`, loops, closures) omit the two next-line alternatives (§14.9.7). | Moving a long right-hand side to the next line avoids breaking an expression only because its left-hand side is long. Block-like expressions already start a new line after `{`. |
| N-H-53 | `use` declarations are moved to the top of the file, after the module header, in two groups (`use`, then `pub use`) separated by one blank line. Within a group they are sorted bytewise by the canonical one-line text without `pub` (a stable sort). `UseItems` elements are sorted by name, then by alias. `UseItems` are printed `.{A, B}`, without spaces inside the braces (§14.7.2, §14.4.2 rule 13). | `use` order has no meaning (spec/04 §4.3.1), so a canonical format must fix one. Bytewise order needs no locale. The `.{A, B}` spelling is the one used by every example of the specification; see conflict (l) in the report of this chapter. |
| N-H-54 | Blank lines are kept only between format units, at most one. Exactly one blank line separates two top-level items if either is printed on more than one line, except within the `use` block. The file start is printed in the order shebang, header comments, module docs, `unsafe module`, `use` block, other items, with one blank line between parts (§14.6.4). | Blank lines group statements and are the author's only layout freedom. Inside a unit, they would break F6. |
| N-H-55 | Comments in the leading trivia of a unit's first token that are separated from it by a blank line are detached: they keep their position when units are reordered. All other comments are attached to the unit or token they precede or follow and move with it (§14.6.1). | A file header or a section comment does not belong to the next declaration, while a comment directly above a declaration documents it. |
| N-H-56 | Continuation lines of simple strings and the content lines of multiline strings are reindented to the output indentation plus 4, without changing the string's value. Raw strings are printed verbatim (§14.4.3). | Strings move with the code that contains them. Reindentation is only possible where the indentation is not part of the value. |
| N-H-57 | The exponent marker of a float literal is printed as `e` (§14.1.3, §14.4.1). | One spelling per value; `e` is the more common form and is not confused with the hexadecimal digit `E`. |
| N-H-58 | The leading `|` of a pattern is removed. A broken or-pattern breaks after every `|`, with the alternatives after the first at +4 (§14.10.1). | The leading `|` only helps hand-broken patterns. A break after `|` is safe in every context; a break before it is not. |
| N-H-59 | In a broken brace list, an element other than the last that ends with an open range (`a..`, `..`) is wrapped in parentheses (§14.5.4 rule 5). | Its line break would not end the element, because `..` is not in E (D-LEX-01). Parentheses are the only safe change. |
| N-H-60 | Formatting a format unit of a file that is canonical outside the edited range produces exactly the text that formatting the whole file would produce. If the unit's layout affects an enclosing node, the next enclosing unit is formatted instead (§14.3.6). | Fixes, structured edits, and range formatting must leave canonical files canonical and must not touch unrelated text (D-TOOL-01, N-H-10). |
| N-H-61 | The canonical one-line text of a node is the formatter output with every group flat and unlimited width, without comments, with multi-element blocks written `{ … }` and strings with line breaks written as escaped simple strings (§14.13). | Contract panic messages, sort keys, diffs, and diagnostic messages need a single deterministic way to quote code on one line. |
| N-H-62 | `sai.package` files are formatted without reordering anything. `NL` lists are always broken. A requirement always breaks before each `from`, `only`, and `grant` at +8, even when it fits. Width never breaks a version requirement, though comments may force physical continuations after `@`, a comparator, or a comma. A comment may force a continuation before `dir` at +8. No blank line is inserted (§14.12). | Tools that edit the contract preserve the order of declarations (package/02 §2.10.4), and the contract is short and mostly hand-written. Breaking only at continuation keywords keeps each line meaningful. Breaking always, as the examples of package/01 do, puts the clauses of all requirements in one column and makes a change of a source or grant a one-line diff. Comment-forced continuations preserve attachment while remaining token-safe under package/01 §1.2.6. |
| N-H-81 | The field list of a record or resource declaration and the variant list of an enum declaration are always broken when they are not empty, one element per line, even when they fit (§14.5.4 rule 4, §14.7.3). Other brace lists (match arms, effect sets, `borrows` lists, `covers` sets, invariant predicates, `UseItems`) are flat when they fit (D-SYN-07). | A declaration is read and changed one field or variant at a time: adding, removing, or renaming one is then a one-line change in a diff and a review, and a declaration never switches between a one-line and a multi-line layout as it grows (P3, P8). The draft writes its declarations this way. |

---

## 14.15 Diagnostics

This chapter owns one code. The other codes are reported by the components that own them; the table lists them because they determine whether the formatter refuses a file (§14.2.2, §14.12.1) or because the formatter reports them (§14.11.2).

| Code | Severity | Description |
|---|---|---|
| `UNFORMATTED_FILE` | error | `sai fmt --check`: the file is not in canonical format; the primary location is the first differing line (§14.11.4). |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6. Reported with `component` `"sai_fmt"` when a verification check fails (§14.11.2). |
| `CLAUSE_ORDER` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; reported by [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.5.12. Does not refuse; the formatter reorders the clauses (§14.2.2, §14.7.5). |
| `EXPECTED_TOKEN`, `EXPECTED_EXPRESSION`, `UNEXPECTED_NEWLINE` | error | defined in [spec/03](../spec/03-grammar.md) §3.16 (the first two) and [spec/02](../spec/02-lexical-structure.md) §2.10 (`UNEXPECTED_NEWLINE`); reported during the parser recovery of [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.5.11. The file is refused (§14.2.2). |
| `CHAINED_COMPARISON`, `CHAINED_RANGE`, `DUPLICATE_CLAUSE`, `BLOCK_IN_CONDITION`, `NO_DOUBLE_COLON`, `USE_AND_OR`, `ITEM_IN_BLOCK`, and every other `parser` row | error | defined in [spec/03](../spec/03-grammar.md) §3.16, except `NO_DOUBLE_COLON` and `USE_AND_OR` ([spec/02](../spec/02-lexical-structure.md) §2.12) and `ITEM_IN_BLOCK` ([spec/06](../spec/06-expressions-and-statements.md) §6.25.1); reported by [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.5.12. The file is refused (§14.2.2). |
| `INCONSISTENT_INDENT` and every other lexer error | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.12. The file is refused (§14.2.2, §14.4.3). |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | defined in [spec/01](../spec/01-introduction.md) §1.5.4; reported by the lexer and the parser (impl/02 §2.2.2, §2.5.3). The file is refused (§14.2.2, §14.2.3). |
| `MISSING_FN_BODY`, `UNKNOWN_ATTRIBUTE`, `INFER_IN_SIGNATURE`, and every other `validation` row | error | defined in [spec/03](../spec/03-grammar.md) §3.16, except `INVALID_EXTERN_BLOCK` ([spec/12](../spec/12-unsafe-and-ffi.md) §12.7.1, §12.9.1) and `INTERFACE_FN_CLAUSE` ([spec/13](../spec/13-components-and-wit.md) §13.2.2); reported by [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.5.12. Does not refuse (§14.2.2). |
| `MISPLACED_DOC_COMMENT` | warning | defined in [spec/02](../spec/02-lexical-structure.md) §2.3; reported by [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6.4. Does not refuse; the demoted comment is kept as an ordinary comment (§14.6.5). |
| `UNNECESSARY_RAW_IDENT` | warning (lint) | defined in [spec/02](../spec/02-lexical-structure.md) §2.4. Does not refuse; raw identifiers are printed as written (§14.4.1). |
| `INVALID_UTF8`, `INVALID_LINE_ENDING`, `FORBIDDEN_CHARACTER`, `UNEXPECTED_CHARACTER`, `UNTERMINATED_COMMENT`, `UNTERMINATED_STRING`, `INVALID_ESCAPE`, `NO_SEMICOLON` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.12; reported by the package lexer of [package/01](../package/01-package-contract.md) §1.2. A package contract with one of these is refused (§14.12.1). |
| `UNSUPPORTED_STRING_FORM`, `INVALID_VERSION`, `BUILD_METADATA_NOT_ALLOWED` | error | defined in [package/01](../package/01-package-contract.md) §1.2. A package contract with one of these is refused (§14.12.1). |
| `UNKNOWN_DECLARATION`, `UNKNOWN_ARTIFACT_KIND` | error | defined in [package/01](../package/01-package-contract.md) §1.3. The package contract is refused (§14.12.1). |
| `MISSING_PACKAGE_DECL`, `PACKAGE_DECL_NOT_FIRST`, `DUPLICATE_DECLARATION` | error | defined in [package/01](../package/01-package-contract.md) §1.3.1. Does not refuse (§14.12.1). |
| `DUPLICATE_LIST_ITEM` | warning | defined in [package/01](../package/01-package-contract.md) §1.3.1. Does not refuse (§14.12.1). |
