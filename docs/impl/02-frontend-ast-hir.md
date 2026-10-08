# 02. Frontend: Lexer, Parser, CST, AST, and HIR

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 0 (lexer, parser, CST, AST views); 1 (HIR and HIR lowering). Per-feature stages are marked inline.
**Related:** [spec/01-introduction.md](../spec/01-introduction.md), [spec/02-lexical-structure.md](../spec/02-lexical-structure.md), [spec/03-grammar.md](../spec/03-grammar.md), [spec/04-modules-and-names.md](../spec/04-modules-and-names.md), [spec/05-types.md](../spec/05-types.md), [spec/06-expressions-and-statements.md](../spec/06-expressions-and-statements.md), [spec/10-errors-and-panics.md](../spec/10-errors-and-panics.md), [spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md), [spec/16-core-library.md](../spec/16-core-library.md), [decisions.md](../design/decisions.md) D-LEX-01..06, D-SYN-01..10, [01-architecture.md](01-architecture.md), [03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md), [04-mir.md](04-mir.md), [11-diagnostics.md](11-diagnostics.md), [12-semantic-tooling.md](12-semantic-tooling.md), [14-formatter.md](14-formatter.md)

This chapter specifies the front end of the reference compiler: how source text becomes tokens, how tokens become a lossless concrete syntax tree (CST), how typed AST views are layered over the CST, and how the AST is lowered to the high-level intermediate representation (HIR) consumed by name resolution and type checking ([03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md)) and by MIR construction ([04-mir.md](04-mir.md)). The lexical rules are owned by [spec/02](../spec/02-lexical-structure.md) and the grammar by [spec/03](../spec/03-grammar.md); this chapter implements them and never changes them. The semantics of every desugaring listed in §2.9 is owned by spec/06, spec/10, and spec/16; this chapter specifies only the HIR shape that the desugaring produces.

---

## 2.1 Overview and general requirements

### 2.1.1 Pipeline position

```
source text ──lex──▶ token stream ──parse──▶ green tree (CST) ──▶ red tree + AST views
                                                                       │
                                               HIR lowering (per owner) ▼
                                                            HIR + source maps
```

| Artifact | Producer | Main consumers |
|---|---|---|
| `SourceFile` (text, line index) | §2.2 | everything that reports positions |
| token stream | lexer (§2.4) | parser (§2.5) |
| green tree | parser (§2.5) | formatter ([14-formatter.md](14-formatter.md)), semantic tooling ([12-semantic-tooling.md](12-semantic-tooling.md)), AST views |
| AST views | §2.7 | HIR lowering, syntactic validation (§2.5.12), IDE features |
| HIR | §2.8, §2.9 | resolver and type checker (impl/03), MIR building (impl/04) |

### 2.1.2 Assumptions about the query system

The architecture and the query system are owned by [01-architecture.md](01-architecture.md). This chapter assumes the following, and every query named here is a memoized, side-effect-free function of its inputs in that system.

- Inputs are `source_text(FileId) -> Arc<str>` and the package/module layout (`package_files(PackageId) -> [FileId]`).
- Queries defined by this chapter: `parse(FileId) -> Parse`, `line_index(FileId) -> Arc<LineIndex>`, `ast_id_map(FileId) -> Arc<AstIdMap>`, `def_collector(FileId) -> Arc<ItemTree>`, `hir_owner(OwnerId) -> Arc<HirOwner>`, `hir_source_map(OwnerId) -> Arc<HirSourceMap>`, `attrs(DefId) -> Arc<ValidatedAttrs>`.
- The current VFS exposes decoded `source_text(FileKey) -> SourceText`, where `FileKey` is stable across sessions and `SourceText` retains status and decoding metadata. `SyntaxQueries` memoizes the green parse by that stable key. Its `parse(FileInterner, WorkspaceSnapshot, FileId)` adapter checks the key and raw bytes, then binds the green tree, diagnostic templates, and fix recipes to the current snapshot; the public `parse(snapshot, file_id)` entry point remains available without a query database.
- `AstIdMap` stores current-snapshot pointers and IDs, so the memoized query value is instead the ordered `SyntaxKind` sequence of item-like nodes. `SyntaxQueries::ast_id_map` validates that shape while rebuilding pointers for the current red root. Body-only edits that preserve the item-like sequence backdate the shape value; adding or removing an item-like node changes it.
- `ItemTree` contains current-snapshot source pointers and session-local `Symbol`s, so it is not itself a stable query value. `ItemTree::stable_shape` resolves symbols to owned text and drops file IDs, pointers, and source ranges while retaining declaration IDs and semantic item data. The HIR query group and current-snapshot rebinding path are still to be implemented.
- Diagnostics are not side effects. Every query returns its diagnostics as part of its value (`Parse::errors`, `HirOwner::diagnostics`, and so on); the driver collects them through the diagnostic sink of [11-diagnostics.md](11-diagnostics.md). The phrase "report X" in this chapter means "push a `Diagnostic` with code X into the diagnostics vector of the query being computed".
- Queries can be cancelled between steps; the lexer and the parser are pure functions with no global state, so cancellation never leaves partial state behind.

### 2.1.3 General requirements

1. **Totality.** The lexer, the parser, and HIR lowering MUST accept every byte sequence and MUST terminate. They MUST NOT panic, abort, or overflow the native stack on any input. Internal invariant violations are reported as the internal error `INTERNAL_COMPILER_ERROR` (owned by [11-diagnostics.md](11-diagnostics.md)) and the query returns an error-recovered result.
2. **Single diagnostic channel.** Every lexical, syntactic, and lowering diagnostic MUST go through the diagnostic sink described above. No component prints to a stream.
3. **Determinism.** The token stream, the tree, the HIR, the IDs, and the diagnostic list (contents and order) are pure functions of the input text and the package layout. Diagnostics of one file are ordered by primary span start, then by the order in which they were reported.
4. **Losslessness.** Concatenating the text of all tokens and trivia of a green tree reproduces the input text byte for byte, including invalid parts (§2.6.6).
5. **Error tolerance.** A syntax error never prevents the construction of a tree that covers the whole file. HIR lowering accepts trees with error nodes and produces `Err` HIR nodes in their place (§2.8.6).

---

## 2.2 Source text handling

### 2.2.1 Files and decoding

```rust
pub struct FileId(u32);                 // interned (package key, package-relative path); assigned by impl/01

pub struct SourceFile {
    pub id: FileId,
    pub text: Arc<str>,                 // decoded text (see below); offsets refer to this text
    pub had_bom: bool,
    pub line_index: Arc<LineIndex>,
}
```

Decoding is done once, before lexing, by `source_text`:

1. A leading UTF-8 BOM (`EF BB BF`) is removed from the text and recorded in `had_bom`. All offsets are relative to the text without the BOM; the line index records a 3-byte shift so that byte offsets in the original file can be recovered for tools that need them.
2. Ill-formed UTF-8 is reported as `INVALID_UTF8` once per maximal ill-formed subsequence (as defined by the Unicode "maximal subpart" practice). Each such subsequence is replaced by U+FFFD in the decoded text. The decoded text is what the lexer sees. Outside quote-delimited literals, the replaced region lexes as an `ERROR` token, so no further diagnostic is produced for it (§2.4.12). Inside a quote-delimited literal, the literal keeps its token kind and receives `HAS_ERROR`; this preserves the literal boundary for parser recovery (IR-016).
3. Line endings are not rewritten. CRLF is kept in the text and treated as one line break by the lexer; a lone CR is reported by the lexer (`INVALID_LINE_ENDING`, §2.4.2) and then treated as a line break.

### 2.2.2 Spans and the line index

- A `TextRange { start: TextSize, end: TextSize }` is a half-open range of UTF-8 byte offsets into the decoded text. `TextSize` is a `u32`; a file larger than the reference limit of 16 MiB (the minimum of [spec/01](../spec/01-introduction.md) §1.5.4; offsets themselves could address up to `u32::MAX - 1` bytes) is rejected before lexing with `IMPLEMENTATION_LIMIT_EXCEEDED` (error, data `limit = "size of a source file"`, `value` = the byte length) (N-E-1).
- A `Span { file: FileId, range: TextRange }` identifies a source region globally.
- `LineIndex` stores the start offset of every line (a line ends after LF, after CRLF, or after a lone CR) and, per line, the positions of non-ASCII characters. It provides:
  - `line_col(offset) -> (line: u32, col: u32)`: both 1-based, column counted in bytes (spec/02 §2.1);
  - `to_utf16(line, col)` and `from_utf16(line, col16)` for the LSP ([12-semantic-tooling.md](12-semantic-tooling.md));
  - `offset(line, col)`.
- An offset inside a multi-byte character is never produced by the lexer or the parser. Conversion functions given such an offset round down to the start of the character.

### 2.2.3 Forbidden characters

The lexer checks every character against spec/02 §2.1 item 4, using three classes:

| Class | Members | Forbidden where |
|---|---|---|
| control | C0 controls other than TAB, LF, and CR; DEL | outside comments and literals |
| bidi | U+202A–U+202E, U+2066–U+2069 | everywhere, including comments and literals |
| foreign whitespace | every `White_Space=yes` code point other than U+0009–U+000D and U+0020, plus U+200B | outside comments and literals |

"Literals" means string, byte string, format string text, character, and byte literals (N-E-3). Each maximal run of forbidden characters of the same class is reported once as `FORBIDDEN_CHARACTER`, with a note naming the first character by code point and Unicode name. Outside comments and literals the run becomes an `ERROR` token (§2.4.12). Inside a comment or literal it stays part of that comment or literal, so the tree is unaffected; a literal containing one gets `HAS_ERROR`.

The table controls which classes are forbidden in each context: C0 controls and foreign whitespace are permitted inside comments and literals, while bidi controls are forbidden everywhere. For f-strings, only `FSTRING_START`, `FSTRING_TEXT`, and `FSTRING_END` carry literal text; interpolation expressions and format-specifier syntax follow the ordinary source rules.

### 2.2.4 Shebang

If the text starts with `#!`, the first line up to (not including) its line break is a `SHEBANG` trivia piece attached as leading trivia of the first token (§2.6.3). It is never a token, so it does not affect NEWLINE generation.

---

## 2.3 Tokens

### 2.3.1 Syntax kinds

Tokens and CST nodes share one `u16` enumeration, `SyntaxKind` (N-E-2). Token kinds are listed here; node kinds are listed in §2.6.5.

```rust
#[repr(u16)]
pub enum SyntaxKind {
    // ── identifier-like and literal tokens ─────────────────────────────
    LIDENT, UIDENT, PKG_PATH,
    INT, FLOAT, STRING, BYTE_STRING, CHAR, BYTE, LABEL,
    // format string pieces (Stage 3), see §2.4.7
    FSTRING_START,          // f" ... up to the first `{` or the closing `"`
    FSTRING_TEXT,           // literal text between interpolations
    FSTRING_INTERP_OPEN,    // `{` that opens an interpolation
    FSTRING_SPEC,           // `:name` or `:name(INT)`
    FSTRING_INTERP_CLOSE,   // `}` that closes an interpolation
    FSTRING_END,            // closing `"` (plus any text after the last interpolation)
    DOC_OUTER, DOC_INNER,

    // ── strict keywords (spec/02 §2.5.1) ───────────────────────────────
    AND_KW, AS_KW, ASSERT_KW, ASYNC_KW, AWAIT_KW, BORROW_KW, BREAK_KW, CONST_KW,
    CONTINUE_KW, ELSE_KW, ENUM_KW, EXPORT_KW, EXTERN_KW, FALSE_KW, FN_KW, FOR_KW,
    IF_KW, IMPL_KW, IN_KW, LET_KW, LOOP_KW, MATCH_KW, MOVE_KW, MUT_KW, NOT_KW, OR_KW,
    OWNED_KW, PUB_KW, RETURN_KW, SELF_KW, SELF_TYPE_KW /* Self */, TRAIT_KW, TRUE_KW,
    TYPE_KW, UNSAFE_KW, USE_KW, VAR_KW, WHERE_KW, WHILE_KW,

    // ── punctuators (spec/02 §2.8) ─────────────────────────────────────
    L_PAREN, R_PAREN, L_BRACE, R_BRACE, L_BRACKET, R_BRACKET,
    COMMA, DOT, DOT2, DOT2EQ, COLON, EQ, THIN_ARROW, FAT_ARROW, AT, QUESTION,
    PLUS, MINUS, STAR, SLASH, PERCENT, PLUS_PERCENT, MINUS_PERCENT, STAR_PERCENT,
    AMP, PIPE, CARET, TILDE, SHL, LT, LE, EQ2, NEQ,
    PLUS_EQ, MINUS_EQ, STAR_EQ, SLASH_EQ, PERCENT_EQ,
    PLUS_PERCENT_EQ, MINUS_PERCENT_EQ, STAR_PERCENT_EQ,
    AMP_EQ, PIPE_EQ, CARET_EQ, SHL_EQ,
    GT,                     // always a single `>`; see TokenFlags::JOINT
    UNDERSCORE,

    // ── composite tokens: created only by the parser by gluing joint tokens ──
    SHR, GE, SHR_EQ,        // `>>`, `>=`, `>>=` from GT GT / GT EQ / GT GT EQ
    COLON2,                 // `::` (error recovery only, NO_DOUBLE_COLON)
    AMP2, PIPE2,            // `&&`, `||` (error recovery only, USE_AND_OR)

    // ── synthetic and error tokens ─────────────────────────────────────
    NEWLINE,                // zero-width, see §2.4.11
    ERROR,                  // invalid input, see §2.4.12
    EOF,

    // ── node kinds follow (§2.6.5) ─────────────────────────────────────
    // ...
}
```

Contextual keywords (spec/02 §2.5.2) are lexed as `LIDENT`. The parser recognizes them by text with `at_contextual(kw)`, which compares the token text exactly (a raw identifier `r#effects` never matches, because its text starts with `r#`). Reserved words (spec/02 §2.5.3) are lexed as `LIDENT` with the flag `RESERVED` set and a diagnostic (§2.4.3).

### 2.3.2 Token records

```rust
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,          // the token text, excluding trivia
    pub flags: TokenFlags,
    pub leading: TriviaRange,      // index range into LexOutput::trivia
    pub trailing: TriviaRange,
}

bitflags! {
    pub struct TokenFlags: u16 {
        const JOINT              = 1 << 0;  // the next character after this token starts the next token (no trivia between)
        const AFTER_LINE_BREAK   = 1 << 1;  // at least one line break (in trivia) between the previous token and this one
        const RESERVED           = 1 << 2;  // LIDENT whose text is a reserved word
        const RAW                = 1 << 3;  // LIDENT/UIDENT written with r#
        const HAS_ERROR          = 1 << 4;  // token with a reported lexical error
        const IN_E               = 1 << 5;  // counts as a member of E for NEWLINE generation (§2.4.11)
    }
}

pub struct LexOutput {
    pub tokens: Vec<Token>,            // ends with exactly one EOF
    pub trivia: Vec<Trivia>,
    pub errors: Vec<Diagnostic>,
}

pub struct Trivia { pub kind: TriviaKind, pub range: TextRange }

pub enum TriviaKind {
    Whitespace,        // spaces and tabs
    LineBreak,         // "\n", "\r\n", or a lone "\r" (the latter also reported)
    LineComment,       // "// ..." including "////..." and misplaced doc comments (§2.6.4)
    BlockComment,      // "/* ... */", nested
    Shebang,
    Skipped,           // text of a token the parser skipped during recovery (§2.5.11)
}
```

Token values (integer values, decoded string contents, and so on) are not stored in the token. They are computed on demand from the text by the AST views (§2.7.3), with the same functions the lexer uses for validation, so the lexer and later stages cannot disagree.

### 2.3.3 Keyword table

The keyword table is a perfect hash from text to kind, generated at build time from the lists in spec/02 §2.5. Lookup happens only for identifier-shaped text without the `r#` prefix.

| Class | Result of lexing | Diagnostic |
|---|---|---|
| strict keyword | its `*_KW` kind | none |
| contextual keyword | `LIDENT` | none |
| reserved: `null` `nil` `undefined` | `LIDENT` + `RESERVED` | `NO_NULL` (fix: `Option.None` / `None`) |
| reserved: `try` `catch` `throw` | `LIDENT` + `RESERVED` | `NO_EXCEPTIONS` (note: use `Result` and `?`) |
| other reserved words | `LIDENT` + `RESERVED` | `RESERVED_WORD` (fix: `r#name`) |

A reserved word is still an identifier token so that the parser can continue. Name resolution never sees a reserved identifier as a valid name: HIR lowering maps it to an error name (§2.8.6) because the lexical error has already been reported.

`sai_syntax::is_reserved_identifier` exposes this same classification for downstream lowering. It accepts a bare, non-raw spelling; callers remove `r#` first and must allow valid raw identifiers to use reserved spellings.

---

## 2.4 Lexer

### 2.4.1 Lexer state

The lexer is a hand-written, single-pass scanner over the decoded text. It runs to completion for a whole file; the parser never drives it (N-E-4). The only context the lexer uses is the previous significant token and the bracket stack.

```rust
struct Lexer<'a> {
    src: &'a str,
    pos: usize,                         // byte offset of the next unread character
    out: LexOutput,
    brackets: Vec<Bracket>,             // spec/02 §2.10.1 bracket stack
    pending: Vec<Trivia>,               // trivia read since the previous token
    prev: Option<PrevTok>,              // previous emitted token other than NEWLINE
    nl_armed: bool,                     // §2.4.11: conditions 1 and 2 held at the first line break
    nl_emitted_since_prev: bool,        // a NEWLINE was emitted after `prev`
}

enum Bracket { Paren, Square, Brace, Interp }

struct PrevTok { kind: SyntaxKind, text_is: PrevText, in_e: bool, index: usize }
enum PrevText { Use, Export, Import, Include, Other }   // for PKG_PATH mode (§2.4.10)
```

### 2.4.2 Main loop

```rust
fn lex_file(src: &str) -> LexOutput {
    let mut lx = Lexer::new(src);
    lx.lex_shebang();                               // §2.2.4
    loop {
        lx.lex_trivia();                            // fills `pending`, arms NEWLINE (§2.4.11)
        if lx.at_eof() {
            lx.finish_eof();                        // §2.4.11 rule 5, then EOF
            return lx.out;
        }
        let tok = lx.lex_token();                   // one token; never fails (§2.4.12)
        lx.maybe_emit_newline_before(&tok);         // §2.4.11 rule 3
        lx.push_token(tok);                         // attaches trivia (§2.6.3), updates prev and brackets
        if tok.kind == FSTRING_START { lx.lex_fstring_rest(); }   // §2.4.7
    }
}
```

`lex_trivia` consumes, in a loop: spaces and tabs (`Whitespace`); `\n`, `\r\n`, and a lone `\r` (`LineBreak`; a lone `\r` is reported as `INVALID_LINE_ENDING`); line comments; and block comments. A line comment starting with exactly `///` or `//!` is not trivia: `lex_trivia` stops in front of it and `lex_token` produces `DOC_OUTER` or `DOC_INNER` (§2.4.8).

`lex_token` dispatches on the first character:

| First character(s) | Routine |
|---|---|
| `r#` followed by an identifier start | raw identifier (§2.4.3) |
| `r"` or `r#…"` | raw string (§2.4.6) |
| `b'` / `b"` / `br"` / `br#` | byte literal / byte string (§2.4.5, §2.4.6) |
| `f"` | format string start (§2.4.7) |
| identifier start | PKG_PATH attempt (§2.4.10), then identifier or keyword (§2.4.3) |
| digit | number (§2.4.4) |
| `'` | char or label (§2.4.5) |
| `"` | string (§2.4.6) |
| `///`, `//!` | doc comment (§2.4.8) |
| punctuator character | punctuator (§2.4.9) |
| anything else | `ERROR` token (§2.4.12) |

### 2.4.3 Identifiers and keywords

```rust
fn lex_ident(&mut self) -> Token {
    let start = self.pos;
    let raw = self.eat("r#");
    // IdentStart ::= XID_Start | "_" ; IdentContinue ::= XID_Continue  (spec/02 §2.4)
    self.bump_while(|c| is_xid_continue(c));        // first char already known to be XID_Start or '_'
    let text = &self.src[start..self.pos];
    let name = if raw { &text[2..] } else { text };
    let mut flags = TokenFlags::empty();
    if raw { flags |= RAW; }
    if !name.is_ascii() {
        flags |= NON_ASCII;                          // NON_ASCII_IDENTIFIER / NON_ASCII_PUBLIC_NAME are reported later (§2.10.3)
        if !is_nfc(name) { self.error(IDENT_NOT_NFC, start..self.pos).fix(replace_with(nfc(name))); }
    }
    if name == "_" && !raw { return self.tok(UNDERSCORE, start, flags); }
    if raw {
        if matches!(name, "self" | "Self" | "_") { self.error(INVALID_RAW_IDENT, start..self.pos); }
        else if keyword_class(name).is_none() { self.warn(UNNECESSARY_RAW_IDENT, start..self.pos).fix(remove("r#")); }
        return self.tok(case_kind(name), start, flags);
    }
    match keyword_class(name) {
        Some(Strict(kind))     => self.tok(kind, start, flags),
        Some(Contextual)       => self.tok(LIDENT, start, flags),
        Some(Reserved(code))   => { self.error(code, start..self.pos); self.tok(LIDENT, start, flags | RESERVED) }
        None                   => self.tok(case_kind(name), start, flags),
    }
}

fn case_kind(name: &str) -> SyntaxKind {          // D-LEX-02, spec/02 §2.4 item 4
    match name.chars().find(|&c| c != '_') {
        Some(c) if is_lu_or_lt(c) => UIDENT,
        _ => LIDENT,
    }
}
```

- `TokenFlags::NON_ASCII` is bit 6 of `TokenFlags` (it is listed here because only identifiers use it).
- The NFC check uses the Unicode quick-check property first and runs full normalization only when quick-check answers "maybe" or "no".
- Identifier equality everywhere after lexing compares the NFC-normalized name (spec/02 §2.4 item 2). Because a non-NFC identifier is already an error, HIR lowering interns the normalized form so that later stages see consistent names.

### 2.4.4 Numbers

```rust
fn lex_number(&mut self) -> Token {
    let start = self.pos;
    let after_dot = self.prev_kind() == Some(DOT);            // spec/02 §2.6.2 item 3
    if !after_dot {
        if let Some(radix) = self.peek_prefix() {              // "0x" | "0o" | "0b"
            self.pos += 2;
            return self.lex_radix_digits(start, radix);
        }
        if self.peek_upper_prefix() {                          // "0X" | "0O" | "0B"
            self.pos += 2;
            let t = self.lex_radix_digits(start, radix_of_upper());
            return self.malformed(t, "uppercase radix prefix");
        }
    }
    self.lex_dec_digits();                                     // Digit (Digit | '_')*
    let mut kind = INT;
    if !after_dot && self.peek() == '.' && self.peek_nth(1).is_ascii_digit() {
        self.pos += 1; self.lex_dec_digits(); kind = FLOAT;
    }
    if !after_dot && matches!(self.peek(), 'e' | 'E') && self.exponent_follows() {
        self.lex_exponent(); kind = FLOAT;
    }
    // trailing identifier characters: a suffix such as 10u8 or 1.0f32
    if is_xid_continue(self.peek()) {
        self.bump_while(is_xid_continue);
        return self.malformed(self.tok(kind, start, HAS_ERROR), "literal suffixes do not exist");
    }
    let t = self.tok(kind, start, TokenFlags::empty());
    self.validate_separators_and_leading_zero(t)            // MALFORMED_NUMBER cases of spec/02 §2.6.1
}
```

- `exponent_follows` returns true when the characters after `e`/`E` are an optional sign followed by a digit. Otherwise the `e` is part of a suffix and the literal is `MALFORMED_NUMBER`.
- `validate_separators_and_leading_zero` checks, in this order, and reports only the first violation: `_` directly after a prefix; `_` at the end of any digit run; `__`; a decimal `INT` (not a FLOAT, not an exponent mantissa) with a leading `0` followed by more digits. Digits that are invalid for the radix (`0b12`, `0o8`) are consumed as part of the token and reported as `MALFORMED_NUMBER`.
- After a `DOT` token the lexer reads only `DecInt` (no prefix, no fraction, no exponent), so `t.0.1` lexes as `t . 0 . 1` and `t.0e1` lexes as `INT(0)` followed by a malformed suffix. The `_` check of spec/03 §3.13.2 (no `_` in a tuple index) is done by the parser, which sees `DOT INT`.
- The lexer never computes numeric values. Range checks (`LITERAL_OUT_OF_RANGE`) need the type and happen in the type checker ([03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md) §3.7.6).

### 2.4.5 Characters, bytes, and labels

```rust
fn lex_quote(&mut self) -> Token {                // at '\''
    let start = self.pos; self.pos += 1;
    if is_ident_start(self.peek()) {
        let id_end = self.scan_ident_from(self.pos);
        if self.char_at(id_end) == '\'' && self.single_scalar(self.pos, id_end) {
            self.pos = id_end + 1; return self.tok(CHAR, start, empty());      // 'a'
        }
        if self.char_at(id_end) != '\'' {
            self.pos = id_end; return self.tok(LABEL, start, empty());          // 'outer
        }
        // 'ab' : identifier characters followed by a quote but more than one scalar value
        self.pos = id_end + 1;
        return self.error_tok(CHAR, start, INVALID_CHAR_LITERAL);
    }
    self.lex_char_body(start, CharMode::Char)      // escapes, other characters, or errors
}
```

- `lex_char_body` reads one `CharBody` or `Escape` and then expects `'`. A line break or EOF before the closing quote is `UNTERMINATED_CHAR`; the token ends before the line break. `''`, a raw TAB, or more than one scalar value before the closing quote is `INVALID_CHAR_LITERAL`; in that case the token extends to the next `'` on the same line if there is one, and otherwise ends after the first scalar value.
- Byte literals (`b'..'`) use the same routine with `ByteEscape` and `AsciiBody`. A non-ASCII or non-printable character is `INVALID_CHAR_LITERAL`.
- An invalid escape (`\q`, `\u{}`, `\u{1234567}`, a surrogate, a value above `10FFFF`, `\x` in a `CHAR`, `\u` in a `BYTE`) is `INVALID_ESCAPE` with a span covering the escape only. The token still ends at the closing quote and gets `HAS_ERROR`.

### 2.4.6 Strings

1. **Simple strings** (`"…"`, `b"…"`). Characters are read until the closing `"`. `\` starts an escape; `\` followed by LF (or CRLF) is a line continuation, which also consumes the following spaces and tabs. A raw line break before the closing quote is `UNTERMINATED_STRING`, reported at the opening quote with a secondary span at the line break; the token ends **before** that line break (N-E-5), so the next line is lexed normally. EOF before the closing quote is handled the same way.
2. **Multiline strings** (`"""`). Three quotes always start a multiline string token (maximal munch). The opening `"""` must be followed by a line break; if it is not, the lexer reports `MALFORMED_MULTILINE_STRING` and continues as if a line break followed, so the token still ends at the next closing `"""` (N-E-5). The lexer reads up to a line that consists only of whitespace followed by `"""`. The base indentation is that whitespace. Each non-empty content line must start with exactly the base indentation bytes; a line that does not is `INCONSISTENT_INDENT` (one diagnostic per offending line). EOF before the closing line is `UNTERMINATED_STRING`, and the token extends to EOF.
3. **Raw strings** (`r"…"`, `r#"…"#`, up to 255 `#`). More than 255 `#` is `MALFORMED_RAW_STRING` (error). The token ends at the first `"` followed by the same number of `#`. EOF first is `UNTERMINATED_STRING` and the token extends to EOF. Raw strings may span lines; their line breaks are part of the token and do not produce NEWLINEs.
4. **Byte strings** accept only ASCII characters and `ByteEscape`s; a non-ASCII character is `INVALID_ESCAPE` with the note "byte strings may contain only ASCII characters".

The lexer validates escapes and indentation; decoded values are produced later by `ast::StringLit::value()` and `ast::ByteStringLit::value()` (§2.7.3), which use the shared escape scanner in "value" mode. The decoders in value mode never fail: they replace invalid escapes, non-ASCII byte-string characters, inconsistent-indentation lines, and forbidden bidi controls with U+FFFD (encoded as UTF-8 bytes in a byte string). Raw string and raw byte-string values, and multiline string values, retain internal line endings as written, including a diagnosed lone CR, so that later stages can continue with the source value.

### 2.4.7 Format strings (Stage 3)

A format string is lexed as a token sequence, so that the interpolated expressions become ordinary CST nodes (N-E-6):

```
f"Hello {name}, you are {age:pad(3)} years old{{!}}"

FSTRING_START("f\"Hello ")
FSTRING_INTERP_OPEN("{") LIDENT("name") FSTRING_INTERP_CLOSE("}")
FSTRING_TEXT(", you are ")
FSTRING_INTERP_OPEN("{") LIDENT("age") FSTRING_SPEC(":pad(3)") FSTRING_INTERP_CLOSE("}")
FSTRING_END(" years old{{!}}\"")
```

```rust
fn lex_fstring_rest(&mut self) {                         // after FSTRING_START was pushed
    loop {
        match self.scan_fstring_text() {                 // reads text, escapes, "{{" and "}}" until '{', '"', LF, or EOF
            Stop::Quote  => { self.push(FSTRING_END); return; }
            Stop::Brace  => {
                self.push_text_if_nonempty(FSTRING_TEXT);
                self.push(FSTRING_INTERP_OPEN); self.brackets.push(Interp);
                let mut depth = 0;                       // local ( [ { depth inside the interpolation
                loop {
                    self.lex_trivia_no_newlines();       // whitespace only; a line break is an error
                    match self.peek() {
                        '\n' | '\r' | EOF => { self.error(INVALID_INTERPOLATION, "line break in interpolation");
                                               self.brackets.pop(); return; }   // string ends here (unterminated)
                        '"' => { self.error(INVALID_INTERPOLATION, "string literal in interpolation");
                                 /* the quote closes the f-string (recovery) */ self.brackets.pop();
                                 self.push(FSTRING_END); return; }
                        ':' if depth == 0 => { self.lex_format_spec(); /* expects '}' next */ }
                        '}' if depth == 0 => { self.brackets.pop(); self.push(FSTRING_INTERP_CLOSE); break; }
                        _ => { let t = self.lex_token(); depth += delta(t.kind); self.push_token(t); }
                    }
                }
            }
            Stop::LineBreakOrEof => { self.error(UNTERMINATED_STRING); self.push(FSTRING_END /* empty */); return; }
        }
    }
}
```

- `lex_format_spec` reads `:` LIDENT ( `(` INT `)` )? as one `FSTRING_SPEC` token (spec/02 §2.6.5). Anything else after `:`, or anything other than `}` after the spec, is `INVALID_FORMAT_SPEC`; the lexer then skips to the next `}` of the interpolation (or the end of the string) as part of an `ERROR` token. Whether the spec name is known is checked by the type checker, not by the lexer.
- An empty interpolation `{}` or `{ }` is `INVALID_INTERPOLATION` ("empty interpolation").
- A single `}` in text that is not part of `}}` is `INVALID_INTERPOLATION` ("unmatched `}`; write `}}`").
- For NEWLINE generation, the whole sequence behaves as one `FSTRING` token: `FSTRING_END` carries `IN_E`, and no line break can occur inside the sequence without an error, which ends the sequence.
- Interpolated tokens are lexed with the ordinary rules, including PKG_PATH mode and error tokens; `{`/`}` inside the interpolation (for example a block expression) adjust `depth`.

### 2.4.8 Comments and doc comments

- `//` comments run to the end of the line (excluding the line break). `////…` is an ordinary line comment.
- `/*` comments nest. The lexer counts `/*` and `*/` pairs; EOF inside a block comment is `UNTERMINATED_COMMENT` reported at the outermost `/*`, and the comment extends to EOF.
- `///` produces `DOC_OUTER` and `//!` produces `DOC_INNER`. The token text is the whole line from the slashes to the end of the line (excluding the line break). The doc text value (`ast::DocComment::text()`) removes the three-character prefix and then one leading space if present (spec/02 §2.3 item 5).
- The lexer does not decide whether a doc comment is placed correctly; the parser does (§2.6.4). A doc comment token takes part in NEWLINE generation as an ordinary token that is neither in E nor in C, even if the parser later demotes it to a comment (N-E-7). The consequence is that `x\n/// d\n.y()` is not a method chain; this mirrors the fact that a doc comment in that position is itself a mistake.

### 2.4.9 Punctuators

Punctuators are lexed with maximal munch over the table of spec/02 §2.8, except:

- `>` is always a single `GT`. Every token carries `JOINT` when the next character starts the next token without trivia, so the parser can glue `GT GT`, `GT EQ`, and `GT GT EQ` (§2.5.2).
- `!` not followed by `=` is an `ERROR` token reported as `NO_BANG_OPERATOR` (fix: `not`). `!=` is `NEQ`.
- `;` is an `ERROR` token reported as `NO_SEMICOLON` (fix: delete). For NEWLINE generation it is treated as a line break: if the preceding token is in E and the bracket condition holds, a NEWLINE is emitted after the `;` (N-E-8). This keeps `let a = 1; let b = 2` from cascading into further errors.
- `::` is two `COLON` tokens, the first with `JOINT`. The parser reports `NO_DOUBLE_COLON` and glues them into `COLON2`, which it then treats as `DOT`.
- `&&` and `||` are two `AMP` / `PIPE` tokens with `JOINT`. The parser reports `USE_AND_OR` where it expects a binary operator and glues them into `AMP2` / `PIPE2`, treated as `and` / `or`.
- `#` outside a raw-string prefix is an `ERROR` token reported as `UNEXPECTED_CHARACTER` (§2.4.12).
- `$`, `` ` ``, `\` outside literals, and any other non-identifier, non-punctuator character produce `ERROR` tokens with `UNEXPECTED_CHARACTER`.

### 2.4.10 PKG_PATH mode

```rust
fn try_pkg_path(&mut self) -> Option<Token> {
    let prev = self.prev.as_ref()?;
    if !matches!(prev.text_is, PrevText::Use | PrevText::Export | PrevText::Import | PrevText::Include) { return None; }
    let start = self.pos;
    let seg1 = self.match_pkg_seg(start)?;                 // [a-z][a-z0-9]*(-[a-z0-9]+)*
    if self.char_at(seg1) != ':' { return None; }          // spec/02 §2.9 item 1: must start with PkgSeg ":"
    let mut end = self.match_pkg_seg(seg1 + 1)
        .unwrap_or_else(|| { self.error(MALFORMED_PACKAGE_PATH, start..seg1 + 1); seg1 + 1 });
    while self.char_at(end) == '/' {
        match self.match_path_seg(end + 1) {               // [a-z][a-z0-9_]*(-[a-z0-9]+)*
            Some(e) => end = e,
            None => break,
        }
    }
    // A path that continues with characters that cannot end it is malformed, for example
    // `sai:http/Client` or `acme:my--pkg`: the rest of the run is absorbed into the token.
    if matches!(self.char_at(end), '/' | '-' | ':') || is_xid_continue(self.char_at(end)) {
        let run_end = self.scan_while(end, |c| c == '/' || c == '-' || c == ':' || is_xid_continue(c));
        self.error(MALFORMED_PACKAGE_PATH, start..run_end);
        end = run_end;
        self.pos = end; return Some(self.tok(PKG_PATH, start, HAS_ERROR | IN_E));
    }
    self.pos = end;
    Some(self.tok(PKG_PATH, start, IN_E))
}
```

- `PrevText::Import` and `PrevText::Include` are set only for a non-raw `LIDENT` whose text is exactly `import` / `include`; `PrevText::Use` and `PrevText::Export` for the keywords `use` and `export`.
- Trivia between the keyword and the path is allowed (`use  sai:core/fmt`), because `prev` refers to the previous token, not the previous character.
- The PKG_PATH attempt happens before identifier lexing, so `use sai:http/client` never produces `LIDENT(sai) COLON …`.

### 2.4.11 NEWLINE generation

This is the D-LEX-01 / spec/02 §2.10 algorithm, written as code. "E" is the set of spec/02 §2.10.1; a token is in E when its kind is listed there, or it is `FSTRING_END`, or it is an `ERROR` token (N-E-9). "C" is the continuation set.

```rust
const E_KINDS: &[SyntaxKind] = &[
    LIDENT, UIDENT, PKG_PATH, INT, FLOAT, STRING, BYTE_STRING, CHAR, BYTE, LABEL, FSTRING_END,
    TRUE_KW, FALSE_KW, SELF_KW, SELF_TYPE_KW, RETURN_KW, BREAK_KW, CONTINUE_KW, AWAIT_KW,
    R_PAREN, R_BRACKET, R_BRACE, GT, QUESTION, UNDERSCORE, ERROR,
];

fn bracket_condition(&self) -> bool {                  // rule 1
    match self.brackets.last() { None | Some(Bracket::Brace) => true, _ => false }
}

// Called by lex_trivia for every LineBreak trivia piece, and for every BlockComment
// whose text contains a line break, and for a `;` ERROR token (N-E-8).
fn on_line_break(&mut self) {
    if self.nl_armed || self.nl_emitted_since_prev { return; }        // rule 4: collapse
    let prev_in_e = self.prev.as_ref().map_or(false, |p| p.in_e);
    if prev_in_e && self.bracket_condition() {                        // rules 1 and 2, evaluated at the break
        self.nl_armed = true;
    }
}

fn maybe_emit_newline_before(&mut self, next: &Token) {
    if !self.nl_armed { return; }
    self.nl_armed = false;
    if self.is_continuation(next) { return; }                          // rule 3
    self.emit_newline();
}

fn is_continuation(&self, next: &Token) -> bool {                      // set C
    match next.kind {
        AND_KW | OR_KW | ELSE_KW => true,
        DOT => matches!(self.peek_token_kind_after(next), LIDENT | AWAIT_KW | INT),
        _ => false,
    }
}

fn finish_eof(&mut self) {                                             // rule 5
    let prev_in_e = self.prev.as_ref().map_or(false, |p| p.in_e);
    if self.nl_armed || (prev_in_e && !self.nl_emitted_since_prev) {
        self.emit_newline();
    }
    self.push_eof();
}

fn emit_newline(&mut self) {
    // Zero-width token placed at the end of the previous token's trailing trivia (N-E-10).
    let at = self.end_of_prev_trailing_trivia();
    self.out.tokens.push(Token { kind: NEWLINE, range: TextRange::empty(at), flags: empty(), .. });
    self.nl_emitted_since_prev = true;
}
```

- `push_token` resets `nl_emitted_since_prev` to false and updates `prev` for every token except `NEWLINE`.
- `peek_token_kind_after(next)` lexes ahead (with a cloned cursor, skipping trivia) to classify the token after a `.`; the result is discarded. The look-ahead uses the same routines, so `.0` (DOT INT) and `.await` are classified exactly as they will later be lexed.
- At EOF, rule 5 applies even when there is no trailing line break (`fn f() {}` without a final LF still ends with `NEWLINE EOF`).
- The bracket stack is updated by `push_token`: `L_PAREN`, `L_BRACKET`, `L_BRACE`, and `FSTRING_INTERP_OPEN` push; `R_PAREN`, `R_BRACKET`, `R_BRACE`, and `FSTRING_INTERP_CLOSE` pop. A closer that does not match the top pops up to and including the nearest matching opener if there is one, and is otherwise ignored for stack purposes (N-E-9). The lexer reports no bracket diagnostics; unbalanced brackets are syntax errors reported by the parser.
- The rule 6 behavior (skipping a NEWLINE directly after `{` and before `}`) belongs to the parser (§2.5.2).

Each emitted NEWLINE records, in a side table `LexOutput::newline_causes: Vec<NewlineCause>`, the index of the token that armed it and the trivia piece that triggered it (a line break, a multi-line block comment, or a `;`). The parser uses this to produce the note required for `UNEXPECTED_NEWLINE` (spec/02 §2.10.3): "a NEWLINE was generated here because the previous token `x` can end a statement and the next line does not start with `.name`, `and`, `or`, or `else`".

### 2.4.12 Error tokens and lexer recovery

1. The lexer never stops early. Every character belongs to exactly one token or trivia piece.
2. An `ERROR` token is produced for: a run of forbidden characters outside comments and literals; a lone `!`; `;`; `#` that does not start a raw string; and any other character that cannot start a token (`UNEXPECTED_CHARACTER`). Consecutive offending characters of the same cause form one token and one diagnostic.
3. Literal errors (malformed numbers, invalid escapes, unterminated strings and characters, inconsistent indentation) do **not** produce `ERROR` tokens. They produce a token of the literal's own kind with `HAS_ERROR`, so the parser sees a well-formed expression and no cascade arises.
4. Ill-formed UTF-8 inside a quote-delimited literal is covered by the literal token's `HAS_ERROR` flag and the decoder's `INVALID_UTF8` diagnostic. Elsewhere, the replacement character is emitted as an `ERROR` token. Neither path reports a second diagnostic for the same malformed byte sequence (IR-016).
5. The unterminated-literal end positions are fixed (N-E-5): a simple string, character, byte, or format string ends before the offending line break; a multiline string, raw string, or block comment extends to EOF.
6. Each lexical diagnostic carries a primary span covering exactly the offending characters. Forbidden-character diagnostics include a note with the first character's code point and Unicode name. The pure `lex()` query keeps fix intents independent of the opaque workspace snapshot; `lex_with_fixes()` binds them and plans token-level fixes for NFC normalization, unnecessary `r#`, lone `!`, and `;`. The parser adds `.` for `::`, `and` / `or`, and raw-prefix fixes when it owns the relevant syntax operation.

---

## 2.5 Parser

### 2.5.1 Architecture

The parser is a hand-written recursive-descent parser. Expressions are parsed with a Pratt (precedence-climbing) loop driven by the D-SYN-05 table (§2.5.6). The parser does not build the tree directly. It produces a flat list of events, which a tree builder turns into the green tree (§2.6) afterwards.

The public `parse(snapshot, file_id)` entry point lexes and parses a file, merges lexical and parser diagnostics in source order, and returns a red root bound to that exact workspace snapshot. Low-level use of `Parser` and `build_green_tree` produces an unbound green tree; callers outside `sai_syntax` obtain a snapshot-bound root through `parse`.

```rust
pub enum Event {
    Start { kind: SyntaxKind, forward_parent: Option<u32> },   // forward_parent supports `precede`
    Finish,
    Token { kind: SyntaxKind, n_raw: u8 },  // n_raw > 1 glues joint raw tokens (SHR, GE, SHR_EQ, COLON2, AMP2, PIPE2)
    DemoteToTrivia,                         // the next raw token (a misplaced doc comment) becomes LineComment trivia
    Error { diag: Diagnostic },
    Tombstone,                              // abandoned marker
}

pub struct Parser<'t> {
    tokens: &'t [Token],
    pos: usize,                // index of the current raw token
    events: Vec<Event>,
    expected: ExpectedSet,     // kinds tested by at()/eat() since the last bump, for EXPECTED_TOKEN
    fuel: u32,                 // §2.5.3
    depth: u32,                // §2.5.3
    restrictions: Restrictions,// §2.5.6: NO_BLOCK_OPERAND in condition position
    spec_failures: FxHashSet<u32>,   // §2.5.7 memoized failed speculations
}

pub struct Marker { pos: u32 }            // index of a Start event; must be completed or abandoned
pub struct CompletedMarker { pos: u32, kind: SyntaxKind }
impl CompletedMarker { fn precede(self, p: &mut Parser) -> Marker { /* sets forward_parent */ } }
```

- Left-recursive constructs (binary expressions, postfix chains, assignment statements, `name @ pattern`) are built by completing the left operand first and then calling `precede`.
- A `Marker` that is dropped without `complete` or `abandon` is a bug. The check is a debug assertion; in release builds the tree builder treats an unfinished `Start` as `Tombstone` and reports `INTERNAL_COMPILER_ERROR`.
- The parser is generated from no grammar tool. Every grammar function corresponds to one nonterminal of spec/03 and is named after it (`fn_decl`, `let_stmt`, `postfix_expr`, and so on), so that review against spec/03 is mechanical.

### 2.5.2 Token access, `nl`, and composite tokens

- `current()` and `nth(n)` look at raw tokens. NEWLINE tokens are visible: the grammar decides where they may be skipped.
- `eat_nl()` consumes a NEWLINE if one is current. Every `nl` in spec/03 is one call of `eat_nl()`. In particular, `block` calls `eat_nl()` directly after `{` and directly before expecting `}` (spec/02 §2.10.2 rule 6), and so do all brace-delimited lists.
- `at_contextual("effects")` is true when the current token is a non-raw `LIDENT` with that exact text.
- Composite tokens are recognized from `JOINT` flags and glued with `Event::Token { n_raw }`:

| Composite | Raw tokens | Recognized where |
|---|---|---|
| `SHR` (`>>`) | `GT`(joint) `GT` | binary operator position in expressions |
| `GE` (`>=`) | `GT`(joint) `EQ` | binary operator position in expressions |
| `SHR_EQ` (`>>=`) | `GT`(joint) `GT`(joint) `EQ` | assignment operator position |
| `COLON2` | `COLON`(joint) `COLON` | anywhere; reports `NO_DOUBLE_COLON`, then treated as `DOT` |
| `AMP2`, `PIPE2` | `AMP`(joint) `AMP`, `PIPE`(joint) `PIPE` | binary operator position; reports `USE_AND_OR`, then treated as `and` / `or` |

  In type context and in generic argument lists, `GT` is never glued: each `GT` closes one list, so `Vec<Vec<I32>>` needs no special case. `EQ` after a joint `GT` that closes a generic argument list is not glued either (`let v: Vec<I32>= x` is `Vec<I32>` `=` `x`).

### 2.5.3 Termination and resource limits

1. **Progress.** Every loop in the parser either consumes at least one token per iteration or exits. `nth()` decrements `fuel`; `bump()` resets it to 256. If `fuel` reaches 0, the parser has a bug: it reports `INTERNAL_COMPILER_ERROR`, wraps all remaining tokens into one `ERROR` node, and finishes the tree. This is a guard, not a recovery mechanism.
2. **Nesting.** `depth` is incremented on entry to `expr`, `pattern`, `type_`, and `block`, and decremented on exit. When it would exceed 512, the parser reports `IMPLEMENTATION_LIMIT_EXCEEDED` (error, data `limit = "nesting depth"`, `value = 513`; [spec/01](../spec/01-introduction.md) §1.5.4) once per file, skips the innermost construct as an `ERROR` node (a balanced bracket group, or up to the next NEWLINE at the same depth), and continues (N-E-11). A single source-level nesting level can enter at most two of these functions (a block expression enters `expr` and then `block`), so the counted limit of 512 guarantees the combined minimum depth of 256 required by spec/01. Because of this limit the parser's native stack use is bounded independently of the input. Later recursive passes (HIR lowering, type checking, MIR building) rely on the same bound.
3. **Size.** The event vector and the tree are linear in the number of tokens; no grammar function allocates per token more than a constant.

### 2.5.4 Files and items

```rust
fn source_file(p: &mut Parser) {
    let m = p.start();
    module_header(p);                         // DOC_INNER* ( 'unsafe' "module" NL )?
    while !p.at(EOF) {
        if p.eat(NEWLINE) { continue; }        // only reachable after recovery
        item(p);
        if !p.at(EOF) && !p.eat(NEWLINE) {
            p.error_expected_item_separator();     // EXPECTED_TOKEN { NEWLINE }, or UNEXPECTED_* special cases
            recover_to_item_start(p);              // §2.5.11
        }
    }
    p.bump(EOF);
    m.complete(p, SOURCE_FILE);
}
```

- `item` parses `OuterDocs`, then `Attributes` (each `@name(args)` followed by `eat_nl()`), then dispatches on the first token(s) of `ItemBody`. `pub` and `export` are parsed as `VISIBILITY` first. `pub export` is reported as `REDUNDANT_PUB_EXPORT` (fix: delete `pub`) and parsed as `export`. `pub use` goes to `use_decl`.
- Dispatch table (contextual keywords require the given follow token so that, for example, `test` stays usable as a name elsewhere):

| Lookahead | Item |
|---|---|
| `use` | `use_decl` |
| `fn`, `unsafe fn`, `async fn`, `unsafe async fn` | `fn_decl` |
| `type` | `type_decl` |
| `enum` | `enum_decl` |
| `distinct` UIDENT | `distinct_decl` |
| `resource` UIDENT | `resource_decl` |
| `trait`, `unsafe trait` | `trait_decl` |
| `impl`, `unsafe impl` | `impl_decl` |
| `const` | `const_decl` |
| `invariant` UIDENT | `invariant_decl` |
| `effect` LIDENT | `effect_decl` |
| `interface` LIDENT | `interface_decl` |
| `world` LIDENT | `world_decl` |
| `extern`, `unsafe extern` | `extern_block` |
| `test` STRING | `test_decl` |
| `property` STRING | `property_decl` |

- `export` before anything other than `fn` (or `unsafe fn` / `async fn`) is reported as `INVALID_EXPORT_TARGET` and the item is parsed as if the visibility were `pub`. A `pub` before `impl`, `invariant`, `extern`, `test`, or `property` is reported as `EXPECTED_TOKEN` with the note "this item has no visibility" and ignored.
- **Type declarations.** `type_decl` parses `type UIDENT GenericParams?`, then selects `alias_def` when `=` follows or `struct_def` otherwise. A record may have a `where` clause before its `FIELD_LIST`; fields parse outer docs, attributes, optional `pub`, a lowercase name, and a type, with comma or newline separators. An alias parses a type and optional `REFINEMENT_CLAUSE`. Generic parameters and bounds reuse the function-signature grammar, and types reuse `type_`. A newline inside a generic parameter or argument list is consumed only immediately before `>`; otherwise it remains a recovery boundary for the next statement or item. When a comma suppresses the newline token, recovery also stops before a physical line start that matches an item or a statement that cannot begin a valid generic parameter or argument. A `fn(...)` start is a function-pointer type when no closure body follows, including its optional return type and effects; a following body brace makes it a closure boundary even when its optional return type is malformed. Qualified type-path lookahead requires the grammar's uppercase type segments and rejects call postfixes, so method-call statements remain available to block recovery. Recovery preserves valid multiline generic parameters and function-pointer type arguments, then lets the block parser resume a statement that follows an item-in-block. An incomplete record header does not absorb that following construct. Syntactic parsing preserves generic refined aliases; the validation pass reports `GENERIC_REFINED_TYPE`.
- **Enum declarations.** `enum_decl` retains unit, tuple, and record variants, parses signed discriminants, and reports `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` once at the first explicit discriminant when any payload exists. During missing-brace recovery, uppercase calls with expression-shaped payloads are treated as statements only when a later line-start statement or item confirms the boundary. Type lookahead peels nested `borrow`, `owned`, and parenthesized types iteratively, checks that a grouped type ends at a tuple separator, and rejects expression operators in the group. Function-pointer payloads are distinguished from closures, matching right parentheses and braces is precomputed, and call-chain boundary results are cached so repeated recovery lookahead stays linear.
- **Distinct declarations.** `distinct_decl` parses the contextual `distinct` followed by an uppercase name, optional generic parameters, `=`, optional inner `pub` (which exposes the constructor and `.value`), a base type and an optional refinement clause. Outer `pub` remains the item's visibility; `export` is diagnosed as `INVALID_EXPORT_TARGET`. Syntactic parsing preserves generic refined distinct types for the later `GENERIC_REFINED_TYPE` validation. Physical line breaks after `=` and inner `pub` do not produce `NEWLINE` tokens under the lexer rules, so the following base type is parsed as whitespace continuation. If a compound base-type prefix such as `borrow` lacks its operand at a physical-line item boundary, parsing diagnoses the missing type and leaves that item for the source-file parser.
- **Resource declarations.** `resource_decl` recognizes contextual `resource` followed by an uppercase name. Its body is optional; when present, it reuses `field_list` and `field_decl` so docs, attributes, field visibility, types, and list recovery follow record syntax. Outer `pub` remains the resource visibility, `export` receives `INVALID_EXPORT_TARGET`, and declarations inside blocks use `ITEM_IN_BLOCK` recovery. Resource ownership and borrow restrictions remain in later stages; interface-resource syntax is parsed by `iface_resource`.
- **Trait declarations.** `trait_decl` parses optional `unsafe`, generic parameters, supertrait bounds, a `where` clause, and a brace-delimited `ITEM_LIST`. Trait items are associated type declarations with optional bounds or function declarations; function bodies are optional for trait methods, and their `requires`/`ensures` clauses use the ordinary expression parser. Trait items cannot have visibility markers, and `export trait` is diagnosed as `INVALID_EXPORT_TARGET`. Missing-brace recovery preserves following declarations at physical-line item boundaries. Trait member uniqueness, inherited bounds, and method semantics remain for name resolution and type checking.
- **Impl declarations.** `impl_decl` parses optional `unsafe`, generic parameters, an inherent type target, a trait reference followed by `for` and a type target, or a lowercase `ModPath` interface target, then an optional `where` clause and an `ITEM_LIST`. Associated-type bindings are enabled while parsing the trait reference. Impl items are associated type definitions or functions with required bodies. Only functions in inherent impls accept `pub`; `pub` on a trait or interface impl function reports `VISIBILITY_ON_TRAIT_IMPL_ITEM`, while visibility on an associated type or any `export` marker reports `EXPECTED_TOKEN`. A `pub impl` marker reports `EXPECTED_TOKEN`, and `export impl` uses `INVALID_EXPORT_TARGET`. Missing-close recovery preserves following declarations that cannot start an impl item; a line-start function or associated type is kept in the impl because it is indistinguishable from a member. An associated type with a missing right-hand type reports `EXPECTED_TOKEN` and resumes at the next line-start impl member even when the lexer suppresses a `NEWLINE` after `=`.
- **Function heads.** `fn_decl` parses `FnHead` and then `FnBody?`. After the parameter list, `eat_nl()` is tried before `->` (spec/03 `( nl '->' ReturnType )?`), restoring the NEWLINE if no `->` follows. `FnClauses` are parsed by a loop that accepts the five clause kinds in **any** order, each preceded by an optional NEWLINE; a clause that appears after a clause that must follow it is reported as `CLAUSE_ORDER` (with a fix that reorders the clauses), and a second `where`, `borrows`, or `effects` clause is reported as `DUPLICATE_CLAUSE` (N-E-12). `requires` and `ensures` may repeat. The body is `eat_nl()` followed by a block if the current token (after the optional NEWLINE) is `{`; otherwise there is no body and the NEWLINE is left for the item separator.
- **Constants.** `const_decl` parses an uppercase name, `: Type`, `=`, and an expression initializer. The ordinary item visibility handling permits `pub` and diagnoses `export` as `INVALID_EXPORT_TARGET`. A missing type before a new item is diagnosed without consuming that item. Since a physical newline after `=` may be suppressed, a line-start item is treated as a missing initializer only when it matches item syntax; valid multiline expressions, including `fn()` closures, still parse.
- **Invariants.** `invariant_decl` dispatches the contextual keyword only when followed by a type name and parses a required, brace-delimited `INVARIANT_EXPR_LIST`. Predicates use the ordinary expression parser and the `Sep` separators (comma or newline); an empty list is diagnosed. A missing close preserves a following line-start item when it cannot be an expression. `pub invariant` reports `EXPECTED_TOKEN`, `export invariant` reports `INVALID_EXPORT_TARGET`, and placement and target-type checks remain in validation.
- **Effects.** `effect_decl` dispatches the contextual `effect` only when followed by a lowercase name and parses an optional `covers` clause containing the shared `EFFECT_SET`. The ordinary visibility handling permits `pub` and diagnoses `export` as `INVALID_EXPORT_TARGET`. If an effect set is missing its closing brace, recovery preserves a following line-start item when its tokens cannot form an effect reference.
- **Interfaces.** `interface_decl` dispatches contextual `interface` only when followed by a lowercase name. Its `ITEM_LIST` parses records and aliases through `type_decl`, enums through `enum_decl`, optional interface-resource bodies with function-head items, and bodyless interface functions. Interface and resource members have no visibility; invalid markers are retained under `ERROR`. `FnBodyPolicy::Forbidden` reports `INTERFACE_FN_BODY` at a supplied body and wraps the parsed block in `ERROR`. On a missing close, an unambiguous following top-level item is left for the enclosing parser. A line-start `FnHead`, `TypeDecl`, `EnumDecl`, or `IfaceResource` is ambiguous because each is also a valid interface member, so it remains inside the interface.
- **Worlds.** `world_decl` dispatches contextual `world` only when followed by a lowercase name. World items are retained as `WORLD_IMPORT`, `WORLD_EXPORT`, or `WORLD_INCLUDE`, with a `WORLD_REF` containing either a `PKG_PATH` or `MOD_PATH`; imported and exported `FnHead`s reuse `fn_decl` with `FnBodyPolicy::Forbidden`. World function heads reject generics and all clauses using `GENERIC_INTERFACE_ITEM`, `INTERFACE_FN_CLAUSE`, or `EFFECTS_ON_INTERFACE_FN`; a supplied body reports `INTERFACE_FN_BODY`. A whole-package `PKG_PATH` in an import or export reports `WORLD_IMPORT_NEEDS_INTERFACE`; path resolution and whether a reference names the right kind of entity remain later checks. An invalid reference token is retained under `ERROR` and consumed once, avoiding recovery cascades. Attributes before world items are diagnosed as `INVALID_ATTRIBUTE_TARGET`. A missing close preserves a following top-level item that is not a world item; `export FnHead` is ambiguous with a top-level exported function and remains inside the world.
- **Extern blocks.** `extern_block` parses optional `unsafe`, the decoded ABI string, an optional `EXTERN_MODULE`, and an `ITEM_LIST` of documented or attributed function heads. The supported ABI values are `"wasm"` and `"C"`; any other valid string reports `UNKNOWN_ABI` without also reporting `INVALID_EXTERN_BLOCK`. Wasm requires a valid `module STRING`, and C requires `unsafe`; these block errors are reported at `extern`. Unsafe C reports `FEATURE_NOT_IMPLEMENTED` with `stage = 7` and `feature = "extern-c"`. `FnBodyPolicy::Extern` permits a bodyless declaration, reports Stage 5 async functions, and reports a supplied body as `UNEXPECTED_FN_BODY` while preserving the block under `ERROR`. Visibility markers are diagnosed and retained. Missing-close recovery leaves a following non-function item for the source-file parser; a line-start function remains in the block because it is also a valid `ExternItem`.
- **Tests and properties.** Contextual `test` and `property` dispatch only in item position when followed by a `STRING`. A test parses its required block; a property parses a `PARAM_LIST` of `PARAM` nodes using the `NamedParam` grammar (`var` is optional and `_` is accepted), followed by its required block. Empty property parameter lists are accepted as explicitly permitted by spec/14 §14.2.2 item 8. `pub` is diagnosed with `EXPECTED_TOKEN`, while `export` uses `INVALID_EXPORT_TARGET`; attributes on the declarations remain available to later target validation. Attributes before a property parameter are parsed inside its `PARAM` node and diagnosed as `INVALID_ATTRIBUTE_TARGET`, matching ordinary parameter recovery. These declarations are Stage 0 syntax, so execution, name checks, parameter type constraints, body typing, and effect checks are deferred.
- Whether a body is required, allowed, or forbidden depends on the context and is checked after parsing (§2.5.12).
- In an `extern` block, a `{` after an `ExternItem` head is reported as `UNEXPECTED_FN_BODY`; the block is parsed and discarded into an `ERROR` node.
- Interface, interface-resource, and world function heads report `INTERFACE_FN_BODY` at a supplied block and retain the parsed block under `ERROR`. `UNEXPECTED_FN_BODY` is reserved for `extern` blocks.

### 2.5.5 Blocks and statements

```rust
fn block(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.expect(L_BRACE);
    p.eat_nl();
    while !p.at(R_BRACE) && !p.at(EOF) {
        stmt(p);
        if p.eat(NEWLINE) { if p.at(R_BRACE) { break; } continue; }    // `nl '}'`
        if !p.at(R_BRACE) { p.error_stmt_separator(); recover_in_block(p); }
    }
    p.expect(R_BRACE);
    m.complete(p, BLOCK)
}

fn stmt(p: &mut Parser) {
    match p.current() {
        LET_KW    => let_stmt(p),
        VAR_KW    => var_stmt(p),
        ASSERT_KW => assert_stmt(p),
        _ => {
            let lhs = expr(p);                                 // full Expr
            if let Some(op) = p.at_assign_op() {               // '=' '+=' ... '<<=' and glued '>>='
                let m = lhs.precede(p); p.bump_assign_op(op); p.eat_nl(); expr(p);
                m.complete(p, ASSIGN_STMT);
            } else {
                lhs.precede(p).complete(p, EXPR_STMT);
            }
        }
    }
}
```

- `let_stmt`: `let` Pattern ( `:` Type )? `=` nl Expr, then `else` (after an optional NEWLINE) starts the let-else block. An `if` initializer consumes a following `else` itself (spec/03 `IfExpr`), so `let x = if c { a } else { b }` is an ordinary let; a let-else whose initializer is an `if` without `else` cannot be written, and the parser does not try to reinterpret it.
- `var_stmt`: `var` LIDENT ( `:` Type )? `=` nl Expr. A pattern after `var` (`var (a, b) = …`) is reported as `EXPECTED_TOKEN` with the note "use `let (var a, var b) = …`".
- A `let` or `var` without `=` is reported as `MISSING_INITIALIZER` (error, D-SYN-10) and parsed as if `= <error>` followed.
- A token that can only start an item at statement start is reported as `ITEM_IN_BLOCK` (error, D-SYN-10, the code of [spec/06](../spec/06-expressions-and-statements.md)), and the item is parsed with `item` and discarded into an `ERROR` node. The item starters are `fn` followed by LIDENT, `type`, `enum`, `trait`, `impl`, `use`, `extern`, `const` followed by UIDENT, `pub`, `export`, an `@` attribute followed (after further attributes) by one of these starters (§2.10.1), and the contextual keywords `distinct`, `effect`, `invariant`, `test`, `property`, `interface`, `world`, and `resource` when followed by the token that starts their declaration form in spec/03. A nested safe, synchronous, non-generic, private, non-exported `fn NAME(…)` receives Help suggesting `let NAME = fn(..) { .. }`; generic, `unsafe`, `async`, public, or exported nested functions are directed outside the block because a local closure cannot preserve their declaration properties or semantics. A machine-applicable edit awaits parser-side CST identity and snapshot-aware fix planning. (`fn` followed by `(` is a closure expression.)
- Until a declaration form has a parser production, its block occurrence is retained by balanced token recovery in the same `ERROR` node instead of propagating `UnsupportedItem`; the corresponding top-level form remains unsupported by the incremental parser. Recovery recognizes declaration-specific newline `where` clauses and bodies, distinguishes an effect set in a generic bound from the declaration body, preserves a separate block expression after an effect's `covers` set, and synchronizes at clear later statement starts. While an unclosed declaration generic list is being recovered, it recognizes the expected parameter shape and also synchronizes on expression starts that cannot continue generic syntax, including at nested angle depth; nested associated-type `=` bindings do not disable angle tracking. Once a production is implemented, the ordinary `item` parser supplies its structured subtree.
- An expression statement may be followed directly by an assignment operator only at statement level; inside expressions, `=` is never an operator, so `f(a = 1)` reports `EXPECTED_TOKEN` at `=`.

### 2.5.6 Expressions (Pratt parsing)

The binding powers implement D-SYN-05 and spec/03 §3.13.1. A larger number binds tighter. Each infix operator has a left power `l` and a right power `r`; the loop continues while `l > min_bp`.

| Level | Operators | `l` | `r` | Associativity |
|---|---|---|---|---|
| or | `or` (and `PIPE2` after `USE_AND_OR`) | 1 | 2 | left |
| and | `and` (and `AMP2`) | 3 | 4 | left |
| not | prefix `not` | — | 5 (operand parsed with `min_bp = 6`) | prefix |
| comparison | `==` `!=` `<` `<=` `>` `>=` | 7 | 8 | non-associative |
| range | `..` `..=` (infix, postfix-open, prefix) | 9 | 10 | non-associative |
| bit or | `\|` | 11 | 12 | left |
| bit xor | `^` | 13 | 14 | left |
| bit and | `&` | 15 | 16 | left |
| shift | `<<` `>>` | 17 | 18 | left |
| additive | `+` `-` `+%` `-%` | 19 | 20 | left |
| multiplicative | `*` `/` `%` `*%` | 21 | 22 | left |
| prefix | `-` `~` `*` `borrow` `borrow mut` | — | operand is a prefix expression | prefix |
| cast (recovery only) | `as` Type | 23 | — | postfix |
| postfix | `.name` `.0` `.await` call index `?` | 25 | — | postfix |

```rust
fn expr_bp(p: &mut Parser, min_bp: u8) -> Option<CompletedMarker> {
    let mut lhs = prefix_or_primary(p)?;                     // handles `not`, `-`, `~`, `*`, `borrow`, prefix ranges
    loop {
        let Some(op) = p.at_infix_op() else { break };       // gluing SHR/GE; never `=` or assignment ops
        let (l, r) = binding_power(op);
        if l <= min_bp { break; }
        if op.is_comparison() && lhs.kind_is_comparison() { p.error(CHAINED_COMPARISON, lhs_span..op_span); }
        if op.is_range() && lhs.kind_is_range() { p.error(CHAINED_RANGE, ...); }
        let m = lhs.precede(p);
        p.bump_op(op);
        if op.is_range() {
            if p.at_expr_start() && !(p.restrictions.no_block_operand && p.at(L_BRACE)) {
                expr_bp(p, r);
            } else if op == DOT2EQ {
                p.error(EXPECTED_EXPRESSION, "`..=` needs an end");
            }
            lhs = m.complete(p, RANGE_EXPR);
        } else {
            if expr_bp(p, r).is_none() { p.error_expected_expression_after(op); }
            lhs = m.complete(p, BIN_EXPR);
        }
    }
    Some(lhs)
}
```

- **Non-associativity.** A comparison operand is parsed with `min_bp = 8`, so a following comparison operator stops the inner call and is seen by the outer loop, where `CHAINED_COMPARISON` is reported. The chain is still built (left-nested) to keep the tree complete. Ranges are handled the same way with `CHAINED_RANGE` (N-E-13).
- **`not`.** `not` parses its operand with `min_bp = 6`, so `not a == b` is `not (a == b)` and `not a and b` is `(not a) and b`, as in spec/03.
- **Prefix operators.** `-`, `~`, `*`, `borrow`, and `borrow mut` parse their operand by calling `prefix_or_primary` again (followed by postfix operators), never an infix loop, so `-a.b()` is `-(a.b())` and `borrow x.y` is `borrow (x.y)`. `-INT` stays `PREFIX_EXPR(MINUS, LITERAL)` in the tree; the type checker treats it as one negative constant (spec/02 §2.6.1 item 5).
- **Prefix ranges.** `..` or `..=` in operand position starts a `RANGE_EXPR` without a start; the end is parsed with `min_bp = 10` when an expression can start, subject to the same `{` restriction.
- **Condition restriction.** In condition position (the `Condition` of `if` / `while`, the scrutinee of `match`, and the expression after `in` of `for`) the parser sets `NO_BLOCK_OPERAND`. Under this restriction, an **optional** trailing operand (the end of a range, and the value of `return` and `break`) is not parsed when the current token is `{`, so `for i in 0.. {` takes `{` as the body (N-E-14). Operands that the grammar requires (the right operand of `==`, for example) are parsed normally. The restriction is cleared inside `( )`, `[ ]`, and blocks.
- **`BLOCK_IN_CONDITION`.** If an expression in condition position starts with `{`, the parser reports `BLOCK_IN_CONDITION` (fix: parenthesize), parses the block as the condition's first operand, and continues; the next `{` is then the body.
- **NEWLINE as an operand.** If an operand is required and the current token is NEWLINE, the parser reports `EXPECTED_EXPRESSION` with the spec/02 §2.10.3 note and a structured fix that moves the operator to the end of the previous line or wraps the expression in parentheses. It does not consume the NEWLINE.
- **`as` after an operand.** SAI has no cast ([spec/06](../spec/06-expressions-and-statements.md) §6.10), but the parser recovers from one so that the type checker can offer the conversion that fits. In the loop of `expr_bp`, an `as` token on the same line as the end of the left operand is treated as a postfix operator with `l = 23`: if `23 > min_bp`, the parser bumps `as`, parses a type (§2.5.9), and completes `CAST_EXPR` around the operand, the keyword and the type. So `a * b as T` is `a * (b as T)`, and `-x as T` is `(-x) as T`. If no type can start after `as`, the parser reports `EXPECTED_TOKEN` (§2.5.11) and completes `CAST_EXPR` without a type. The parser reports no other diagnostic for a cast: `NO_AS_CAST` is reported by the type checker ([impl/03](03-name-resolution-and-type-checking.md) §3.10.3), which knows the operand's type (N-E-64). An `as` at the start of a line is not in the continuation set C of §2.4.11, so the NEWLINE before it ends the expression and `as` is a syntax error of the next statement, not a cast.
- **Postfix operators** are parsed by `postfix_expr` in a loop after the primary expression: `.` followed by `Ident` (with optional generic arguments, §2.5.7), `.` INT (tuple index; the `INT` text must consist of decimal digits without `_` and without a leading `0` other than `0` itself, otherwise `INVALID_TUPLE_INDEX`, the code of [spec/06](../spec/06-expressions-and-statements.md)), `.` `await` (Stage 5, `AWAIT_EXPR`), call arguments `( … )`, index `[ … ]`, and `?` (`TRY_EXPR`). A postfix `(` or `[` must be on the same line; a NEWLINE before it ends the expression, by the lexer rules.
- **Call arguments** are `ARG` (positional), `NAMED_ARG` (`LIDENT ':' Expr`), and `BASE_ARG` (`'..' Expr`). A `BASE_ARG` that is not last is reported as `BASE_ARG_NOT_LAST` (error).
- **Primary expressions** dispatch on the current token: literals (including the `FSTRING_START … FSTRING_END` sequence, parsed into an `FSTRING_EXPR` with one `FSTRING_INTERP` node per interpolation), `Ident` with optional generic arguments, `self`, `Self`, `.` UIDENT (`IMPLICIT_MEMBER_EXPR`), parenthesized forms (unit, parenthesized, one-element tuple, tuple), arrays, blocks, `unsafe` block, `async` [`move`] block (Stage 5; see below for `async` before a closure), `if`, `match`, `loop`/`while`/`for` with an optional `LABEL ':' nl` prefix, closures (`move`? `fn` `(`), `region` LIDENT block (only when followed by `LIDENT {`), `return`, `break`, `continue`. A `LABEL` before anything other than `loop`, `while`, or `for` is reported as `EXPECTED_TOKEN` with the note "only loops can be labeled".
- **`async` before a closure.** In operand position, `async` followed by `fn`, or by `move` and then `fn`, is reported as `NO_ASYNC_CLOSURE` at the `async` token, never as `EXPECTED_TOKEN` ([spec/11](../spec/11-async-and-concurrency.md) §11.2.5, N-I-1). The check is made by the primary dispatch before the `async` block alternative, by looking ahead one or two tokens. The parser then parses the closure as an ordinary `CLOSURE_EXPR` (with its `move` marker) and completes an `ERROR` node around `async` and the closure, which lowering turns into `ExprKind::Err` (§2.8.6 item 4): the closure body is not checked until the mistake is fixed, because its meaning as an async closure is undefined. The fix, marked maybe-incorrect, deletes `async` and the space after it, and wraps the contents of the closure body in `async move { … }`, which gives the two rewrites of spec/11 §11.2.5. When the closure has a return type `-> T`, the fix also deletes `-> T`, since the closure now returns the future and not `T`.
- **`return` and `break` values.** The value is parsed only if the next token can start an expression and is on the same line; since `return` and `break` are in E, a line break after them produces a NEWLINE, which is not an expression start.
- **Match arms** are parsed as `List<MatchArm, Sep>` inside braces; an arm is `Pattern ( 'if' Expr )? '=>' nl Expr`. The guard is parsed without the condition restriction.

### 2.5.7 Generic arguments in expressions (D-SYN-04)

```rust
fn maybe_generic_args_expr(p: &mut Parser) {          // current token is LT, directly after an Ident
    if p.spec_failures.contains(&(p.pos as u32)) { return; }   // already known to fail here
    let cp = p.checkpoint();                           // pos, events.len(), and a muted diagnostic sink
    p.set_speculative(true);                           // any error aborts the speculation
    let ok = generic_args(p).is_ok() && p.at_any(&FOLLOW_GENERIC_ARGS);
    p.set_speculative(false);
    if ok { p.commit(cp); } else { p.spec_failures.insert(cp.pos as u32); p.rewind(cp); }
}

const FOLLOW_GENERIC_ARGS: &[SyntaxKind] =
    &[L_PAREN, DOT, R_PAREN, R_BRACKET, R_BRACE, COMMA, QUESTION, NEWLINE, FAT_ARROW, L_BRACE, EOF];
```

- `generic_args` uses the type grammar of spec/03 §3.15 (`GenericArg ::= Type | INT | '-' INT | CHAR | 'true' | 'false' | UIDENT '=' Type`) and closes with a single `GT`, preceded by `eat_nl()`.
- In speculative mode the parser never reports diagnostics, never performs error recovery, and never glues `GT`. The first error makes `generic_args` return `Err`.
- The test of the follow token uses the raw token after the closing `GT`. `FOLLOW_GENERIC_ARGS` is the set F of spec/03 §3.13.3, in the same order.
- Because a type path must end with a `UIDENT` (spec/03 §3.15), `a < b` with a lowercase `b` never succeeds as generic arguments. A failed speculation at a position is remembered in `spec_failures`, so the total speculation cost is linear in the length of the longest type-like token run.

### 2.5.8 Patterns

`pattern` parses an optional leading `|`, then `AltPattern`s separated by `|` into an `OR_PAT` when there are two or more. `primary_pattern` dispatches:

| Lookahead | Pattern |
|---|---|
| `_` | `WILDCARD_PAT` |
| `borrow` [`mut`] LIDENT, `var` LIDENT | `BINDING_PAT` with `BINDING_MODE` |
| LIDENT followed by `@` | `AT_PAT` (binding, `@`, primary pattern) |
| LIDENT followed by `.` | `CONST_PATH` (module-qualified), then as for UIDENT |
| LIDENT otherwise | `BINDING_PAT` |
| `-` INT, INT, STRING, BYTE_STRING, CHAR, BYTE, `true`, `false` | `LITERAL_PAT`, or `RANGE_PAT` if `..` / `..=` follows |
| `..=` | `RANGE_PAT` without a start |
| UIDENT | `CONST_PATH`, then `CTOR_PAT` with `CTOR_PAT_ARGS` if `(` follows, or `RANGE_PAT` if `..` / `..=` follows |
| `.` UIDENT | `IMPLICIT_MEMBER_PAT`, with arguments if `(` follows |
| `(` | unit, parenthesized (`PAREN_PAT`), one-element tuple, or tuple (`TUPLE_PAT`) |
| `[` | `SLICE_PAT`; an element `..` with an optional `BindingMode? LIDENT` is a `REST_PAT` |
| FLOAT | reported as `FLOAT_PATTERN` (error, spec/03 §3.14), parsed as `LITERAL_PAT` |

- A range end after `..` is parsed only if the current token can start a `RangeEnd`; `RANGE_PAT` with `..=` and no end is `EXPECTED_TOKEN`.
- A second `REST_PAT` in one slice pattern is reported as `MULTIPLE_REST_PATTERNS` (error).
- `CtorPatArgs` accept `LIDENT ':' Pattern` (`NAMED_PAT_ARG`) and `Pattern` elements in any order, optionally followed by a final `..` (`REST_PAT`); a `..` that is not last is `EXPECTED_TOKEN { ')' }`. Whether the constructor is a record or a tuple variant is checked by the resolver (impl/03).

### 2.5.9 Types

`type_(allow_fx)` follows spec/03 §3.15. `TypeNoFx` positions (return types of functions, closures, and function pointer types, and where-clause bounds) call `type_(false)`; there, `fn(..)` is parsed without an `effects` clause, so a following `effects` belongs to the enclosing construct. In `type_(true)`, an `effects` clause after a function pointer type is part of the type. In type context, `<` always opens generic arguments. A `_` type is accepted everywhere by the parser and rejected in signatures by validation (`INFER_IN_SIGNATURE`, §2.5.12).

### 2.5.10 Lists

All bracketed lists share one routine:

```rust
fn list(p: &mut Parser, open: SyntaxKind, close: SyntaxKind, sep: SepKind, elem: fn(&mut Parser) -> bool) {
    p.expect(open);
    if open == L_BRACE { p.eat_nl(); }
    while !p.at(close) && !p.at(EOF) {
        if !elem(p) { recover_in_list(p, close, sep); }        // §2.5.11
        match sep {
            SepKind::Comma => if !p.eat(COMMA) { break; },     // inside ( ) [ ] < >
            SepKind::Sep   => if p.eat(COMMA) { p.eat_nl(); } else if !p.eat(NEWLINE) { break; },   // inside { }
        }
        if open == L_BRACE && p.at(R_BRACE) { break; }
    }
    if open == L_BRACE { p.eat_nl(); }
    p.expect(close);
}
```

A trailing separator is accepted everywhere (spec/03 `List<X, S>`). A missing separator between two elements on the same line is `EXPECTED_TOKEN { ',' }` (or `{ ',', NEWLINE }` inside braces), and the loop continues with the next element when the current token can start one.

### 2.5.11 Error recovery

Recovery is deterministic and based on synchronization sets.

| Level | Synchronization set (stop **before** these tokens) | Used by |
|---|---|---|
| item | a token with `AFTER_LINE_BREAK` that can start an item (`@`, `///`, `pub`, `export`, `use`, `fn`, `unsafe`, `async`, `type`, `enum`, `trait`, `impl`, `const`, `extern`, and the contextual item keywords of §2.5.4 with their follow tokens), at bracket depth 0 relative to the start of recovery; EOF | `source_file`, trait/impl/interface/world bodies |
| statement | NEWLINE at the block's depth; the `}` that closes the current block; EOF | `block` |
| list | the list's separator at the list's depth; the list's closing bracket; NEWLINE when the list is a `{ }` list; EOF | `list` |
| clause | NEWLINE followed by a clause keyword (`where`, `borrows`, `effects`, `requires`, `ensures`) or by `{` | `fn_decl` |

- In an interface body with a missing `}`, a line-start `FnHead`, `TypeDecl`, `EnumDecl`, or `IfaceResource` is also a valid member, so recovery keeps it in the interface. In an interface-resource body, a line-start `FnHead` remains a member for the same reason.
- In a world body with a missing `}`, `export FnHead` is also a valid world item and remains inside the world, even though the same line can start a top-level exported function.
- While skipping, the parser skips balanced bracket groups as single units, so recovery never stops at a token inside a nested group and never consumes a closing bracket that belongs to an enclosing construct.
- Skipped tokens are wrapped in one `ERROR` node per recovery. A recovery reports no diagnostic of its own; the diagnostic that triggered it is the only one (no cascades).
- `expect(kind)` that fails reports `EXPECTED_TOKEN` with the current `expected` set and **does not consume** the current token. If the current token is NEWLINE, the code is `UNEXPECTED_NEWLINE` instead, with the `newline_causes` note (§2.4.11). At most one `EXPECTED_TOKEN`/`UNEXPECTED_NEWLINE` diagnostic is reported per token position; further failures at the same position are silent.
- **Operator-continuation recovery.** If a statement starts with a token that can only be a binary operator (`+`, `/`, `%`, `+%`, `*%`, `==`, `!=`, `<`, `<=`, `>`, `&`, `|`, `^`, `<<`; `and` and `or` cannot occur here because they are in C, and `-`, `*`, and `..` are excluded because `-x`, `*p = v`, and `..b` are valid statement starts) and the previous statement ends with an expression (an initializer, an assignment right-hand side, or an expression statement), the parser reports `UNEXPECTED_NEWLINE` at the NEWLINE that separates them, with the spec/02 §2.10.3 note and a structured fix that moves the operator to the end of the previous line. It then parses the operator and its right operand and wraps them in one `ERROR` node, so the right operand produces no further diagnostics (N-E-15).
- **Unclosed delimiters.** At EOF with open brackets, the parser reports one `EXPECTED_TOKEN` for the innermost unclosed bracket, with a secondary label at the opening bracket, and completes all open nodes.

### 2.5.12 Syntactic validation

Some rules of spec/03 depend on context that is awkward to track while parsing. They are checked by a validation pass over the finished CST (`validate(Parse) -> Vec<Diagnostic>`), which is part of the `parse` query, so they are reported even when no HIR is built (for example by `sai fmt --check`).

| Code | Where | Check |
|---|---|---|
| `EXPECTED_TOKEN`, `EXPECTED_EXPRESSION`, `UNEXPECTED_NEWLINE` | parser | §2.5.11 |
| `CHAINED_COMPARISON`, `CHAINED_RANGE` | parser | §2.5.6 |
| `CLAUSE_ORDER`, `DUPLICATE_CLAUSE` | parser | §2.5.4 |
| `BLOCK_IN_CONDITION` | parser | §2.5.6 |
| `NO_ASYNC_CLOSURE` | parser | §2.5.6 |
| `NO_AS_CAST` | impl/03 | the parser builds `CAST_EXPR` without a diagnostic (§2.5.6); the type checker reports the code (impl/03 §3.10.3) |
| `REDUNDANT_PUB_EXPORT`, `INVALID_EXPORT_TARGET` | parser | §2.5.4 |
| `NO_DOUBLE_COLON`, `USE_AND_OR` | parser | §2.5.2 |
| `MISSING_INITIALIZER`, `ITEM_IN_BLOCK`, `BASE_ARG_NOT_LAST`, `INVALID_TUPLE_INDEX`, `FLOAT_PATTERN`, `MULTIPLE_REST_PATTERNS`, `UNEXPECTED_FN_BODY`, `INTERFACE_FN_BODY`, `IMPLEMENTATION_LIMIT_EXCEEDED` | parser | §2.5.3–§2.5.8 |
| `MISSING_FN_BODY` | validation | a free function, or a function in an inherent impl, trait impl, or interface impl, without a body. A body is optional for trait functions (a default method). Interface functions, interface-resource functions, world function items, and extern items are `FnHead`s. Supplied interface, interface-resource, and world bodies report `INTERFACE_FN_BODY`; extern bodies report `UNEXPECTED_FN_BODY` (§2.5.4) |
| `INVALID_SELF_TYPE` | validation | a `self` parameter whose type is not syntactically `Self`, `borrow Self`, or `borrow mut Self` (spec/03 §3.5, D-TYPE-08; `owned Self` is accepted as a spelling of `Self`, D-OWN-01); `self` that is not the first parameter, or `self` in a function that is not inside an `impl`, a `trait`, an interface resource, or an `extern` block, is also `INVALID_SELF_TYPE`, with a specific message. A `self` parameter of an extern function is `INVALID_EXTERN_SIGNATURE` (spec/12 §12.7.4 item 2), reported by impl/03 §3.6.9 |
| `NAMED_ARGUMENT_ON_FUNCTION` | validation, and impl/03 | validation: a `NAMED_ARG` or `BASE_ARG` in a call whose callee ends with a `LIDENT` segment (by the case rule it cannot be a constructor). impl/03: a callee that resolves to anything other than a record constructor |
| `INVALID_ASSIGN_TARGET` | validation, and impl/03 | validation: the left-hand side is not a path, field, tuple index, index, or `*` expression (parentheses allowed). impl/03: a path that does not resolve to a local binding |
| `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` | parser | an enum with at least one variant with a shape and at least one `= INT`; reported once at the first explicit discriminant |
| `UNKNOWN_ATTRIBUTE` | validation | §2.10.1 |
| `UNKNOWN_ABI` | parser | `extern` STRING whose value is not `"wasm"` or `"C"` |
| `INVALID_EXTERN_BLOCK` | parser | an `extern "wasm"` block without a valid `module STRING`, or an `extern "C"` block without `unsafe` (spec/12 §12.7.1 item 1, §12.9.1 item 1); reported at the `extern` keyword. A block with an unknown ABI string is only `UNKNOWN_ABI` |
| `FEATURE_NOT_IMPLEMENTED` | parser for async functions in interfaces, interface resources, worlds, and extern blocks; validation for other Stage 5 async syntax | use of a staged feature before its implementation stage; interface async functions are Stage 5 (spec/01 §1.4.5, spec/11 §11.1.2, spec/13 §13.2.2), and native `extern "C"` blocks are Stage 7 (spec/01 §1.4.5, spec/12 §12.9.1) |
| `GENERIC_INTERFACE_ITEM` | parser for interface, interface-resource, and world function heads; validation for other interface items and interface-impl functions | generic parameters or a `where` clause where the interface rules prohibit them (spec/13 §§13.2.2, 13.3.1, 13.4.1) |
| `INTERFACE_FN_CLAUSE` | parser for interface, interface-resource, and world function heads; validation for interface-impl functions | a `borrows`, `requires`, or `ensures` clause, or an `unsafe` marker, where the interface rules prohibit them; an interface impl may add `requires` and `ensures` (spec/13 §13.2.2, §13.3.1; spec/12 §12.2.2 item 5) |
| `EFFECTS_ON_INTERFACE_FN` | parser for interface, interface-resource, and world function heads; validation for other interface functions | an `effects` clause on an interface function or resource member; world function heads follow the interface-function restrictions (spec/08 §8.6.1, spec/13 §§13.2.2, 13.4.1) |
| `WORLD_IMPORT_NEEDS_INTERFACE` | parser | a world `import` / `export` whose `WorldRef` is a `PKG_PATH` without an interface segment (`import wasi:http`), reported at the path (spec/03 §3.9, D-WIT-03). `import FnHead` and `export FnHead` are world function items and are valid. A whole package in `include` is `INVALID_WORLD_ITEM`, reported by [16](16-interfaces-and-wit.md) §16.8.1 |
| `INFER_IN_SIGNATURE` | validation | an `INFER_TYPE` anywhere in an item signature: parameter types, return types, field types, alias right-hand sides, generic bounds, impl headers, const types |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | validation | `pub` on an item of an impl whose target is `TraitRef for Type` or an interface path |
| `OR_PATTERN_BINDING_MISMATCH` | HIR lowering, and impl/03 | lowering: the alternatives bind different sets of names or use different binding modes (§2.9.13). impl/03: the same name has different types |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | impl/03 | needs name resolution |
| `MISPLACED_DOC_COMMENT` | parser | §2.6.4 |

---

## 2.6 Lossless concrete syntax tree

### 2.6.1 Green tree

The green tree is an immutable, position-independent, structurally shared tree. It is the single syntax artifact of a file; everything else (red nodes, AST views, the formatter's document model) is derived from it.

```rust
pub struct GreenNode(Arc<GreenNodeData>);
pub struct GreenNodeData {
    kind: SyntaxKind,
    text_len: TextSize,              // sum of the full lengths (with trivia) of all children
    children: Box<[GreenElement]>,
}
pub enum GreenElement { Node(GreenNode), Token(GreenToken) }

pub struct GreenToken(Arc<GreenTokenData>);
pub struct GreenTokenData {
    kind: SyntaxKind,
    text: SmolStr,                   // token text only; may be empty for NEWLINE, EOF, or an error-marked FSTRING_END
    leading: Box<[GreenTrivia]>,
    trailing: Box<[GreenTrivia]>,
    has_error: bool,                 // preserves TokenFlags::HAS_ERROR for AST value decoders
}
pub struct GreenTrivia { kind: TriviaKind, text: SmolStr }

pub struct NodeCache { /* hash-consing of tokens and of nodes with at most 3 children */ }
```

- Trivia is stored on tokens (leading and trailing lists), not as sibling elements (N-E-16). This makes every comment belong to exactly one token, which is what the formatter (impl/14) and semantic edits (impl/12) need in order to move comments together with the code they describe.
- Green tokens retain the lexer's `HAS_ERROR` bit for any token with a reported lexical error, including malformed literals and package paths. AST literal decoders use it to avoid treating malformed literal text as a valid value. Other lexer flags are not part of the persistent syntax tree.
- The tree builder interns tokens and small nodes through a `NodeCache` owned by the `parse` query invocation. Interning is an optimization only; equality of green nodes is structural.
- `text_len` of a token is the sum of the lengths of its leading trivia, its text, and its trailing trivia.

### 2.6.2 Red tree

```rust
#[derive(Clone)]
pub struct SyntaxNode(Rc<NodeData>);
struct NodeData {
    green: GreenNode,
    parent: Option<SyntaxNode>,
    index_in_parent: u32,
    offset: TextSize,                // absolute offset of the start of this node's text including leading trivia
}
pub struct SyntaxToken { parent: SyntaxNode, index_in_parent: u32, offset: TextSize }
pub enum SyntaxElement { Node(SyntaxNode), Token(SyntaxToken) }
```

- Red node handles and a compact node/child index are materialized when a red root is created. Structural traversal then clones cached `Rc` handles without allocating per child. The token-offset index is built lazily on the first `token_at_offset` call. Red nodes are not `Send`; a thread that needs a tree creates its own red root from the shared green root.
- `SyntaxNode::text_range()` excludes the leading trivia of the first token and the trailing trivia of the last token. `full_range()` includes them. Diagnostics and AST pointers use `text_range()`.
- Traversal API: `children`, `children_with_tokens`, `parent`, `ancestors`, `descendants` (pre-order), `first_token`, `last_token`, `token_at_offset(offset) -> TokenAtOffset` (none, single, or between two tokens), and `covering_element(range)`.

### 2.6.3 Trivia attachment

The lexer computes trivia attachment and the tree builder copies it into green tokens unchanged (N-E-16). For each token `t` (including EOF):

1. **Trailing trivia of `t`** is the maximal run of `Whitespace`, `LineComment`, and `BlockComment` trivia that directly follows `t` and contains no `LineBreak` trivia. A block comment that starts on the same line as `t` is trailing trivia of `t` even if it spans several lines. The run stops before the first `LineBreak`.
2. **Leading trivia of `t`** is all trivia between the end of the previous token's trailing trivia and `t`, including line breaks, blank lines, and comments on their own lines. For the first token of the file, this includes the `Shebang` trivia.
3. All trivia after the last real token is leading trivia of `EOF`.
4. A zero-width `NEWLINE` token has no trivia of its own. It is placed directly after the trailing trivia of the token that ends the line, so the line break and everything after it is leading trivia of the next real token.
5. `Skipped` trivia (§2.5.11) is never created by the lexer. When the parser demotes a token (`Event::DemoteToTrivia`), the token's text, together with its own leading and trailing trivia, is appended to the leading trivia of the next token in the token stream, as `LineComment` for a demoted doc comment.

Consequences used by the formatter and by tooling:

- `foo() // note` keeps `// note` on `)`'s trailing trivia, so the comment stays on the line of the statement it ends.
- A comment on its own line above an item is leading trivia of the item's first token. Moving the item moves its comments.
- No trivia is attached to a node; a node's leading comments are the leading trivia of its first token.

### 2.6.4 Doc comments

- A `DOC_OUTER` token sequence directly before an item, a field (`FieldDecl`), a variant (`VariantDecl`), a trait item, an impl item, an interface item, an interface resource item, a world item, or an extern item is parsed into a `DOCS` node, which is the first child of that item's node. The `DOCS` node contains the `DOC_OUTER` tokens in order; blank lines and ordinary comments between them are their trivia.
- `DOC_INNER` tokens at the start of a file are parsed into a `DOCS` node that is the first child of `MODULE_HEADER`.
- Any other `DOC_OUTER` or `DOC_INNER` (before a statement, at the end of a block, after the last item of a file, after an attribute, inside an expression list, or `//!` after the first item) is reported as `MISPLACED_DOC_COMMENT` (warning) and demoted to `LineComment` trivia (§2.6.3 item 5). Demoted doc comments play no further role; the formatter keeps their text unchanged.
- `DOCS` text is extracted by `ast::Docs::text()`: for each token, remove `///` or `//!` and then at most one leading space (spec/02 §2.3 item 5), and join the lines with LF. The CommonMark interpretation belongs to documentation tooling.

### 2.6.5 Node kinds

The node kinds are the following `SyntaxKind` values. Each corresponds to one nonterminal or one alternative of spec/03; the AST view of the same name (§2.7) gives typed access.

| Group | Kinds |
|---|---|
| file | `SOURCE_FILE`, `MODULE_HEADER`, `DOCS`, `ATTRIBUTE`, `ATTR_ARGS`, `ATTR_ARG`, `VISIBILITY` |
| items | `USE_DECL`, `USE_ALIAS`, `USE_ITEMS`, `USE_ITEM`, `FN_DECL`, `PARAM_LIST`, `SELF_PARAM`, `PARAM`, `RET_TYPE`, `WHERE_CLAUSE`, `WHERE_PRED`, `BORROWS_CLAUSE`, `PROVENANCE`, `EFFECTS_CLAUSE`, `EFFECT_SET`, `EFFECT_REF`, `REQUIRES_CLAUSE`, `ENSURES_CLAUSE`, `GENERIC_PARAM_LIST`, `TYPE_PARAM`, `CONST_PARAM`, `BOUND_LIST`, `TRAIT_BOUND`, `FN_SUGAR_BOUND`, `TYPE_DECL`, `STRUCT_DEF`, `FIELD_LIST`, `FIELD_DECL`, `ALIAS_DEF`, `REFINEMENT_CLAUSE`, `ENUM_DECL`, `VARIANT_LIST`, `VARIANT_DECL`, `TUPLE_SHAPE`, `RECORD_SHAPE`, `VARIANT_FIELD`, `DISCRIMINANT`, `DISTINCT_DECL`, `RESOURCE_DECL`, `TRAIT_DECL`, `ASSOC_TYPE_DECL`, `IMPL_DECL`, `ASSOC_TYPE_DEF`, `ITEM_LIST`, `CONST_DECL`, `INVARIANT_DECL`, `EFFECT_DECL`, `COVERS_CLAUSE`, `INTERFACE_DECL`, `IFACE_RESOURCE`, `WORLD_DECL`, `WORLD_IMPORT`, `WORLD_EXPORT`, `WORLD_INCLUDE`, `WORLD_REF`, `MOD_PATH`, `EXTERN_BLOCK`, `EXTERN_MODULE`, `TEST_DECL`, `PROPERTY_DECL` |
| statements | `BLOCK`, `LET_STMT`, `LET_ELSE`, `VAR_STMT`, `ASSIGN_STMT`, `ASSERT_STMT`, `EXPR_STMT` |
| expressions | `LITERAL`, `FSTRING_EXPR`, `FSTRING_INTERP`, `PATH_EXPR`, `SELF_EXPR`, `SELF_TYPE_EXPR`, `IMPLICIT_MEMBER_EXPR`, `UNIT_EXPR`, `PAREN_EXPR`, `TUPLE_EXPR`, `ARRAY_EXPR`, `BLOCK_EXPR`, `UNSAFE_BLOCK_EXPR`, `ASYNC_BLOCK_EXPR`, `IF_EXPR`, `CONDITION`, `LET_CONDITION`, `ELSE_BRANCH`, `MATCH_EXPR`, `MATCH_ARM_LIST`, `MATCH_ARM`, `MATCH_GUARD`, `LOOP_EXPR`, `WHILE_EXPR`, `FOR_EXPR`, `LOOP_LABEL`, `CLOSURE_EXPR`, `CLOSURE_PARAM_LIST`, `CLOSURE_PARAM`, `REGION_EXPR`, `RETURN_EXPR`, `BREAK_EXPR`, `CONTINUE_EXPR`, `BIN_EXPR`, `PREFIX_EXPR`, `RANGE_EXPR`, `FIELD_EXPR`, `TUPLE_INDEX_EXPR`, `AWAIT_EXPR`, `CALL_EXPR`, `ARG_LIST`, `ARG`, `NAMED_ARG`, `BASE_ARG`, `INDEX_EXPR`, `TRY_EXPR`, `GENERIC_ARG_LIST`, `TYPE_ARG`, `CONST_ARG`, `ASSOC_TYPE_BINDING` |
| patterns | `WILDCARD_PAT`, `BINDING_PAT`, `BINDING_MODE`, `AT_PAT`, `LITERAL_PAT`, `RANGE_PAT`, `TUPLE_PAT`, `PAREN_PAT`, `SLICE_PAT`, `REST_PAT`, `CTOR_PAT`, `CTOR_PAT_ARGS`, `NAMED_PAT_ARG`, `IMPLICIT_MEMBER_PAT`, `CONST_PATH`, `OR_PAT` |
| types | `PATH_TYPE`, `PATH_SEGMENT`, `SELF_TYPE`, `BORROW_TYPE`, `OWNED_TYPE`, `TUPLE_TYPE`, `PAREN_TYPE`, `INFER_TYPE`, `FN_PTR_TYPE` |
| names | `NAME` (a defining occurrence: one `LIDENT`, `UIDENT`, or `UNDERSCORE`), `NAME_REF` (a use occurrence) |
| errors | `ERROR`, `CAST_EXPR` (the recovery form of an `as` cast, §2.5.6; it corresponds to no spec/03 nonterminal) |

- `PATH_EXPR` holds one `NAME_REF` and an optional `GENERIC_ARG_LIST`. A dotted expression `a.b.C` is a chain of `FIELD_EXPR` nodes; whether a link is a module qualification, an associated item, a field, or a method is decided by the resolver (spec/03 §3.13.2). A method call is a `CALL_EXPR` whose callee is a `FIELD_EXPR`.
- `PATH_TYPE` holds `PATH_SEGMENT`s, each a `NAME_REF` with an optional `GENERIC_ARG_LIST`.
- Every node that defines a name has exactly one `NAME` child.

### 2.6.6 Tree invariants

The tree builder and the parser guarantee, and `debug_assert`-check in test builds:

1. **Losslessness.** Concatenating the leading trivia, text, and trailing trivia of all tokens of the root, in order, yields the input text exactly (after BOM removal; the BOM is restored from `SourceFile::had_bom` by tools that write files).
2. **Exactly one root.** The root is `SOURCE_FILE`, and its last token is `EOF`.
3. **Tokens in order.** Token offsets are nondecreasing. Zero-width `NEWLINE` tokens and error-marked zero-width `FSTRING_END` tokens may share an offset with adjacent tokens.
4. **Every token belongs to a node.** No token is a direct child of a token. `ERROR` nodes may contain any tokens and nodes.
5. **No empty nodes**, except `ARG_LIST`, `PARAM_LIST`, `CTOR_PAT_ARGS`, `FIELD_LIST`, `VARIANT_LIST`, `ITEM_LIST`, `MATCH_ARM_LIST`, `EFFECT_SET`, `USE_ITEMS`, and `BLOCK`, which always contain their brackets, and `ERROR` nodes created by `expect` failures, which may be empty (zero-width) to mark the position of a missing construct.
6. **Determinism.** The same text always produces the same green tree, bit for bit.

### 2.6.7 Node identity and reparsing

- A `SyntaxNodePtr` records the `FileId`, workspace-snapshot identity, pre-order node index in the red-tree index, `SyntaxKind`, and `TextRange` of a node. The node index distinguishes malformed zero-width nodes that share a kind and range and resolves in constant time when the same snapshot's red root is recreated. The public `parse(snapshot, file_id)` entry point binds its root to that snapshot after building the tree from the snapshot's source; the lower-level root-binding constructor is crate-private. An unbound syntax root gives pointers valid only for that red tree. A pointer resolves only against the same file root from the exact workspace snapshot, so any edit that creates a new snapshot invalidates it, even when another file changed. `AstId` has the same snapshot scope (§2.7.4); stable cross-edit identity is provided by `DefId` (§2.8.2) and, for tooling, by the symbol identities of impl/12.
- v0.1 always reparses the whole file after an edit (N-E-17). The `parse` query is per file, and the downstream queries (`ast_id_map`, `def_collector`, `hir_owner`) compare their outputs structurally, so an edit inside one function body does not invalidate the item tree or other owners (§2.8.3). Incremental reparsing of blocks is permitted as an optimization only if it produces a tree identical to a full reparse.

---

## 2.7 Typed AST views

### 2.7.1 The `AstNode` trait

```rust
pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}
pub trait AstToken: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(token: SyntaxToken) -> Option<Self>;
    fn syntax(&self) -> &SyntaxToken;
}
```

- An AST view is a newtype around a `SyntaxNode` of one kind (`struct FnDecl(SyntaxNode)`), or an enum over several kinds (`enum Expr { Literal(Literal), BinExpr(BinExpr), ... }`, `enum Item`, `enum Pattern`, `enum Type`, `enum Stmt`).
- The views and their accessors are generated at build time from a grammar description file (`sai.ungram`) that lists, for each node kind, its children with their roles. The generator also emits the node-kind list of §2.6.5, so the two cannot drift. The grammar description is checked against spec/03 by the conformance tests (impl/15).
- **Current implementation slice.** `AstNode` and `AstToken` include typed views for source items and the `Item`, `Stmt`, `Pattern`, `Expr`, and `Type` unions, plus test/property declarations, functions, parameters, blocks, core statements, control-flow and postfix expressions, call arguments, type constructors and generic arguments, function-pointer signatures, casts, range endpoints, core recursive pattern forms, binary operands, tuple indexes, literal tokens, format specs, and f-strings. Concrete node wrappers and `AST_NODE_KINDS` are generated from `sai.ungram`; the build script checks that the node inventory matches `SyntaxKind` and validates declared role parents and target AST types. `child` roles select by a zero-based position among direct children of the declared AST type, while `child_before` and `child_after` constrain selection to one side of a direct token boundary; `child_before_any` and `child_after_any` accept a pipe-separated set of boundary token kinds. These child roles generate optional node accessors. `children_before` generates an iterator over matching direct children before a token boundary. `children` roles generate iterators over repeated child nodes, and `token` roles generate optional typed token accessors. Generated structural roles currently cover source-file items and test/property declarations, function signatures and bodies, parameters and return types, block statements, let/var/assignment/assert/expression statements, if/match/loop/while/for/closure/region/block expressions, their conditions/arms/parameters, paths, ranges, calls, field/tuple/index/try/await expressions, tuple/array elements, call argument forms, f-string interpolation expressions/specs, type paths and constructors, generic arguments, function-pointer parameters and return types, casts, alternation, `@`, binding, tuple, parenthesized, slice, constructor, named-field, rest, implicit-member and constant-path patterns, range-pattern endpoints, and binary operands. `BinExpr::op()`, `AssignStmt::op()`, and `PrefixExpr::op()` return both the direct operator token and its typed operator kind. `BindingMode::kind()` identifies explicit `var`, `borrow`, and `borrow mut` modes. `BorrowType::mut_token()` exposes the explicit borrow mutability marker, `OwnedType::owned_token()` exposes the ownership spelling, and `FnPtrType::unsafe_token()` exposes function-pointer safety. `LiteralExpr::token()` returns its direct token as one of the `LiteralToken` variants, including boolean keywords. `LiteralPat::{minus_token, value_token}` expose the optional direct sign and literal-value tokens. Token-relative selection keeps a later same-type child from taking the role of an earlier missing child during recovery. Roles for other items and expressions and spec/03 conformance checks remain incomplete. `IntLit::value`, `FloatLit::{value_f64, value_f32}`, `TupleIndexExpr::index`, `StringLit::value`, `ByteStringLit::value`, `CharLit::value`, `ByteLit::value`, `FStringExpr::pieces`, and `FormatSpec::{name, arg}` are implemented; `StringLit::text` still exposes raw token text. The compact red-node index is materialized at root creation so structural accessors can return cached handles without allocating; the token-offset index remains lazy.
- **Current item-declaration roles.** `TypeDecl` exposes its name, optional generic parameters, and either its record or alias definition. Record definitions expose their optional where clause and field list; fields expose docs, repeated attributes, their direct `pub` marker, name, and type. Alias definitions expose their type and optional refinement clause, whose expression is separately accessible. All node-valued accessors remain optional for incomplete syntax, while repeated fields and attributes use iterators.
- **Enum declaration roles.** Enums expose their name, generic parameters, optional where clause, and variant list. Variants expose docs, attributes, their name, optional tuple or record shape, and optional discriminants; tuple shapes iterate types, record shapes iterate named fields, and discriminants expose the integer token and optional minus token separately.
- **Distinct and resource roles.** Distinct declarations expose their name, generic parameters, optional base type and refinement, plus `inner_pub_token()` for the constructor-visibility marker. Resources expose their name and optional field list; the shared `FieldDecl` accessors describe resource fields.
- **Const, invariant, and effect roles.** Constants expose name, type, and initializer; invariants expose their name and predicate list; effects expose their name and optional covers clause. Covers clauses and function effects clauses expose their effect sets, which iterate effect references, whose segments are individually accessible.
- **Trait and impl roles.** Traits expose their name, generic parameters, bounds, optional where clause, and item list. Item lists provide separate iterators for functions, associated type declarations, and associated type definitions. Impl blocks expose generic parameters, where clauses, item lists, and target accessors that distinguish trait references/`for` types, inherent types, and interface paths; unsafe markers remain directly accessible.
- **Interface, world, and extern roles.** Interfaces expose their name and item list; the shared item list iterates interface types, enums, resources, and functions. Worlds iterate import/export/include nodes, whose references distinguish package-path tokens from module paths and whose function items are accessible separately. `ModPath::segments()` preserves the ordered names in interface implementation and world-reference paths. Extern blocks expose ABI and module string tokens, their item list, and an optional unsafe marker; the module node exposes its string token.
- **Function signature and clause roles.** Functions expose generic parameters in source order as `GenericParam::{Type, Const}`, unsafe/async markers, self parameters alongside named parameters, return types, where/borrows/effects clauses, and repeated requires/ensures clauses. Where predicates and generic bound lists expose ordered `Bound::{Trait, FnSugar}` iterators as well as typed convenience iterators. Bounds expose paths, parameter and return types, and effect caps. Borrow provenances expose their name segments, and contract clauses expose their expressions.
- **Module header and use roles.** Source files expose an optional module header with its inner docs and unsafe marker. Use declarations expose docs, visibility, package path, optional module alias, and selected items; each selected name and optional rename remain distinct.
- **Item metadata and attribute roles.** Declarations expose their outer docs and repeated attributes, with visibility accessors on syntax forms that permit it. Attributes expose their names and optional argument lists; argument lists iterate nested arguments, which distinguish an argument name, nested calls, an identifier value after `=`, and direct string or integer tokens. Attributes on parameters remain reachable even when the parser reports their target as invalid.
- **Pattern binding roles.** `BindingPat` exposes an optional `BindingMode`; explicit mode nodes map to `var`, `borrow`, or `borrow mut` through `BindingMode::kind()`. A missing mode node represents the ordinary by-value binding. Slice-rest bindings use the same accessor path.
- **Implicit-member expressions.** `ImplicitMemberExpr::name()` exposes the direct variant name in `.Variant` expressions, which lower to `ExprKind::ImplicitMember`.
- **Const-argument tokens.** `ConstArg::minus_token()` and `ConstArg::value_token()` expose the optional direct sign and the integer, character, or boolean value token. The value token remains optional for an incomplete recovered argument.
- **Type modifier roles.** Borrow types expose their optional `mut` token; `owned` types expose their source marker; and function-pointer types expose their optional `unsafe` marker. These direct-token accessors preserve the distinction between safe and unsafe function pointers and between shared and mutable borrows.
- **Let-else role.** `LetElse::block()` exposes the block executed when its pattern fails. It remains optional on a recovered incomplete tree.

### 2.7.2 Accessors

- Every **structural role accessor** returns an `Option` (or an iterator), because the tree may be incomplete: `FnDecl::name() -> Option<Name>`, `FnDecl::params() -> Option<ParamList>`, `FnDecl::body() -> Option<Block>`, `BinExpr::lhs() -> Option<Expr>`, `BinExpr::op() -> Option<(SyntaxToken, BinOp)>`, `BinExpr::rhs() -> Option<Expr>`, `AssignStmt::op() -> Option<(SyntaxToken, AssignOp)>`, and `PrefixExpr::op() -> Option<(SyntaxToken, PrefixOp)>`. Token text, decoded literal values, and scalar predicates may return their value directly.
- When two children have the same kind, the accessor names the role, and the child is found by position (`BinExpr::lhs` is the first `Expr` child, `rhs` the second; `RangeExpr::start` is the `Expr` child before the operator token and `end` the one after it).
- Composite tokens: `BinExpr::op()` returns `BinOp::Shr` for a glued `SHR` token and maps the `AMP2` and `PIPE2` recovery forms to logical `And` and `Or`, while retaining the token spelling. `AssignStmt::op()` maps the glued `SHR_EQ` token to `AssignOp::Shr`. `COLON2` is separate: it is the path-access recovery form for `::`, reported as `NO_DOUBLE_COLON` and interpreted as `.`.
- Structural accessors never panic and never allocate except for iterators. Literal value decoders do not panic; they may allocate when removing numeric separators or when their documented `Cow` result must own decoded text.

### 2.7.3 Literal values

Value decoders run the same scanning functions as the lexer (§2.3.2), in value mode:

| View | Method | Result |
|---|---|---|
| `IntLit` | `value() -> Option<u128>` | `None` if the literal has `HAS_ERROR` or does not fit in `u128`. The range check against the literal's type is done by impl/03, which reports `LITERAL_OUT_OF_RANGE`; a value that does not fit in `u128` is out of range for every SAI integer type. |
| `FloatLit` | `value_f64() -> Option<f64>`, `value_f32() -> Option<f32>` | correctly rounded (roundTiesToEven) conversion of the decimal text without `_`; `±inf` results are returned as such and turned into `LITERAL_OUT_OF_RANGE` by impl/03 |
| `StringLit` | `value() -> Cow<str>` | escapes decoded, line continuations removed, multiline indentation stripped; invalid escapes, inconsistent indentation, and forbidden bidi controls are replaced by U+FFFD (they were already reported); raw and multiline line endings are retained as written |
| `ByteStringLit` | `value() -> Cow<[u8]>` | as above, bytes; invalid escapes, non-ASCII characters, and forbidden bidi controls are replaced by UTF-8 bytes for U+FFFD |
| `CharLit` | `value() -> char` | U+FFFD if invalid |
| `ByteLit` | `value() -> u8` | `0` if invalid |
| `FStringExpr` | `pieces() -> impl Iterator<Item = FStringPiece>` | `Text(Cow<str>)` with `{{` and `}}` decoded, or `Interp { expr: Option<Expr>, spec: Option<FormatSpec> }` |
| `FormatSpec` | `name() -> &str`, `arg() -> Option<u128>` | |
| `TupleIndexExpr` | `index() -> Option<u32>` | `None` if the digits do not fit in `u32`; impl/03 then reports `TUPLE_INDEX_OUT_OF_RANGE` ([spec/06](../spec/06-expressions-and-statements.md)) |

**Current implementation slice.** Every accessor in this table is implemented. `LiteralExpr::token()` returns a `LiteralToken` variant for each numeric, string, character, byte, and boolean literal, so callers can use the corresponding typed token's value decoder directly. Integer values use checked accumulation without allocating; float values remove `_` into a temporary string only when the spelling contains separators, then parse directly to the requested IEEE width. Tuple indexes accept only plain decimal spellings without leading zeroes and return `None` when they do not fit in `u32`. Character, byte, string, and byte-string values use the lexer's shared escape scanner. String and byte-string values borrow their content when no transformation is needed; byte-string replacement characters are returned as UTF-8 bytes. Multiline strings retain internal line endings as written after removing the boundary line breaks, and literal C0 controls and foreign whitespace are preserved. `FStringExpr::pieces()` decodes escapes and doubled braces, merges adjacent text pieces, and returns each interpolation's direct expression and optional `FormatSpec`; `FormatSpec::arg()` uses checked integer decoding and returns `None` when an argument is absent, malformed, or does not fit in `u128`. Invalid character and byte literals produce U+FFFD and `0`, respectively.

### 2.7.4 `AstPtr` and `AstIdMap`

- `AstPtr<N>` is a typed `SyntaxNodePtr`. HIR source maps (§2.8.4) store `SyntaxNodePtr`s because one map can refer to heterogeneous syntax-node kinds; these pointers are valid for the snapshot the HIR was built from.
- `AstIdMap` (query `ast_id_map(FileId)`) assigns each **item-like node**—a source node that can become a `DefId` or is stored in the item tree with its own source identity—an `ErasedAstId(u32)`, which is its index in a pre-order traversal of item-like nodes only. This includes `USE_DECL`, whose item-tree entry has an AstId although a use declaration defines no name. Expressions, statements, and patterns never get an `AstId`; generic parameters do. Closure and async-block `DefId`s are derived from owner-local ordinals and do not have an `AstId`.
- Because only item-like nodes are numbered, an edit inside a function body does not change the `ErasedAstId` indices of the file's item-like nodes. The typed `AstId<N>` also carries its file and workspace-snapshot scope, so an ID cannot be reused with any map from a new snapshot, even if this file's source is unchanged. Adding or removing an item shifts the erased indices of later items; the `DefId`s of those items do not change, because `DefId`s are derived from `DefPath`s, not from `AstId`s (§2.8.2).
- `AstId<N>` is a typed `ErasedAstId` scoped to its file and snapshot. `ast_id_map.get(id) -> Option<AstPtr<N>>` and `ast_id_map.ast_id(&node) -> Option<AstId<N>>` return `Some` for item-like nodes and IDs from the current map; IDs from another file or snapshot and non-item nodes are rejected. `AstIdMap` requires a snapshot-bound `SOURCE_FILE` root; roots created with `new_root` are tree-local and cannot produce reusable `AstId`s.

**Current implementation slice.** `parse(snapshot, file_id)` builds a lossless tree from the snapshot source, binds its root to the snapshot identity, and returns lexical and parser diagnostics in source order. `GreenNode` has a deterministic structural `StableHash` implementation that includes token text, error metadata, trivia, node kinds, and child order while traversing iteratively; it omits allocation and cache metadata. Diagnostics convert to `DiagnosticTemplate` values that retain source byte ranges and semantic fields while dropping snapshot-specific file references. `SyntaxQueries::green_parse(FileKey)` memoizes a `GreenParse` containing that tree, diagnostic templates, and stable fix recipes; `SyntaxQueries::parse` validates the interner and raw source bytes, then rebinds the result to the requesting snapshot and recreates snapshot-validated fixes. The ephemeral red root used for unknown-attribute analysis is query-local, and its pointers and IDs do not escape. Template conversion rejects contextual diagnostic state and structured data deeper than 128 values. Lower-level `Parser` and `build_green_tree` use remains unbound. `SyntaxNodePtr::new` and `AstPtr::new` are fallible; for snapshot-bound roots they reject a `FileId` that does not match the root, while an unbound tree uses the caller-supplied file only as a pointer namespace. Pointers store the file, workspace-snapshot identity, pre-order red-node index, kind, and range, so the node index disambiguates identical kinds and ranges without retaining every ancestor path. Roots created with `new_root` instead receive a tree-local identity. `AstPtr<N>` adds a typed AST view. `AstId<N>` carries its file and snapshot identity along with its erased index, so a same-kind ID from another map cannot accidentally resolve to an item in this map; only the erased numeric index is stable across an edit that preserves item order. `AstIdMap` accepts only a snapshot-bound `SOURCE_FILE` root and assigns pre-order IDs to source items and their item-like children, including generic parameters, `USE_DECL`, and `EXTERN_BLOCK` because the item tree stores them. It rejects a mismatched file, non-file root, or unbound root at construction and returns `None` for nodes from another snapshot, an out-of-range ID, or a kind mismatch.

**Implementation update (IR-200).** `SyntaxQueries::ast_id_map` memoizes the ordered kinds of item-like green nodes by stable `FileKey`. `AstIdMap` checks the sequence against the current snapshot-bound red root before creating `SyntaxNodePtr`s, so its returned pointers and typed IDs remain scoped to that root while unchanged item-like order is reusable across edits.

---

## 2.8 HIR

### 2.8.1 Role

The HIR is the desugared, owner-partitioned representation of a package's source. It is produced from AST views without any semantic information and is the input of name resolution and type checking ([03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md)) and of MIR building ([04-mir.md](04-mir.md)).

- **Before resolution.** HIR is built before name resolution (N-E-18). Paths in HIR are unresolved name sequences. Resolutions are stored by impl/03 in side tables keyed by `HirId`, never in the HIR itself, so that a change in an imported module does not change the HIR of an importing module.
- **Per owner.** HIR is partitioned into owners (§2.8.2). The query `hir_owner(OwnerId)` lowers one owner from the AST, so an edit inside one owner's source invalidates only that owner's HIR.
- **Desugared.** The constructs of §2.9 are rewritten into a smaller set of node kinds. Nodes that the desugaring creates refer to core items through `LangItem` paths, never through names, so that user code cannot shadow them (hygiene, N-E-19). Bindings that the desugaring creates have synthetic names that no user identifier can spell.
- **Total.** Lowering never fails. Missing or erroneous syntax is lowered to `Err` nodes (§2.8.6).

### 2.8.2 Identifiers

```rust
pub struct PackageId;                           // opaque session-local package identity (impl/01)
pub struct DefIndex(u32);
pub struct DefId;                               // package + index; local DefIds retain a private table scope
pub struct LocalDefId;                           // opaque package- and definition-table-scoped local DefId

pub struct DefPath { pub package: PackageId, pub data: Vec<DisambiguatedDefPathData> }
pub struct DisambiguatedDefPathData { pub data: DefPathData, pub disambiguator: u32 }
pub enum DefPathData {
    Module(Symbol),                  // one per module path segment; the root module has no component
    TypeNs(Symbol),                  // struct, alias, refined type, enum, distinct, resource, trait, interface resource
    ValueNs(Symbol),                 // fn, const, extern fn
    Interface(Symbol), World(Symbol), Effect(Symbol),
    Field(Symbol), Variant(Symbol),
    AssocType(Symbol), AssocFn(Symbol),
    TypeParam(Symbol), ConstParam(Symbol),
    Impl { header_hash: u64 },       // see "Impl disambiguation" below
    DerivedImpl { trait_name: Symbol },
    Invariant(Symbol),               // the type name as written after `invariant`; a child of the module
    ExternBlock { abi: Symbol, module: Option<Symbol> },
    Test(SmolStr), Property(SmolStr),
    Closure, AsyncBlock,             // disambiguator = pre-order index within the enclosing owner
    WorldItem { import: bool, name: Symbol },
    WorldInclude(Symbol),            // the text of the WorldRef of an `include` (impl/16 §16.5.3)
}
pub struct DefPathHash(pub u128);              // stable fingerprint of a DefPath

pub struct OwnerId { pub def_id: LocalDefId }
pub struct ItemLocalId(u32);                   // 0 is the owner node itself
pub struct HirId { pub owner: OwnerId, pub local_id: ItemLocalId }
pub struct BodyId { pub owner: OwnerId }       // an owner has at most one body
pub struct BindingId(u32);                     // local binding within an owner's body, in pre-order of binding sites
```

**Definitions.** Every item-like node (§2.7.4) with a `NAME` gets a `DefId`, and so do generic parameters, closures, async blocks, and derived impls. A `use` declaration does not define anything; its bindings are import entries of impl/03.

**Owners.** The owners are: every item except `use` and `extern` blocks; every trait item and impl item; every extern item; every interface item and interface resource item; every world item; every derived impl (§2.9.17); and every invariant declaration. Fields, variants, generic parameters, closures, and async blocks are **not** owners: their HIR nodes belong to the enclosing owner. Closures and async blocks still have `DefId`s, because the type checker and monomorphization need to name their types and instances.

**`DefPath` construction.** A `DefPath` is built from the module path (one `Module` component per segment, derived from the file path by impl/03 §3.2) followed by the components of the lexical nesting of the definition. Except for closures and async blocks, `disambiguator` is 0 unless two definitions under the same parent have equal `data`, in which case those definitions are numbered 0, 1, 2, … in source order. For closures and async blocks, the disambiguator is instead their shared pre-order index among both kinds of nested definition syntax in the enclosing owner. The synthetic async block of an `async fn` with a body participates at ordinal 0, as if introduced at the function head; explicit closure and async-block syntax receives later ordinals in source preorder. Each path component remains nested below its nearest lexical closure or async block, but its ordinal is independent of its parent; a preceding closure or async block can therefore shift the ordinals of later ones anywhere in that owner. Syntax belonging to a nested item owner is excluded.

**Impl disambiguation** (N-E-20). `header_hash` is the low 64 bits of the stable hash of the impl header's token sequence: the kinds and texts of all tokens from `impl` up to (not including) the `{` of the item list, excluding trivia and NEWLINE tokens. So an impl keeps its `DefPath` when other impls are added, removed, or reordered, and when its body changes. Changing the header (for example adding a bound) gives the impl a new identity.

**`DefPathHash`** is a 128-bit stable hash (SipHash-1-3 with a fixed zero key, 128-bit output) of the package identity string defined by [package/01-package-contract.md](../package/01-package-contract.md) (namespace, name, and compatibility line) followed by the components of the `DefPath`, each encoded as a tag byte, the length-prefixed UTF-8 of its symbol or string, and the little-endian `u64` hash or `u32` disambiguator. It is independent of `DefIndex`, file names (beyond the module path), and source positions. `DefPathHash` is the key for every persistent artifact (incremental caches, `.sai/index` entries of D-TOOL-02, and mangled symbol names in impl/06); `DefIndex` is never persisted.

The canonical package key is encoded first as a length-prefixed UTF-8 string. The stable component tags are:

| Tag | `DefPathData` |
|---|---|
| `0x01` | `Module` |
| `0x02` | `TypeNs` |
| `0x03` | `ValueNs` |
| `0x04` | `Interface` |
| `0x05` | `World` |
| `0x06` | `Effect` |
| `0x07` | `Field` |
| `0x08` | `Variant` |
| `0x09` | `AssocType` |
| `0x0A` | `AssocFn` |
| `0x0B` | `TypeParam` |
| `0x0C` | `ConstParam` |
| `0x0D` | `Impl` |
| `0x0E` | `DerivedImpl` |
| `0x0F` | `Invariant` |
| `0x10` | `ExternBlock` |
| `0x11` | `Test` |
| `0x12` | `Property` |
| `0x13` | `Closure` |
| `0x14` | `AsyncBlock` |
| `0x15` | `WorldItem` |
| `0x16` | `WorldInclude` |

Every component ends with its little-endian `u32` disambiguator. `Impl` encodes its little-endian `u64` header hash before that value. `ExternBlock` encodes its ABI string, a one-byte module-presence flag, and the optional module string. `WorldItem` encodes a one-byte import flag before its name. Components with no string or hash payload encode only their tag and disambiguator. These tag assignments are persistent format and MUST NOT be reused or renumbered.

`Symbol::ERROR` is not a source string. In a symbol payload position, it is encoded as the length `u64::MAX` with no following bytes; no Rust string can have that byte length, so this cannot alias a real symbol. The sentinel is accepted only for an already-diagnosed or missing name.

**`DefIndex` assignment.** `LocalDefId`s are interned from `DefPath`s by the query system, so a definition keeps its `LocalDefId` across revisions as long as its `DefPath` exists. Foreign `DefId`s are read from the dependency's metadata, where definitions are numbered in ascending `DefPathHash` order.

In the reference implementation, each `LocalDefId` carries its package and the identity of the package's session-local definition table in addition to its `DefIndex`. This prevents an index from one table from resolving to an unrelated definition in another table. `to_def_id()` uses the package stored in the local ID.

A local `DefId` retains that definition-table identity so two accidentally distinct tables for the same package cannot compare as the same definition. A foreign `DefId` is created from its metadata package and `DefIndex` and has no local-table identity.

`DefPathInterner::intern_nested` appends a `Closure` or `AsyncBlock` component to its lexical parent's path, using the preorder disambiguator assigned within the owner. It rejects other component kinds so callers cannot bypass symbol-session validation for named path components.

`DefPathInterner::intern_children` atomically appends a source-ordered batch of caller-disambiguated definition components below one owner. `intern_assoc_items`, `intern_variants`, `intern_fields`, and `intern_generic_param` provide narrower helpers where their component kinds have additional invariants.

**`HirId` assignment.** Within an owner, `ItemLocalId`s are assigned in pre-order of HIR node creation during lowering, starting with 0 for the owner node. Desugared nodes are numbered at the position of the construct they replace, in the order in which §2.9 lists their children. The numbering depends only on the owner's own source, so an edit inside one owner never changes another owner's `HirId`s.

**`BindingId`.** Every `PatKind::Binding`, every body parameter, every closure parameter, and every synthetic binding of a desugaring gets a `BindingId`, numbered in pre-order within the owner. The resolver maps each path use of a local to a `BindingId` (impl/03).

### 2.8.3 Stability rules for incrementality and symbol identity

| Change | Effect on identifiers |
|---|---|
| Edit inside a function body (no closures added or removed before an existing one) | That owner's `HirId`s and `BindingId`s may change. No `DefId` or `DefPath` changes; item-like nodes keep their `ErasedAstId` indices. Typed `AstId`s are newly scoped to the new workspace snapshot. The `ItemTree` (§2.8.7) is unchanged. |
| Add or remove a closure in a body | `Closure` disambiguators of later closures in the same owner shift. Other owners are unaffected. |
| Add, remove, or reorder items in a file | The `ErasedAstId` indices of later item-like nodes shift, and typed `AstId`s are newly scoped to the new snapshot; `DefPath`s and `DefId`s of other items do not change. |
| Rename an item | The item gets a new `DefPath`. Symbol identity across the rename is preserved by impl/12 through `.sai/index` (D-TOOL-02), which maps the old `DefPathHash` to the persisted symbol ID. |
| Move an item to another module | New `DefPath`; same treatment as a rename. |
| Edit an impl header | The impl and its items get new `DefPath`s. |
| Edit whitespace or comments | No `DefId`, `DefPath`, `HirId`, `BindingId`, or `ErasedAstId` index changes (lowering ignores trivia). Typed `AstId`s are newly scoped to the new snapshot. Spans change, and are recomputed; spans are never part of a query key. |

Spans are stored in HIR nodes but are excluded from the equality used for early cutoff of downstream queries; queries that need positions read them from the source map (§2.8.4) and depend on it explicitly.

### 2.8.4 Spans and source maps

```rust
pub struct Span { pub file: FileId, pub range: TextRange, pub ctxt: SpanCtxt }
pub enum SpanCtxt { Root, Desugared(DesugaringKind) }
pub enum DesugaringKind {
    WhileLoop, WhileLet, IfLet, ForLoop, ForAwait,   // §2.9.2–§2.9.6
    VarStmt, AsyncFn, AsyncBlock, Await, TryOp, CompoundAssign, Assert, FormatString, Derive,
}

pub struct HirSourceMap {
    owner: OwnerId,
    node_to_syntax: IndexVec<ItemLocalId, SyntaxNodePtr>,  // total: every HIR node has a source node
    syntax_to_nodes: FxHashMap<SyntaxNodePtr, SmallVec<[ItemLocalId; 1]>>,
    binding_to_syntax: IndexVec<BindingId, SyntaxNodePtr>,
    diagnostics: Vec<Diagnostic>,
}
```

The fields are private. Read-only accessors expose `owner()`, `node_to_syntax()`, `syntax_to_nodes()`, `binding_to_syntax()`, and `diagnostics()`.

- Every HIR node has a span. The span of a desugared node is the range of the source construct it was generated from, with `SpanCtxt::Desugared(kind)`. For example, every synthetic node of a `for` loop has the range of the `for` expression (or of the iterator expression, for the `into_iter` call) and the context `ForLoop`. The synthetic `Await` in `for await` is the exception: its context is `ForAwait`, but its span range is the `await` keyword so async-context diagnostics point at that keyword; its source-map entry still points to the complete `ForExpr`.
- Diagnostics MUST be reported against the user-visible construct. The diagnostics layer (impl/11) uses the desugaring context to choose the wording ("this `for` loop's iterator …") and to suppress diagnostics that make sense only for user-written code (for example `UNREACHABLE_PATTERN` on a synthesized arm, §2.9.1).
- `node_to_syntax` is total. Several HIR nodes may map to one syntax node (all nodes of a desugaring); a syntax node may map to no HIR node (for example a `PAREN_EXPR`, which is not lowered to a node of its own, maps to the node of its content).
- The source map is a separate query (`hir_source_map`), so that HIR consumers that do not need positions are not invalidated by edits that only move text.

**Current implementation slice.** `HirSourceMapBuilder` binds a map to one owner and one parsed file root, requiring the supplied `AstIdMap` to match both that root and the supplied workspace snapshot. It maps the owner node to local ID zero, checks every added node and binding against the same file and syntax-tree identity, and builds both node-to-syntax and syntax-to-node mappings; repeated HIR nodes may therefore share one syntax pointer. Its `add_node_with_span`, `add_binding_with_span`, and `span_for` methods create spans only for nodes validated against that root, preserve the caller's `SpanCtxt`, and couple node/binding ID allocation to source-map entries. `text_for_syntax` returns the exact decoded source slice after the same tree validation, which lets `assert` retain its condition spelling. Diagnostics are bound to that same snapshot and rejected if their locations or existing snapshot binding do not match. The completed map exposes read-only accessors so callers cannot break the correspondence between its forward and reverse mappings. Owner-level APIs in `sai_hir::lower` assemble the supported top-level function, type, resource, enum, trait, impl, const, invariant, effect, test, property, interface, world, and extern-block declarations; associated trait, impl, and interface members; world items; extern items; and `DerivedImpl` owners. `lower_derived_impl_owners` creates one synthetic owner under each supported type's DefPath for every currently validated derive. `HirLoweringContext` supplies the current AST map/root/snapshot, arena, symbol interner, and DefPath interner. Shared owner-entry validation checks the ItemTree's retained snapshot identity, item kind, visibility, and complete recognized raw-attribute values/spans/pointers before lowering. Each API receives the complete expected `DefPath`, verifies that the path resolved from `OwnerId` matches it exactly, and checks the relevant semantic component and canonical source-order disambiguator. Callers derive the parent path because ItemTree does not record it. The owner constructors return `HirOwner` plus `HirSourceMap`; they are not yet wired into a query system. The reverse map currently uses the standard-library `HashMap` and `Vec` in place of the planned `FxHashMap` and `SmallVec`; the workspace has no existing dependencies for those types, and this choice avoids adding dependencies before profiling establishes a need.

### 2.8.5 Data structures

HIR nodes are allocated in a typed arena owned by the query session. `HirOwner` is a logical owner whose node and body fields borrow from that arena; it does not contain the arena that stores those values. The session arena outlives its memoized HIR owners and reclaims its allocations together. This avoids making `HirOwner` self-referential. `'hir` is the arena-borrow lifetime.

```rust
pub struct HirOwner<'hir> {
    pub id: OwnerId,
    pub node: OwnerNode<'hir>,
    pub body: Option<&'hir Body<'hir>>,
    pub raw_attrs: &'hir [RawAttr],           // validated by the `attrs` query (§2.10)
    pub diagnostics: Vec<Diagnostic>,         // lowering diagnostics (§2.9.13, §2.10.3)
}

pub enum OwnerNode<'hir> {
    Item(&'hir Item<'hir>),
    TraitItem(&'hir TraitItem<'hir>),
    ImplItem(&'hir ImplItem<'hir>),
    ExternItem(&'hir ExternItem<'hir>),
    InterfaceItem(&'hir InterfaceItem<'hir>),  // impl/16 §16.5.1
    WorldItem(&'hir WorldItem<'hir>),          // impl/16 §16.5.1
    DerivedImpl(&'hir DerivedImpl),           // §2.9.17
}

pub struct Name { pub sym: Symbol, pub span: Span }   // Symbol: interned NFC string; Symbol::ERROR for missing or reserved names
pub enum Visibility { Private, Pub, Export }
pub enum Safety { Safe, Unsafe }
pub enum Mutability { Not, Mut }

pub struct Item<'hir> { pub owner_id: OwnerId, pub name: Name, pub vis: Visibility, pub kind: ItemKind<'hir>, pub span: Span }

pub enum ItemKind<'hir> {
    Fn { sig: FnSig<'hir>, generics: Generics<'hir>, has_body: bool },
    Struct { generics: Generics<'hir>, fields: &'hir [FieldDef<'hir>] },
    Alias { generics: Generics<'hir>, ty: &'hir Ty<'hir> },
    Refined { base: &'hir Ty<'hir> },                         // the predicate is the owner's body, with `self` bound
    Enum { generics: Generics<'hir>, variants: &'hir [Variant<'hir>] },
    Distinct { generics: Generics<'hir>, pub_ctor: bool, base: &'hir Ty<'hir>, has_predicate: bool },
    Resource { fields: &'hir [FieldDef<'hir>] },
    Trait { safety: Safety, generics: Generics<'hir>, supertraits: &'hir [Bound<'hir>], items: &'hir [OwnerId] },
    Impl { safety: Safety, generics: Generics<'hir>, kind: ImplKind<'hir>, items: &'hir [OwnerId] },
    Const { ty: &'hir Ty<'hir> },                             // the initializer is the owner's body
    Invariant { target: Path<'hir> },                         // the predicates are `Body::invariant_preds`
    Effect { covers: &'hir [EffectRef<'hir>] },
    Interface { items: &'hir [OwnerId] },
    World { items: &'hir [OwnerId] },
    ExternBlock { safety: Safety, abi: ExternAbi, module: Option<Symbol>, items: &'hir [OwnerId] },
    Test { name: &'hir str },
    Property { name: &'hir str, params: &'hir [FnParam<'hir>] },
}
pub enum ExternAbi { Wasm, C, Err }
pub enum ImplKind<'hir> {
    Inherent { self_ty: &'hir Ty<'hir> },
    Trait { trait_ref: Path<'hir>, self_ty: &'hir Ty<'hir> },
    Interface { path: Path<'hir> },                           // ModPath; resolved by impl/03
}

pub struct TraitItem<'hir> { pub owner_id: OwnerId, pub trait_def: LocalDefId, pub name: Name, pub kind: TraitItemKind<'hir>, pub span: Span }
pub enum TraitItemKind<'hir> {
    AssocType { bounds: &'hir [Bound<'hir>] },               // AssocTypeDecl
    Fn { sig: FnSig<'hir>, generics: Generics<'hir>, has_body: bool },   // has_body: a default method
}
pub struct ImplItem<'hir> { pub owner_id: OwnerId, pub impl_def: LocalDefId, pub name: Name, pub vis: Visibility, pub kind: ImplItemKind<'hir>, pub span: Span }   // vis: Private or Pub
pub enum ImplItemKind<'hir> {
    AssocType { ty: &'hir Ty<'hir> },                         // AssocTypeDef
    Fn { sig: FnSig<'hir>, generics: Generics<'hir>, has_body: bool },   // no body: MISSING_FN_BODY (§2.5.12)
}
pub struct ExternItem<'hir> {                                 // a FnHead; INVALID_EXTERN_SIGNATURE: impl/03 §3.6.9
    pub owner_id: OwnerId, pub block: LocalDefId, pub name: Name, pub sig: FnSig<'hir>, pub generics: Generics<'hir>, pub span: Span,
}

pub struct Generics<'hir> { pub params: &'hir [GenericParam<'hir>], pub where_preds: &'hir [WherePred<'hir>] }
pub enum GenericParamKind<'hir> { Type { bounds: &'hir [Bound<'hir>] }, Const { ty: &'hir Ty<'hir> } }
pub struct GenericParam<'hir> { pub def_id: LocalDefId, pub name: Name, pub kind: GenericParamKind<'hir> }
pub struct WherePred<'hir> { pub hir_id: HirId, pub ty: &'hir Ty<'hir>, pub bounds: &'hir [Bound<'hir>] }
pub enum BoundKind<'hir> {
    Trait(Path<'hir>),                                        // generic args may contain associated type bindings
    FnSugar { kind: FnTraitKind, params: &'hir [Ty<'hir>], ret: Option<&'hir Ty<'hir>> },
}
pub struct Bound<'hir> { pub hir_id: HirId, pub kind: BoundKind<'hir>, pub effects: Option<&'hir [EffectRef<'hir>]>, pub span: Span }
pub enum FnTraitKind { Fn, FnMut, FnOnce }

pub struct FnSig<'hir> {
    pub safety: Safety,
    pub is_async: bool,                       // Stage 5; see §2.9.15
    pub params: &'hir [FnParam<'hir>],
    pub ret: Option<&'hir Ty<'hir>>,          // None: written without `->` (unit)
    pub borrows: Option<&'hir [Provenance<'hir>]>,
    pub effects: Option<&'hir [EffectRef<'hir>]>,
    pub requires: &'hir [HirId],              // HirIds of contract expressions in the body (see Body)
    pub ensures: &'hir [HirId],
}
pub struct FnParam<'hir> { pub hir_id: HirId, pub name: Option<Name>, pub is_self: bool, pub mutable: bool, pub ty: &'hir Ty<'hir> }
pub struct Provenance<'hir> { pub root: Name /* `self` or a parameter */, pub fields: &'hir [Name] }
pub struct EffectRef<'hir> { pub hir_id: HirId, pub segments: &'hir [Name] }

pub struct FieldDef<'hir> { pub hir_id: HirId, pub def_id: LocalDefId, pub vis: Visibility, pub name: Name, pub ty: &'hir Ty<'hir> }
pub struct Variant<'hir> { pub hir_id: HirId, pub def_id: LocalDefId, pub name: Name, pub shape: VariantShape<'hir>, pub discriminant: Option<i128> }
pub enum VariantShape<'hir> { Unit, Tuple(&'hir [Ty<'hir>]), Record(&'hir [FieldDef<'hir>]) }

pub struct Body<'hir> {
    pub owner: OwnerId,
    pub params: &'hir [BodyParam],            // one per FnParam or property parameter, in order
    pub value: &'hir Expr<'hir>,              // the function block, the const initializer, or the predicate
    pub contracts: &'hir [Contract<'hir>],    // requires and ensures, in source order
    pub invariant_preds: &'hir [&'hir Expr<'hir>],  // only for invariant owners: one predicate per listed expression
}
pub struct BodyParam { pub hir_id: HirId, pub binding: BindingId, pub name: Option<Name>, pub mutable: bool }
pub struct Contract<'hir> { pub hir_id: HirId, pub kind: ContractKind, pub expr: &'hir Expr<'hir> }
pub enum ContractKind { Requires, Ensures }
```

The owner's body has these forms:

| Owner | `Body::value` | Other parts |
|---|---|---|
| function with a body (including default trait methods) | the body block | `params`, `contracts` |
| function without a body, with `requires` or `ensures` (only possible for trait functions) | a synthetic `Tuple([])` | `params`, `contracts` |
| function without a body and without contracts | none (`HirOwner::body` is `None`) | — |
| const | the initializer | — |
| refined alias, distinct with a predicate | the predicate; `self` is the value being refined | — |
| invariant | a `Unit` expression | `invariant_preds` |
| test | the block | — |
| property | the block | `params` |

A bodiless trait function can carry `requires` / `ensures`, which every impl must respect ([spec/09](../spec/09-contracts-and-refinements.md)). Its synthetic body exists only to own the contract expressions; `TraitItemKind::Fn::has_body` stays `false`, and MIR building never builds MIR for it.

Expressions, statements, patterns, and types:

```rust
pub struct Expr<'hir> { pub hir_id: HirId, pub kind: ExprKind<'hir>, pub span: Span }
pub enum ExprKind<'hir> {
    Lit(Lit<'hir>),
    Path(Path<'hir>),                                     // one segment; `self` is PathRoot::SelfValue, `Self` is PathRoot::SelfType
    ImplicitMember(Name),                                 // `.Variant`; resolved against the expected type (impl/03)
    Field { base: &'hir Expr<'hir>, name: Name, args: Option<&'hir GenericArgs<'hir>> },   // `a.b`, unresolved meaning
    TupleField { base: &'hir Expr<'hir>, index: Option<u32> },
    Call { callee: &'hir Expr<'hir>, args: &'hir [Arg<'hir>], base: Option<&'hir Expr<'hir>> },
    Unary(UnOp, &'hir Expr<'hir>),                        // Neg, BitNot, Not, Deref
    Borrow(Mutability, &'hir Expr<'hir>),
    Binary(BinOp, &'hir Expr<'hir>, &'hir Expr<'hir>),    // includes the short-circuit And / Or
    Range { start: Option<&'hir Expr<'hir>>, end: Option<&'hir Expr<'hir>>, inclusive: bool },
    Index(&'hir Expr<'hir>, &'hir Expr<'hir>),
    Try(&'hir Expr<'hir>),                                // §2.9.8
    Await(&'hir Expr<'hir>),                              // Stage 5, §2.9.15
    Tuple(&'hir [Expr<'hir>]),                            // `()` is Tuple([])
    Array(&'hir [Expr<'hir>]),
    Block(&'hir Block<'hir>),
    AsyncBlock { def_id: LocalDefId, capture: CaptureBy, block: &'hir Block<'hir> },   // Stage 5
    If { cond: &'hir Expr<'hir>, then: &'hir Block<'hir>, els: Option<&'hir Expr<'hir>> }, // `els` is a Block or If expression
    Match { scrutinee: &'hir Expr<'hir>, arms: &'hir [Arm<'hir>], source: MatchSource },
    Loop { body: &'hir Block<'hir>, label: Option<Label>, source: LoopSource },
    Break { label: Option<Label>, value: Option<&'hir Expr<'hir>> },
    Continue { label: Option<Label> },
    Return(Option<&'hir Expr<'hir>>),
    Closure(&'hir Closure<'hir>),
    Region { name: Name, binding: BindingId, block: &'hir Block<'hir> },
    FormatString(&'hir [FmtPiece<'hir>]),                // Stage 3, §2.9.11
    ContractResult,                                       // `result` inside `ensures`
    ContractOld { args: &'hir [Arg<'hir>], base: Option<&'hir Expr<'hir>> }, // `old(...)` inside `ensures`
    AsCast { operand: &'hir Expr<'hir>, ty: &'hir Ty<'hir> },   // `CAST_EXPR`; always `NO_AS_CAST` (impl/03 §3.10.3)
    Err,
}
pub enum Lit<'hir> { Int { value: Option<u128>, negated: bool }, Float { text: Symbol, negated: bool }, Str(Symbol), ByteStr(&'hir [u8]), Char(char), Byte(u8), Bool(bool) }
pub struct Arg<'hir> { pub name: Option<Name>, pub expr: &'hir Expr<'hir> }      // record punning is decided by impl/03
pub enum UnOp { Neg, BitNot, Not, Deref }
pub enum BinOp { Add, Sub, Mul, Div, Rem, WrapAdd, WrapSub, WrapMul, BitAnd, BitOr, BitXor, Shl, Shr, Eq, Ne, Lt, Le, Gt, Ge, And, Or }
pub enum CaptureBy { Move, Infer }
pub struct Label { pub name: Symbol, pub span: Span }
pub enum MatchSource { Normal, IfLet, WhileLet, ForLoop, ForAwait, Try }
pub enum LoopSource { Loop, While, WhileLet, ForLoop, ForAwait }
pub struct Arm<'hir> { pub hir_id: HirId, pub pat: &'hir Pat<'hir>, pub guard: Option<&'hir Expr<'hir>>, pub body: &'hir Expr<'hir>, pub span: Span, pub synthesized: bool }
pub struct Closure<'hir> { pub def_id: LocalDefId, pub capture: CaptureBy, pub params: &'hir [ClosureParam<'hir>], pub ret: Option<&'hir Ty<'hir>>, pub body: &'hir Block<'hir> }
pub struct ClosureParam<'hir> { pub hir_id: HirId, pub binding: BindingId, pub name: Option<Name>, pub mutable: bool, pub ty: Option<&'hir Ty<'hir>> }
pub enum FmtPiece<'hir> { Text(Symbol), Arg { expr: &'hir Expr<'hir>, spec: Option<FmtSpec> } }
pub struct FmtSpec { pub name: Symbol, pub arg: Option<u128>, pub span: Span }

pub struct Block<'hir> { pub hir_id: HirId, pub stmts: &'hir [Stmt<'hir>], pub tail: Option<&'hir Expr<'hir>>, pub safety: Safety, pub span: Span }
pub struct Stmt<'hir> { pub hir_id: HirId, pub kind: StmtKind<'hir>, pub span: Span }
pub enum StmtKind<'hir> {
    Let { pat: &'hir Pat<'hir>, ty: Option<&'hir Ty<'hir>>, init: &'hir Expr<'hir>, els: Option<&'hir Block<'hir>> },
    Assign { lhs: &'hir Expr<'hir>, rhs: &'hir Expr<'hir> },
    AssignOp { op: BinOp, lhs: &'hir Expr<'hir>, rhs: &'hir Expr<'hir> },      // §2.9.10
    Assert { cond: &'hir Expr<'hir>, msg: Option<&'hir Expr<'hir>>, cond_text: Symbol }, // §2.9.12
    Expr(&'hir Expr<'hir>),
}

pub struct Pat<'hir> { pub hir_id: HirId, pub kind: PatKind<'hir>, pub span: Span }
pub enum PatKind<'hir> {
    Wild,
    Binding { mode: BindingMode, name: Name, binding: BindingId, sub: Option<&'hir Pat<'hir>> },   // `sub` for `x @ p`
    Lit(Lit<'hir>),
    Range { start: Option<PatRangeEnd<'hir>>, end: Option<PatRangeEnd<'hir>>, inclusive: bool },
    Tuple(&'hir [Pat<'hir>]),
    Slice { before: &'hir [Pat<'hir>], rest: Option<Option<&'hir Pat<'hir>>>, after: &'hir [Pat<'hir>] },
    Ctor { path: Path<'hir>, args: Option<&'hir [PatArg<'hir>]>, has_rest: bool },   // `args: None` means no parentheses
    ImplicitMember { name: Name, args: Option<&'hir [PatArg<'hir>]>, has_rest: bool },
    Or(&'hir [Pat<'hir>]),
    Err,
}
pub enum BindingMode { Value, Var, Borrow, BorrowMut }
pub struct PatArg<'hir> { pub name: Option<Name>, pub pat: &'hir Pat<'hir> }   // record punning is decided by impl/03
pub enum PatRangeEnd<'hir> { Lit(Lit<'hir>), Path(Path<'hir>) }

pub struct Ty<'hir> { pub hir_id: HirId, pub kind: TyKind<'hir>, pub span: Span }
pub enum TyKind<'hir> {
    Path(Path<'hir>),                                     // TypePath, including `T.Item` and `Self.Item`
    Borrow(Mutability, &'hir Ty<'hir>),
    Tuple(&'hir [Ty<'hir>]),                              // `()` is Tuple([])
    FnPtr { safety: Safety, params: &'hir [Ty<'hir>], ret: Option<&'hir Ty<'hir>>, effects: Option<&'hir [EffectRef<'hir>]> }, // None ret: omitted `->` (unit); None effects: omitted (pure); Some([]): explicit empty set
    Infer,
    Err,
}

pub struct Path<'hir> { pub hir_id: HirId, pub root: PathRoot, pub segments: &'hir [PathSegment<'hir>], pub span: Span }
pub enum PathRoot { Plain, SelfValue, SelfType, Lang(LangItem) }
pub struct PathSegment<'hir> { pub hir_id: HirId, pub name: Name, pub args: Option<&'hir GenericArgs<'hir>> }
pub struct GenericArgs<'hir> { pub args: &'hir [GenericArg<'hir>], pub span: Span }
pub enum GenericArg<'hir> { Type(&'hir Ty<'hir>), Const(ConstArg), Binding { name: Name, ty: &'hir Ty<'hir> } }
pub enum ConstArg { Int { value: Option<u128>, negated: bool }, Char(char), Bool(bool) }
```

**Current implementation slice.** `sai_hir::types` defines `Name`, the complete `LangItem` list, type/path nodes, generic arguments and parameters, where predicates, bounds, effect references, and provenance. `sai_hir::literals` and `sai_hir::patterns` define decoded HIR literals and the complete pattern node family. `sai_hir::expressions` defines expression, block, statement, match-arm, closure, and format-string nodes; `sai_hir::body` defines function signatures, body parameters, contracts, and bodies. `sai_hir::items` defines `HirOwner`, all ordinary, associated, extern, interface, and world item nodes, fields, variants, and derived-impl owners. Source-bearing records compare and hash their semantic contents while excluding spans, so moving syntax does not change their value equality. `Lit::ByteStr` borrows its bytes from the arena. Test/property names and WIT package paths use arena-backed `&str` values in place of the planned `SmolStr`; the crate has no `SmolStr` dependency, and borrowed strings keep item values `Copy`. The session-owned `sai_hir::arena::HirArena` provides typed storage for type, pattern, expression, statement, block, arm, closure, body, item, field, variant, literal, string, and child-list data. Syntax lowering currently covers type paths, `Self` paths, borrow, tuple/unit, inference, and function-pointer types, with `owned` and parenthesized types erased; type-path generic arguments retain mixed type, literal-constant, and associated-type-binding order. Expression lowering covers literals, unit, parenthesized and tuple expressions, simple and projected paths, prefix, binary, range, call, index, try, await, return, break, continue, ordinary expression blocks, expression statements, `let`, `var`, simple and compound assignment, assertions, plain `if`/`else`/`else if`, normal `match` expressions and arms, and `if let` desugaring, plus a subset of patterns. Complete owner construction currently covers top-level function, type alias, record, refined type, distinct type, resource, enum, trait, impl, const, invariant, effect, test, property, interface, world, and extern-block declarations; derived-impl owners and associated trait, impl, and interface members, world items, and extern items are assembled by dedicated lowerers. Query integration remains follow-up work.

Notes on the shape:

- `owned T` is lowered to the HIR of `T` (D-OWN-01); `(T)` is lowered to `T`; `PAREN_EXPR` and `PAREN_PAT` are lowered to their content. The CST keeps them for the formatter.
- `var x: T = e` is lowered to `Let { pat: Binding { mode: Var, name: x } }` with the desugaring context `VarStmt`, which only affects diagnostic wording.
- A block whose last statement is an expression statement is lowered with that expression as `tail`. Whether the tail's value becomes the block's value is defined by spec/06 and checked by impl/03. A final assignment, `let`, or `assert` leaves `tail` empty.
- `-INT` and `-FLOAT` in expression position are lowered to `Lit { negated: true }` (spec/02 §2.6.1 item 5). Any other `-e` is `Unary(Neg, e)`.
- `result` (a non-raw single-segment path) and calls whose callee is the non-raw single-segment path `old` are lowered to `ContractResult` and `ContractOld` only inside `ensures` expressions. `ContractOld` preserves every argument and an optional `..base`, including malformed arity or named-argument shapes, so later semantic analysis can validate the intrinsic's one-positional-expression rule. Elsewhere `result` and `old(...)` are ordinary paths and calls (spec/02 §2.5.2; spec/09 §9.3.2).
- A path such as `a.b.C` is lowered as nested `Field` expressions over `Path(a)`. The resolver re-interprets a prefix that names modules or types as a qualified path; this keeps HIR independent of resolution (N-E-18).
- A normal `match` retains its scrutinee and arms in source order. Parser `ERROR` nodes in an arm list become arms with `Err` pattern and body placeholders; exhaustiveness, reachability, and guard typing are later semantic checks.

### 2.8.6 Lowering of errors

1. A missing or `ERROR` expression, pattern, or type becomes `ExprKind::Err`, `PatKind::Err`, or `TyKind::Err`. A missing name becomes `Name { sym: Symbol::ERROR }`. An identifier token with the `RESERVED` flag is lowered as `Symbol::ERROR` (§2.3.3).
2. Lowering reports no diagnostic for these nodes, because the lexer or the parser already did. Every later stage treats `Err` nodes and `Symbol::ERROR` as already diagnosed: the resolver does not report `UNRESOLVED_NAME` for them, and the type checker gives them the error type (impl/03 §3.5.6), which unifies with everything.
3. An item whose `NAME` is missing is still collected, with the error name and a `DefPathData` that uses `Symbol::ERROR` plus a disambiguator. It can never be referenced, but its body is lowered and checked, so that errors inside it are still reported.
4. An `ERROR` node that contains a complete expression (for example the operator-continuation recovery of §2.5.11) is lowered to `Err`; the contained expression is not lowered.
5. A `CAST_EXPR` (§2.5.6) is the exception to item 4: it is lowered to `ExprKind::AsCast` with its operand and its type (a missing type is `TyKind::Err`), and lowering reports nothing. The node is kept so that the type checker can report `NO_AS_CAST` with a fix chosen from the operand's type ([impl/03](03-name-resolution-and-type-checking.md) §3.10.3). No later stage sees it: the body is tainted by that error, and a tainted body is not lowered to MIR ([impl/04](04-mir.md) §4.4.1).
6. An `ERROR` contained by a `CLOSURE_PARAM` or `CLOSURE_PARAM_LIST` is recovered in the parameter list when any other errors in the closure expression are independently recoverable at their statement, argument, interpolation, or cast sites. A missing parameter name becomes `Name { sym: Symbol::ERROR }` with a `BindingId` mapped to the name or recovery syntax, so later parameters and recoverable body expressions remain available. If another closure error is not recoverable at its own site, the closure expression follows the ordinary error rule above.

**Current implementation slice.** `sai_hir::lower::lower_expr` dispatches literals to `lower_literal`, lowers `UNIT_EXPR` to an empty `ExprKind::Tuple`, erases `PAREN_EXPR` by returning its content, and lowers ordered tuple elements to `ExprKind::Tuple`. A path without generic arguments becomes a one-segment `PathRoot::Plain`; `self` and `Self` become empty paths rooted at `SelfValue` and `SelfType`, and `.Variant` becomes `ExprKind::ImplicitMember`. Path expressions record separate HIR IDs for the expression, path, and segment, all mapped to their source syntax. `FieldExpr` lowers recursively to nested `ExprKind::Field`, and `TupleIndexExpr` to `ExprKind::TupleField`; a non-decoding index is retained as `None`, with any parser diagnostic remaining authoritative. `BinExpr` lowers to `ExprKind::Binary` with every arithmetic, wrapping, bitwise, shift, comparison, and boolean operator mapped explicitly. `PrefixExpr` lowers to the corresponding `Unary` or `Borrow` node; direct negative integer and float literals are folded to one `Lit` node with `negated: true`, mapped to the full prefix range. Other negated expressions remain `Unary(Neg, …)`. `RangeExpr` lowers to `ExprKind::Range`, preserving optional start and end operands and whether the direct operator is `..=`; endpoints lower in source order. This covers all six range shapes, including the empty `..` range. A parser-diagnosed chained range retains its nested syntax shape, while an error-marked range becomes `ExprKind::Err` without another diagnostic. `IndexExpr`, `TryExpr`, and `AwaitExpr` lower to `ExprKind::Index`, `Try`, and `Await`, respectively. Nested postfix expressions retain their parser-established association; indexing lowers the base before the index, and each postfix parent receives its source-map ID before its operands. `CallExpr` lowers to `ExprKind::Call`. Positional and named arguments retain their source order, names use the shared identifier lowering, and one `..base` argument is kept in the dedicated base field; the callee and argument expressions receive IDs after the call ID in source order. Explicit type and constant generic arguments on direct function- and method-call callees lower into the callee path or field segment; this also works when the callee is parenthesized. Generic arguments on path and field expressions lower in every value and callee position into their corresponding HIR path-segment or field arguments; this also works when the callee is parenthesized. More than one base argument, or an incomplete call shape, becomes `ExprKind::Err`. Error-marked postfix expressions become `Err`, and unsupported descendants are preflighted before any parent or child HIR nodes and source-map entries are allocated. `lower_literal` lowers integer, float, string, byte-string, character, byte, and boolean syntax literals to `ExprKind::Lit`. Strings are decoded and interned, byte strings are copied into the HIR arena, and underscores are removed from the retained float spelling so separator-only edits do not change its HIR value. `lower_pattern` dispatches literal patterns to `PatKind::Lit`, folding `-INT` into its `negated` field, and dispatches range patterns with literal or constant-path endpoints to `PatKind::Range`, including open and inclusive forms, without creating separate HIR patterns for literal endpoints. It also handles wildcard, binding, `name @ pattern`, tuple, slice, constructor, implicit-member, or-pattern, and parenthesized patterns; parentheses are stripped from the HIR shape, while tuple and slice structure is preserved. Constructor and range paths retain their ordered segments under `PathRoot::Plain`, and path and segment HIR IDs map back to their `ConstPath` and `NameRef` syntax. Constructor argument presence, source order, and trailing rest are preserved, and supported child patterns lower recursively. Binding modes and owner-local binding IDs are retained. Or-pattern alternatives are flattened, corresponding names share a `BindingId`, and mismatched names or modes and duplicate bindings produce their documented HIR diagnostics. Missing, reserved, or invalid raw binding names use `Symbol::ERROR`; an invalid raw identifier that parses in a constructor or range endpoint, such as `r#Self` or `r#Self..=4`, becomes `PatKind::Err` using its existing lexer diagnostic. The `ConstPath` grammar uses identifier tokens and does not admit the reserved `Self` token, so these paths currently have a plain root. Other identifiers are interned in NFC after removing a valid `r#` prefix. Missing tokens, `ERROR` nodes, malformed numeric tokens, invalid range shapes, duplicate slice rests, and float patterns become `Err` without adding diagnostics; float patterns already carry `FLOAT_PATTERN` from parsing. Error-marked string, byte-string, character, and byte tokens keep the replacement values returned by their total AST decoders, while their existing syntax diagnostics remain authoritative. Implemented HIR nodes use the source-map builder for owner-local IDs and snapshot-validated spans. Async-function stage gating and integration with the complete owner-body table remain follow-up work.

`ClosureExpr` lowers to `ExprKind::Closure`; its parameters and return type use ordinary type/name lowering, `var` sets the parameter's mutable bit, and `move` selects `CaptureBy::Move`. Every parameter gets a `BindingId` mapped to its `Name` syntax, including `_`, whose HIR name is `None`. The body remains a safe block in the enclosing owner's HIR and `HirId` space. The source-map builder must receive the same `DefPathInterner` that interned the owner. Lowering assigns `Closure` and `AsyncBlock` paths in shared owner-local preorder and nests each path under its nearest lexical closure or async block; syntax beneath another item owner does not take part in this owner's preorder.

`AsyncBlockExpr` lowers to `ExprKind::AsyncBlock { def_id, capture, block }`. Its body remains a safe block in the enclosing owner's HIR and `HirId` space, `move` selects `CaptureBy::Move`, and omission selects `CaptureBy::Infer`. Async blocks and closures receive definition paths in one owner-wide preorder, including when the expression being lowered contains async blocks but no closures. This syntax-level HIR API constructs the Stage 5 shape; a compiler stage gate and the async type/MIR work remain outside this lowering slice.

`sai_hir::lower::lower_function_with_generics` lowers `FnDecl`s into an `FnSig`, `Generics`, and optional `Body`. The compatibility entry point `lower_function` retains its `(FnSig, Option<Body>)` result and historical behavior, omitting generic bounds and `where` predicates; use the full-result API when the owner needs generic HIR. Both retain parameters in source order, including `self`, wildcard names, mutability, types, safety, return type, borrow provenances, and effect references; `FnSig.is_async` retains the async marker. The full-result API lowers function type and const parameters, const-parameter types, bounds, and `where` predicates in the same operation. Bounds and predicates retain source order, including `Fn`-family parameter and return types and optional effect caps; an explicit empty cap remains `Some([])`. Generic parameter `DefId`s are interned below the function owner, with source-order disambiguators for duplicate names of the same parameter kind. Generic bounds receive HIR IDs before signature parameters, and `where` predicates receive IDs after the return type. A missing const-parameter or where-subject type becomes `TyKind::Err`; an `Fn`-family bound with a written arrow but missing return type also retains `Some(TyKind::Err)`, while an omitted arrow keeps the unit-return representation `None`. Missing `Fn`-family parameter slots become `TyKind::Err`, and parser recovery consumes the missing slot's comma so later valid parameter types remain attached to the bound. A malformed trait path recovers as an empty plain path instead of exposing a valid-looking prefix, and its path node maps to the malformed path syntax. Full generic lowering requires the owner's `DefPathInterner`, and a failed batch of parameter paths leaves no partial entries.

`sai_hir::lower::lower_function_owner` resolves a top-level function by its `ErasedAstId` in the current `ItemTree`, then builds its `Item`, `HirOwner`, and `HirSourceMap`. `HirLoweringContext` carries the current AST map, root, snapshot, arena, symbol interner, and DefPath interner. The function checks that the item tree's retained snapshot identity matches the AST map, that the entry resolves to a function in the supplied current root and snapshot, and that the item name, visibility, and complete recognized raw-attribute data match the syntax, including spans on nested arguments. It requires the caller to supply the complete expected `DefPath`, then verifies that the supplied `OwnerId` resolves to that exact path and that its terminal component has the function's value-namespace symbol. The caller derives the parent path and sibling disambiguator because ItemTree does not record either. The function's generic definitions, signature, body, item span, raw attributes, and source map are then assembled together. Invalid raw or reserved identifiers keep parser recovery behavior and lower with `Symbol::ERROR`. Trait and impl methods use distinct owner node types and are not accepted by this entry point; the remaining owner kinds and query integration remain follow-up work.

`sai_hir::lower::lower_const_owner` resolves a top-level constant from the same snapshot-checked item tree and builds its `ItemKind::Const`, initializer `Body`, and source map. It preflights the declared type and initializer before allocating HIR. The caller supplies the complete expected owner `DefPath`, and lowering verifies that `OwnerId` resolves to it exactly. A missing diagnosed type becomes `TyKind::Err`; a missing diagnosed initializer becomes an `ExprKind::Err` body value so the constant owner remains available to later stages.

`sai_hir::lower::lower_invariant_owner` resolves a top-level invariant through the same snapshot, visibility, and raw-attribute checks and verifies the complete expected owner `DefPath`. Its `ItemKind::Invariant` target is a plain path with the single declared type name. The owner has a synthetic unit body value and stores every parser-retained predicate in source order in `Body::invariant_preds`. It preflights all predicates before allocating HIR and prepares nested closure and async-block paths across the entire declaration, so ordinals continue across predicate boundaries. Target-type validity, defining-module placement, duplicate invariants, and predicate typing remain semantic validation responsibilities.

`sai_hir::lower::lower_test_owner` resolves a top-level test through the same snapshot, visibility, and raw-attribute checks. It requires the grammar-required string literal, decodes it once, and uses the resulting text for the `ItemKind::Test` name and the terminal `DefPathData::Test` component; the interned `Item.name` must agree with the snapshot-checked ItemTree entry. The parser recognizes `test` as a declaration only when followed by a string literal, so a missing literal remains unsupported-item recovery rather than an owner with an empty name. The test block becomes the body value, and a diagnosed missing block recovers as `ExprKind::Err`. The whole block is preflighted and nested closure and async-block paths are prepared over the declaration before lowering.

`sai_hir::lower::lower_property_owner` applies those owner checks to a top-level property, decodes its string name once, and checks that the complete owner path ends in the matching `DefPathData::Property`. It retains each declaration parameter's name, mutability, and lowered type in `ItemKind::Property.params` as `FnParam`, then creates the corresponding body binding in `Body.params` as `BodyParam`. This keeps the type available for later `Arbitrary` bound checking while keeping binding IDs local to the body. Parameter types and the property block are preflighted before HIR allocation, and nested closure and async-block paths are prepared over the entire declaration. Missing or malformed parameter types and a missing body use the parser-diagnosed error recovery already supported by `lower_function_param` and the expression lowerer. Generic parameters, `Arbitrary` implementation checks, property result typing, and execution semantics remain later semantic or test-harness work.

`sai_hir::lower::lower_alias_owner` resolves only top-level `ItemTreeKind::Alias` entries; record and refined aliases use separate item kinds. It requires the `TypeDecl` to contain an actual `AliasDef`, checks the complete expected `DefPath` in the type namespace, lowers generic parameter definitions and bounds under that owner, and stores the alias's lowered type in `ItemKind::Alias`. A parser-diagnosed missing right-hand type inside an `AliasDef` recovers as `TyKind::Err`; a declaration with neither an alias definition nor a record body is rejected instead of being fabricated as an alias. Type resolution and other semantic checks remain later work.

`sai_hir::lower::lower_struct_owner` resolves top-level record declarations, lowers type/const generic parameters and the record's `where` predicates, then lowers fields in source order with their visibility, types, source mappings, and `DefPathData::Field` identities. Duplicate field names receive source-order disambiguators so semantic duplicate-field checking can still inspect each declaration. A diagnosed missing field type recovers as `TyKind::Err`; the record has no body.

`sai_hir::lower::lower_refined_owner` resolves a top-level `ItemTreeKind::Refined` declaration, stores its base type in `ItemKind::Refined`, and uses the refinement predicate as the owner's body value. The predicate's `self` expression remains a `SelfValue` path; its base type comes from the refined item, so no explicit `BodyParam` is created. Missing base types and predicates recover as `TyKind::Err` and `ExprKind::Err`. Since refined types cannot be generic and `ItemKind::Refined` has no generic field, generic parameter declarations are not lowered or assigned HIR definition paths; ItemTree retains their entries for the later `GENERIC_REFINED_TYPE` validation.

`sai_hir::lower::lower_distinct_owner` resolves a top-level `ItemTreeKind::Distinct` declaration, lowers its generic parameters and base type, and preserves the inner `pub` as `ItemKind::Distinct::pub_ctor` independently of outer item visibility. A refinement clause sets `has_predicate` and becomes the owner's body value, with implicit `self` and no explicit body parameters; an absent clause yields no body. Missing base types and predicates recover as `TyKind::Err` and `ExprKind::Err`. Type resolution, `GENERIC_REFINED_TYPE`, and predicate checking remain later semantic validation.

`sai_hir::lower::lower_resource_owner` resolves a top-level `ItemTreeKind::Resource` declaration and lowers its fields in source order using the same field HIR records and `DefPathData::Field` identities as records. Resource fields preserve their visibility, types, source mappings, and missing-type recovery; the resource has no body. Resource ownership and borrow/type restrictions remain semantic validation.

`sai_hir::lower::lower_enum_owner` resolves a top-level `ItemTreeKind::Enum` declaration, lowers generic parameters and `where` predicates, then lowers unit, tuple, and record variants in source order. A parser-recovered enum with no `VariantList` returns `MissingRequiredSyntax` so it cannot be confused with a valid empty, uninhabited enum. Duplicate variant names and record-variant field names remain separate HIR entries with per-name source-order path disambiguators. Tuple and record payload types preserve parser-diagnosed missing-type recovery. Explicit signed discriminants representable as `i128` are retained; implicit discriminants and explicit values outside that representation remain `None` for later enum validation, with variant source mappings retaining access to the written declaration.

`sai_hir::lower::lower_interface_owner` resolves a top-level interface, validates its name, visibility, attributes, complete `DefPath`, and source-order disambiguator among same-named interfaces, and stores its direct member owners in source order. Type declarations, enums, and resources use `TypeNs` child paths; interface functions use `ValueNs`, with duplicate path components disambiguated in source order. The mixed owner batch is interned atomically. An explicit empty item list is valid; a missing list returns `MissingRequiredSyntax`.

`sai_hir::lower::lower_interface_item_owner` resolves one direct record, alias, refined type, enum, resource, or function below an already-lowered interface. It validates the current ItemTree and AST snapshot, the parent interface's source span, visibility, attributes and DefPath, direct list membership, the parent's source-ordered owner slot, and the member's namespace component and canonical disambiguator. Records and record-variant fields are always `Pub`; records, aliases, refined bases, enum variants, and function signatures retain their source-level type information. A refined interface type stores its base and refinement-clause span without a body. Resource member function DefPaths are interned below the resource in source order; their separate `InterfaceItem` owners are lowered individually. Interface functions use the signature-only function lowerer: diagnosed generic parameters remain available in `Generics`, while forbidden bodies and contracts are not turned into owner bodies or signature contract IDs. Interface restriction and WIT compatibility diagnostics remain later validation work.

`sai_hir::lower::lower_interface_resource_member_owner` resolves one resource member below the exact interface and resource owners. It verifies both parent source declarations, the resource's source-ordered slot under the interface, the member's source-ordered slot under the resource, and the complete `[…, Interface, TypeNs(resource), AssocFn(member)]` path with its duplicate-name disambiguator. A first signature parameter with `is_self` makes the member a `Method`; otherwise a member named `new` is a `Constructor` only when its return type is exactly `Self` or the resource's own name, and every other member is `Static`. Thus `new -> Result<Self, E>` is `Static` under spec/13 §13.2.4. The owner receives its three source AST identities together as `InterfaceResourceMemberIds`. It lowers the signature and generics while leaving the owner body absent and dropping forbidden contract IDs. Interface resource restriction and WIT receiver checks remain later validation work.

`sai_hir::lower::lower_world_owner` resolves a top-level world, validates its current ItemTree metadata and complete `World` DefPath, then stores its import, export, and include owners in source order. It derives the world's same-name disambiguator from current top-level AST declarations and re-derives each child's name from the current function name or WorldRef, rejecting a mismatch before path interning. Equal `WorldItem { import, name }` and `WorldInclude(name)` components receive independent source-order disambiguators; all child paths are interned atomically. An explicit empty item list is valid, while a missing list returns `MissingRequiredSyntax`.

`sai_hir::lower::lower_world_item_owner` resolves one direct item below an already-lowered world. It validates the parent world's AST, source span, visibility, attributes, DefPath, and source-ordered owner slot, then checks the child's direct syntax membership and complete one-component DefPath. Import/export references lower `PKG_PATH` to `WorldRef::Pkg` and module segments to `WorldRef::Mod`; includes use the same reference lowering. Import/export functions lower their nested `FnDecl` signature without a body or contract IDs. Generic parameters and `where` clauses are omitted because the world-function HIR schema stores only `FnSig` and the grammar diagnoses these declarations. A malformed `WorldRef` with no usable package or module path lowers to an empty module path mapped to the reference node, preserving its owner for later name-resolution diagnostics.

`sai_hir::lower::lower_extern_block_owner` resolves an extern block and validates its complete `ExternBlock { abi, module }` DefPath, including the source-order disambiguator for identical headers. It stores direct function owners in source order and atomically interns their `ValueNs` paths, numbering duplicate function names in that order. The block retains its safe/unsafe marker, decoded module name, and `Wasm`/`C` ABI; an unknown ABI retains its exact DefPath ABI symbol and lowers to `ExternAbi::Err` for the parser diagnostic. An explicit empty list is valid, and the block has no body.

`sai_hir::lower::lower_extern_item_owner` resolves one function below an already-lowered extern block. It checks the block's current AST and ItemTree declaration, ABI/module/safety metadata, attributes, source span, complete DefPath, and source-order disambiguator, then verifies direct function membership, the parent's owner slot, and the child's complete `ValueNs` DefPath. The function's signature, generics, `where` predicates, and `requires`/`ensures` expressions are lowered with the shared full function lowerer. A bodyless import without contracts has no owner body; contracts retain their expression nodes and parameter bindings in the shared synthetic contract body. Diagnosed function visibility markers do not create visibility on `ExternItem`.

`sai_hir::lower::lower_trait_owner` resolves a top-level trait, lowers its source-declared type/const generic parameters, supertrait bounds, and `where` predicates, and stores associated type/function owners in source order. Each associated declaration receives a `DefPathData::AssocType` or `DefPathData::AssocFn` component, with duplicate-name disambiguators scoped to that component. The trait's implicit `Self` is not a source-backed generic parameter in this owner and remains for later trait-definition construction. An empty item list is valid; a missing item list returns `MissingRequiredSyntax` instead of creating an empty trait.

`sai_hir::lower::lower_trait_item_owner` resolves one direct associated declaration under an already-lowered trait owner. It checks that the parent HIR span identifies the trait AST declaration from the current ItemTree, that the child is a direct child of the trait's item list, that the parent's source-ordered owner list selects the same `OwnerId`, and that the complete expected `DefPath` is exactly one component below the trait. Duplicate path disambiguators must match the declaration's source order. Associated types lower their bounds and have no body. Associated functions lower their complete signature, generic parameters, `where` predicates, contracts, and optional default body through `lower_function_with_generics`; `has_body` reflects a written method body, so a bodiless method with contracts can still own the synthetic contract body without becoming a default method.

`sai_hir::lower::lower_impl_owner` resolves an impl using its ItemTree header hash and the caller-supplied complete impl `DefPath`, then validates the source-order disambiguator among entries with the same header hash. It lowers the generic parameters and `where` predicates, preserves the inherent, trait, or interface target in the corresponding `ImplKind`, and interns associated type/function owners in source order. Trait references retain generic arguments and associated-type bindings. An explicit empty item list is valid; a missing list returns `MissingRequiredSyntax`. A missing self type recovers as `TyKind::Err`. A written `for` token selects trait-impl recovery even when both type accessors are missing; because `ImplKind::Trait` stores a `Path`, a malformed or non-path trait reference recovers as an empty path mapped to its syntax, while a missing trait reference recovers as an empty path mapped to the impl syntax. Interface paths lower their module segments without generic arguments. Impl identity and owner assembly do not perform coherence, visibility, trait-item conformance, or interface validation.

`sai_hir::lower::lower_impl_item_owner` lowers one direct associated type definition or function under an already-lowered impl owner. It validates that the parent span and DefPath identify the current impl, the child is direct in the current ItemList, the parent's source-ordered owner list selects the requested owner, and the associated DefPath is exactly one component below that impl with its canonical source-order disambiguator. Associated type definitions lower their right-hand type, recovering a diagnosed missing type as `TyKind::Err`. Functions lower their generics, signature, contracts, and optional body through `lower_function_with_generics`; `has_body` reflects the written body and `ImplItem::vis` preserves source visibility. Duplicate-name diagnostics, impl coherence and conformance, and the required-body diagnostic for bodyless impl methods remain outside owner assembly.

`sai_hir::lower::lower_effect_owner` resolves a top-level effect declaration through the same snapshot, visibility, and raw-attribute checks. It requires `OwnerId` to resolve to the complete expected `DefPath`, ending in the effect namespace and the declaration's name. The resulting `ItemKind::Effect` has no body and retains complete `covers` references in source order. Omitted `covers` is empty; malformed sets, including reserved and invalid raw identifier segments, recover all-or-nothing to an empty list so an invalid prefix cannot imply capabilities.

Only syntactically complete `borrows { ... }` and `effects { ... }` sets become `Some`; an explicitly empty set remains `Some([])`, while a missing or malformed set recovers as `None`. Reserved and invalid raw identifier segments make an effect set malformed as a whole, so no valid-looking prefix is retained in function or function-pointer effect clauses; top-level effect covers recover as empty. The lowerer walks `where`, `borrows`, `effects`, `requires`, and `ensures` child clauses in written order, including declarations where the parser has diagnosed clause order; each contract's expression HIR ID is also stored in its corresponding `FnSig` list. When duplicate `borrows` or `effects` clauses are present, the first clause is retained, matching the AST role accessor; parser diagnostics report the duplicates. Within an `ensures` expression, the non-raw single-segment path `result` becomes `ExprKind::ContractResult`, and every call with the non-raw single-segment callee `old` becomes `ExprKind::ContractOld { args, base }`, preserving its source arguments even when its arity or argument form is invalid. Outside that context both remain ordinary paths/calls. A synchronous body is a block expression. An async function with a body is lowered to a synthetic outer block whose tail is a `move` `AsyncBlock` containing the source body block. The generated wrapper expression, outer block, and async-block expression map to the full function declaration with `SpanCtxt::Desugared(AsyncFn)`; the user body block keeps its root span. The generated async block receives owner-local nested-definition ordinal 0, before explicit closures and async blocks in source preorder. Closures and async blocks in the source body are nested below this wrapper; closures in function contracts remain directly under the function owner. A bodyless async declaration has no generated async block. A bodiless function with contracts receives the specified synthetic empty-tuple body value; a bodiless function without contracts has no body and creates no body-parameter bindings. Each signature parameter and corresponding body parameter receive distinct `HirId`s because they are separate HIR records; body-parameter bindings map to the written name or recovery syntax. The lowering preflights generic and signature types, contracts, and the complete block before allocating HIR nodes or bindings, and prepares nested closure/async-block paths over the whole function owner. Feature gating, async typing/MIR, generic lowering for other item kinds, and integration with the complete owner-body table remain follow-up work.

Call-argument recovery preserves an enclosing call when an individual expression is missing. The parser may omit an empty positional `Arg` node but retain a zero-width `ERROR`; lowering stores an `Err` expression at that argument's position so following valid arguments remain ordered and available. Error wrappers containing only invalid argument attributes do not become arguments.

Array expressions lower to `ExprKind::Array` with elements in source order and the array's HIR ID allocated before its children. Direct `ERROR` children are retained as `ExprKind::Err` elements when enclosing recovery preserves the array, while attribute-only error wrappers are skipped. Valid unsupported descendants are rejected before the array or its elements receive HIR IDs.

`return`, `break`, and `continue` lower to their corresponding HIR expressions. Return and break values and break/continue labels remain optional; label names are normalized after removing the leading apostrophe, and the label span covers the full `LABEL` token. Transfer expressions receive their HIR ID before any value expression; label target lookup remains semantic work.

Ordinary block expressions lower to `ExprKind::Block` with safe `Block` nodes. Unsafe block expressions use the same HIR shape with `Block.safety = Unsafe`; both the expression wrapper and body block retain their own source spans. `RegionExpr` lowers to `ExprKind::Region` with its normalized source name, a `BindingId` mapped to the name syntax, and its source body as an ordinary safe block; arena behavior remains represented by the region node for downstream semantic lowering. Earlier expression statements become `StmtKind::Expr`, while the final expression statement becomes `Block::tail`; empty blocks have neither statements nor a tail. `let` lowers its pattern, optional type, initializer, and optional let-else block to `StmtKind::Let`. `var name` lowers to the same statement shape with a synthesized `BindingMode::Var` pattern; the pattern and name use `SpanCtxt::Desugared(VarStmt)`, while the statement keeps its source span. A written type marker with a missing type remains `Some(TyKind::Err)`, and a missing initializer becomes `ExprKind::Err`. Simple assignment becomes `StmtKind::Assign`; all compound operators map to their corresponding `StmtKind::AssignOp` binary operator. `assert` lowers to `StmtKind::Assert`; its condition text is sliced from the snapshot-bound source after trimming surrounding whitespace, and an omitted condition or message is retained as `ExprKind::Err` when parser recovery supplies an error node. Error nodes contained by lowered let/var/assignment/assert statements are recovered at their fields so the surrounding block can retain the statement. Direct block `ERROR` nodes, including attribute-wrapped recovery nodes, become error expression statements and prevent an earlier expression from being promoted to the tail. A final non-expression statement has no tail. Unsupported expression descendants in statement operands are rejected during block preflight before HIR allocation.

`loop` lowers directly to `ExprKind::Loop` with `LoopSource::Loop`, preserving its label and source block. `while` desugars to a `Loop` whose body tail is an `If`; its false branch is a synthesized block containing an unlabeled `Break`. `while let` desugars to a `Loop` whose body tail is a `Match` with `MatchSource::WhileLet`, a source-backed user arm, and a synthesized wildcard arm containing an unlabeled `Break`. All generated nodes map to the complete loop expression with `SpanCtxt::Desugared(WhileLoop)` or `SpanCtxt::Desugared(WhileLet)`; source conditions, patterns, and user blocks retain root spans. Labels and expression children are lowered in source order, and unsupported descendants are rejected before HIR allocation.

`for` lowers according to §2.9.5: the outer `ForLoop` match calls `lang::IntoIteratorIntoIter`, binds the iterator as synthetic mutable `%iter`, and runs a `ForLoop` loop whose tail calls `lang::IteratorNext` through `borrow mut %iter`. The inner match lowers the source pattern under `lang::OptionSome` and adds a synthesized `lang::OptionNone` arm that breaks. Generated nodes use `SpanCtxt::Desugared(ForLoop)`; the iterable, source pattern, user block, and label keep root spans. `for await` consumes the source stream directly, binds synthetic mutable `%stream`, and uses `MatchSource::ForAwait` and `LoopSource::ForAwait`; the loop tail awaits `lang::StreamNext(borrow mut %stream)` before matching `Option.Some` and `Option.None`. Generated nodes use `SpanCtxt::Desugared(ForAwait)`; the synthetic `Await` span is narrowed to the `await` keyword for diagnostic precision while its source-map entry points to the full `ForExpr`. Unsupported descendants are rejected before HIR allocation. This low-level API constructs the Stage 5 HIR form; feature gating and async typing/MIR remain outside this lowering slice.

Format strings stay as `ExprKind::FormatString`. The AST piece view decodes text and escaped braces and merges adjacent text pieces; each interpolation expression lowers recursively with its source span. An `ERROR` inside an interpolation becomes an `Err` argument while the surrounding text and other arguments are retained; errors outside interpolations use the ordinary recovery rules. Format-specifier names are NFC-normalized and interned, optional integer arguments are retained, and the specifier span covers its source token. Interpolation expressions are preflighted before allocating the format-string HIR node.

`CAST_EXPR` is preserved as `ExprKind::AsCast` so the type checker can report `NO_AS_CAST` with a source-type-specific fix. The operand and target type lower in source order, and a missing operand or type is retained as `ExprKind::Err` or `TyKind::Err` without a lowering diagnostic. Cast recovery bypasses the general error-node collapse, while unsupported valid descendants are still rejected before allocating the cast or its children.

Plain `if` conditions lower to `ExprKind::If` with their condition and `then` block in source order. An `else` block becomes an `ExprKind::Block`, and `else if` chains lower recursively. A missing condition becomes an `Err` condition and a malformed explicit else branch becomes an `Err` else expression when the enclosing `if` remains recoverable; a missing required `then` block lowers the whole expression to `Err`. `if let` lowers to `ExprKind::Match { source: MatchSource::IfLet }` with the source pattern and block in its first arm, followed by a synthesized wildcard fallback. Without an `else`, that fallback returns an empty tuple; block and `else if` fallbacks lower from their source syntax and nested `if let` recurses. Desugared match, arm, fallback-pattern, and empty-tuple nodes carry `SpanCtxt::Desugared(IfLet)` over the complete source expression. Unsupported conditions or descendants are rejected before HIR allocation.

`sai_hir::lower::lower_type` lowers plain and `Self`-rooted paths, borrow types, tuple and unit types, inference types, and safe or unsafe function-pointer types. Type-path generic arguments retain their written order across types, integer/character/boolean constants, and associated-type bindings. `owned T` and `(T)` are transparent in HIR. An omitted function-pointer return and effect clause remain `None` (unit return and pure effects); an explicit empty effect set is `Some([])`. Missing and error-marked type syntax becomes `TyKind::Err`. An error in a tuple element or generic-argument slot is retained as an `Err` at that position so valid neighboring types and arguments survive; empty slots indicated by consecutive commas are retained even when parser recovery places the comma just beyond the list node. A comma owned by an enclosing list remains that outer separator and does not create an inner slot. Terminal zero-width errors for missing closing delimiters do not become phantom elements. A malformed literal constant argument becomes a type-shaped `Err` placeholder, while a syntactically valid integer that exceeds `u128` remains `ConstArg::Int { value: None }` for the later range diagnostic. Because HIR has no error-effect entry, an explicit function-pointer effects clause with a missing or malformed set makes the whole function-pointer type `Err`.

### 2.8.7 Item tree

The query `def_collector(FileId) -> ItemTree` produces a summary of a file's item-like nodes without bodies:

```rust
pub struct ItemTree {
    pub file: FileId,
    snapshot_id: WorkspaceSnapshotId,         // private binding metadata; excluded from equality
    pub module_docs: Option<AstPtr<ast::Docs>>, // inner docs from the module header
    pub is_unsafe_module: bool,
    pub uses: Vec<UseEntry>,                  // one per `use` declaration, in source order
    pub items: Vec<ItemTreeEntry>,            // top-level items, in source order
}
pub struct UseEntry { pub ast_id: ErasedAstId, pub is_pub: bool, pub pkg_path: SmolStr, pub alias: Option<Symbol>,
                      pub items: Vec<(Symbol, Option<Symbol>, TextRange)>, pub raw_attrs: Vec<RawAttr> }
pub struct ItemTreeEntry {
    pub ast_id: ErasedAstId,
    pub kind: ItemTreeKind,                   // Function, Struct, Alias, Refined, Enum, Distinct, Resource, Trait, Impl,
                                              // Const, Invariant, Effect, Interface, World, ExternBlock, Test, Property,
                                              // TypeParam, ConstParam, Field, Variant, VariantField, AssocType, AssocFn,
                                              // InterfaceFn, ExternFn, WorldItem { import }, WorldInclude
    pub name: Symbol,                         // Symbol::ERROR if missing
    pub vis: Visibility,
    pub raw_attrs: Vec<RawAttr>,
    pub children: Vec<ItemTreeEntry>,         // direct item-like children and generic parameters, in source order
    pub header_hash: Option<u64>,             // impls only (§2.8.2)
}
```

`TypeParam` and `ConstParam` are the `ItemTreeKind` variants for generic-parameter children. Each generic parameter is an `ItemTreeEntry` with its own `AstId`, name, kind, and nested position in its parent's `children`. Named record fields, enum variants, and named fields of record-shaped variants are also entries. Tuple-shaped members are part of the item's type/signature syntax and are not separate entries because they are not item-like AST nodes.

Children from mixed declaration categories are kept in source order. This includes generic parameters alongside fields, variants, and associated declarations. A world function is one `WorldItem` entry whose source `AstId` belongs to its enclosing `WorldImport` or `WorldExport`; its `FnDecl` remains reachable in the CST. Uses are kept in their own source-ordered list because they do not define items.

The collector stores inner module documentation separately from attributes because the grammar gives it a `DOCS` node on `MODULE_HEADER`. `UseEntry` carries raw attributes because `@target` may apply to a use declaration. An attribute's `has_error` bit records parser recovery or lexical errors even when its argument list is absent.

Item-tree equality is structural within one symbol-interner session. It ignores the snapshot identity used to validate AST-map pairing, excludes snapshot pointers and all absolute source ranges (including nested attribute-argument spans and selected-use ranges), and compares their semantic names and values instead. Module-document source text remains part of equality. This lets edits confined to bodies preserve item-tree equality when later declarations shift in the file, while the newly collected identity, pointers, and spans remain bound to the current snapshot. The query layer must retain the newly collected value when it backdates an equal result.

**Current implementation slice.** `ItemTree::collect` requires a snapshot-bound `AstIdMap` matching the supplied file root and verifies that the supplied decoded source exactly matches the root's lossless token and trivia text. It retains the map's snapshot identity as private validity metadata; `matches_ast_ids` rejects an item tree from another snapshot while structural equality continues to ignore that identity. It collects all current top-level declarations and nested named item-like children, in source order, and records `@impl` header hashes independently of impl bodies. It interns identifiers in NFC after the lexer reports any required `IDENT_NOT_NFC`; invalid raw and reserved names use `Symbol::ERROR`. Only names in the closed attribute table are retained in `RawAttr`. `filter_for_target` checks the supplied tree against the current snapshot-bound AST and a fresh collection, validates only module-level `@target` attributes first, evaluates them against `TargetCfg`'s target name and effective capability set, and removes false entries together with all their nested children before other attribute checks. For retained entries it validates all recognized attributes, deriving top-level function body and export status from current syntax and visibility. The result contains each retained entry's `ValidatedAttrs`; diagnostics on removed entries are suppressed. It filters `use` declarations separately. `collect_for_target` is the preferred combined collection-and-filter entry point before name binding and duplicate checks.

**Implementation update (IR-201).** `ItemTree::stable_shape` produces a `StableItemTree` whose stable hash uses resolved symbol text and explicit tags for item kinds, names, visibility, and attribute argument forms. It preserves semantic item data, ordered `ErasedAstId`s, module-document text, and impl header hashes while omitting `FileId`, snapshot identity, AST pointers, and every source range. `Symbol::ERROR` has its own stable representation, and resolving a non-error symbol through a foreign interner returns an error. This is groundwork only: it does not yet define or wire `def_collector(FileId)`, nor rebuild a current `ItemTree` from the shape.

---

## 2.9 HIR-lowering desugarings

### 2.9.1 General rules

1. This section fixes the **HIR shape** of each construct. The meaning of the construct (typing, evaluation order, temporaries, drops, panics) is owned by [spec/06](../spec/06-expressions-and-statements.md), [spec/10](../spec/10-errors-and-panics.md), [spec/11](../spec/11-async-and-concurrency.md), and [spec/16](../spec/16-core-library.md). A shape is chosen so that applying the ordinary rules to it gives exactly the meaning that those chapters define for the original construct. Where a construct needs a rule that the ordinary shape cannot express, it is **kept** as a dedicated HIR node instead of being rewritten, and the rule is implemented by the stage named in the table of §2.9.18.
2. Desugared nodes carry `SpanCtxt::Desugared(kind)` and the range of the source construct (§2.8.4).
3. Synthesized match arms have `Arm::synthesized = true`. The exhaustiveness checker (impl/03 §3.12) never reports `UNREACHABLE_PATTERN` for a synthesized arm. When the synthesized fallback arm of an `if let` or `while let` is unreachable, because the user pattern is irrefutable, impl/03 reports `IRREFUTABLE_CONDITION_PATTERN` (warning, [spec/06](../spec/06-expressions-and-statements.md) §6.14.2, §6.16.2) instead.
4. Desugared nodes refer to core definitions through `PathRoot::Lang(item)` (N-E-19). The lang items used by this chapter are:

```rust
pub enum LangItem {
    IntoIterator, IntoIteratorIntoIter, Iterator, IteratorNext,     // D-EXPR-11
    Stream, StreamNext,                                             // Stage 5, D-ASYNC-01
    Option, OptionSome, OptionNone, Result, ResultOk, ResultErr,
    Range, RangeInclusive, RangeFrom, RangeTo, RangeToInclusive, RangeFull,   // §2.9.16
    Format, FmtWriter,                                              // D-EXPR-12
    Panic,                                                          // `panic(msg: Str) -> Never`, D-EXPR-06
    // the derivable traits of D-TYPE-08, for §2.9.17:
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Arbitrary,
    Add, Sub, Mul, Div, Rem, Neg, BitAnd, BitOr, BitXor, BitNot,
}
```

   The mapping from each `LangItem` to a definition in `sai:core` / `sai:alloc` is owned by spec/16; impl/03 §3.3.6 resolves it. `IntoIteratorIntoIter`, `IteratorNext`, and `StreamNext` denote the trait methods `IntoIterator.into_iter`, `Iterator.next`, and the `Stream` method that `for await` uses to obtain the next element.
5. In the shapes below, `⟦x⟧` is the lowering of the source construct `x`, `%name` is a synthetic binding (a `BindingId` with a synthetic name), `lang::X` is `PathRoot::Lang(X)`, and `[synth]` marks a synthesized arm. The shapes use SAI-like notation in `text` blocks; they are not SAI source.

### 2.9.2 `while` (Stage 1)

```text
LABEL? while COND BLOCK
⟹ Loop { label: LABEL, source: While, body: Block {
       tail: If { cond: ⟦COND⟧, then: ⟦BLOCK⟧, els: Block { tail: Break { label: None, value: None } } } } }
```

The synthesized `break` targets this loop, because it is not nested in any other loop. A `break` or `continue` inside `COND` targets this loop as well, which is the meaning of spec/06.

### 2.9.3 `while let` (Stage 1)

```text
LABEL? while let PAT = EXPR BLOCK
⟹ Loop { label: LABEL, source: WhileLet, body: Block {
       tail: Match { scrutinee: ⟦EXPR⟧, source: WhileLet, arms: [
           Arm { pat: ⟦PAT⟧, body: ⟦BLOCK⟧ },
           Arm [synth] { pat: Wild, body: Break { label: None, value: None } } ] } } }
```

### 2.9.4 `if let` (Stage 1)

```text
if let PAT = EXPR BLOCK ( else ELSE )?
⟹ Match { scrutinee: ⟦EXPR⟧, source: IfLet, arms: [
       Arm { pat: ⟦PAT⟧, body: ⟦BLOCK⟧ },
       Arm [synth] { pat: Wild, body: ⟦ELSE⟧ or, without else, Tuple([]) } ] }
```

- `ELSE` is a block or another `if` (`else if` and `else if let` chains lower recursively).
- A plain `if COND BLOCK else ELSE` stays `ExprKind::If`.
- `MatchSource::IfLet` is preserved so that MIR building applies the temporary rule that spec/06 defines for the scrutinee of `if let` and so that diagnostics say "`if let`" rather than "`match`".

### 2.9.5 `for` (Stage 2; iteration protocol per D-EXPR-11)

```text
LABEL? for PAT in EXPR BLOCK
⟹ Match { scrutinee: Call { callee: Path(lang::IntoIteratorIntoIter), args: [⟦EXPR⟧] }, source: ForLoop, arms: [
       Arm { pat: Binding { mode: Var, name: %iter }, body:
           Loop { label: LABEL, source: ForLoop, body: Block {
               tail: Match { scrutinee: Call { callee: Path(lang::IteratorNext),
                                               args: [Borrow(Mut, Path(%iter))] },
                             source: ForLoop, arms: [
                   Arm { pat: Ctor { path: lang::OptionSome, args: [⟦PAT⟧] }, body: ⟦BLOCK⟧ },
                   Arm [synth] { pat: Ctor { path: lang::OptionNone, args: None }, body: Break { label: None, value: None } } ] } } } } ] }
```

- The outer `match` (rather than a `let`) keeps the temporaries of `EXPR` alive until the loop ends, which is the meaning of spec/06 for the iterator expression of `for`.
- `PAT` must be irrefutable. Because the inner match covers `Some(_)` only through `PAT`, a refutable `PAT` makes it non-exhaustive; impl/03 reports this case as `REFUTABLE_PATTERN_IN_FOR` ([spec/06](../spec/06-expressions-and-statements.md) §6.16.3), not as `NON_EXHAUSTIVE_MATCH`, using `MatchSource::ForLoop`.
- `break` with a value targeting the loop is `INVALID_BREAK_VALUE`, reported by impl/03 using `LoopSource::ForLoop` (the loop's type is `()`, spec/06 N-C-15).
- The outer `match` matches the shape of spec/06 §6.16.3, which uses a block with a hidden `var`; the two are equivalent except for the temporary scope of `EXPR`, which spec/06 (N-C-26) extends to the whole loop exactly as the `match` does.
- The call `IntoIterator.into_iter(e)` takes `e` by value (D-EXPR-11); `for x in borrow xs` passes a `borrow Vec<T>`.
- The shape needs nothing beyond Stage 1, but `IntoIterator` and `Iterator` are Stage 2 lang items (spec/16 §16.3.2), so `for` is Stage 2. Before Stage 2, impl/03 §3.14.4 reports it with the feature `sai:core/iter.IntoIterator`.

### 2.9.6 `for await` (Stage 5)

```text
LABEL? for await PAT in EXPR BLOCK
⟹ Match { scrutinee: ⟦EXPR⟧, source: ForAwait, arms: [
       Arm { pat: Binding { mode: Var, name: %stream }, body:
           Loop { label: LABEL, source: ForAwait, body: Block {
               tail: Match { scrutinee: Await(Call { callee: Path(lang::StreamNext), args: [Borrow(Mut, Path(%stream))] }),
                             source: ForAwait, arms: [
                   Arm { pat: Ctor { path: lang::OptionSome, args: [⟦PAT⟧] }, body: ⟦BLOCK⟧ },
                   Arm [synth] { pat: Ctor { path: lang::OptionNone, args: None }, body: Break { label: None, value: None } } ] } } } } ] }
```

`for await` iterates a value whose type implements `Stream` (spec/11 §11.6). `Stream` itself only has the unsafe `poll_next` (D-ASYNC-01), so `lang::StreamNext` is not a library function: it is a compiler-internal marker (impl/03 §3.3.6, N-E-26) that impl/03 §3.7.11 recognizes in exactly this shape and types as an await of the next element, and that impl/04 lowers to an `Await` terminator of kind `StreamNext`, which the async transform implements with `poll_next` (spec/11 §11.6.2). A compiler without Stage 5 reports `FEATURE_NOT_IMPLEMENTED` at the `await` token and lowers the loop as `Err`.

### 2.9.7 `let … else` (kept)

`let PAT = EXPR else BLOCK` is kept as `StmtKind::Let { els: Some(⟦BLOCK⟧) }`. MIR building lowers it (impl/04): it evaluates `EXPR`, tests `PAT`, binds on success, and runs `BLOCK` on failure. impl/03 checks that `BLOCK` has type `Never`. The temporaries of `EXPR` are dropped before `BLOCK` runs when the match fails, and at the end of the statement when it succeeds ([spec/06](../spec/06-expressions-and-statements.md) §6.24.2 item 7). Keeping the statement avoids a desugaring into a `match` whose arm would have to introduce bindings into the enclosing block's scope.

### 2.9.8 `?` (kept)

`EXPR?` is kept as `ExprKind::Try(⟦EXPR⟧)`. Whether it applies to `Result` or `Option` depends on the operand type, which impl/03 records as a `TryKind` (D-EXPR-05: no conversion, identical error types). MIR building lowers it to a discriminant switch and an early return (impl/04).

### 2.9.9 Implicit members (kept)

`.Variant` and `.Variant(args)` in expressions are kept as `ExprKind::ImplicitMember(name)` (the arguments are a `Call` around it), and in patterns as `PatKind::ImplicitMember`. impl/03 resolves the name against the expected type ([spec/06](../spec/06-expressions-and-statements.md)), reporting `IMPLICIT_MEMBER_NEEDS_TYPE` when there is none, and records the resolved variant in its results tables.

### 2.9.10 Compound assignment (kept)

`PLACE op= EXPR` is kept as `StmtKind::AssignOp { op, lhs: ⟦PLACE⟧, rhs: ⟦EXPR⟧ }`. D-EXPR-02 defines it as `{ let t = e; place = place op t }` with the subexpressions of `place` evaluated once. A source-level rewrite would duplicate `PLACE`, so MIR building implements the rule directly: it evaluates `EXPR` into a temporary, evaluates `PLACE` once into a MIR place, and assigns `place = place op t` (impl/04).

### 2.9.11 Format strings (Stage 3, kept)

`f"…"` is kept as `ExprKind::FormatString(pieces)`, where adjacent text pieces are merged, `{{` and `}}` are decoded to `{` and `}`, and each interpolation is `FmtPiece::Arg { expr: ⟦e⟧, spec }`. impl/03 type-checks each argument against `Format` and the specifier (D-EXPR-12); MIR building expands the expression into the call sequence of [spec/06](../spec/06-expressions-and-statements.md) §6.20.3. A compiler without Stage 3 reports `FEATURE_NOT_IMPLEMENTED` at `FSTRING_START`, with the feature `f-strings` (impl/03 §3.14.4).

### 2.9.12 `assert` (kept)

`assert COND` and `assert COND, MSG` are kept as `StmtKind::Assert { cond, msg, cond_text }`, where `cond_text` is the source text of `COND` with leading and trailing whitespace removed, captured at lowering time because the default message needs it ([spec/06](../spec/06-expressions-and-statements.md) §6.20.2). MIR building (impl/04) evaluates `COND` and, only when it is `false`, evaluates `MSG` (or uses `"assertion failed: " + cond_text`) and panics with the kind `UserAssert` (D-MIR-01) at the `assert` keyword. impl/03 checks `COND: Bool` and `MSG: Str` or `String`.

### 2.9.13 Patterns

Lowering performs the purely syntactic pattern checks:

1. **Duplicate bindings.** A name bound twice in one pattern (outside of different alternatives of an or-pattern) is `DUPLICATE_BINDING` (error, the code of D-MOD-03), reported at the second occurrence. Redeclaration across statements of one block is checked by the resolver (impl/03 §3.4).
2. **Or-patterns.** For `p1 | … | pn`, lowering computes for each alternative the set of `(name, binding mode)` pairs it binds. If the sets differ, it reports `OR_PATTERN_BINDING_MISMATCH` once, naming the first name that is missing from, or has a different mode in, some alternative. All alternatives bind the same `BindingId` for the same name, assigned at its first occurrence in the first alternative. Equality of the bound types is checked by impl/03.
3. Nested or-patterns are flattened (`(a | b) | c` is `Or([a, b, c])`); a leading `|` is dropped.
4. `PAREN_PAT` is lowered to its content. `name @ p` is `Binding { sub: Some(⟦p⟧) }`.
5. `-INT` in a literal pattern or a range end is `Lit::Int { negated: true }`.
6. In a slice pattern, the elements before the rest element go to `before`, the rest element to `rest` (`Some(None)` for `..`, `Some(Some(binding))` for `..x`), and the elements after it to `after`.
7. A positional `CtorPatArg` that is a bare binding is kept positional (`name: None`). If the constructor turns out to be a record, impl/03 treats it as punning (spec/03 §3.14).

**Current implementation slice.** `sai_hir::lower::lower_literal_pat` lowers integer, string, byte-string, character, byte, and boolean patterns to `PatKind::Lit`. A direct minus sign is preserved as `Lit::Int { negated: true }`; integer overflow remains `value: None` for the type checker's range diagnostic. Float patterns and malformed numeric tokens become `PatKind::Err` because parsing already reported their errors. The shared literal decoder preserves the documented recovery values for error-marked text and character tokens.

`sai_hir::lower::lower_pattern` additionally lowers wildcard, binding, constructor, implicit-member, and or-patterns, recursively lowers tuples, slices, and `name @ pattern`, and strips parenthesized patterns to their contents. Constructor paths retain all ordered `ConstPath` segments, use `PathRoot::Plain`, and map the path and each segment to their source syntax. `Self` is a reserved token excluded by the `ConstPath` grammar. Constructor patterns preserve absent versus explicit argument lists, interleaved positional and named arguments, and a trailing `..`; supported child patterns are preflighted and recursively lowered before any constructor nodes or bindings are allocated. `()` is an empty `PatKind::Tuple`, and `(pattern,)` remains a one-element tuple. Slice elements before and after the optional rest are retained in order; a bare `..` is distinct from no rest, and a bound rest reuses binding lowering. Multiple rest nodes become `PatKind::Err` after the parser's `MULTIPLE_REST_PATTERNS` diagnostic. Or-patterns lower to `PatKind::Or`; nested and parenthesized alternatives are flattened and a leading `|` is dropped. Missing or error-node patterns become `PatKind::Err`.

`lower_pattern` dispatches range patterns to `sai_hir::lower::lower_range_pat`, which supports literal and constant-path endpoints and the documented `a..=b`, `a..b`, `a..`, and `..=b` forms. A constant-path endpoint becomes `PatRangeEnd::Path`; its path and ordered segments receive source-map IDs after the range pattern's ID. Endpoint validity and range shape are checked before those path IDs are allocated, so an invalid endpoint does not leave path nodes in the source map. Invalid raw paths, endpoint literals, or endpoint arrangements rejected by parsing become `PatKind::Err`.

Binding patterns retain `Value`, `Var`, `Borrow`, and `BorrowMut` modes. `name @ pattern` becomes one `PatKind::Binding` with a recursively lowered `sub`; its `BindingId` is assigned before bindings in the subpattern. Missing, reserved, and invalid raw binding names become `Symbol::ERROR` after the lexer diagnostic, while valid raw identifiers are normalized and interned without their prefix. Invalid raw identifiers that parse as constructor paths or range endpoints, such as `r#Self` and `r#Self..=4`, become `PatKind::Err`; valid constant paths lower through the shared path lowerer.

Tuple patterns preserve their ordered child patterns in `PatKind::Tuple`; `()` has no elements and `(pattern,)` has one. The tuple receives its HIR ID before its elements. Recursive preflight keeps future unsupported pattern variants from leaving partial HIR nodes or binding IDs.

Slice lowering partitions the ordered pattern elements around the optional `REST_PAT` by source position, placing the parts in `before`, `rest`, and `after`. A second rest node becomes `PatKind::Err` after the parser's existing diagnostic. Bound rests use the shared binding lowerer, so their mode, normalized name, source span, and `BindingId` follow the same rules as other bindings.

Constructor and implicit-member patterns preserve unresolved names for later resolution. Their positional and named arguments remain in source order, an absent argument list stays `None`, explicit `()` becomes an empty list, and a trailing `..` sets `has_rest`.

Or-pattern binding planning compares each alternative's normalized names and binding modes before HIR nodes are allocated. Names in the first alternative get their `BindingId` there; a name that occurs only in later, mismatching alternatives gets an ID at its first occurrence in those alternatives, and later occurrences share it. The source map keeps the first actual occurrence as each binding's canonical syntax. A differing name set or mode adds one `OR_PATTERN_BINDING_MISMATCH` diagnostic at the `OR_PAT`; type equality remains an impl/03 check. For recovery, the planner retains the union of names from all alternatives when checking sibling patterns, so a duplicate present in any alternative is not missed. A repeated name within one alternative adds `DUPLICATE_BINDING` at the later occurrence; duplicate occurrences that alias a binding shared across alternatives receive a distinct recovery `BindingId`.

### 2.9.14 Closures

`move? fn(params) -> RET? BLOCK` is lowered to `ExprKind::Closure(Closure { def_id, capture: Move or Infer, params, ret, body })`. The closure's `DefId` has the `DefPathData::Closure` component under the enclosing owner (§2.8.2). The body is part of the enclosing owner's HIR and of its `HirId` space, so a closure is type-checked together with the function that contains it (D-TYPE-05). Capture analysis is done after type checking by impl/03 §3.11; the HIR does not list captures.

### 2.9.15 `async fn`, `async` blocks, and `.await` (Stage 5)

```text
async fn NAME(PARAMS) -> T CLAUSES BLOCK
⟹ FnSig { is_async: true, ret: T, … }   with   Body { params: PARAMS, value: Block {
       tail: AsyncBlock { def_id: <AsyncBlock under the fn>, capture: Move, block: ⟦BLOCK⟧ } } }
```

- The signature keeps the written return type `T` and `is_async: true`. The type of a call (an anonymous future type implementing `Future<Output = T>`, D-ASYNC-01) is computed by impl/03 from the signature.
- The synthesized async block captures by move, and impl/03 §3.11.2 makes it capture every parameter, in parameter order, whether or not the body uses it, so the future owns all its inputs (D-ASYNC-01, spec/11 §11.2.1 and §11.5.5). `requires` clauses are checked at the call (D-CONTRACT-01); `ensures` clauses refer to the value produced by the future, as spec/09 and spec/11 define.
- The synthetic async block of an `async fn` is the first owner-local nested-definition ordinal (0), as if introduced at the function head. Explicit closure and async-block syntax then receives later ordinals in source preorder. Closures and async blocks inside the source body use the synthetic async block as their nearest lexical parent; nested definitions in contracts remain under the function owner.
- `async move? BLOCK` is `ExprKind::AsyncBlock { capture: Move or Infer }`. `EXPR.await` is `ExprKind::Await(⟦EXPR⟧)`. MIR building keeps `Await` as a terminator until the coroutine transform (impl/04).
- A compiler without Stage 5 reports `FEATURE_NOT_IMPLEMENTED` at the first `async` or `await` token of each owner, with the feature of spec/01 §1.4.5 for the construct of that token (`async-fn`, `async-block`, `await` or `for-await`), and lowers the construct as `Err`.

### 2.9.16 Ranges (kept)

Range expressions are kept as `ExprKind::Range { start, end, inclusive }`. impl/03 gives them the types of [spec/06](../spec/06-expressions-and-statements.md) §6.19 (D-EXPR-09, N-C-19): `Range<T>`, `RangeInclusive<T>`, `RangeFrom<T>`, `RangeTo<T>`, `RangeToInclusive<T>`, and `RangeFull`, which are the `LangItem`s of the same names (§2.9.1). MIR building constructs them as aggregates of their fields `start` and `end`.

### 2.9.17 `@derive`

For each trait named in a valid `@derive(…)` on a record, enum, distinct or refined declaration (validation in §2.10; derivability per kind is spec/05 §5.12.4), lowering creates a synthetic owner. A resource is excluded: spec/15 §15.2.4 and §15.3.2 require `DERIVE_NOT_SUPPORTED`.

```rust
pub struct DerivedImpl {
    pub owner_id: OwnerId,           // DefPath: <type's DefPath>/DerivedImpl(trait_name)
    pub trait_: LangItem,            // one of the derivable traits of D-TYPE-08
    pub self_def: LocalDefId,        // the type declaration
    pub span: Span,                  // the trait name inside @derive(...), with SpanCtxt::Desugared(Derive)
}
```

- A derived impl has no HIR body. Its generic parameters are those of the type; its header and where-clauses are the generated header of spec/05 §5.12.1 (`Ti: Tr` for every type parameter of a record or enum, `B: Tr` for a distinct type), built by impl/03 §3.8.6. The method bodies are generated directly as MIR shims by impl/04, so they never pass through type inference.
- impl/03 treats a `DerivedImpl` like a trait impl for coherence, impl selection, and method resolution.

### 2.9.18 Summary

| Construct | HIR | Meaning owned by | Lowered further by |
|---|---|---|---|
| `while c b` | `Loop` + `If` + `Break` (§2.9.2) | spec/06 | — |
| `while let` | `Loop` + `Match` (§2.9.3) | spec/06 | — |
| `if let` | `Match { source: IfLet }` (§2.9.4) | spec/06 | — |
| `for` | `Match` + `Loop` + `Match` (§2.9.5) | spec/06, D-EXPR-11 | — |
| `for await` | as `for`, with `Await` (§2.9.6) | spec/11 | impl/04 (Await) |
| `var x = e` | `Let` with `BindingMode::Var` | spec/06 | — |
| let-else | `StmtKind::Let { els }` | spec/06 | impl/04 |
| `?` | `ExprKind::Try` | spec/10, D-EXPR-05 | impl/03 (TryKind), impl/04 |
| `.Variant` | `ImplicitMember` | spec/06 | impl/03 |
| `op=` | `StmtKind::AssignOp` | spec/06, D-EXPR-02 | impl/04 |
| `f"…"` | `FormatString` | spec/06 §6.20.3, D-EXPR-12 | impl/03, impl/04 |
| `assert` | `StmtKind::Assert` | spec/06 §6.20.2, D-EXPR-06 | impl/04 |
| closures | `Closure` | spec/06, spec/07, D-TYPE-09 | impl/03 (captures), impl/04 |
| `async fn` / `async` / `.await` | `AsyncBlock`, `Await` | spec/11 | impl/04 |
| ranges | `ExprKind::Range` | spec/06, D-EXPR-09 | impl/03, impl/04 |
| `@derive` | `DerivedImpl` owner | spec/15, D-TYPE-08 | impl/04 (shims) |
| `-INT`, `-FLOAT` | negated `Lit` | spec/02 §2.6.1 | impl/03 |
| `owned T`, parentheses | removed | D-OWN-01 | — |
| `region r { }` | `ExprKind::Region` | spec/07, D-OWN-07 | impl/04 |

---

## 2.10 Attributes and lowering-time checks

Attributes are a closed set (D-SYN-09). Their targets, argument grammars, and meanings are defined by [spec/15](../spec/15-attributes-and-lints.md); `@derive` semantics by [spec/05](../spec/05-types.md) §5.12. This section defines **where** the reference implementation checks them, the data structures that carry them, and the identifier checks done during lowering.

### 2.10.1 Parsing and the closed name set (Stage 0)

1. The parser builds `ATTRIBUTE` nodes with the generic `AttrArgs` grammar of spec/03 §3.3 and does not interpret names or arguments. Attributes are accepted in every position that spec/03 lists: before an `Item`, `FieldDecl`, `VariantDecl`, `TraitItem`, `ImplItem`, `InterfaceItem`, `IfaceResourceItem`, and `ExternItem`.
2. **Statement position.** An attribute sequence at the start of a statement is parsed with the same function. If the token after the last attribute is an item starter (§2.5.5), the whole construct is `ITEM_IN_BLOCK`. Otherwise the attributes are wrapped in an `ERROR` node, reported once as `INVALID_ATTRIBUTE_TARGET` (error, spec/15) at the first `@`, and the statement is parsed normally. Attributes before an expression-list element (including a positional, named, or base call argument, an array element, or a tuple element) are likewise reported once, retained in an `ERROR` node, and followed by the parsed expression. Attributes before a parameter or generic parameter are reported once and parsed into their surrounding parameter node.
3. **Closed set.** The validation pass of the `parse` query (§2.5.12) checks each attribute name against this table, which is D-SYN-09:

   | `AttrName` | Source name | | `AttrName` | Source name |
   |---|---|---|---|---|
   | `Derive` | `derive` | | `ExportName` | `export_name` |
   | `Inline` | `inline` | | `ImportName` | `import_name` |
   | `Cold` | `cold` | | `Repr` | `repr` |
   | `MustUse` | `must_use` | | `Flags` | `flags` |
   | `Deprecated` | `deprecated` | | `Target` | `target` |
   | `Allow` | `allow` | | `Cases` | `cases` |
   | `Warn` | `warn` | | `Seed` | `seed` |
   | `Deny` | `deny` | | `Doc` | `doc` |
   | `PanicHook` | `panic_hook` | | | |

   Any other name is `UNKNOWN_ATTRIBUTE` (error, spec/03), reported at the name. If exactly one table name has an edit distance of at most 2 (Damerau–Levenshtein over bytes), the diagnostic carries a fix that replaces the name; if several do, it carries none, so the fix never depends on a tie-break (P8). An unknown attribute is kept in the CST (for the formatter) and is omitted from every `RawAttr` list, so no later stage sees it.
4. Raw identifiers are not attribute names: `@r#derive` is `UNKNOWN_ATTRIBUTE`.

**Current implementation slice.** The snapshot-bound `parse` query walks every `ATTRIBUTE` after building the CST and checks its exact source spelling against `ATTRIBUTE_NAMES`. It reports `UNKNOWN_ATTRIBUTE` over the name, including for raw identifiers. It attaches a machine-applicable `replace_node` fix when exactly one table entry is within byte-wise Damerau–Levenshtein distance 2; ties and more distant names have no fix. The candidate set is length-filtered before distance calculation, so arbitrarily long unknown names do not allocate a correspondingly large distance matrix. `AttrName::from_source_name` uses the same table when the item tree decodes recognized attributes.

### 2.10.2 Raw and validated attributes

The item tree and the HIR carry attributes in a decoded but unvalidated form:

```rust
pub struct RawAttr {
    pub name: AttrName,                        // §2.10.1 table; unknown names never reach here
    pub args: Option<Vec<RawAttrArg>>,         // None: no parentheses; Some(vec![]): `()`
    pub span: Span,                            // from `@` to the closing `)` or the name
    pub ptr: AstPtr<ast::Attribute>,
    pub has_error: bool,                       // parser recovery or lexical error in this attribute
}
pub enum RawAttrArg {
    Word { name: Symbol, raw: bool, is_upper: bool, args: Option<Vec<RawAttrArg>>, span: Span }, // Ident; preserves UIDENT vs LIDENT
    Str { value: SmolStr, span: Span },                                          // STRING, decoded
    Int { value: Option<u128>, span: Span },                                     // INT; None if it exceeds u128
    KeyValue { key: Symbol, raw: bool, value: RawAttrValue, value_span: Span, span: Span }, // Ident '=' …
}
pub enum RawAttrValue { Str(SmolStr), Int(Option<u128>), Word(Symbol) }
```

Validation turns the raw list of one attribute target into a `ValidatedAttrs` value. It is the query `attrs(DefId) -> Arc<ValidatedAttrs>` (§2.1.2), computed from the `RawAttr` list of the item-tree entry, so it does not depend on bodies:

```rust
pub struct ValidatedAttrs {
    pub derives: Vec<(LangItem, Span)>,        // in source order, duplicates removed (with a diagnostic)
    pub inline: Option<(InlineHint, Span)>,    // InlineHint::{Hint, Never}
    pub cold: Option<Span>,
    pub must_use: Option<Span>,
    pub deprecated: Option<Deprecation>,       // message and optional replacement path, as spec/15 defines
    pub lint_levels: Vec<LintAttr>,            // @allow / @warn / @deny, in source order
    pub export_name: Option<(SmolStr, Span)>,
    pub import_name: Option<(SmolStr, Span)>,
    pub repr: Option<(ReprAttr, Span)>,        // ReprAttr::{C, Int(PrimTy)}
    pub flags: Option<Span>,
    pub target: Option<(TargetPredicate, Span)>,
    pub cases: Option<(u64, Span)>,
    pub seed: Option<(u64, Span)>,
    pub doc_hidden: Option<Span>,
    pub panic_hook: Option<Span>,
    pub diagnostics: Vec<Diagnostic>,
}
pub struct LintAttr { pub level: LintLevel, pub name: Symbol, pub span: Span }   // LintLevel::{Allow, Warn, Deny}
pub enum TargetPredicate {
    Target(SmolStr), Capability(SmolStr),
    Not(Box<TargetPredicate>), All(Vec<TargetPredicate>), Any(Vec<TargetPredicate>),
}
```

Every field holds the first valid occurrence; the span is kept so that later stages can point at the attribute. The `attrs` query performs the checks that need only the attribute and the syntactic kind of its target, in this order, and reports each problem at the attribute or argument concerned:

| Check | Code (owner) | Example |
|---|---|---|
| the attribute's stage (spec/15 §15.2.3) is implemented by the toolchain; otherwise the feature is `attribute @name` and no later check runs | `FEATURE_NOT_IMPLEMENTED` (spec/01), spec/15 §15.2.4 step 2 | `@derive` before Stage 2 |
| target kind (the syntactic kind of the annotated declaration, from the item tree) is allowed for the attribute | `INVALID_ATTRIBUTE_TARGET` (spec/15) | `@flags` on a function |
| argument shape matches the attribute's argument grammar | `INVALID_ATTRIBUTE_ARGUMENT` (spec/15) | `@cases("x")`, `@inline(always)` |
| the attribute occurs at most once on the target, except the lint-level attributes | `DUPLICATE_ATTRIBUTE` (spec/15) | two `@repr` |
| a list argument names the same entry twice | `DUPLICATE_ATTRIBUTE_ARGUMENT` (spec/15) | `@derive(Clone, Clone)` |
| attributes that exclude each other | `CONFLICTING_ATTRIBUTES` (spec/15) | `@cold` with `@inline` |
| a derive name is one of the fixed names | `UNKNOWN_DERIVE` (spec/05) | `@derive(Debug)` |

A failed check drops that attribute (or argument) from `ValidatedAttrs`; the remaining attributes are still validated. The set of allowed targets, the argument grammar of each attribute, the exclusion pairs, and the special target cases are those of spec/15, encoded in a static `ATTRIBUTE_SCHEMAS: [AttrSchema; 17]` table indexed by `AttrName`:

```rust
pub struct AttrSchema {
    pub name: AttrName,
    pub stage: u8,
    pub targets: &'static [TargetRule],
    pub args: ArgGrammar,
    pub repeatable: bool,                      // true only for Allow, Warn, Deny
    pub conflicts: &'static [AttributeConflict],
    pub special_target_cases: &'static [SpecialTargetCase],
}
pub struct TargetRule { pub kind: TargetKind, pub condition: TargetCondition }
pub enum TargetCondition {
    Any, ModuleLevel, HasBody, IsExported, InherentImpl,
    ExternWasmOrUnsafeC, ExternWasm, InsideInterface, TraitDeclaration,
}
pub enum ArgGrammar {
    None, DeriveList, Inline, OptionalNonEmptyString, Deprecated, LintCodes, NonEmptyString,
    Repr, TargetPredicate, IntegerRange { min: u64, max: u64 }, Word(&'static str),
}
pub struct AttributeConflict { pub other: AttrName, pub condition: ConflictCondition }
pub enum ConflictCondition { Always, InlineIsHint }
pub enum SpecialTargetCase {
    DeriveOnResource, ReprInsideInterface, FlagsOutsideInterface,
}
pub enum TargetKind {
    Fn, TraitFn, ImplFn, ExternFn, InterfaceFn, Record, Enum, Distinct, Refined, Alias, Resource, Trait, Impl,
    Const, Invariant, Effect, Interface, World, ExternBlock, Use, Test, Property, Field, Variant, AssocType,
}
```

**Current implementation slice.** `sai_hir::attrs::validate_attrs` applies the schema to one raw attribute list and an explicit `AttributeTarget`. It checks the stage, target context, argument grammar, duplicate attributes and arguments, and conflicts in source order. Stage 0 values are retained for `@inline`, `@cold`, `@must_use`, `@deprecated`, `@allow`/`@warn`/`@deny`, `@repr`, `@target`, and `@doc(hidden)`. Repeated `@target` operands are compared by their written source spelling, so different string-literal spellings with the same decoded value are not treated as duplicates. An invalid empty `instead` path is dropped while a valid deprecation message is kept; raw `instead` keys remain distinguishable and are rejected. Attributes with `has_error` are skipped so parser or lexer recovery does not produce cascaded attribute diagnostics. A missing target kind suppresses all attribute checks, as required for a declaration whose kind could not be recovered. Syntax positions where attributes are prohibited by §15.2.1 are diagnosed once by the parser and skipped by HIR attribute validation; there is no target-kind sentinel for those positions. Diagnostics are bound to the supplied workspace snapshot and convert decoded source spans back to original-file offsets, including a leading BOM.

The schema contains every stage so the same table can drive later work, but `IMPLEMENTED_ATTRIBUTE_STAGE` is currently 0; production validation reports `FEATURE_NOT_IMPLEMENTED` for later-stage attributes before target or argument checks. Stage 2 `@derive` validation is implemented and exercised through the stage-parameterized unit-test path, while the public stage gate remains at 0. `@target` names and capabilities are validated and its predicate is retained; `TargetPredicate::matches` evaluates it, and `ItemTree::collect_for_target` is the combined API for target-filtered module collection. The current workspace has no package collector, name resolver, query engine, or compiler driver, so no build path calls the API yet. Name-resolution and semantic checks are deferred to their owning phases. For retained ItemTree entries, collection derives function kind and body presence, export visibility, trait/inherent/interface impl kind, extern ABI and safety, and interface and trait membership from the current AST ancestry. It validates recognized attributes on nested declarations and valid fields and variants, and returns results keyed by `ErasedAstId`; parser-rejected positions receive empty results so the parser remains the sole owner of their target diagnostic. The `attrs(DefId)` query remains follow-up work; direct callers of `validate_attrs` still supply their own `AttributeTarget`. Function parameters do not have an attribute-collection query because spec/15 §15.2.1 forbids attributes on parameters and the parser reports `INVALID_ATTRIBUTE_TARGET`.

`@derive` names are matched textually against the fixed list of spec/05 §5.12.1 and are never resolved in scope. The raw item-tree argument preserves whether a word used `UIDENT` or `LIDENT`, so lowercase spellings fail the argument grammar instead of being accepted as derives. Duplicate trait arguments report `DUPLICATE_ATTRIBUTE_ARGUMENT` and are dropped; an unknown trait reports `UNKNOWN_DERIVE`, drops the whole attribute, and offers a machine-applicable replacement when exactly one fixed name is within Damerau–Levenshtein distance 2 over Unicode scalar values. A second `@derive` reports `DUPLICATE_ATTRIBUTE` with a merge fix that combines its arguments into the first attribute, preserving argument text and comments and following the source file's first-line ending convention. Valid names map to `LangItem`s in `ValidatedAttrs.derives`, in source order. `lower_derived_impl_owners` revalidates that complete list against the current ItemTree and syntax, checks the parent HIR owner and canonical source-order DefPath, then creates one `DerivedImpl` owner and source map per entry. It supports top-level and interface-member records, enums, distinct types, and refined types; resource declarations remain excluded by spec/15. The production attribute stage remains at 0 until earlier-stage attributes are complete. Whether a valid trait is derivable for the declaration kind and whether field types satisfy its obligations remain semantic checks owned by impl/03 (§2.10.4).

Lint-level attributes are validated here for shape, per-target consistency, and name class. Each argument MUST be a plain `Word`, with case preserved in its interned name; groups, raw identifiers, and wildcards are rejected (§15.15.3–§15.15.5). Repeating a name at one level reports `DUPLICATE_ATTRIBUTE_ARGUMENT`; assigning it different levels reports `CONFLICTING_LINT_LEVEL` at the later assignment with a label at the first, and drops the later assignment. After those checks, the HIR validator consults the complete v0.1 name registry in `sai_diagnostics`, which contains all 492 Appendix A codes and metadata for the 28 lints, including each default level and stage. The non-exhaustive `Code` enum, `Code::ALL`, and `Code::from_name` cover the same complete snapshot; the enum is generated from the sorted checked-in name list and build-time validation rejects malformed or duplicate entries. A registered lint is retained even when its stage is newer than the implemented compiler stage, as required by N-A-62. A hard code is dropped with `NOT_A_LINT`; an unregistered name is dropped with `UNKNOWN_LINT` and lint-only candidates within distance 2, ordered by distance then by lint declaration order and capped at 10. If ASCII uppercasing exactly names a lint, the diagnostic has a machine-applicable token replacement; uppercasing a hard code never gets that fix. `sai_diagnostics::resolve_lint_occurrence` now applies the documented precedence, attribute-range selection, dependency cap, and `--deny-warnings` to caller-supplied validated inputs. Manifest/CLI validation, package-policy and workspace-membership collection, construction of attribute scopes from HIR owners, diagnostic-pipeline integration, and the `lint_levels(OwnerId)` query remain follow-up work. The precedence of levels is owned by spec/15, and its application to diagnostics by impl/11.

The generated `Code` enum keeps the 76 pre-existing variants first in their original order so derived `Ord` stays compatible; newly added registry codes follow the sorted registry order. The frozen legacy list is checked during the build.

**`@target` and item collection.** `@target` is the only attribute that changes which items exist. `ItemTree::collect_for_target` decodes its predicate from `RawAttr` before a `DefId` exists and evaluates it against the driver's `TargetCfg` (impl/03 §3.3.1); `filter_for_target` provides the same operation for an already collected tree and rejects stale inputs. A false predicate removes the complete module-level entry, including nested items, before other attribute validation, name binding, and duplicate checks. No attribute diagnostics for that entry or its children are returned; lexer and parser diagnostics remain attached to the original parse result. A missing or invalid predicate keeps the entry, and invalid `@target` attributes return their validation diagnostics. For every retained item-tree entry, including nested associated items, valid fields, and variants, the filter returns a `ValidatedAttrs` result keyed by `ErasedAstId`; `use` entries are included separately. Generic parameters, record-variant fields, and world items/includes have empty results because §15.2.1 prohibits attributes there and the parser already diagnoses each sequence once. `TargetFilterResult::scoped_lint_levels` converts the retained validated lint attributes into `ScopedLintLevel`s, using each current AST node's complete text range and original-file byte offsets for both scopes and attribute locations. It checks the tree/map/root/snapshot identity, verifies that each retained entry and `use` has exactly one validated-attribute result, and rejects invalid or foreign source data. The resulting scopes can be passed to `sai_diagnostics::resolve_lint_occurrence`; declarations removed by `@target` contribute no scopes. Attribute context is derived from the current AST, including function body and export status, enclosing impl kind, extern ABI and safety, and interface or trait membership. `TargetCfg::new` rejects unknown targets/capabilities and combinations that contradict the target table, including `memory64` on non-wasm64 targets; its caller supplies artifact-dependent capabilities such as `host`. The filtered tree retains the current snapshot binding and current AST pointers. The CST keeps every declaration, so the formatter and syntax-based tooling still see filtered-out source. This workspace contains no package collector, name resolver, query engine, or compiler driver to call the API, so production phase wiring remains follow-up work.

### 2.10.3 Identifier checks during lowering

HIR lowering performs the identifier checks of spec/02 §2.4 and D-LEX-03 that depend on the declaration, not only on the token:

1. **`NON_ASCII_IDENTIFIER`** (warning lint, spec/02). Reported once per declaration of a binding, item, field, variant, generic parameter, label, or parameter whose name contains a non-ASCII character, at the name. Uses of the name are not reported again. The lint is suppressed for a name that is also reported as `NON_ASCII_PUBLIC_NAME`.
2. **`NON_ASCII_PUBLIC_NAME`** (error, spec/02 §2.4 item 5, D-LEX-03, which list the same names). Reported at the name of:
   - an item declared `pub` or `export`, and an item of a `pub` trait, which has the visibility of the trait (spec/04 §4.6.2);
   - a `pub` field, a variant of a `pub` enum, and a field of such a variant, which has the visibility of the enum (spec/04 §4.6.2);
   - an interface or world, whatever its visibility, and every name declared inside it.
   The methods and associated types of a trait impl repeat the names of the trait and are checked at the trait (N-E-21). Module and file names are checked by impl/03 §3.2 (spec/04); package names by package/01 (spec/02 §2.9).
3. **`IDENT_NOT_NFC`** and **`UNNECESSARY_RAW_IDENT`** are lexical and are reported by the lexer (§2.4); lowering compares identifiers by their NFC-normalized `Symbol`, as spec/02 requires.

These diagnostics are stored in `HirOwner::diagnostics`; those of item-tree-only declarations (fields and variants) are stored with the owner of the enclosing item.

### 2.10.4 Semantic attribute checks performed later

The following checks need resolved names, types, or the whole package. They are listed here so that every attribute check has a defined place; the rules are those of the owning documents.

| Attribute | Check | Code (owner) | Performed by |
|---|---|---|---|
| `@derive` | derivable for the declaration kind | `DERIVE_NOT_SUPPORTED` (spec/05) | impl/03 §3.8 |
| `@derive` | field and payload types implement the trait | `DERIVE_FIELD_MISSING_IMPL` (spec/05) | impl/03 §3.8 |
| `@derive` | supertraits implemented | `MISSING_SUPERTRAIT_IMPL` (spec/05) | impl/03 §3.8 |
| `@derive` | no hand-written impl of the same trait | `OVERLAPPING_IMPLS` (spec/05) | impl/03 §3.8 |
| `@repr` | `C` on records and unit-only enums; an integer type on unit-only enums; discriminants in range | spec/15; `DISCRIMINANT_OUT_OF_RANGE` (spec/05) | impl/03 §3.6 |
| `@flags` | declared inside an interface; unit-only enum without explicit discriminants; at most 32 variants | `INVALID_FLAGS_ENUM` (spec/13 §13.9.4); `WIT_EMPTY_TYPE` for no variants (spec/13 §13.2.2) | `adt_def`, [16](16-interfaces-and-wit.md) §16.9.1 |
| `@export_name` | unique export names across the package, and not `memory` | `DUPLICATE_EXPORT_NAME` (spec/15 §15.7) | impl/03 §3.3 |
| `@panic_hook` | signature, root package, at most one | `PANIC_HOOK_SIGNATURE_MISMATCH`, `PANIC_HOOK_OUTSIDE_ROOT`, `DUPLICATE_PANIC_HOOK` (spec/10 §10.6.1) | impl/03 §3.6.9 |
| `@must_use` | unused results | `UNUSED_MUST_USE` (spec/06 §6.25.2) | impl/03 §3.15 (on typed HIR) |
| `@deprecated` | uses of the item | `DEPRECATED_REFERENCE` (spec/15) | impl/03 §3.4 |
| `@inline`, `@cold` | none at check time; consumed by code generation | — | impl/06, impl/09 |
| `@cases`, `@seed` | only on `property` (target check, §2.10.2) | spec/14 | the harness plan ([04](04-mir.md) §4.13.10), read by the test runner ([13](13-cli.md) §13.6.6.2 item 4) |
| `@doc(hidden)` | none; consumed by documentation and semantic tooling | — | impl/12 |
| `@must_use`, `@export_name`, `@import_name`, `@repr`, `@doc(hidden)`, `@allow`, `@warn`, `@deny` | the attribute has an effect on its target | `UNUSED_ATTRIBUTE` (spec/15 §15.18.6) | impl/03 §3.6.10 |

---

## 2.11 Decisions introduced by this chapter

These decisions fill gaps that [decisions.md](../design/decisions.md) and the language chapters leave open. They are provisional until merged into the decision record. IDs continue in [03](03-name-resolution-and-type-checking.md) and [04](04-mir.md).

| ID | Rule | Rationale |
|---|---|---|
| N-E-1 | The reference implementation's limits are the minimums of spec/01 §1.5.4. In particular a source file larger than 16 MiB is rejected before lexing with `IMPLEMENTATION_LIMIT_EXCEEDED`, so every offset fits in a `u32` `TextSize` (§2.2.2). | P8 (deterministic acceptance, spec/01 §1.5.4 item 2); P7 (compact spans). |
| N-E-2 | Tokens and CST nodes share one `SyntaxKind` enumeration. The lexer emits only single-character `>` and `&`/`|` tokens with a `JOINT` flag; the parser glues `>>`, `>=`, `>>=`, `&&`, `||`, and `::` (§2.3.1, §2.5.2). | P8: one kind space for tooling; spec/02 §2.8 requires `>` to be single. |
| N-E-3 | For the forbidden-character rule, "string literals" includes byte strings, format-string text, character literals, and byte literals; bidirectional controls are forbidden everywhere (§2.2.3). | P2: Trojan Source protection; P8: one rule for all quoted text. |
| N-E-4 | The lexer runs to completion over the whole file before parsing; the only lexer context is the previous token (for `PKG_PATH` and tuple indices) and the bracket stack (§2.4). | P8 and incremental tooling: the token stream is a pure function of the text. |
| N-E-5 | Unterminated simple strings, characters, bytes, and format strings end before the offending line break; multiline strings, raw strings, and block comments extend to EOF. A multiline opener without a line break is `MALFORMED_MULTILINE_STRING` and is lexed as if one followed (§2.4.6, §2.4.12). | P8: errors stay local to one line where the grammar allows it. |
| N-E-6 | A format string is lexed as the token sequence `FSTRING_START`, `FSTRING_TEXT`, `FSTRING_INTERP_OPEN`, …, `FSTRING_SPEC`, `FSTRING_INTERP_CLOSE`, `FSTRING_END`, so interpolations are ordinary CST expressions (§2.4.7). | P3 and P8: tooling sees every interpolated expression. |
| N-E-7 | Doc-comment tokens take part in NEWLINE generation as tokens that are in neither E nor C, even when the parser later demotes them (§2.4.8). | P8: NEWLINE generation never depends on the parser. |
| N-E-8 | A `;` (reported as `NO_SEMICOLON`) acts as a line break for NEWLINE generation (§2.4.9). | P8: `a = 1; b = 2` yields one diagnostic, not a cascade. |
| N-E-9 | `ERROR` tokens belong to E. A mismatched closer pops the bracket stack up to the nearest matching opener, if any, and is otherwise ignored for the stack (§2.4.11). | P8: recovery that keeps later lines' NEWLINEs correct. |
| N-E-10 | A NEWLINE token is zero-width and placed directly after the trailing trivia of the token that ends the line (§2.4.11, §2.6.3). | P8: the line break remains trivia of the next token, so the CST is lossless. |
| N-E-11 | The parser counts entries into `expr`, `pattern`, `type_`, and `block`; beyond 512 it reports `IMPLEMENTATION_LIMIT_EXCEEDED` once per file and skips the innermost construct as an `ERROR` node (§2.5.3). | spec/01 §1.5.4 minimum of 256 combined levels; P8: bounded stack use on every input. |
| N-E-12 | Function clauses are parsed in any order; misordering is `CLAUSE_ORDER` with a reordering fix, and a repeated `where`, `borrows`, or `effects` clause is `DUPLICATE_CLAUSE` (§2.5.4). | P8 and P3: one precise diagnostic and a mechanical fix instead of a parse failure. |
| N-E-13 | `a..b..c` and `..a..b` are `CHAINED_RANGE`; the tree is still built (§2.5.6). | spec/03 makes ranges non-associative; P8: dedicated diagnostic like `CHAINED_COMPARISON`. |
| N-E-14 | Under the `NO_BLOCK_OPERAND` restriction (conditions, scrutinees, `for` iterables), an optional operand (range end, `return`/`break` value) is not parsed when the next token is `{` (§2.5.6). | P8: `for i in 0.. {` means what it looks like. |
| N-E-15 | A line that starts with a binary operator other than `-`, `*`, and `..` after a complete statement is reported as `UNEXPECTED_NEWLINE` with a fix that moves the operator, and the operator and operand are wrapped in one `ERROR` node (§2.5.11). | P8 and P3: the common mistake gets one actionable diagnostic. |
| N-E-16 | Trivia is stored on tokens as leading and trailing lists, with the attachment rule of §2.6.3. | P3: comments move with their tokens in structured edits and formatting. |
| N-E-17 | v0.1 reparses the whole file after every edit; the parse is cached per file revision (§2.6.7). | P8 and simplicity; parsing is linear and well within the latency budget. |
| N-E-18 | HIR is built before name resolution: paths stay unresolved in HIR, and resolution results live in side tables of impl/03 (§2.8.1). | P8: HIR is a function of one file's syntax; incrementality. |
| N-E-19 | Desugared code refers to core items through `PathRoot::Lang` and binds synthetic names that user code cannot spell (hygiene, §2.8.1, §2.9.1). | P8: desugarings cannot be broken by user shadowing. |
| N-E-20 | Impls are disambiguated in `DefPath`s by the low 64 bits of the stable hash of their header tokens (§2.8.2). | P3: impl identity survives edits elsewhere in the file. |
| N-E-21 | `NON_ASCII_PUBLIC_NAME` uses the visibility of spec/04 §4.6.2: items of a `pub` trait and fields of the variants of a `pub` enum are public names. Each name is checked once, at its declaration, so the items of a trait impl are not checked (§2.10.3). | spec/02 §2.4 item 5 and D-LEX-03 list the same names; a trait impl can only repeat names of its trait, so a second report would duplicate the first (P8). |

## 2.12 Diagnostics

A code defined in another document starts its description with "defined in" and the owning section. All other codes are defined by this chapter. A bare section reference after it, such as (§2.5.6), is the section of this chapter that reports the code.

| Code | Severity | Description |
|---|---|---|
| `INVALID_UTF8` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; ill-formed UTF-8 (§2.2.1) |
| `INVALID_LINE_ENDING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; lone CR (§2.2.1) |
| `FORBIDDEN_CHARACTER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; forbidden control, bidirectional, or invisible character (§2.2.3) |
| `UNTERMINATED_COMMENT` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.3; unterminated block comment |
| `MISPLACED_DOC_COMMENT` | warning | defined in [spec/02](../spec/02-lexical-structure.md) §2.3; doc comment in an invalid position, demoted to a comment (§2.6.4) |
| `IDENT_NOT_NFC` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.4; identifier not in NFC |
| `NON_ASCII_IDENTIFIER` | warning (lint) | defined in [spec/02](../spec/02-lexical-structure.md) §2.4; non-ASCII identifier, reported at its declaration (§2.10.3) |
| `NON_ASCII_PUBLIC_NAME` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.4; non-ASCII public name (§2.10.3) |
| `UNNECESSARY_RAW_IDENT` | warning (lint) | defined in [spec/02](../spec/02-lexical-structure.md) §2.4; `r#` on a name that is not a keyword |
| `RESERVED_WORD` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.5.3; use of a reserved word |
| `NO_NULL` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.5.3; use of `null`, `nil`, or `undefined` |
| `NO_EXCEPTIONS` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.5.3; use of `try`, `catch`, or `throw` |
| `MALFORMED_NUMBER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.1; malformed numeric literal |
| `LITERAL_OUT_OF_RANGE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.1; literal out of range for its type (checked by impl/03) |
| `INVALID_ESCAPE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.3; invalid escape sequence |
| `UNTERMINATED_STRING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.4; unterminated string |
| `INCONSISTENT_INDENT` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.4; multiline string line does not start with the base indentation |
| `UNTERMINATED_CHAR` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.3; unterminated character or byte literal |
| `NO_SEMICOLON` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.8; use of `;` |
| `NO_BANG_OPERATOR` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.8; use of a lone `!` |
| `NO_DOUBLE_COLON` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.8; use of `::` |
| `USE_AND_OR` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.8; use of `&&` or `\|\|` |
| `UNEXPECTED_NEWLINE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.10; NEWLINE where the grammar does not allow one (§2.5.11) |
| `INVALID_RAW_IDENT` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.4; `r#self`, `r#Self`, or `r#_` (§2.4.3) |
| `INVALID_CHAR_LITERAL` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.3; empty character literal, raw TAB, or more than one scalar value between the quotes (§2.4.5) |
| `MALFORMED_RAW_STRING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.4; raw string with more than 255 `#` (§2.4.6) |
| `MALFORMED_MULTILINE_STRING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.4; opening `"""` not followed by a line break (§2.4.6) |
| `INVALID_INTERPOLATION` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.5; line break, string literal, or empty interpolation, or unmatched `}` in a format string (§2.4.7) |
| `INVALID_FORMAT_SPEC` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.5; format specifier that is not `LIDENT ( '(' INT ')' )?` (§2.4.7) |
| `MALFORMED_PACKAGE_PATH` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.9; `PKG_PATH` segment that violates its rules (§2.4.10) |
| `UNEXPECTED_CHARACTER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.2; character that cannot start any token (§2.4.9) |
| `EXPECTED_TOKEN` | error | defined in [spec/03](../spec/03-grammar.md) §3.16; a specific token was expected (§2.5.10, §2.5.11) |
| `EXPECTED_EXPRESSION` | error | defined in [spec/03](../spec/03-grammar.md) §3.16; an expression was expected |
| `CHAINED_COMPARISON` | error | defined in [spec/03](../spec/03-grammar.md) §3.13.1; `a < b < c` (§2.5.6) |
| `CHAINED_RANGE` | error | defined in [spec/03](../spec/03-grammar.md) §3.13.1; chained range operators, such as `a..b..c` (§2.5.6, N-E-13) |
| `CLAUSE_ORDER` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; function clauses out of order (§2.5.4) |
| `DUPLICATE_CLAUSE` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; a second `where`, `borrows`, or `effects` clause on one function (§2.5.4, N-E-12) |
| `MISSING_FN_BODY` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; function without a body where one is required (§2.5.12) |
| `UNEXPECTED_FN_BODY` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; body on a function of an `extern` block (§2.5.4) |
| `INVALID_SELF_TYPE` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; receiver type other than `Self`, `borrow Self`, `borrow mut Self` (§2.5.12) |
| `NAMED_ARGUMENT_ON_FUNCTION` | error | defined in [spec/03](../spec/03-grammar.md) §3.13.2; named argument in a call that resolves to a function (reported by impl/03) |
| `INVALID_ASSIGN_TARGET` | error | defined in [spec/03](../spec/03-grammar.md) §3.12; assignment to an expression that is not a place expression (§2.5.12) |
| `BLOCK_IN_CONDITION` | error | defined in [spec/03](../spec/03-grammar.md) §3.13.5; unparenthesized record construction or block-like operand in a condition (§2.5.6) |
| `NO_ASYNC_CLOSURE` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.5; reported by the parser at `async` before a closure, which is kept in an `ERROR` node, with the rewrite fix (§2.5.6) |
| `NO_AS_CAST` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.10; parsed as `CAST_EXPR` without a diagnostic and lowered to `ExprKind::AsCast` (§2.5.6, §2.8.6); reported by [impl/03](03-name-resolution-and-type-checking.md) §3.10.3 |
| `REDUNDANT_PUB_EXPORT` | error | defined in [spec/03](../spec/03-grammar.md) §3.3; `pub export` (§2.5.4) |
| `INVALID_EXPORT_TARGET` | error | defined in [spec/03](../spec/03-grammar.md) §3.3; `export` on an item that is not a function (§2.5.12) |
| `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` | error | defined in [spec/03](../spec/03-grammar.md) §3.6; discriminant on an enum with payload variants (§2.5.12) |
| `UNKNOWN_ATTRIBUTE` | error | defined in [spec/03](../spec/03-grammar.md) §3.3; attribute name outside the closed set (§2.10.1) |
| `UNKNOWN_ABI` | error | defined in [spec/03](../spec/03-grammar.md) §3.10; ABI string other than `"wasm"` and `"C"` (§2.5.4) |
| `INVALID_EXTERN_BLOCK` | error | defined in [spec/12](../spec/12-unsafe-and-ffi.md) §12.7.1; `extern "wasm"` block without a valid `module STRING`, or `extern "C"` block without `unsafe` (§2.5.4) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; `async fn` in an interface, interface resource, world, or extern block before Stage 5, and `unsafe extern "C"` before Stage 7 (§2.5.4, §2.5.12) |
| `GENERIC_INTERFACE_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; generic parameters or `where` on an interface, interface-resource, or world function head (§2.5.4, §2.5.12) |
| `INTERFACE_FN_CLAUSE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; `borrows`, `requires`, `ensures` or `unsafe` on an interface function, resource member, or world function; `unsafe` on a function of an interface impl (§2.5.12) |
| `EFFECTS_ON_INTERFACE_FN` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.6.1; `effects` on an interface, interface-resource, or world function head (§2.5.4, §2.5.12) |
| `INTERFACE_FN_BODY` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.2.2; body on a function of an interface, an interface resource or a world (§2.5.4) |
| `WORLD_IMPORT_NEEDS_INTERFACE` | error | defined in [spec/03](../spec/03-grammar.md) §3.9; world `import` or `export` that names a whole package instead of an interface (§2.5.12) |
| `UNEXPECTED_FN_BODY` | error | defined in [spec/03](../spec/03-grammar.md) §3.5; body on an extern function (§2.5.4) |
| `INFER_IN_SIGNATURE` | error | defined in [spec/03](../spec/03-grammar.md) §3.15; `_` in an item signature (§2.5.12) |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | defined in [spec/03](../spec/03-grammar.md) §3.7; `pub` on an item of a trait impl (§2.5.12) |
| `OR_PATTERN_BINDING_MISMATCH` | error | defined in [spec/03](../spec/03-grammar.md) §3.14; alternatives of an or-pattern bind different names or modes (§2.9.13) |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | error | defined in [spec/03](../spec/03-grammar.md) §3.8; invariant declared outside the module of its type (checked by impl/03) |
| `MISSING_INITIALIZER` | error | defined in [spec/03](../spec/03-grammar.md) §3.12; `let` or `var` without `=` (D-SYN-10, §2.5.5) |
| `ITEM_IN_BLOCK` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.25.1; item declared inside a block (§2.5.5, §2.10.1) |
| `BASE_ARG_NOT_LAST` | error | defined in [spec/03](../spec/03-grammar.md) §3.13.2; `..base` argument that is not the last argument (§2.5.6) |
| `INVALID_TUPLE_INDEX` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.2; tuple index that is not a plain decimal integer (§2.5.6) |
| `TUPLE_INDEX_OUT_OF_RANGE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.2; tuple index too large (reported by impl/03, §2.7.3) |
| `FLOAT_PATTERN` | error | defined in [spec/03](../spec/03-grammar.md) §3.14; floating-point literal in a pattern (§2.5.8) |
| `MULTIPLE_REST_PATTERNS` | error | defined in [spec/03](../spec/03-grammar.md) §3.14; more than one `..` in a slice pattern (§2.5.8) |
| `DUPLICATE_BINDING` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.4.3; a name bound twice in one pattern (§2.9.13) |
| `REFUTABLE_PATTERN_IN_FOR` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.3; refutable `for` pattern (reported by impl/03, §2.9.5) |
| `INVALID_BREAK_VALUE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.16.2; `break` with a value targeting `while`, `while let`, or `for` (reported by impl/03, §2.9.5) |
| `IRREFUTABLE_CONDITION_PATTERN` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.14.2; irrefutable pattern in `if let` or `while let` (reported by impl/03, §2.9.1) |
| `UNREACHABLE_PATTERN` | warning (lint) | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.1; never reported for synthesized arms (§2.9.1) |
| `NON_EXHAUSTIVE_MATCH` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.15.1; never reported for desugared `for` matches (§2.9.5) |
| `IMPLICIT_MEMBER_NEEDS_TYPE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.4.3; implicit member without an expected type (reported by impl/03, §2.9.9) |
| `INVALID_ATTRIBUTE_TARGET` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.2.1; attribute on a target it does not apply to (§2.10.1, §2.10.2) |
| `INVALID_ATTRIBUTE_ARGUMENT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.2.4; attribute arguments that do not match its grammar (§2.10.2) |
| `DUPLICATE_ATTRIBUTE` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.2.4; a non-repeatable attribute given twice (§2.10.2) |
| `DUPLICATE_ATTRIBUTE_ARGUMENT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.2.4; the same entry twice in an attribute's list (§2.10.2) |
| `CONFLICTING_ATTRIBUTES` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.2.4; mutually exclusive attributes on one target (§2.10.2) |
| `UNKNOWN_LINT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.15.4; lint-level attribute naming an unknown lint (§2.10.2) |
| `NOT_A_LINT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.15.4; lint-level attribute naming a hard code (§2.10.2) |
| `CONFLICTING_LINT_LEVEL` | error | defined in [package/01](../package/01-package-contract.md) §1.13; used for level attributes by spec/15 §15.15.3 item 4: two different levels for one lint on one node (§2.10.2) |
| `DEPRECATED_REFERENCE` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.6.4; a reference to a deprecated declaration outside a deprecated context (reported by impl/03, §2.10.4) |
| `UNKNOWN_DERIVE` | error | defined in [spec/05](../spec/05-types.md) §5.12.1; name in `@derive` outside the fixed list (§2.10.2) |
| `DERIVE_NOT_SUPPORTED` | error | defined in [spec/05](../spec/05-types.md) §5.12.4; trait not derivable for the declaration kind (§2.10.4) |
| `DERIVE_FIELD_MISSING_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.12.1; a field type lacks the derived trait (§2.10.4) |
| `MISSING_SUPERTRAIT_IMPL` | error | defined in [spec/05](../spec/05-types.md) §5.9.1; derived trait whose supertrait is not implemented (§2.10.4) |
| `OVERLAPPING_IMPLS` | error | defined in [spec/05](../spec/05-types.md) §5.10.2; derived impl overlapping a written impl (§2.10.4) |
| `DISCRIMINANT_OUT_OF_RANGE` | error | defined in [spec/05](../spec/05-types.md) §5.6.2; discriminant outside the `@repr` type (§2.10.4) |
| `DUPLICATE_EXPORT_NAME` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.7; two exports with the same name, or the export name `memory` (§2.10.4) |
| `UNUSED_MUST_USE` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.25.2; a discarded value of a must-use type or call (D-EXPR-01, §2.10.4) |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | defined in [spec/01](../spec/01-introduction.md) §1.5.4; source file or nesting depth beyond the implementation limit (§2.2.2, §2.5.3) |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; staged feature not implemented (attributes before their stage, f-strings before Stage 3, async before Stage 5) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11](11-diagnostics.md) §11.4.6; parser fuel exhausted, a compiler defect (§2.5.3) |
