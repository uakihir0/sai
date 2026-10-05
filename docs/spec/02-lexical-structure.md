# 02. Lexical Structure

**Status:** Normative
**Stage:** 0 (format strings: Stage 3)
**Related:** [03-grammar.md](03-grammar.md), [decisions.md](../design/decisions.md) D-LEX-01..06

This chapter defines how a SAI source file (`.sai`) is turned into a sequence of tokens. `sai.package` and `sai.lock` follow separate lexical rules (see [package/01-package-contract.md](../package/01-package-contract.md)).

---

## 2.1 Source text

1. A source file MUST be encoded in UTF-8. Ill-formed UTF-8 is the error `INVALID_UTF8`.
2. A leading BOM (U+FEFF) is ignored. The formatter removes it.
3. The line terminator is LF (U+000A). CR LF is treated as a single LF. A lone CR is the error `INVALID_LINE_ENDING`. The formatter normalizes line endings to LF.
4. The following characters are the error `FORBIDDEN_CHARACTER` when they appear outside comments and string literals:
   - C0 control characters other than TAB, and DEL
   - bidirectional control characters (U+202A–U+202E, U+2066–U+2069). As a defense against Trojan Source attacks, these are an error **inside comments and string literals as well**.
   - non-ASCII whitespace outside string literals (U+00A0, U+2000–U+200B, U+3000, and so on)
5. Whitespace consists of SPACE (U+0020) and TAB (U+0009). TAB is accepted, but the formatter replaces it with four spaces.
6. A source position (span) is a half-open range `[start, end)` of **UTF-8 byte offsets** from the start of the file. Diagnostics also report the line (1-based) and the column (in UTF-8 bytes, 1-based). Conversion to UTF-16 columns is provided for LSP clients ([impl/11-diagnostics.md](../impl/11-diagnostics.md)).

## 2.2 Token kinds

The lexer produces the following tokens.

| Kind | Examples |
|---|---|
| `LIDENT` | `user`, `parse_json`, `_tmp`, `r#type` |
| `UIDENT` | `User`, `I32`, `MAX_SIZE` |
| keyword | `fn`, `let`, `borrow`, ... (§2.5) |
| `INT` | `0`, `1_000`, `0xFF` |
| `FLOAT` | `1.0`, `2.5e-3` |
| `STRING` | `"a"`, `"""..."""`, `r#"..."#` |
| `FSTRING` | `f"x = {x}"` (Stage 3) |
| `BYTE_STRING` | `b"abc"` |
| `CHAR` | `'a'`, `'\n'` |
| `BYTE` | `b'a'` |
| `LABEL` | `'outer` |
| `PKG_PATH` | `sai:http/client`, `wasi:http/outgoing-handler` (§2.9) |
| `DOC_OUTER` | `/// ...` |
| `DOC_INNER` | `//! ...` |
| punctuator | `(` `)` `{` `}` `[` `]` `,` `.` `:` `=` `->` `=>` ... (§2.8) |
| `NEWLINE` | statement terminator (§2.10) |
| `EOF` | end of input |

Comments (`//`, `/* */`) and whitespace are **trivia** and are not emitted as tokens. The CST, however, keeps all trivia for the formatter and for semantic tooling ([impl/02-frontend-ast-hir.md](../impl/02-frontend-ast-hir.md)).

Tokenization uses **maximal munch**, with one exception: `>` is always emitted as a single-character token (§2.8). A character that cannot start any token (outside comments and literals) is the error `UNEXPECTED_CHARACTER`; the lexer skips it and continues.

## 2.3 Comments

```
LineComment  ::= "//" (not "/" and not "!") ...to end of line
              |  "//"                         // empty line comment
              |  "////" ...                   // four or more slashes: ordinary comment
BlockComment ::= "/*" ( BlockComment | any character )* "*/"    // nests
DocOuter     ::= "///" ...to end of line      // documents the following item / field / variant
DocInner     ::= "//!" ...to end of line      // documents the enclosing module (file)
```

1. Block comments nest. An unterminated block comment is the error `UNTERMINATED_COMMENT`.
2. For NEWLINE generation (§2.10), a block comment that contains one or more line breaks counts as a single line break.
3. `DOC_OUTER` may appear only immediately before an item, field, variant, trait item, or interface item. Anywhere else it is the warning `MISPLACED_DOC_COMMENT` and is treated as an ordinary comment.
4. `DOC_INNER` may appear only at the start of a file, before the first item.
5. The content of a doc comment is interpreted as CommonMark. One leading space is removed from each line.

## 2.4 Identifiers

```
Ident         ::= RawPrefix? IdentStart IdentContinue*
RawPrefix     ::= "r#"
IdentStart    ::= XID_Start | "_"
IdentContinue ::= XID_Continue
```

1. Identifiers follow the Default Identifier Syntax of Unicode Standard Annex #31 (XID_Start / XID_Continue).
   For v0.1, the Unicode data version is 16.0.0 ([D-LEX-06](../design/decisions.md#d-lex-06-unicode-data-version)).
2. An identifier MUST be **in NFC**. Otherwise it is the error `IDENT_NOT_NFC`, and the normalized form is offered as a fix. Two identifiers are the same if and only if their code point sequences are equal after NFC normalization.
3. A lone `_` is not an identifier; it is the wildcard token `_`.
4. **Case classification** (D-LEX-02): if the first character, after removing the raw prefix `r#` and any leading `_` characters, is uppercase (General Category Lu or Lt), the identifier is a `UIDENT`. Otherwise it is a `LIDENT`. `_Foo` is a `UIDENT`, `__init` is a `LIDENT`, and an identifier consisting only of underscores (`__`) is a `LIDENT`.
   - Note: a leading `_` means "allowed to be unused" (it exempts the name from the lint `UNUSED_VARIABLE`), so using `_Foo` as a type name is discouraged.
5. An identifier that contains non-ASCII characters is subject to the lint `NON_ASCII_IDENTIFIER` (warning). The following names MUST be ASCII (`NON_ASCII_PUBLIC_NAME`, D-LEX-03): items that are `pub` or `export`; `pub` fields and the variants of `pub` enums; modules; interfaces and worlds, and every name declared inside them, because they all become WIT names. Package names are ASCII by the syntax of `PkgSeg` (§2.9). Because module names come from file names, file names MUST be ASCII as well.
6. A raw identifier `r#name` is used to write an identifier spelled like a keyword. The identifier's name is the part after `r#`. `r#self`, `r#Self`, and `r#_` are not allowed (`INVALID_RAW_IDENT`). Putting `r#` on a name that is not a keyword is the lint `UNNECESSARY_RAW_IDENT` (warning).
7. There is no limit on identifier length. An implementation MUST handle identifiers of at least 1024 bytes.

## 2.5 Keywords

### 2.5.1 Strict keywords

The following words are always keywords and cannot be used as identifiers (except with `r#`).

```
and      as       assert   async    await    borrow   break    const
continue else     enum     export   extern   false    fn       for
if       impl     in       let      loop     match    move     mut
not      or       owned    pub      return   self     Self     trait
true     type     unsafe   use      var      where    while
```

### 2.5.2 Contextual keywords

The following words are lexed as `LIDENT`. The parser treats them as keywords only in grammar positions that accept them (written `"kw"` in [03-grammar.md](03-grammar.md)). Everywhere else they are ordinary identifiers.

```
borrows   covers    distinct  effect    effects   ensures   import
include   interface invariant module    old       property  region
requires  resource  result    test      world
```

- `result` (inside an `ensures` clause) and `old` (inside an `ensures` clause, in the form `old(`) have special meaning only within contract predicates.
- `module` is a keyword only in `unsafe module` (at the start of a file) and in `extern "wasm" module "env"`.

### 2.5.3 Reserved words

The following words are reserved for future use. Using one as an identifier is the error `RESERVED_WORD`.

```
actor  catch  class  defer  dyn  macro  nil  null  static  throw
try    undefined  yield
```

Using `null`, `nil`, or `undefined` produces the diagnostic `NO_NULL` (error), which suggests `Option` and `None`. Using `try`, `catch`, or `throw` produces `NO_EXCEPTIONS` (error), which suggests `Result` and `?`.

## 2.6 Literals

### 2.6.1 Integer literals

```
INT      ::= DecInt | HexInt | OctInt | BinInt
DecInt   ::= Digit ( Digit | "_" )*
HexInt   ::= "0x" HexDigit ( HexDigit | "_" )*
OctInt   ::= "0o" OctDigit ( OctDigit | "_" )*
BinInt   ::= "0b" BinDigit ( BinDigit | "_" )*
```

1. `_` is a digit separator and does not affect the value. It cannot appear at the end, directly after a prefix (`1_` and `0x_1` are `MALFORMED_NUMBER`), or twice in a row.
2. A decimal literal cannot have other digits after a leading `0` (`007` is not allowed), so that it cannot be mistaken for an octal literal.
3. Uppercase prefixes (`0X`, `0B`, `0O`) are not allowed. Hexadecimal digits may be uppercase or lowercase; the formatter normalizes them to uppercase.
4. There are no type suffixes (such as `10u8`). An identifier character directly after the digits is `MALFORMED_NUMBER`.
5. Integer literals are unsigned. `-5` is the unary `-` applied to the literal `5`. However, the type checker treats a literal directly preceded by `-` as a single negative constant, so that `-128` can be written for `I8` ([05-types.md](05-types.md)).
6. Literal values are represented with arbitrary precision. Once the literal's type is determined, a value outside the range of that type is the error `LITERAL_OUT_OF_RANGE`.

### 2.6.2 Floating-point literals

```
FLOAT     ::= DecDigits "." DecDigits Exponent?
           |  DecDigits Exponent
Exponent  ::= ( "e" | "E" ) ( "+" | "-" )? DecDigits
DecDigits ::= Digit ( Digit | "_" )*
```

1. At least one digit is required on each side of the decimal point (neither `1.` nor `.5` is allowed).
2. There are no hexadecimal float literals.
3. When the previous token is `.` (as in the tuple field access `t.0.1`), `0.1` is not lexed as a FLOAT but as `INT(0)` `.` `INT(1)`. Precisely: when the previous token is `.`, the lexer lexes a digit sequence only as a `DecInt`.
4. The value is rounded to the type (`F32` / `F64`) with IEEE 754 roundTiesToEven. A finite literal that rounds to infinity is `LITERAL_OUT_OF_RANGE`.

### 2.6.3 Character and byte literals

```
CHAR       ::= "'" ( CharBody | Escape ) "'"
BYTE       ::= "b'" ( AsciiBody | ByteEscape ) "'"
Escape     ::= "\n" | "\r" | "\t" | "\\" | "\'" | "\"" | "\0" | "\u{" HexDigit{1,6} "}"
ByteEscape ::= "\n" | "\r" | "\t" | "\\" | "\'" | "\"" | "\0" | "\x" HexDigit HexDigit
```

1. A `CHAR` has type `Char` and contains exactly one Unicode scalar value. A `\u{..}` escape that denotes a surrogate (D800–DFFF) or a value above 10FFFF is `INVALID_ESCAPE`.
2. `CharBody` is any single character (scalar value) other than `'`, `\`, a line break, or TAB. An empty literal `''`, a raw TAB, or more than one scalar value between the quotes is `INVALID_CHAR_LITERAL`. A literal without its closing `'` is `UNTERMINATED_CHAR`.
3. A `BYTE` has type `Byte`. `AsciiBody` is a printable ASCII character (0x20–0x7E) other than `'` and `\`.
4. **Distinguishing labels**: when `'` is followed by identifier characters, the lexer reads the identifier. If the character right after it is `'`, the token is a `CHAR`; otherwise it is a `LABEL`. `'a'` is a CHAR and `'outer` is a LABEL.

### 2.6.4 String literals

```
STRING           ::= SimpleString | MultilineString | RawString
SimpleString     ::= '"' ( StrChar | Escape | LineContinuation )* '"'
LineContinuation ::= "\" LF Whitespace*      // removes the line break and the next line's leading whitespace
MultilineString  ::= '"""' LF ( any character | Escape )* LF Whitespace* '"""'
RawString        ::= "r" HashN '"' any characters '"' HashN     // same number of # (0..255) on both sides
BYTE_STRING      ::= "b" ( SimpleString | RawString )           // ASCII and ByteEscape only
```

1. A `SimpleString` cannot contain a raw line break (`UNTERMINATED_STRING`).
2. Multiline strings: the opening `"""` must be followed directly by a line break. The closing `"""` must be on a line of its own (it may be preceded by whitespace). The whitespace before the closing `"""` is the **base indentation**. The value is obtained by removing the base indentation from the start of every line. A non-empty line that does not start with the base indentation is the error `INCONSISTENT_INDENT`. The first line break and the line break directly before the closing `"""` are not part of the value.
3. Raw strings do not interpret escapes. A `r"..."` string (without `#`) cannot contain `"`. More than 255 `#` is `MALFORMED_RAW_STRING`. An opening `"""` that is not followed directly by a line break is `MALFORMED_MULTILINE_STRING`.
4. A string literal (`STRING`) has type `Str`. Its value is placed in static data and its region is `'static` ([07-ownership-and-borrowing.md](07-ownership-and-borrowing.md)).
5. A `BYTE_STRING` has type `Slice<Byte>`. Like a string literal, its value is placed in static data and its region is `'static`.
6. String contents are **not** NFC-normalized; the value is exactly the code point sequence that was written.

### 2.6.5 Format strings (Stage 3)

```
FSTRING    ::= 'f"' ( StrChar | Escape | "{{" | "}}" | Interp )* '"'
Interp     ::= "{" Expr ( ":" FormatSpec )? "}"
FormatSpec ::= LIDENT ( "(" INT ")" )?            // e.g. {x:hex}, {v:pad(8)}
```

1. After reading `f"`, the lexer alternates between string segments and interpolated segments, and tokenizes each interpolated segment recursively with the ordinary rules. It counts matching `{` and `}`, and the first matching `}` ends the interpolation. An interpolation cannot contain line breaks or nested string literals (v0.1). A line break or a string literal inside an interpolation, an empty interpolation `{}`, and an unmatched `}` are `INVALID_INTERPOLATION`. A format specifier that does not match `FormatSpec` is `INVALID_FORMAT_SPEC`. Whether a well-formed specifier is known is checked later (`UNKNOWN_FORMAT_SPEC`, [16-core-library.md](16-core-library.md)).
2. `{{` and `}}` denote the characters `{` and `}`.
3. The meaning is defined by the desugaring rules of `core.fmt` in [16-core-library.md](16-core-library.md).

## 2.7 Wildcard

- `_` is a token of its own. It is used as the wildcard pattern, as an inference hole in types (`Vec<_>`), and to discard a value (`let _ = e`).

## 2.8 Punctuators

```
(   )   {   }   [   ]   ,   .   ..  ..=  :   =   ->  =>  @   ?
+   -   *   /   %   +%  -%  *%  &   |   ^   ~   <<  <   <=  ==  !=
+=  -=  *=  /=  %=  +%= -%= *%= &=  |=  ^=  <<=
>   (`>` is always a single token)
```

1. **Handling of `>`**: the lexer always emits `>` as a single-character `GT` token, with a `joint` flag recording whether the next character follows without intervening whitespace. In expression context, the parser reads `GT(joint) GT` as `>>`, `GT(joint) =` as `>=`, and `GT(joint) GT(joint) =` as `>>=`. This lets types like `Vec<Vec<I32>>` be parsed without special cases. A `=` is also a candidate for composition when it directly follows a joint `GT`.
2. `!` on its own is not a punctuator; only `!=` exists. Using `!` alone is the error `NO_BANG_OPERATOR`, which suggests `not`.
3. `;` is not a punctuator. Using `;` is the error `NO_SEMICOLON`, with a fix that removes it.
4. `::` is not a punctuator (it lexes as two `:`). The parser reports the sequence `:` `:` as the error `NO_DOUBLE_COLON`, with a fix that replaces it with `.`.
5. `&&` and `||` are lexed as `&` `&` and `|` `|`. The parser detects them and suggests `and` / `or` (`USE_AND_OR`).
6. `#` may appear only as part of a raw string.

## 2.9 Package path tokens

`use` declarations and the `import` / `export` / `include` declarations of worlds must treat package and interface paths, which may contain kebab-case, as a single token.

```
PKG_PATH   ::= PkgSeg ":" PkgSeg ( "/" PathSeg )*
PkgSeg     ::= [a-z] [a-z0-9]* ( "-" [a-z0-9]+ )*
PathSeg    ::= [a-z] [a-z0-9_]* ( "-" [a-z0-9]+ )*
```

1. When the previous token is `use` (strict keyword), `export` (strict keyword), `import` (LIDENT), or `include` (LIDENT), the lexer first tries to lex a `PKG_PATH`. If the input starts with `PkgSeg ":"`, it is lexed as a `PKG_PATH` with maximal munch. Otherwise the ordinary rules apply.
2. A `PKG_PATH` cannot contain whitespace.
3. A `PKG_PATH` ends before `.`, whitespace, a line break, `{`, `,`, `}`, and so on. `use sai:http/client.{HttpClient}` lexes as `use` `PKG_PATH(sai:http/client)` `.` `{` `UIDENT` `}`.
4. `PKG_PATH` belongs to the set E (§2.10).
5. Input that starts with `PkgSeg ":"` in such a position but has a segment that violates `PkgSeg` or `PathSeg` (for example an uppercase letter, `--`, or a trailing `-`) is `MALFORMED_PACKAGE_PATH`.

## 2.10 NEWLINE generation (statement termination)

SAI has no semicolons. Statements and items are separated by `NEWLINE` tokens, and the lexer decides where to emit them with the following rules (D-LEX-01).

### 2.10.1 Definitions

- **Terminator set E**: the set of the following tokens.
  - `LIDENT`, `UIDENT`, `PKG_PATH`
  - `INT`, `FLOAT`, `STRING`, `FSTRING`, `BYTE_STRING`, `CHAR`, `BYTE`, `LABEL`
  - the keywords `true` `false` `self` `Self` `return` `break` `continue` `await`
  - `)` `]` `}` `>` `?` `_`
- **Continuation set C**: a line is treated as a continuation of the previous line when its first token (ignoring trivia) is one of:
  - `.` followed by a `LIDENT`, `await`, or an `INT` (method chains, field access, `.await`, tuple indexing)
  - `and`, `or`
  - `else`
- **Bracket stack**: the lexer pushes `(`, `[`, `{`, and the interpolation `{` of an `FSTRING`, and pops on the matching closing bracket.

### 2.10.2 Rules

When the lexer reads a line break (or a block comment containing a line break), it emits one `NEWLINE` after the previous token if and only if **all** of the following hold:

1. The bracket stack is empty, or its top is `{`. (Line breaks inside `(` or `[` are always treated as whitespace.)
2. The last token emitted before the line break belongs to E.
3. The first token on the next line does not belong to C.

In addition:

4. Consecutive `NEWLINE`s collapse into one. (Blank lines and comment-only lines do not add NEWLINEs.)
5. If the last token before EOF belongs to E, one `NEWLINE` is emitted before EOF.
6. The parser skips a `NEWLINE` directly after `{` and directly before `}` ([03-grammar.md](03-grammar.md) §3.1.3).

### 2.10.3 Consequences and examples

Each binding below has its own name, because redeclaring a name in one block is `DUPLICATE_BINDING` (D-MOD-03).

```sai
let x = compute(a,
                b)          // OK: inside `(`, so the line break is ignored
let total = price
    + tax                   // rejected: EXPECTED_EXPRESSION
let gross = price +
    tax                     // OK: `+` is not in E
let net = (price
    - discount)             // OK: inside `(`
let both = a > b and
    c > d                   // OK: `and` is not in E
let either = a > b
    or c > d                // OK: `or` is in C
let users = repo
    .find_all()
    .filter(fn(u) { u.active })   // OK: a line starting with `.find_all` is in C
if ready {
    start()
}
else {                      // OK: `else` is in C
    wait()
}
```

In the second binding a NEWLINE follows `price`, so `let total = price` is a complete statement and `+ tax` starts a new one. There is no prefix `+` ([03](03-grammar.md) §3.13.1), so the parser reports `EXPECTED_EXPRESSION` at `+`.

- A `>` or `>>` at the end of a line produces a NEWLINE, because `>` is in E. `a >\n b` is the syntax error `EXPECTED_EXPRESSION`, with a fix suggesting parentheses or a line break after the operator.
- A NEWLINE in a position where none is expected produces the dedicated diagnostic `UNEXPECTED_NEWLINE`, whose note explains which rule generated the NEWLINE.

### 2.10.4 Positions where the parser skips NEWLINE

Independently of the lexer rules, the grammar allows an optional `NEWLINE` (written `nl`) at specific positions: before each clause of a function signature, before the body block `{` of a function, before `else`, and so on. The exact positions are marked `nl` in [03-grammar.md](03-grammar.md).

## 2.11 Shebang

If the first line of a file starts with `#!`, that line is ignored. This is for scripts run with `sai run`, a Stage 4 command ([impl/13](../impl/13-cli.md)).

## 2.12 Lexical diagnostics

| Code | Severity | Description |
|---|---|---|
| `INVALID_UTF8` | error | ill-formed UTF-8 |
| `INVALID_LINE_ENDING` | error | lone CR |
| `FORBIDDEN_CHARACTER` | error | forbidden control character, bidirectional control character, or invisible whitespace |
| `UNTERMINATED_COMMENT` | error | unterminated block comment |
| `MISPLACED_DOC_COMMENT` | warning | doc comment in an invalid position |
| `IDENT_NOT_NFC` | error | identifier not in NFC |
| `NON_ASCII_IDENTIFIER` | warning (lint) | non-ASCII identifier |
| `NON_ASCII_PUBLIC_NAME` | error | non-ASCII character in a public name or module name |
| `UNNECESSARY_RAW_IDENT` | warning (lint) | unnecessary `r#` |
| `INVALID_RAW_IDENT` | error | `r#self`, `r#Self`, or `r#_` |
| `RESERVED_WORD` | error | use of a reserved word |
| `NO_NULL` | error | use of `null` / `nil` / `undefined` |
| `NO_EXCEPTIONS` | error | use of `try` / `catch` / `throw` |
| `MALFORMED_NUMBER` | error | malformed numeric literal |
| `LITERAL_OUT_OF_RANGE` | error | literal out of range for its type |
| `INVALID_ESCAPE` | error | invalid escape sequence |
| `UNTERMINATED_STRING` | error | unterminated string |
| `INCONSISTENT_INDENT` | error | multiline string line does not match the base indentation |
| `UNTERMINATED_CHAR` | error | unterminated character or byte literal |
| `INVALID_CHAR_LITERAL` | error | empty character literal, raw TAB, or more than one scalar value between the quotes |
| `MALFORMED_RAW_STRING` | error | raw string with more than 255 `#` |
| `MALFORMED_MULTILINE_STRING` | error | opening `"""` not followed by a line break |
| `INVALID_INTERPOLATION` | error | line break, string literal, or empty interpolation, or unmatched `}`, in a format string |
| `INVALID_FORMAT_SPEC` | error | format specifier that does not match `FormatSpec` |
| `MALFORMED_PACKAGE_PATH` | error | `PKG_PATH` segment that violates §2.9 |
| `UNEXPECTED_CHARACTER` | error | character that cannot start any token |
| `NO_SEMICOLON` | error | use of `;` |
| `NO_BANG_OPERATOR` | error | use of a lone `!` |
| `NO_DOUBLE_COLON` | error | use of `::` |
| `USE_AND_OR` | error | use of `&&` / `||` |
| `UNEXPECTED_NEWLINE` | error | NEWLINE in a position the grammar does not allow |
