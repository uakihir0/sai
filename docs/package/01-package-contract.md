# 01. The Package Contract (`sai.package`)

**Status:** Normative
**Stage:** 3 (`artifact`, `wit`, and `export world`: Stage 4)
**Related:** [decisions.md](../design/decisions.md) D-PKG-01..06, D-CAP-01, D-CAP-02, D-TGT-01, D-TGT-02, D-WASM-05; [02-resolver.md](02-resolver.md); [03-lock-file.md](03-lock-file.md); [04-distribution-and-security.md](04-distribution-and-security.md); [spec/02-lexical-structure.md](../spec/02-lexical-structure.md); [spec/04-modules-and-names.md](../spec/04-modules-and-names.md); [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md); [spec/13-components-and-wit.md](../spec/13-components-and-wit.md); [spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md); [targets/targets-and-profiles.md](../targets/targets-and-profiles.md); [impl/13-cli.md](../impl/13-cli.md)

This chapter defines the language of the `sai.package` file: its lexical rules, its complete grammar, and the meaning and validation of every declaration. A `sai.package` file is the **package contract**: it states what a package is (identity and version), what it contains (source, WIT, prebuilt artifacts), what it needs (dependencies), where it runs (targets), and what it may do (capability policy). Version selection is defined in [02-resolver.md](02-resolver.md), the lock file in [03-lock-file.md](03-lock-file.md), and fetching, artifacts, and security in [04-distribution-and-security.md](04-distribution-and-security.md). The meaning of capabilities is owned by [spec/08](../spec/08-effects-and-capabilities.md); this chapter owns only their spelling in `sai.package` and the checks that need nothing but manifests.

---

## 1.1 Overview

### 1.1.1 The package and its file

1. A **package** is a directory that contains a file named exactly `sai.package` (D-PKG-01). The directory is the **package directory**. All relative paths in the file are resolved against it.
2. One file describes one package. The file has no arbitrary code execution, no conditionals, no variables, and no includes (D-PKG-01, draft §31). Reading it never runs code and never accesses the network.
3. The **root package** is the package whose directory a command operates on (the directory that holds the `sai.package` found by the CLI, [impl/13-cli.md](../impl/13-cli.md)). Every other package in the build is a **dependency**.
4. Some declarations are **root-only**: they take effect only when the package is the root package and are ignored, without a diagnostic, when it is a dependency (N-G-16). They are marked "root-only" below.
5. A package's **identity** is `namespace:name` (draft §28), aligned with WIT package identity. A specific release is written `namespace:name@version` in prose and diagnostics (for example `acme:image@1.4.2`). Git is a distribution mechanism, not part of the identity (P6).

### 1.1.2 Package kinds

A package has one of three **kinds**, determined by which content declarations it contains (D-PKG-05, N-G-6):

| Kind | `source` | `artifact` | Content |
|---|---|---|---|
| source package | present | absent | SAI source under the source directory |
| artifact package | absent | present | prebuilt Wasm components plus WIT |
| hybrid package | present | present | both; the form is chosen per target and profile ([04-distribution-and-security.md](04-distribution-and-security.md) §4.5) |

For a user of the package, the kind is invisible: the same `use` declarations and the same API work for every kind (draft §34).

### 1.1.3 Declarations at a glance

| Declaration | Multiplicity | Root-only | Section |
|---|---|---|---|
| `package <id> @ <version>` | exactly one, first | no | §1.4 |
| `compiler <req>` | at most one | no | §1.5 |
| `source "<dir>"` | at most one | no | §1.6 |
| `wit "<dir>"` | at most one | no | §1.6 |
| `artifact component { .. }` | at most one | no | §1.7 |
| `targets { .. }` | exactly one | no | §1.8 |
| `export world <name>` | at most one | no | §1.9 |
| `requires { .. }` | at most one | no | §1.10 |
| `requires test { .. }` | at most one | yes | §1.10.6 |
| `allow { .. }` / `deny { .. }` | at most one each | no | §1.11 |
| `policy { .. }` | at most one | no (one item is root-only) | §1.12 |
| `lints { .. }` | at most one | yes | §1.13 |
| `profile <name> { .. }` | at most one per profile | yes | §1.14 |
| `allocator <id>` | at most one | yes | §1.15 |
| `overrides { .. }` | at most one | yes | §1.16 |
| `description`, `license`, `authors`, `repository` | at most one each | no | §1.17 |

`lints`, the metadata declarations, and the per-requirement `grant` clause are additions to the list of D-PKG-01 (N-G-12, N-G-14, N-G-18).

---

## 1.2 Lexical structure

`sai.package` and `sai.lock` share one lexical grammar (D-PKG-03). It is a small subset of the SAI source lexical grammar of [spec/02](../spec/02-lexical-structure.md), with its own token kinds (N-G-1).

### 1.2.1 Source text

1. The file MUST be UTF-8. Ill-formed UTF-8 is the error `INVALID_UTF8`. A leading BOM (U+FEFF) is ignored.
2. The line terminator is LF. CR LF counts as one LF. A lone CR is the error `INVALID_LINE_ENDING`.
3. `FORBIDDEN_CHARACTER` applies exactly as in [spec/02 §2.1](../spec/02-lexical-structure.md) rule 4: C0 controls other than TAB, and DEL, outside comments and strings; non-ASCII whitespace outside strings; and the bidirectional controls U+202A–U+202E and U+2066–U+2069 everywhere, including comments and strings.
4. Whitespace is SPACE and TAB.
5. Outside comments and strings, every character MUST be ASCII. A non-ASCII character there that is not covered by rule 3 is the error `UNEXPECTED_CHARACTER`. (Every name in a package contract is ASCII, D-LEX-03.)
6. Positions in diagnostics are reported as in [spec/02 §2.1](../spec/02-lexical-structure.md) rule 6 (UTF-8 byte offsets, 1-based lines and columns).

### 1.2.2 Comments

1. `//` starts a comment that runs to the end of the line. `///` and `//!` are ordinary comments; `sai.package` has no doc comments.
2. `/* ... */` is a block comment. Block comments nest. An unterminated block comment is the error `UNTERMINATED_COMMENT`.
3. A block comment that contains a line break counts as one line break for NEWLINE generation (§1.2.6).
4. Comments are trivia. Tools that edit the file ([02-resolver.md](02-resolver.md) §2.10) MUST preserve them.

### 1.2.3 Token kinds

```ebnf
WORD      ::= [a-z] [a-z0-9]* ( [-_] [a-z0-9]+ )*
UWORD     ::= [A-Z] [A-Za-z0-9_]*
PKG_PATH  ::= PkgSeg ":" PkgSeg ( "/" PathSeg )*          (* as in spec/02 §2.9 *)
PkgSeg    ::= [a-z] [a-z0-9]* ( "-" [a-z0-9]+ )*
PathSeg   ::= [a-z] [a-z0-9_]* ( "-" [a-z0-9]+ )*
INT       ::= Digits
VERSION   ::= Digits "." Digits ( "." Digits ( "-" PreRelease )? )?
Digits    ::= [0-9]+
PreRelease ::= PreIdent ( "." PreIdent )*
PreIdent  ::= [0-9A-Za-z-]+
STRING    ::= '"' ( StrChar | Escape )* '"'
StrChar   ::= any character except '"', '\', and LF
Escape    ::= '\n' | '\r' | '\t' | '\\' | '\"' | "\'" | '\0' | '\u{' Hex{1,6} '}'
Punct     ::= '{' | '}' | ',' | '@' | '^' | '~' | '=' | '>=' | '>' | '<=' | '<' | '->' | '/'
```

1. Tokenization uses maximal munch. Whenever the input at a token start matches `PkgSeg ":" PkgSeg`, the token is a `PKG_PATH` (unlike SAI source, no preceding keyword is needed, because `:` has no other use here). A `PKG_PATH` contains no whitespace.
2. A token that starts with a digit is an `INT` if it has no `.`, and a `VERSION` otherwise. A `VERSION` with only two numeric components cannot have a pre-release part.
3. Version literals follow SemVer 2.0.0 ([02-resolver.md](02-resolver.md) §2.2), with these lexical checks:
   - A numeric component (in `INT` or `VERSION`) MUST NOT have a leading zero unless it is exactly `0`, and MUST fit in an unsigned 64-bit integer. A violation is the error `INVALID_VERSION`.
   - A pre-release identifier MUST be non-empty. A purely numeric pre-release identifier MUST NOT have a leading zero and MUST fit in an unsigned 64-bit integer. A violation is `INVALID_VERSION`.
   - A `+` directly after a `VERSION` (build metadata) is the error `BUILD_METADATA_NOT_ALLOWED` (N-G-4). Build metadata never appears in a `sai.package` or `sai.lock`.
   - A digit sequence directly followed by a letter, `_`, or a third `.` is `INVALID_VERSION` (for example `1.2.3.4`, `1x`, `1_000`).
4. `UWORD` is used for lint codes (§1.13) and size units (§1.14). Its spelling is validated where it is used.
5. Strings are **simple strings only**: no line continuation, no multiline `"""` strings, no raw `r"..."` strings, no byte strings, and no format strings (N-G-1). Using one of those forms is the error `UNSUPPORTED_STRING_FORM`. A raw line break inside a string is `UNTERMINATED_STRING`. An unknown escape, or a `\u{..}` that is not a Unicode scalar value, is `INVALID_ESCAPE`. The value of a string is its code point sequence after escape processing; it is not NFC-normalized.
6. `->` is used only in `sai.lock` ([03-lock-file.md](03-lock-file.md)). In `sai.package` it is a syntax error (`EXPECTED_TOKEN`). A `/` token occurs only between the segments of a local world name (§1.9); a `/` inside a `PKG_PATH` is part of that token.
7. `;` is the error `NO_SEMICOLON` (with a fix that removes it). Any other character that cannot start a token (for example `*`, `#`, `(`, `[`, `:` not part of a `PKG_PATH`) is the error `UNEXPECTED_CHARACTER`.

### 1.2.4 Keywords

All keywords of `sai.package` are **contextual**: they are `WORD` tokens with a fixed spelling, recognized only in the positions where the grammar (§1.3) expects them. In the EBNF they are written in double quotes (`"package"`), following [spec/03 §3.1.1](../spec/03-grammar.md). The keywords are:

`allocator allow artifact authors budget compiler component deny description dir export from git grant license lints only overrides package path policy profile repository requires source targets test use warn wit world`

Because they are contextual, a word such as `test` or `path` can also appear as a value where a `WORD` is expected (for example a world named `path`).

### 1.2.5 Line-oriented structure

A package contract is a sequence of declarations, one per line. Lists inside `{ }` follow D-SYN-07: their elements are separated by a line break or by `,`, except for the lists whose elements are themselves long entries (`requires`, `overrides`, `artifact component`, `profile`), whose elements are separated by line breaks only (N-G-2).

### 1.2.6 NEWLINE generation and continuation lines

The lexer emits `NEWLINE` tokens with the following rules (N-G-2). They are the rules of [spec/02 §2.10](../spec/02-lexical-structure.md) with package-specific sets E and C. There are no `(` or `[` brackets, so only `{` is on the bracket stack.

- **Terminator set E**: `WORD`, `UWORD`, `PKG_PATH`, `INT`, `VERSION`, `STRING`, and `}`.
- **Continuation set C**: a `WORD` spelled exactly `from`, `dir`, `only`, or `grant`.

When the lexer reads a line break (or a block comment that contains a line break), it emits one `NEWLINE` if and only if:

1. the last token before the line break belongs to E, and
2. the first token of the next line does not belong to C.

In addition:

3. Consecutive `NEWLINE`s collapse into one; blank lines and comment-only lines add none.
4. If the last token before EOF belongs to E, one `NEWLINE` is emitted before EOF.
5. The parser skips a `NEWLINE` directly after `{` and directly before `}`.

Consequences (this layout is accepted but not canonical, §1.3.1 rule 6):

```sai-package noncanonical
requires {
    sai:http @ ^1.4
        from git "https://github.com/sai-lang/http"      // continues the line above: `from` is in C
    sai:wasi @ ^0.2
        from git "https://github.com/sai-lang/wasi"
        only { wasm32-wasi, wasm64-wasi }                // `only` is in C
    acme:legacy @ >=1.2,
        <1.9                                             // no NEWLINE after `,` (not in E)
        from git "https://github.com/acme/legacy"
}
```

- A requirement can therefore be written on one line or split before `from`, `dir`, `only`, or `grant`, and after `,`. Splitting anywhere else ends the requirement, and the parser reports `UNEXPECTED_NEWLINE` (or `EXPECTED_TOKEN`) at the following token, with a note naming the rule that produced the NEWLINE.
- A line that starts with `from`, `dir`, `only`, or `grant` outside a requirement is joined to the previous declaration and then rejected by the parser with `EXPECTED_TOKEN`.

---

## 1.3 Grammar

The grammar uses the notation of [spec/03 §3.1](../spec/03-grammar.md): `"word"` is a contextual keyword, `'{'` a punctuator, `NL` the NEWLINE token, `nl` an optional NEWLINE, and `List<X, S>` one or more X separated by S with an optional trailing separator. `Sep` is `',' nl | NL`.

```ebnf
PackageFile    ::= nl PackageDecl ( NL Decl )* nl EOF

Decl           ::= CompilerDecl | SourceDecl | WitDecl | ArtifactDecl | TargetsDecl
                 | ExportWorldDecl | RequiresDecl | AllowDecl | DenyDecl | PolicyDecl
                 | LintsDecl | ProfileDecl | AllocatorDecl | OverridesDecl | MetaDecl

(* identity and toolchain *)
PackageDecl    ::= "package" PKG_PATH '@' VERSION
CompilerDecl   ::= "compiler" VersionReq

(* content *)
SourceDecl     ::= "source" STRING
WitDecl        ::= "wit" STRING
ArtifactDecl   ::= "artifact" "component" '{' nl List<ArtifactEntry, NL>? nl '}'
ArtifactEntry  ::= WORD STRING                         (* target name, file path *)

(* targets and world *)
TargetsDecl    ::= "targets" '{' nl List<WORD, Sep> nl '}'
ExportWorldDecl ::= "export" "world" WorldName
WorldName      ::= WORD ( '/' WORD )* | PKG_PATH

(* dependencies *)
RequiresDecl   ::= "requires" "test"? '{' nl List<Requirement, NL>? nl '}'
Requirement    ::= PKG_PATH '@' VersionReq SourceClause? OnlyClause? GrantClause?
SourceClause   ::= "from" ( "git" STRING ( "dir" STRING )? | "path" STRING )
OnlyClause     ::= "only" '{' nl List<WORD, Sep> nl '}'
GrantClause    ::= "grant" '{' nl List<CapName, Sep>? nl '}'

VersionReq     ::= Comparator ( ',' Comparator )*
Comparator     ::= Op PartialVersion
Op             ::= '^' | '~' | '=' | '>=' | '<' | '<='
PartialVersion ::= INT | VERSION

(* capability policy *)
AllowDecl      ::= "allow" '{' nl List<CapName, Sep>? nl '}'
DenyDecl       ::= "deny" '{' nl List<CapName, Sep>? nl '}'
CapName        ::= WORD | PKG_PATH
PolicyDecl     ::= "policy" '{' nl List<PolicyItem, Sep>? nl '}'
PolicyItem     ::= "deny" WORD

(* lints and profiles *)
LintsDecl      ::= "lints" '{' nl List<LintItem, Sep>? nl '}'
LintItem       ::= ( "allow" | "warn" | "deny" ) UWORD
ProfileDecl    ::= "profile" WORD '{' nl List<ProfileItem, NL>? nl '}'
ProfileItem    ::= AllowDecl | BudgetDecl
BudgetDecl     ::= "budget" '{' nl List<BudgetItem, Sep>? nl '}'
BudgetItem     ::= WORD '<=' INT UWORD                 (* metric, size, unit *)

(* root-only build configuration *)
AllocatorDecl  ::= "allocator" PKG_PATH
OverridesDecl  ::= "overrides" '{' nl List<Override, NL>? nl '}'
Override       ::= PKG_PATH "use" ( "artifact"
                                  | "path" STRING
                                  | "git" STRING ( "dir" STRING )? )

(* metadata *)
MetaDecl       ::= "description" STRING
                 | "license" STRING
                 | "repository" STRING
                 | "authors" '{' nl List<STRING, Sep>? nl '}'
```

### 1.3.1 Structural rules

1. The first declaration MUST be `package`. A file without one is `MISSING_PACKAGE_DECL`. A `package` declaration in any other position is `PACKAGE_DECL_NOT_FIRST`. Comments and blank lines may come before it.
2. All other declarations may appear in any order. Their order has no meaning.
3. A declaration whose multiplicity is "at most one" or "exactly one" (§1.1.3) that appears twice is `DUPLICATE_DECLARATION`. `requires` and `requires test` are different declarations. Two `profile` blocks with the same name are a duplicate.
4. A line that starts with a `WORD` that is not a declaration keyword is `UNKNOWN_DECLARATION`. The diagnostic suggests the nearest declaration (for example `dependencies` → `requires`, `name` / `version` → `package`, `edition` → `compiler`).
5. A repeated element inside one list (`targets`, `only`, `allow`, `deny`, `grant`, `policy`, `authors`, `budget` metrics) is the warning `DUPLICATE_LIST_ITEM`, with a fix that removes the repetition. Repetitions in `requires`, `lints`, and `overrides` have their own rules (§1.10.4, §1.13, §1.16).
6. The canonical layout, which `sai fmt` and every tool that writes the file produce, follows D-SYN-07 and D-TOOL-04: 4-space indentation; a brace list on one line `{ a, b }` when it fits in 100 columns, otherwise one element per line without commas; an entry list (`requires`, `requires test`, `overrides`, `artifact component`, a `profile` body) always one entry per line; an empty list `{}`; a requirement always split before each of its `from`, `only`, and `grant` clauses, with 8-space continuation indentation, even when it would fit on one line (`dir` stays on the line of its `from`); exactly one space around `@`; no space between an operator and its version (`^1.4`); `, ` between comparators. [impl/14](../impl/14-formatter.md) §14.12 makes this layout precise.

---

## 1.4 Package identity and version

```sai-package
package acme:image @ 1.2.0
```

1. The `PKG_PATH` MUST have the form `namespace:name` with no `/` part. A `/` part is `INVALID_PACKAGE_NAME`.
2. Each segment MUST be 1 to 64 bytes long (N-G-3). The lexical rule (`PkgSeg`) already restricts segments to lowercase ASCII letters, digits, and single hyphens between alphanumeric runs, starting with a letter. Underscores are not allowed; `use acme:user-client` binds `user_client` (D-MOD-01).
3. The namespace `wasi` is reserved for the WASI WIT packages, because `wasi:*` names are capability names (D-CAP-01). Declaring a package in it is `RESERVED_NAMESPACE`.
4. `sai:core` and `sai:alloc` are part of the toolchain (D-PKG-04). Declaring a package with either identity is `RESERVED_PACKAGE_NAME`.
5. The version MUST be a full SemVer version `MAJOR.MINOR.PATCH` with an optional pre-release (N-G-4). A two-component `VERSION` here is `INVALID_VERSION`. Build metadata is not allowed (§1.2.3 rule 3).
6. The version is meaningful beyond resolution: it selects the effect-declaration rule of D-EFF-01 (`MISSING_EFFECTS_DECLARATION` from 1.0.0, the lint `PUBLIC_EFFECTS_INFERRED` below 1.0.0).
7. For a package fetched from Git, the declared identity and version MUST equal the requested ones ([02-resolver.md](02-resolver.md) §2.6, `PACKAGE_IDENTITY_MISMATCH`).

## 1.5 Compiler compatibility

```sai-package
compiler ^0.1
```

1. `compiler` states which versions of the SAI language (the toolchain's language version, a SemVer version reported by `sai --version`) can compile the package. Its value is a version requirement (§1.10.2).
2. A toolchain whose language version does not satisfy the requirement of a package in the build rejects the build with `COMPILER_VERSION_MISMATCH`, naming the package. This holds for the root and for every dependency.
3. The declaration is optional. Omitting it means any language version is accepted. `sai new` always writes it (N-G-5).
4. There are no editions in v0.1. The `compiler` requirement is the only language-version gate (N-G-5). A future edition mechanism would be a new declaration; it is not reserved here.

## 1.6 Source and WIT directories

```sai-package
source "src"
wit "wit"
```

### 1.6.1 Paths in `sai.package`

All paths in `sai.package` (`source`, `wit`, artifact files, `from path`, `use path`) follow these rules (N-G-20). A violation is `INVALID_PACKAGE_PATH`.

1. A path is relative to the package directory. An absolute path (starting with `/`, or with a drive letter such as `C:`) is not allowed.
2. The separator is `/`. A `\` is not allowed.
3. A path is non-empty and has no empty components (no `//`, no leading or trailing `/`), and no `.` components. The single path `"."` is allowed and denotes the package directory itself.
4. `..` components are allowed only in `from path` and `use path` (dependencies outside the package). In `source`, `wit`, and artifact paths, `..` is not allowed: package content MUST lie inside the package directory.
5. Characters: printable ASCII, excluding `<>:"|?*` (so that the package can be materialized on every supported host).
6. No component may be `.git`, compared using ASCII case-insensitive matching.
7. Except for the whole path `"."` and an exact `..` component allowed by rule 4, a component MUST NOT end in `.` or a space. A component whose name before its first `.` suffix, after trimming trailing spaces from that name, is `CON`, `PRN`, `AUX`, `NUL`, `COM1` through `COM9`, or `LPT1` through `LPT9` (ASCII case-insensitive) is also forbidden. These names are reserved by Windows even when followed by an extension.
8. For `source`, `wit`, and artifact paths, the first component MUST NOT be `.sai` or `out`, compared using ASCII case-insensitive matching: those top-level directories hold tool state and build outputs (D-TOOL-03) and are excluded from package checksums ([03-lock-file.md](03-lock-file.md) §3.4).
9. A component is at most 255 bytes, and the whole path is at most 4096 bytes ([03-lock-file.md](03-lock-file.md) §3.4.3).

### 1.6.2 `source`

1. `source "<dir>"` names the **source directory**, the `<source>` of [spec/04 §4.2.3](../spec/04-modules-and-names.md). It MUST exist and be a directory; otherwise the package has no root module (`MISSING_ROOT_MODULE`, spec/04 §4.2.3 rule 1).
2. The root module is `<dir>/lib.sai` (D-SYN-08). If it does not exist, the package is rejected with `MISSING_ROOT_MODULE`. The module tree below it is defined by [spec/04](../spec/04-modules-and-names.md).
3. There is no default source directory. A package without `source` is an artifact package; a package with neither `source` nor `artifact` is `PACKAGE_HAS_NO_CONTENT` (N-G-6).
4. The draft's layout `src/json.sai` (draft §34) is corrected by R-10: the root module is `src/lib.sai`.

### 1.6.3 `wit` (Stage 4)

1. `wit "<dir>"` names the **WIT directory**. It MUST exist and be a directory (`WIT_DIR_NOT_FOUND`).
2. The `.wit` files directly inside it form exactly one WIT package. The interpretation of WIT, including WIT `deps/`, belongs to [spec/13](../spec/13-components-and-wit.md). D-WIT-02: the WIT package becomes visible to SAI code as declarations.
3. The WIT package's name MUST equal the SAI package identity `namespace:name`. If the WIT package declares a version, it MUST equal the package version. A mismatch is `WIT_PACKAGE_MISMATCH` (N-G-19; draft §28: identity is aligned with WIT).
4. `wit` is REQUIRED for artifact and hybrid packages (`WIT_REQUIRED`). For source packages it is optional.
5. The WIT checksum of the lock file ([03-lock-file.md](03-lock-file.md) §3.5) is computed over this directory.

## 1.7 Artifacts (Stage 4)

```sai-package
artifact component {
    wasm32-wasi "dist/wasm32/image.wasm"
    wasm64-wasi "dist/wasm64/image.wasm"
}
```

1. `artifact component { .. }` lists prebuilt **Wasm components**, one per target. `component` is the only artifact kind in v0.1 (N-G-19). Any other word after `artifact` (for example `module`) is `UNKNOWN_ARTIFACT_KIND`. Prebuilt core modules are not supported, because a core module has no typed interface to check against (P4).
2. Each entry is `<target> "<file>"`. The target MUST be a target name ([targets/targets-and-profiles.md](../targets/targets-and-profiles.md); an unknown name is `UNKNOWN_TARGET`) that is listed in the package's `targets` (`ARTIFACT_TARGET_NOT_DECLARED`), and it MUST be a component target (`wasm32-wasi` or `wasm64-wasi`). A target that is not a component target, that is a core target or a native target ([targets](../targets/targets-and-profiles.md) §1.3, §2.8), is `ARTIFACT_ON_CORE_TARGET`, because no build for such a target consumes a component (D-PKG-05; for native targets, [targets](../targets/targets-and-profiles.md) §1.4 item 6, N-F-62).
3. A target may appear at most once. A second entry for the same target is `DUPLICATE_DECLARATION`.
4. The file MUST exist inside the package directory and be a regular file (`ARTIFACT_NOT_FOUND`). Its path follows §1.6.1.
5. An artifact package (no `source`) MUST have an artifact for **every** target in its `targets` (`MISSING_ARTIFACT_FOR_TARGET`). As a consequence, an artifact package can only list component targets: it can never list a core target or a native target. A hybrid package may omit artifacts for some targets; for those targets the source is used (D-PKG-05).
6. An artifact or hybrid package MUST declare `wit` (§1.6.3) and `export world` (§1.9); otherwise `WIT_REQUIRED` or `MISSING_EXPORT_WORLD` (N-G-19). The artifact is verified against them ([04-distribution-and-security.md](04-distribution-and-security.md) §4.5).
7. The draft's §35 key `wasm32` is not a target name. It is written `wasm32-wasi` (§1.18.4).

## 1.8 Targets

```sai-package
targets { wasm32-core, wasm32-wasi }
```

1. `targets` lists every target the package supports. It is REQUIRED (`MISSING_TARGETS`) and MUST be non-empty (N-G-7, P1). There is no implicit "all targets".
2. Each element MUST be a target name defined in [targets/targets-and-profiles.md](../targets/targets-and-profiles.md) (`UNKNOWN_TARGET`). The v0.1 Wasm targets are `wasm32-core`, `wasm64-core`, `wasm32-wasi`, and `wasm64-wasi` (D-TGT-01). Native targets (Stage 7) are named there as well.
3. Building the root package for a target that is not in its `targets` is `TARGET_NOT_DECLARED`.
4. Every dependency that is active for a target (§1.10.5) MUST list that target in its own `targets`. Otherwise the build for that target is rejected with `TARGET_NOT_SUPPORTED_BY_DEPENDENCY`, naming the dependency and the chain that requires it. `sai add` reports supported and unsupported targets before adding a dependency (draft §75; [04-distribution-and-security.md](04-distribution-and-security.md) §4.7).
5. Listing a target is a promise that is checked: the package must compile for it. In particular, the capability checks of D-CAP-01 are made per target, and reaching a capability that the target does not provide is a compile error (draft §59, [spec/08](../spec/08-effects-and-capabilities.md)).

## 1.9 `export world` (Stage 4)

```sai-package
export world server
```

1. `export world <name>` names the world that the package implements when it is built as a component. At most one is allowed. The semantics of the world (elaboration, exports and imports of the component, commands and reactors) are owned by [spec/13 §13.5](../spec/13-components-and-wit.md); this section defines the declaration and its validation in `sai.package`.
2. The name has one of two forms:
   - a **local world**: a module path relative to the root module, written as `WORD` segments separated by `/` (`client`, `api/client`). Every segment MUST be a lower identifier in snake_case form, `[a-z][a-z0-9_]*`; otherwise it is `INVALID_WORLD_NAME`. A hyphen is not allowed; WIT kebab-case names are mapped by the bijection of D-WIT-01;
   - an **external world**: a `PKG_PATH` with exactly one `/` part (`wasi:http/proxy`); otherwise `INVALID_WORLD_NAME`.
3. Resolution of the name (N-G-8):
   - For a source or hybrid package, a local world is resolved relative to the root module `<source>/lib.sai` (D-SYN-08, [spec/13 §13.5.1](../spec/13-components-and-wit.md)).
   - For an artifact package, which has no source, a local world MUST be a single segment and names a world of the WIT package in the `wit` directory, matched through the D-WIT-01 name mapping (`image_service` ↔ `image-service`).
   - An external world MUST be a world of a WIT package known to the build: a WASI package of the target's WASI version, or a WIT package in the resolved graph.
   - If no such world exists, the error is `UNKNOWN_EXPORT_WORLD`. Draft §80's `export world client` requires a declared world `client` (R-04).
4. On a **component target** (`wasm32-wasi`, `wasm64-wasi`), a root package with `export world` is built as a component that implements that world ([spec/13 §13.5.2](../spec/13-components-and-wit.md)). A root package without `export world` is a command if it defines the entry point of D-CAP-02, and otherwise a reactor without exports (spec/13 §13.5.3).
5. On a **core target** (`wasm32-core`, `wasm64-core`), declaring `export world` is **valid**: no package-level diagnostic is issued, and `targets` may list core and component targets together (N-G-8). The build behaviour is the one of [spec/13 §13.6.2](../spec/13-components-and-wit.md) (N-D-32 there):
   - `sai build` produces a core module whose exports are the package's `export fn` items (D-MOD-02). The world is not applied, and the build emits the information diagnostic `EXPORT_WORLD_NOT_APPLIED` (defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2), which suggests `sai component build`.
   - `sai component build` wraps the core module as a component with the world's exports and no imports (D-TGT-01). Unreachable world imports are permitted and omitted; a reachable one is `TARGET_CAPABILITY_UNAVAILABLE` ([spec/08 §8.12.7](../spec/08-effects-and-capabilities.md)).
   - Rationale: draft §30 lists `wasm32-core` and `wasm32-wasi` together with `export world server`. A package that supports both a freestanding core build and a component build is legitimate (P8: the outcome depends only on the target), and rejecting the combination would force such packages to be split.
6. For a dependency, `export world` has no effect on how the dependent is built, except for artifact and hybrid packages, where it identifies the contract that the artifact implements ([04-distribution-and-security.md](04-distribution-and-security.md) §4.5).

## 1.10 Dependencies: `requires`

```sai-package
requires {
    sai:http @ ^1.4
        from git "https://github.com/sai-lang/http"
    acme:crypto @ >=2.1, <2.6
        from git "https://github.com/acme/monorepo" dir "crypto"
    acme:shared @ ^0.3
        from path "../shared"
    sai:wasi @ ^0.2
        from git "https://github.com/sai-lang/wasi"
        only { wasm32-wasi, wasm64-wasi }
    acme:sqlite @ ^3.1
        from git "https://github.com/acme/sqlite"
        grant { alloc, wasi:filesystem }
}
```

### 1.10.1 Requirement entries

1. Each entry is `<id> @ <requirement>`, followed by a source clause, an optional `only` clause, and an optional `grant` clause, in this order. Any of the clauses may start a continuation line (§1.2.6).
2. `<id>` follows §1.4 rules 1 and 2 (`INVALID_PACKAGE_NAME`).
3. Requiring `sai:core` or `sai:alloc` is `IMPLICIT_PACKAGE_REQUIRED`: they are always available and never declared (D-PKG-04). The fix removes the entry.
4. Requiring the package itself is `PACKAGE_REQUIRES_ITSELF`.
5. Entries are separated by line breaks only, not by commas, because a version requirement itself contains commas (N-G-2).

### 1.10.2 Version requirements

A version requirement is one or more comparators joined by `,` (all must hold). The precise set of versions each comparator denotes is defined in [02-resolver.md](02-resolver.md) §2.3. In summary:

| Requirement | Versions | Notes |
|---|---|---|
| `^1.4` | `>=1.4.0, <2.0.0` | same major line for 1.0.0 and later |
| `^0.4.2` | `>=0.4.2, <0.5.0` | same minor line for `0.y` |
| `^0.0.3` | `=0.0.3` | exact patch for `0.0.z` |
| `~1.4.2` | `>=1.4.2, <1.5.0` | same minor |
| `~1` | `>=1.0.0, <2.0.0` | |
| `=1.4.2` | exactly `1.4.2` | |
| `>=1.2, <2.0` | `>=1.2.0, <2.0.0` | explicit range |
| `>=1.2.0, <=1.4.7` | `>=1.2.0, <=1.4.7` | inclusive upper bound |

Validation rules (N-G-10):

1. Every comparator has an operator. A bare version (`sai:json @ 1.1`) is the error `BARE_VERSION_REQUIREMENT` (D-PKG-01, P1), with a fix that inserts `^`.
2. `>` is the error `UNSUPPORTED_VERSION_OPERATOR`, with a fix that suggests `>=` and the next version. A strict lower bound has no well-defined minimum, and MVS selects minimums ([02-resolver.md](02-resolver.md) §2.4).
3. A requirement has exactly one **lower-bound comparator** (`^`, `~`, `=`, or `>=`). A requirement without one (for example `<2.0`) is `VERSION_REQ_NO_LOWER_BOUND`. Two lower-bound comparators are `INVALID_VERSION_REQUIREMENT`.
4. A requirement has at most one **upper-bound comparator** (`<` or `<=`). A second one is `INVALID_VERSION_REQUIREMENT`.
5. `=` stands alone: it cannot be combined with another comparator (`INVALID_VERSION_REQUIREMENT`).
6. `=` and `<=` require a full three-component version. `^`, `~`, `>=`, and `<` accept partial versions (`1`, `1.4`), which are completed with zeros. A partial version where a full one is required is `INVALID_VERSION_REQUIREMENT`, and so are `^0` and `^0.0`, whose meaning would be ambiguous ([02-resolver.md](02-resolver.md) §2.3.1 rule 4).
7. A requirement that denotes no version at all (for example `>=2.0, <1.5`, or `^1.4, <1.4`) is `EMPTY_VERSION_REQUIREMENT`.
8. The **minimum** of a requirement is the lower bound of its lower-bound comparator, completed with zeros. Its **compatibility line** is the line of its minimum (D-PKG-02, [02-resolver.md](02-resolver.md) §2.2.3). The resolver never selects a version outside that line for the requirement.
9. If the requirement's interval contains the stable start of the next compatibility line (for example `>=1.2` without an upper bound, `>=1.2, <3.0`, or `~0`), it is the warning `VERSION_REQ_SPANS_LINES`. A pre-release below that stable boundary does not trigger the warning by itself: `>=0.7.1, <0.8` admits `0.8.0-rc.1` by precedence but excludes stable `0.8.0`, so it does not warn. The note explains that stable versions outside the line can never be selected, and the fix replaces the requirement with the equivalent one restricted to its line (for `>=1.2, <3.0`: `^1.2`).
10. A pre-release in a requirement is allowed (`^1.0.0-rc.1`); its meaning is defined in [02-resolver.md](02-resolver.md) §2.3.3.

### 1.10.3 Sources

1. Every requirement MUST have a source clause (N-G-9). A requirement without one is `MISSING_SOURCE`. This applies to the toolchain-bundled standard packages as well (`sai:collections`, `sai:fmt`, ...): they are ordinary packages (D-PKG-04), and the pre-populated cache only avoids the network ([04-distribution-and-security.md](04-distribution-and-security.md) §4.4.3). `sai add` writes the canonical URL of a bundled package for the user.
2. `from git "<url>" [dir "<subdir>"]` (D-PKG-01): the package is fetched from the Git repository at `<url>`. The URL rules (allowed schemes, canonical form) are in [04-distribution-and-security.md](04-distribution-and-security.md) §4.2. `dir` names the package directory inside the repository, a path relative to the repository root under the rules of §1.6.1 (no `..`). The resolver fetches the tag `v<version>`, or `<dir>/v<version>` when `dir` is given (D-PKG-02).
3. `from path "<dir>"`: the package is the directory `<dir>`, relative to the requiring package's directory (`..` allowed, §1.6.1). The directory MUST contain a `sai.package` (`PATH_PACKAGE_NOT_FOUND`). Path sources pin their compatibility line to the version declared in that file ([02-resolver.md](02-resolver.md) §2.7). Inside a package fetched from Git, a path source refers to the same repository at the same commit and MUST NOT leave the repository ([02-resolver.md](02-resolver.md) §2.7.2, `PATH_ESCAPES_REPOSITORY`).
4. There are no branch, revision, or commit sources (N-G-27, [02-resolver.md](02-resolver.md) §2.6.3). Commits are pinned only by the lock file. Registries are a future extension (D-PKG-01, O-04).
5. When several requirers name the same package with different sources, the source of the selected version is chosen by the rule of D-PKG-02 ([02-resolver.md](02-resolver.md) §2.8). The root can force a source with `overrides` (§1.16).

### 1.10.4 One requirement per package

1. A package identity may appear at most once across `requires` and `requires test` together. A second entry is `DUPLICATE_REQUIREMENT`, even if the two entries request different compatibility lines. A package's code refers to a dependency by identity (`use acme:json`), so one package cannot use two lines of the same dependency (D-MOD-01). Different packages in the graph may still use different lines of the same package ([02-resolver.md](02-resolver.md) §2.9).
2. Order of entries has no meaning.

### 1.10.5 Target-specific dependencies: `only`

1. `only { <targets> }` makes the requirement **active** only when building for one of the listed targets (D-PKG-01). Without `only`, a requirement is active for every target.
2. Each listed target MUST be a target name (`UNKNOWN_TARGET`) listed in the requiring package's own `targets` (`ONLY_TARGET_NOT_DECLARED`). The list MUST be non-empty (grammar).
3. For resolution and locking, a requirement is included when its `only` set intersects the root package's `targets` ([02-resolver.md](02-resolver.md) §2.5.2), so one lock file serves every target of the root.
4. When building for a target, `use` of a dependency that is not active for that target is the error `INACTIVE_DEPENDENCY` (defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2 step 1.3). Items that use it are made target-specific with `@target(..)` ([spec/15](../spec/15-attributes-and-lints.md) §15.11).

### 1.10.6 Test dependencies: `requires test` (root-only)

1. `requires test { .. }` declares dependencies that are available only to the package's tests ([spec/14-testing.md](../spec/14-testing.md)). Its entries have the same form as `requires`.
2. It takes effect only in the root package. The test dependencies of a dependency are never resolved, fetched, or locked (N-G-11).
3. A `use` of a test dependency is test-only: its bindings may be referred to only in tests and properties. A reference from any other code, or a `pub use` of a test dependency, is the error `TEST_DEPENDENCY_OUTSIDE_TEST` (defined in [spec/14](../spec/14-testing.md) §14.2.5). When the tests of a workspace member that is not the root are analyzed, its test-only uses are `TEST_DEPENDENCY_UNAVAILABLE` (spec/14 §14.2.5 item 5).

### 1.10.7 No optional, development, or feature dependencies

v0.1 has no optional dependencies, no feature flags, and no separate development (build-time) dependencies (N-G-11). Each would make the dependency graph depend on configuration that is not visible at the use site (P1, P8). The only variations are `only` (by target) and `requires test` (tests of the root). There are no build-time dependencies, because there are no build scripts ([04-distribution-and-security.md](04-distribution-and-security.md) §4.10).

### 1.10.8 Per-dependency capability grants: `grant`

```sai-package
requires {
    acme:sqlite @ ^3.1
        from git "https://github.com/acme/sqlite"
        grant { alloc, wasi:filesystem }
    acme:blake3 @ ^1.0
        from git "https://github.com/acme/blake3"
        grant {}
}
```

1. `grant { <capabilities> }` is the maximum set of capabilities that the requiring package grants to the dependency (N-G-12). The names follow §1.11.2. `grant {}` grants nothing.
2. Meaning: for a requirement edge from package P to dependency Q with `grant { G }`, every capability in `Req(Q)` and in `Req(R)` for every package R reachable from Q in the resolved graph ([spec/08 §8.12.4](../spec/08-effects-and-capabilities.md)) MUST be granted by G, taken as the policy `(G, ∅)`: it MUST be covered by some name in G (the *covers* relation of spec/08 §8.12.1) or, for `wasi:io` and its interfaces, implied by a WASI name in G ([targets](../targets/targets-and-profiles.md) §3.3, N-F-69). `grant { wasi:cli }` therefore lets a dependency write to standard output, which also requires `wasi:io/streams`. Otherwise the build is rejected with `CAPABILITY_NOT_GRANTED`, naming the edge, the capability, and the why chain of spec/08 §8.13. This narrows, and never widens, D-CAP-01: the dependency's own `allow` and the root's effective policy still apply. The check runs at build time, together with the checks of spec/08 §8.12.6, and uses the same reachability.
3. Omitting `grant` adds no restriction beyond D-CAP-01.
4. Resolution-time aspects ([02-resolver.md](02-resolver.md) §2.12):
   - If the dependency's declared `allow` (minus its `deny`) is not covered by the grant, this is not an error: the declared `allow` is only a maximum. The capability review shows both. This compares names with names, so the `wasi:io` implication of [targets](../targets/targets-and-profiles.md) §3.3 does not apply to it.
   - If a capability of the footprint recorded in the lock ([spec/08 §8.12.8](../spec/08-effects-and-capabilities.md)) of Q or of a package reachable from Q is not granted by the grant as in item 2 (covered, or for `wasi:io` implied), the capability review reports it as a review item, not as an error. For an artifact, the footprint is all of its imports, which all count as reachable (D-CAP-01), so such an item becomes `CAPABILITY_NOT_GRANTED` in every build that uses the artifact form. Only build time rejects a program for capability reasons (spec/08 §8.12.8).
5. `grant` may appear in `requires test` entries; it then applies when building tests.

## 1.11 Capability policy: `allow` and `deny`

```sai-package
allow { alloc, wasi:http, wasi:clocks }
deny { wasi:filesystem }
```

### 1.11.1 Meaning

1. `allow { .. }` is the **maximum grant** of the package: the capabilities its reachable code may use. It is not a list of capabilities that must be used (draft §22, D-CAP-01).
2. An omitted `allow` means the empty set (D-CAP-01, P1, P5). `allow {}` is equivalent and is the canonical spelling in examples that want to make "no capabilities" explicit.
3. `deny { .. }` overrides `allow`. It may deny part of an allowed capability (`allow { wasi:http }` with `deny { wasi:http/outgoing-handler }`). A `deny` entry that is not covered by `allow` is allowed and has no effect on checking; it records intent and survives later additions to `allow` (draft §30).
4. Listing the same name in both `allow` and `deny` is the error `CAPABILITY_ALLOW_DENY_CONFLICT` (D-CAP-01, [spec/08 §8.12.5](../spec/08-effects-and-capabilities.md)).
5. The check is defined by D-CAP-01 and [spec/08](../spec/08-effects-and-capabilities.md): the effects of all code reachable from the roots of the build ([spec/08 §8.12.3](../spec/08-effects-and-capabilities.md)) are expanded to capabilities, which must be granted by the policy, for the root's policy and for the policy of every dependency: each capability must be covered by `allow` or, for `wasi:io` and its interfaces, implied by another WASI name in `allow` ([targets](../targets/targets-and-profiles.md) §3.3, N-F-69), and must not overlap `deny`. The effect-to-capability table is D-EFF-03. For a dependency in artifact form, all its imports are reachable.
6. A capability that the build target does not provide is a compile error when reached (draft §59), whatever `allow` says. The target capability table is in [targets/targets-and-profiles.md](../targets/targets-and-profiles.md) (C-07).
7. The application entry point receives a bundle of capabilities determined by `allow` (D-CAP-02, [targets/targets-and-profiles.md](../targets/targets-and-profiles.md)).
8. Profiles can restrict the effective policy further (§1.14.2).

### 1.11.2 Capability names

The three kinds of names of D-CAP-01 are written as follows. A name that does not fit any of them is `INVALID_CAPABILITY_NAME`.

| Kind | Token | Valid names |
|---|---|---|
| allocation | `WORD` | `alloc` |
| target capability | `WORD` | `threads`, `host`, `native_ffi`, `process`, `memory64` |
| WIT package | `PKG_PATH` without `/` | `wasi:http`, `wasi:filesystem`, `acme:storage` |
| WIT interface | `PKG_PATH` with exactly one `/` part | `wasi:filesystem/types`, `wasi:http/outgoing-handler` |

1. A WIT package name covers all of its interfaces (D-CAP-01).
2. A `PKG_PATH` with two or more `/` parts is `INVALID_CAPABILITY_NAME`.
3. Capability names are unversioned in v0.1 (`wasi:http@0.2.0` cannot be written; `@` is not part of a `PKG_PATH`). When the graph contains two compatibility lines of a package ([02 §2.9](02-resolver.md)), a name therefore covers the package or interface in both lines ([spec/13](../spec/13-components-and-wit.md) §13.7.1).
4. A WIT package or interface name MUST denote a WIT package or interface that is known to the build: one of the WASI packages known to the toolchain, or one defined by the WIT of a package in the resolved graph. Otherwise it is `UNKNOWN_CAPABILITY`. This check runs after resolution, because it needs the graph. The draft's `wasi:filesystem/read` and `wasi:filesystem/write` (draft §74) are not real interfaces and are rejected by this rule (R-13). Use `wasi:filesystem` or `wasi:filesystem/types`; read and write are distinguished by the effects `fs.read` and `fs.write` (D-EFF-03).
5. A WASI name that is neither `wasi:io` nor an interface of it does not cover `wasi:io`. A policy or a `grant` that contains such a name nevertheless grants `wasi:io` and its interfaces, unless a `deny` overlaps them (the implication of [targets](../targets/targets-and-profiles.md) §3.3, N-F-69). `allow { wasi:cli }` is therefore enough for a command that writes to standard output. Where one name must be covered by another, as for a profile `allow` (§1.14.2 item 2), the implication does not apply.

## 1.12 `policy`

```sai-package
policy { deny unsafe, deny ambient_imports }
```

1. `policy { .. }` holds compiler-verifiable restrictions that are not capabilities. Each item is `deny <item>`. There is no `allow` item; a policy item that is not listed is simply not in force.
2. The v0.1 items and their scope (N-G-13):

| Item | Effect | Scope |
|---|---|---|
| `deny unsafe` | every unsafe construct of D-UNSAFE-01 in the package's own source that [spec/12](../spec/12-unsafe-and-ffi.md) §12.11.1 item 1 lists, except `unsafe trait` declarations, is the error `UNSAFE_DENIED` ([spec/12](../spec/12-unsafe-and-ffi.md) §12.11.3): `unsafe` blocks, `unsafe fn` declarations (including the `unsafe fn` items of `extern "wasm"` blocks), `unsafe impl`, `unsafe module` headers, and `unsafe extern "wasm"` and `unsafe extern "C"` blocks | the package's own code |
| `deny ambient_imports` | the lint `AMBIENT_IMPORT` (D-CAP-02) becomes an error | the package's own code |
| `deny runtime_contracts` | the lint `CONTRACT_RUNTIME_CHECK` (D-CONTRACT-01) becomes an error | the package's own code |
| `deny duplicate_lines` | the lint `DUPLICATE_PACKAGE_LINE` (D-PKG-02, [02 §2.9](02-resolver.md)) becomes an error | the whole graph; root-only |

3. Package-local items are enforced whenever the package is compiled, including as a dependency: they are promises the package makes about itself. They do not constrain the package's dependencies; graph-wide visibility of unsafe code is provided by `sai inspect unsafe` (D-UNSAFE-01).
4. An unknown item is `UNKNOWN_POLICY_ITEM`.
5. A capability name used as a policy item (for example draft §68's `deny native_ffi`, or `deny host`) is `POLICY_ITEM_IS_CAPABILITY`. The fix moves the name into the `deny { }` declaration: denying a capability is a capability policy (D-CAP-01), and there is exactly one way to write it (P9).

## 1.13 `lints` (root-only)

```sai-package
lints { deny NAMING_CONVENTION, warn CONTRACT_RUNTIME_CHECK, allow PUBLIC_EFFECTS_INFERRED }
```

1. `lints { .. }` sets the package-wide level of lints, with the same meaning as `@allow(CODE)`, `@warn(CODE)`, and `@deny(CODE)` attached to every module of the package (N-G-14). The levels, their precedence, and the list of lints are defined in [spec/15](../spec/15-attributes-and-lints.md). Attributes in source code are more specific and take precedence over `lints`.
2. Each code MUST be a lint name ([spec/15](../spec/15-attributes-and-lints.md); lint names and diagnostic codes share one namespace, D-TOOL-01). An unknown code is `UNKNOWN_LINT`, and the code of a hard diagnostic, which has no level, is `NOT_A_LINT` ([spec/15 §15.15.4](../spec/15-attributes-and-lints.md)); the entry is ignored.
3. The same code listed twice with different levels is `CONFLICTING_LINT_LEVEL`. Listed twice with the same level, it is `DUPLICATE_LIST_ITEM`.
4. `lints` is root-only: when the package is a dependency, its `lints` block is ignored, and lint diagnostics in its code are not reported in the dependent's build. A lint in dependency code, including one added by a newer compiler, therefore never breaks the dependent's build (N-G-14). Errors, including lints turned into errors by `policy` (§1.12), are reported for dependencies as well.
5. Lint configuration never changes the meaning of a program or the set of capabilities it may use (C-03).

## 1.14 Profiles (root-only)

```sai-package
profile tiny {
    allow { alloc }
    budget { wasm <= 16 KiB }
}
```

### 1.14.1 Profile blocks

1. `profile <name> { .. }` extends one of the built-in profiles `dev`, `release`, and `tiny` (D-TGT-02). Any other name is `UNKNOWN_PROFILE`: user-defined profiles do not exist in v0.1. The profiles themselves are defined in [targets/targets-and-profiles.md](../targets/targets-and-profiles.md).
2. A block contains at most one `allow` and at most one `budget` (`DUPLICATE_DECLARATION`).
3. Profile blocks are root-only (N-G-15). A dependency's profile blocks are ignored: a profile is a property of a build, and the root selects it.
4. Profiles never change the semantics of a program (C-03). A profile block can only (a) lift default denials of its profile, within the package-level `allow` (§1.14.2), and (b) add a size budget. Both only decide whether a build is accepted; neither changes the behavior of an accepted program.

### 1.14.2 Profile `allow`

1. Some profiles deny capabilities by default; `tiny` denies WASI, `host`, and `threads` (D-TGT-02; the full list is in [targets/targets-and-profiles.md](../targets/targets-and-profiles.md)). A profile `allow { .. }` lifts the profile's default denial for the listed capabilities.
2. Every name in a profile `allow` MUST also be covered by the package-level `allow` (`PROFILE_ALLOW_EXCEEDS_PACKAGE`): the package-level `allow` is the maximum grant (D-CAP-01), and a profile never widens it.
3. The **effective policy** of a build with profile `p` is: allowed = the package-level `allow`, minus the default denials of `p` that are not lifted by `profile p { allow }`; denied = the package-level `deny`. It replaces the root's policy in the D-CAP-01 check for that build. The exact definition is [targets](../targets/targets-and-profiles.md) §5.5 (N-F-77): a profile `allow` lifts only the profile's own default denials and never adds a capability that the package-level `allow` does not grant. The policies of dependencies are not affected by profiles.
4. The other default restrictions of `tiny` (dynamic linking, reflection) are not capabilities and cannot be lifted in v0.1.
5. Example: with `allow { alloc, wasi:cli }` and `profile tiny { allow { alloc } }`, a `tiny` build may use `alloc` only; `wasi:cli` stays denied by the profile. `alloc` is not denied by `tiny`, so listing it is allowed and has no effect.

### 1.14.3 Budgets

1. `budget { .. }` sets size limits for builds with the profile (draft §47). Each item is `<metric> <= <size>`.
2. The only metric in v0.1 is `wasm`: the size in bytes of the final output for the target and profile (the core module or component that a successful build writes to `out/<target>/<profile>/`, D-TOOL-03). It is measured on the bytes the build produced, before anything is written. Another metric is `UNKNOWN_BUDGET_METRIC`. Size attribution and measurement are defined in [impl/09-optimization-and-size.md](../impl/09-optimization-and-size.md).
3. The unit MUST be `B`, `KiB` (1024 bytes), or `MiB` (1048576 bytes); anything else is `INVALID_SIZE`. The value MUST be greater than 0 and the size in bytes MUST fit in an unsigned 64-bit integer (`INVALID_SIZE`).
4. A build whose output exceeds a budget is rejected with `SIZE_BUDGET_EXCEEDED`, which reports the measured size, the limit, and the largest contributors (draft §75: "SAI does not hide dependency cost"). The diagnostic is an error, so the build fails and no output file is written; the previous output, if any, stays in place ([impl/13](../impl/13-cli.md) §13.5.1). `sai size` still prints its report for such a build, computed from the module built in memory ([impl/13](../impl/13-cli.md) §13.6.14). The budget applies to every target the root is built for with that profile.

## 1.15 `allocator` (root-only)

```sai-package
allocator acme:bump-alloc
```

1. `allocator <id>` replaces the allocator of the build (D-WASM-05). The package MUST be required in the root's `requires` (`ALLOCATOR_NOT_REQUIRED`); `allocator` does not add a dependency by itself (P1).
2. What an allocator package must provide is defined in [impl/10-runtime.md](../impl/10-runtime.md). A program that never reaches `alloc` links no allocator at all, whatever this declaration says (D-WASM-05).
3. In a dependency, `allocator` is ignored (N-G-16).

## 1.16 `overrides` (root-only)

```sai-package
overrides {
    acme:image use artifact
    acme:crypto use path "../crypto"
    sai:http use git "https://github.com/acme/http-fork"
}
```

1. `overrides` lets the root change, for the whole graph, how a package is obtained (D-PKG-01, D-PKG-02, D-PKG-05). Overrides are effective only in the root package; in a dependency they are ignored (N-G-16).
2. The forms (N-G-17):
   - `<id> use artifact`: for a hybrid package, use the artifact instead of the source wherever D-PKG-05 would otherwise prefer the source by default. It does not apply where D-PKG-05 requires the source (the `tiny` profile, a core target, a target without an artifact, or the root's allocator package of §1.15; [04-distribution-and-security.md](04-distribution-and-security.md) §4.5.1). On a package that is not hybrid it is `INVALID_OVERRIDE`.
   - `<id> use path "<dir>"`: replace the source of the package by a local directory. The path package's declared version pins its compatibility line ([02-resolver.md](02-resolver.md) §2.7.3). Other compatibility lines of the same package in the graph are not affected.
   - `<id> use git "<url>" [dir "<subdir>"]`: replace the repository of the package, for every compatibility line, for example by a fork. Versions are still selected by MVS and fetched by tag from the new repository.
3. There are no version overrides. To raise a version, the root adds or raises its own requirement; MVS then selects at least that version ([02-resolver.md](02-resolver.md) §2.4). A version override would break the guarantee that every selected version satisfies every requirement (P8).
4. An identity may have at most one source override (`use path` or `use git`) and at most one `use artifact`. A second override of the same kind is `DUPLICATE_OVERRIDE`.
5. An override whose identity, or for `use path` whose compatibility line, does not occur in the resolved graph is the warning `UNUSED_OVERRIDE`.
6. `sai:core` and `sai:alloc` cannot be overridden (`IMPLICIT_PACKAGE_REQUIRED`).

## 1.17 Metadata

```sai-package
description "Image decoding and resizing"
license "MIT OR Apache-2.0"
authors { "Ada Example <ada@example.com>" }
repository "https://github.com/acme/image"
```

1. Metadata declarations are informative for tools and humans and have **no effect** on resolution, building, or checking (N-G-18).
2. `description` MUST be a single-line string of at most 1024 bytes. A decoded LF or CR, or a value longer than 1024 UTF-8 bytes, is `INVALID_DESCRIPTION`.
3. `license` is an SPDX license expression (SPDX specification, Annex D). An expression that does not parse, or that uses an identifier that is neither on the SPDX license list nor of the form `LicenseRef-...`, is the warning `INVALID_LICENSE_EXPRESSION`.
4. `authors` is a list of free-form strings.
5. `repository` is an informative URL. It is never used for fetching: the source of a dependency is always the `from` clause of a requirement (P6, draft §28). `sai add` does not read it.

## 1.18 Draft examples, corrected

Each example of the draft that shows a package contract is given here in its corrected form.

### 1.18.1 Draft §30: a server package

```sai-package
package acme:server @ 0.4.0
compiler ^0.1
source "src"

requires {
    sai:http @ ^1.4
        from git "https://github.com/sai-lang/http"
    sai:json @ ^1.1
        from git "https://github.com/sai-lang/json"
    acme:crypto @ ^2.0
        from git "https://github.com/acme/crypto"
}

targets { wasm32-core, wasm32-wasi }

allow { alloc, wasi:http, wasi:clocks }
deny { wasi:filesystem }

export world server
```

- The draft's contents are kept. `compiler` is added because `sai new` writes it; it is optional (§1.5).
- `export world server` together with `wasm32-core` is valid (N-G-8, §1.9 rule 5). On `wasm32-wasi`, `sai build` produces a component for the world `server`, which MUST be declared in `src/lib.sai`. On `wasm32-core`, `sai build` produces a core module whose exports are the package's `export fn` items (with the information diagnostic `EXPORT_WORLD_NOT_APPLIED`), and any reachable use of `wasi:http` or `wasi:clocks` is a compile error, because a core target provides no WASI (D-TGT-01, draft §59).
- `deny { wasi:filesystem }` is not covered by `allow` and only records intent (§1.11.1 rule 3).
- The package version is below 1.0.0, so `pub` functions may leave their effects inferred, with the lint `PUBLIC_EFFECTS_INFERRED` (D-EFF-01).

### 1.18.2 Draft §22: policy as a maximum

```sai-package
allow { alloc, wasi:http }
deny { wasi:filesystem }
```

`allow` is the maximum grant, not a list of capabilities the package must use (D-CAP-01).

### 1.18.3 Draft §32: `sai add`

`sai add https://github.com/acme/json` reads the repository's `sai.package` at its newest release tag ([02-resolver.md](02-resolver.md) §2.11), finds `package acme:json @ 1.6.2`, and adds:

```sai-package
requires {
    acme:json @ ^1.6.2
        from git "https://github.com/acme/json"
}
```

The code then writes `use acme:json` (D-MOD-01). The identity comes from the fetched contract, not from the URL (P6).

### 1.18.4 Draft §34 and §35: source, artifact, and hybrid packages

A source package (draft §34, corrected by R-10):

```text
acme-json/
├ sai.package
└ src/
   └ lib.sai
```

```sai-package
package acme:json @ 1.6.2
compiler ^0.1
source "src"
targets { wasm32-core, wasm64-core, wasm32-wasi, wasm64-wasi }
allow { alloc }
```

A Wasm (artifact) package (draft §34):

```text
acme-blake3/
├ sai.package
├ wit/
│  └ hashing.wit
└ dist/
   └ wasm32/
      └ blake3.wasm
```

```sai-package
package acme:blake3 @ 1.0.4
wit "wit"
artifact component {
    wasm32-wasi "dist/wasm32/blake3.wasm"
}
targets { wasm32-wasi }
export world hashing
```

- The draft only shows the files. The world `hashing` is a world of the WIT package `acme:blake3` in `wit/hashing.wit`.
- There is no `allow`, so the artifact may import nothing (D-CAP-01; draft §74, "requires none").
- Users write the same `use acme:blake3` as for a source package (draft §34).

A hybrid package (draft §35):

```sai-package
package acme:image @ 1.2.0
source "src"
wit "wit"
artifact component {
    wasm32-wasi "dist/image.wasm"
}
targets { wasm32-core, wasm32-wasi }
allow { alloc }
export world processing
```

- The draft's artifact key `wasm32` is replaced by the target name `wasm32-wasi`, and `targets` is added (§1.7, §1.8).
- `wit` and `export world` are added because a hybrid package must state the contract that its source and its artifact both implement (§1.7 rule 6).
- "The compiler chooses the best form" becomes the deterministic rule of D-PKG-05: the source is used with `--profile tiny` (the draft's `--tiny`, C-06), on `wasm32-core`, and by default on `wasm32-wasi`. The root forces the artifact with `overrides { acme:image use artifact }` (§1.16). The chosen form is shown by `sai size` and `sai inspect`.

### 1.18.5 Draft §47: a size budget

```sai-package
profile tiny {
    budget { wasm <= 16 KiB }
}
```

`sai build --profile tiny` fails with `SIZE_BUDGET_EXCEEDED` when the output is larger than 16 KiB (16384 bytes) (§1.14.3).

### 1.18.6 Draft §68: compiler-verifiable policy

```sai-package
policy { deny unsafe }
deny { native_ffi }
```

`unsafe` is a policy item. `native_ffi` is a capability (D-CAP-01), so it is denied in `deny { }`; writing it inside `policy` is `POLICY_ITEM_IS_CAPABILITY` (§1.12 rule 5). Both are checked by the compiler, as the draft requires.

### 1.18.7 Draft §74: capability contracts of foreign Wasm packages

The SQLite component (R-13):

```sai-package
package acme:sqlite @ 3.1.0
wit "wit"
artifact component {
    wasm32-wasi "dist/wasm32/sqlite.wasm"
    wasm64-wasi "dist/wasm64/sqlite.wasm"
}
targets { wasm32-wasi, wasm64-wasi }
allow { alloc, wasi:filesystem }
export world database
```

The hash library ("requires none"): no `allow` declaration at all, which means the empty set (D-CAP-01), as in the `acme:blake3` example of §1.18.4.

`sai add https://github.com/acme/sqlite` displays this contract for review (draft §75; [04-distribution-and-security.md](04-distribution-and-security.md) §4.7): the identity `acme:sqlite@3.1.0`, the form (Wasm component), the capabilities `alloc` and `wasi:filesystem`, the supported targets `wasm32-wasi` and `wasm64-wasi`, the unsupported targets (for example `wasm32-core`), and the estimated size contribution.

### 1.18.8 Draft §80: a stable client package

```sai-package
package acme:user-client @ 1.0.0
compiler ^0.1
source "src"

requires {
    sai:http @ ^1.0
        from git "https://github.com/sai-lang/http"
}

targets { wasm32-wasi }

allow { alloc, wasi:http }
deny { wasi:filesystem }

export world client
```

- `src/lib.sai` MUST declare the world `client` (R-04): `world client { import wasi:http/outgoing-handler  export users }`, with the interface `users` of R-04.
- The version is 1.0.0, so every `pub` function with non-empty effects must declare them (`MISSING_EFFECTS_DECLARATION`, D-EFF-01). The functions exposed through the world must declare them as well.
- `use acme:user-client` binds `user_client` in dependents (D-MOD-01).

## 1.19 Complete annotated example

```sai-package
// The identity and version of this package (§1.4). Must come first.
package acme:photo-service @ 1.3.0

// Language versions that can compile this package (§1.5).
compiler ^0.1

// Content (§1.6): the root module is src/lib.sai. The WIT directory is optional
// for a source package; here it publishes the service's interfaces.
source "src"
wit "wit"

// Metadata (§1.17): no effect on the build.
description "Photo upload and thumbnail service"
license "Apache-2.0"
authors { "Acme Platform Team <platform@acme.example>" }
repository "https://github.com/acme/photo-service"

// Supported targets (§1.8). The core target builds a freestanding module
// from the `export fn` items; the WASI target builds a component (§1.9).
targets { wasm32-core, wasm32-wasi }

// The world implemented by the component build (§1.9). Declared in src/lib.sai.
export world photo_service

requires {
    // A requirement split across continuation lines (§1.2.6).
    sai:http @ ^1.4
        from git "https://github.com/sai-lang/http"
    sai:collections @ ^0.3
        from git "https://github.com/sai-lang/collections"
    // A package inside a monorepo: tags are named `image/v<version>` (§1.10.3).
    acme:image @ ^1.2
        from git "https://github.com/acme/media" dir "image"
    // Only active, resolved, and locked for WASI targets (§1.10.5).
    sai:wasi @ ^0.2
        from git "https://github.com/sai-lang/wasi"
        only { wasm32-wasi }
    // The root grants this dependency no capability at all (§1.10.8).
    acme:exif @ >=0.7.1, <0.8
        from git "https://github.com/acme/exif"
        grant {}
    // A local package during development (§1.10.3): pins its line.
    acme:photo-model @ ^1.0
        from path "../photo-model"
}

// Available to tests only, and only when this package is the root (§1.10.6).
requires test {
    acme:fixtures @ ^0.2
        from git "https://github.com/acme/fixtures"
}

// The maximum grant (§1.11). Everything else is denied; omitted `allow` = {}.
allow { alloc, wasi:http, wasi:clocks, wasi:cli }
deny { wasi:http/incoming-handler, wasi:filesystem }

// Compiler-verifiable restrictions (§1.12).
policy { deny unsafe, deny ambient_imports, deny duplicate_lines }

// Package-wide lint levels (§1.13). Root-only.
lints { deny NAMING_CONVENTION, allow CONTRACT_RUNTIME_CHECK }

// Profile extensions (§1.14). Root-only; they never change semantics.
profile tiny {
    allow { alloc }
    budget { wasm <= 64 KiB }
}
profile release {
    budget { wasm <= 2 MiB }
}

// Root-only build configuration (§1.15, §1.16).
overrides {
    acme:image use artifact
    sai:http use git "https://github.com/acme/http-fork"
}
```

Notes:

- `acme:exif @ >=0.7.1, <0.8` is in the compatibility line `0.7`; it admits `0.8.0` pre-releases but excludes the stable `0.8.0` boundary, so no `VERSION_REQ_SPANS_LINES` warning is issued. `^0.7.1` is the equivalent canonical spelling.
- `deny { wasi:http/incoming-handler }` narrows `wasi:http` to the other interfaces of `wasi:http`, such as `wasi:http/outgoing-handler`.
- A `tiny` build may use `alloc` only: WASI is denied by default in `tiny` (D-TGT-02) and is not lifted by `profile tiny { allow }`.
- `acme:image use artifact` takes effect only on `wasm32-wasi` with the `dev` or `release` profile (D-PKG-05, §1.16).
- `policy { deny duplicate_lines }` turns `DUPLICATE_PACKAGE_LINE` into an error for the whole graph, because this package is the root.

## 1.20 Decisions introduced by this chapter

Each entry has a provisional ID. The rationale cites the principles of [decisions.md §0.1](../design/decisions.md).

| ID | Rule | Rationale |
|---|---|---|
| N-G-1 | `sai.package` and `sai.lock` use a dedicated lexical grammar (§1.2.3) with the token kinds `WORD`, `UWORD`, `PKG_PATH`, `INT`, `VERSION`, `STRING`, and a few punctuators. Strings are simple strings only; there are no doc comments; all non-comment, non-string text is ASCII. | P9, P8: one way to write each value; line-oriented files that diff and merge cleanly (D-PKG-03). |
| N-G-2 | NEWLINE generation uses E = {`WORD`, `UWORD`, `PKG_PATH`, `INT`, `VERSION`, `STRING`, `}`} and the continuation set C = {`from`, `dir`, `only`, `grant`}. `requires`, `overrides`, `artifact component`, and `profile` entries are separated by line breaks only. | P8: the rule depends only on the previous token and the next line's first token, as in D-LEX-01; supports `from git ".."` on its own line (draft §30). |
| N-G-3 | Package name segments are `PkgSeg` of 1–64 bytes. The namespace `wasi` and the identities `sai:core` and `sai:alloc` are reserved. | P8: `wasi:*` names are capability names (D-CAP-01); core and alloc are implicit (D-PKG-04). |
| N-G-4 | Package versions are full SemVer versions; build metadata is not allowed in `sai.package` or `sai.lock`. | P8: SemVer ordering ignores build metadata, so two versions differing only in it could not be told apart by MVS. |
| N-G-5 | `compiler <req>` is the only language-version gate; it is optional and `sai new` writes it. There are no editions in v0.1. | P7, P9: one mechanism; editions can be added later as a new declaration. |
| N-G-6 | The package kind is inferred from the presence of `source` and `artifact`. There is no default source directory; a package with neither is `PACKAGE_HAS_NO_CONTENT`. | P1: content is always stated; no separate `kind` declaration that could contradict it (P9). |
| N-G-7 | `targets` is required and non-empty. Every dependency active for a target must list that target. | P1, P2: target support is a checked, visible promise (draft §75). |
| N-G-8 | `export world` names a local world (a module path relative to the root module, or a world of the `wit` package for artifact packages) or an external world `ns:pkg/world`. Declaring it in a package whose `targets` include a core target is valid; the core build follows spec/13 §13.6.2 (N-D-32): `sai build` emits a core module from `export fn` items with `EXPORT_WORLD_NOT_APPLIED`, and `sai component build` wraps it with the world's exports only. | P8: the outcome depends only on the target; keeps draft §30 valid; matches D-TGT-01's wrapping of core output. |
| N-G-9 | Every requirement has an explicit `from` source, including the bundled standard packages. | P1, P6: the source is always visible; the pre-populated cache only avoids the network. |
| N-G-10 | A version requirement has exactly one lower-bound comparator (`^ ~ = >=`) and at most one upper bound (`< <=`); `>` does not exist; `=` stands alone; `=` and `<=` need full versions. The line of a requirement is the line of its minimum; an interval containing the stable start of the next line gets `VERSION_REQ_SPANS_LINES`. A pre-release below that stable boundary does not trigger the warning. | P8: MVS needs a well-defined minimum (D-PKG-02); the warning exposes stable bounds that can never matter while preserving the documented boundary range. |
| N-G-11 | No optional, feature, or build-time dependencies. `requires test` is root-only; a dependency's test requirements are never resolved. An identity appears at most once across `requires` and `requires test`. | P1, P8: the graph does not depend on hidden configuration; `use` by identity is unambiguous. |
| N-G-12 | A requirement may carry `grant { .. }`, the maximum set of capabilities granted to that dependency (including its transitive dependencies). Omitted means no extra restriction. | P5: least authority per dependency; P1: visible at the requirement. |
| N-G-13 | `policy` items are `deny unsafe`, `deny ambient_imports`, `deny runtime_contracts` (package-local), and `deny duplicate_lines` (graph-wide, root-only). A capability name used as a policy item is `POLICY_ITEM_IS_CAPABILITY`. | P9: one way to deny a capability (D-CAP-01); package-local promises are checkable per package. |
| N-G-14 | `lints { .. }` with the levels `allow`, `warn`, and `deny` sets package-wide lint levels below source attributes. It is root-only, and lint diagnostics in dependency code are not reported in the dependent's build. | P8: a dependency's lints never break a dependent's build; semantics unchanged (C-03). |
| N-G-15 | Profile blocks exist only for `dev`, `release`, and `tiny`; a profile `allow` may only lift the profile's default denials within the package `allow`; the only budget metric is `wasm` with the units `B`, `KiB`, `MiB`. Profile blocks are root-only. | P5: the package `allow` stays the maximum (D-CAP-01); C-03; D-TGT-02. |
| N-G-16 | Root-only declarations (`requires test`, `lints`, `profile`, `allocator`, `overrides`, and the policy item `deny duplicate_lines`) are ignored without a diagnostic when the package is a dependency. | P8: a uniform rule; a library may configure its own development builds. |
| N-G-17 | Overrides are `use artifact`, `use path`, and `use git`. There are no version overrides. A `use path` override affects only the line of the path package's version. | P8: every selected version satisfies every requirement (D-PKG-02). |
| N-G-18 | Metadata declarations `description`, `license` (SPDX), `authors`, and `repository` exist and never affect the build; `repository` is never used for fetching. | P6: identity and source are separate from informative locations. |
| N-G-19 | The only artifact kind is `artifact component`, keyed by component target names. Artifact and hybrid packages require `wit` and `export world`; an artifact package needs an artifact for every target. The WIT package name must equal the package identity. | P4: only typed component boundaries; P2: the artifact can be checked against a declared contract. |
| N-G-20 | Paths in `sai.package` are relative and `/`-separated; `..` is allowed only for `from path` and `use path`; content paths stay inside the package directory. Paths are limited to 4096 bytes with 255-byte components and reject host-ambiguous trailing dots/spaces and Windows device names; `.git`, `.sai`, and `out` are reserved case-insensitively where applicable. | P6, P8: portable, host-independent package layout. |

## 1.21 Diagnostics

A compiler that implements Stage 3 but not Stage 4 rejects `wit`, `artifact`, and `export world` with `FEATURE_NOT_IMPLEMENTED`, naming Stage 4, with the field name (`wit`, `artifact` or `export world`) as the `feature` ([spec/01](../spec/01-introduction.md) §1.4.5). Codes defined in another document are marked with that document.

| Code | Severity | Description |
|---|---|---|
| `INVALID_UTF8` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; ill-formed UTF-8 |
| `INVALID_LINE_ENDING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; lone CR |
| `FORBIDDEN_CHARACTER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; forbidden control, bidirectional, or invisible character |
| `UNTERMINATED_COMMENT` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.3; unterminated block comment |
| `UNTERMINATED_STRING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.4; unterminated string or raw line break in a string |
| `INVALID_ESCAPE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.3; invalid escape sequence |
| `NO_SEMICOLON` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.8; use of `;` |
| `UNEXPECTED_NEWLINE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.12; NEWLINE where the grammar does not allow one |
| `EXPECTED_TOKEN` | error | defined in [spec/03](../spec/03-grammar.md) §3.16; an expected token is missing |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; a Stage 4 declaration used with a compiler that does not implement Stage 4 |
| `UNEXPECTED_CHARACTER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.12; here, a character that cannot start a manifest token, or non-ASCII text outside comments and strings |
| `UNSUPPORTED_STRING_FORM` | error | multiline, raw, byte, or format string, or a line continuation, in `sai.package` or `sai.lock` |
| `INVALID_VERSION` | error | malformed version: leading zero, overflow, bad pre-release, too few or too many components |
| `BUILD_METADATA_NOT_ALLOWED` | error | `+build` metadata in a version |
| `MISSING_PACKAGE_DECL` | error | no `package` declaration |
| `PACKAGE_DECL_NOT_FIRST` | error | `package` is not the first declaration |
| `DUPLICATE_DECLARATION` | error | a declaration, profile block, profile item, or artifact target given twice |
| `UNKNOWN_DECLARATION` | error | a line starts with a word that is not a declaration keyword |
| `DUPLICATE_LIST_ITEM` | warning | an element repeated inside one list |
| `INVALID_PACKAGE_NAME` | error | a package identity that is not `namespace:name` or has a segment longer than 64 bytes |
| `RESERVED_NAMESPACE` | error | a package declared in the `wasi` namespace |
| `RESERVED_PACKAGE_NAME` | error | a package declared as `sai:core` or `sai:alloc` |
| `COMPILER_VERSION_MISMATCH` | error | the toolchain's language version does not satisfy a package's `compiler` requirement |
| `INVALID_PACKAGE_PATH` | error | a path in `sai.package` violates §1.6.1 |
| `MISSING_ROOT_MODULE` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.2.3; the `source` directory or `<source>/lib.sai` does not exist (§1.6.2) |
| `PACKAGE_HAS_NO_CONTENT` | error | neither `source` nor `artifact` is declared |
| `WIT_DIR_NOT_FOUND` | error | the `wit` directory does not exist |
| `WIT_PACKAGE_MISMATCH` | error | the WIT package name or version differs from the package identity |
| `WIT_REQUIRED` | error | an artifact or hybrid package without `wit` |
| `UNKNOWN_ARTIFACT_KIND` | error | `artifact` followed by a word other than `component` |
| `ARTIFACT_TARGET_NOT_DECLARED` | error | an artifact for a target missing from `targets` |
| `ARTIFACT_ON_CORE_TARGET` | error | an artifact keyed by a target that is not a component target: a core target, or a native target ([targets](../targets/targets-and-profiles.md) §1.4 item 6, N-F-62) |
| `ARTIFACT_NOT_FOUND` | error | an artifact file that does not exist or is not a regular file |
| `MISSING_ARTIFACT_FOR_TARGET` | error | an artifact package without an artifact for one of its targets |
| `MISSING_EXPORT_WORLD` | error | an artifact or hybrid package without `export world` |
| `MISSING_TARGETS` | error | no `targets` declaration or its list is empty |
| `UNKNOWN_TARGET` | error | a name that is not a target name |
| `TARGET_NOT_DECLARED` | error | building the root for a target not in its `targets` |
| `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` | error | an active dependency does not list the build target |
| `INVALID_WORLD_NAME` | error | an `export world` segment that is not a snake_case lower identifier, or a `PKG_PATH` without exactly one `/` part |
| `UNKNOWN_EXPORT_WORLD` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.1; the named world does not exist where §1.9 rule 3 looks for it |
| `EXPORT_WORLD_NOT_APPLIED` | info | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2; `sai build` for a core target ignores `export world` |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; a reachable capability, including a world import, that the target does not provide |
| `IMPLICIT_PACKAGE_REQUIRED` | error | `sai:core` or `sai:alloc` required or overridden |
| `PACKAGE_REQUIRES_ITSELF` | error | a package requires its own identity |
| `DUPLICATE_REQUIREMENT` | error | an identity required twice across `requires` and `requires test` |
| `TEST_DEPENDENCY_OUTSIDE_TEST` | error | defined in [spec/14](../spec/14-testing.md) §14.2.5; a test dependency used outside tests and properties, or re-exported (§1.10.6 rule 3) |
| `TEST_DEPENDENCY_UNAVAILABLE` | error | defined in [spec/14](../spec/14-testing.md) §14.2.5 item 5; a test-only use in a workspace member that is not the root |
| `BARE_VERSION_REQUIREMENT` | error | a version requirement without an operator |
| `UNSUPPORTED_VERSION_OPERATOR` | error | the operator `>` |
| `VERSION_REQ_NO_LOWER_BOUND` | error | a requirement without `^`, `~`, `=`, or `>=` |
| `INVALID_VERSION_REQUIREMENT` | error | two lower or two upper bounds, `=` combined with another comparator, or a partial version with `=` or `<=` |
| `EMPTY_VERSION_REQUIREMENT` | error | a requirement that no version satisfies |
| `VERSION_REQ_SPANS_LINES` | warning | a requirement interval contains the stable start of the next compatibility line; a boundary pre-release alone does not trigger it |
| `MISSING_SOURCE` | error | a requirement without `from` |
| `PATH_PACKAGE_NOT_FOUND` | error | a `from path` or `use path` directory without `sai.package` |
| `PATH_ESCAPES_REPOSITORY` | error | defined in [02](02-resolver.md) §2.7.2; a path source in a Git package leaves the repository |
| `PACKAGE_IDENTITY_MISMATCH` | error | defined in [02](02-resolver.md) §2.6.2; the fetched contract declares another identity or version |
| `ONLY_TARGET_NOT_DECLARED` | error | an `only` target missing from the requiring package's `targets` |
| `INACTIVE_DEPENDENCY` | error | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.2; a `use` of a dependency that is not active for the build target (§1.10.5 rule 4) |
| `CAPABILITY_NOT_GRANTED` | error | a capability required by a dependency or its dependencies is neither covered by the requirement's `grant` nor, for `wasi:io`, implied by it ([targets](../targets/targets-and-profiles.md) §3.3) (build time) |
| `INVALID_CAPABILITY_NAME` | error | a name that is not a capability name of D-CAP-01 |
| `UNKNOWN_CAPABILITY` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.1; a WIT package or interface name that is not known to the build |
| `CAPABILITY_ALLOW_DENY_CONFLICT` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.5; the same name in `allow` and `deny` |
| `UNKNOWN_POLICY_ITEM` | error | an unknown `policy` item |
| `POLICY_ITEM_IS_CAPABILITY` | error | a capability name used as a policy item |
| `UNSAFE_DENIED` | error | an unsafe construct in a package with `policy { deny unsafe }` |
| `AMBIENT_IMPORT` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.5.3; direct import of a WASI interface; an error under `deny ambient_imports` (D-CAP-02) |
| `CONTRACT_RUNTIME_CHECK` | info (lint) | defined in [spec/09](../spec/09-contracts-and-refinements.md) §9.7.1; a contract checked at run time; an error under `deny runtime_contracts` (D-CONTRACT-01) |
| `DUPLICATE_PACKAGE_LINE` | warning (lint) | defined in [02](02-resolver.md) §2.9; several lines of one package; an error under `deny duplicate_lines` |
| `MISSING_EFFECTS_DECLARATION` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; a public function of a package at 1.0.0 or later without its effects (D-EFF-01) |
| `PUBLIC_EFFECTS_INFERRED` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; inferred public effects below 1.0.0 (D-EFF-01) |
| `UNKNOWN_LINT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.15.4; a code in `lints` that is not a diagnostic code |
| `NOT_A_LINT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.15.4; a code in `lints` that is the code of a hard diagnostic |
| `CONFLICTING_LINT_LEVEL` | error | one lint given two different levels in `lints` |
| `UNKNOWN_PROFILE` | error | a profile name other than `dev`, `release`, `tiny` |
| `PROFILE_ALLOW_EXCEEDS_PACKAGE` | error | a profile `allow` name not covered by the package `allow` |
| `UNKNOWN_BUDGET_METRIC` | error | a budget metric other than `wasm` |
| `INVALID_SIZE` | error | a unit other than `B`, `KiB`, `MiB`, a zero size, or an overflowing size |
| `SIZE_BUDGET_EXCEEDED` | error | the build output exceeds a budget; the build fails and writes no output file (§1.14.3 item 4) |
| `ALLOCATOR_NOT_REQUIRED` | error | `allocator` names a package that the root does not require |
| `INVALID_OVERRIDE` | error | `use artifact` on a package that is not hybrid |
| `DUPLICATE_OVERRIDE` | error | two overrides of the same kind for one identity |
| `UNUSED_OVERRIDE` | warning | an override that matches nothing in the resolved graph |
| `INVALID_DESCRIPTION` | error | `description` contains a line break or exceeds 1024 UTF-8 bytes |
| `INVALID_LICENSE_EXPRESSION` | warning | `license` is not a valid SPDX expression |
