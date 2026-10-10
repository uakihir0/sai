# 03. The Lock File

**Status:** Normative
**Stage:** 3
**Related:** [01-package-contract.md](01-package-contract.md), [02-resolver.md](02-resolver.md), [04-distribution-and-security.md](04-distribution-and-security.md), [decisions.md](../design/decisions.md) (D-PKG-02, D-PKG-03), [draft-reconciliation.md](../design/draft-reconciliation.md) (R-11), [spec/08](../spec/08-effects-and-capabilities.md), [impl/13](../impl/13-cli.md)

This chapter defines `sai.lock` byte for byte: its lexical form, its grammar, its canonical layout, and the meaning and validation of every field. It defines the `sai1:` checksum algorithm precisely (file selection, path normalization, ordering, file modes, line endings, the canonical byte stream, SHA-256, and the digest encoding), the lifecycle of the lock (creation, update, verification), the behaviour under `--locked`, `--frozen`, and `--offline`, merging, and format versioning. It ends with the corrected example of draft §33. How the resolver uses the lock is defined in [02-resolver.md](02-resolver.md) §2.10; how content is fetched and cached is in [04-distribution-and-security.md](04-distribution-and-security.md). The spelling of command-line flags is owned by [impl/13](../impl/13-cli.md).

---

## 3.1 Overview

### 3.1.1 Purpose

1. `sai.lock` records the result of resolution for one root package: the selected version and the source of every resolved node, the exact commit, checksums of the fetched content, the capability footprint of every package, and the edges between them (D-PKG-03).
2. The file name is `sai.lock` (R-11: the draft's text contains two U+200B characters inside the name; they are not part of it). It lives in the root package's directory, next to `sai.package`.
3. The lock is **machine-generated**. Tools write it; users do not edit it. A hand-edited lock is accepted only if it is valid; it is rewritten in canonical form at the next write (§3.7.2).
4. The lock SHOULD be committed to version control for every package, applications and libraries alike (N-G-46). A library's lock is never read by its dependents ([02 §2.10.1](02-resolver.md)), but it makes the library's own tests and CI reproducible.
5. With the same `sai.package`, `sai.lock`, toolchain, target, and profile, a build uses exactly the same dependency content (draft §33, §72; [04 §4.8](04-distribution-and-security.md)).

### 3.1.2 Design constraints

1. **Same lexical rules as `sai.package`** (D-PKG-03): the lock is tokenized by [01 §1.2](01-package-contract.md). No new token kinds are needed; digests and commits are strings.
2. **Line-oriented and canonical:** one fact per line, a fixed order of blocks and fields, and exactly one byte representation for each lock content (§3.2.3). Two toolchains that compute the same resolution write identical bytes.
3. **Merge-friendly:** one block per package, sorted, separated by blank lines, without counters, timestamps, or other global data that would change on every write (§3.9).
4. **Self-validating:** every checksum identifies its algorithm by the prefix `sai1:` (§3.4).

---

## 3.2 Lexical form and layout

### 3.2.1 Lexical rules

1. The lock is UTF-8 text tokenized with the same token kinds and string, comment, and line-ending rules as `sai.package` ([01 §1.2](01-package-contract.md)). Its NEWLINE continuation set is `{ only }`: `only` may continue a requirement field, while `dir` starts a separate lock field and therefore always follows a NEWLINE. The other package-contract continuation words (`from`, `dir`, and `grant`) do not continue a lock line. The diagnostics of spec/02 apply (`INVALID_UTF8`, `INVALID_LINE_ENDING`, `FORBIDDEN_CHARACTER`, ...).
2. The punctuator `->` occurs only in the lock (§3.3).
3. The contextual keywords of the lock are: `artifact`, `capabilities`, `checksum`, `commit`, `compiler`, `dir`, `git`, `imports`, `lock`, `only`, `package`, `path`, `requires`, `root`, `source`, `targets`, `test`, `wit`.
4. Comments are lexically valid in a lock, but the toolchain never writes them, and a lock that contains them is not canonical (§3.2.3).
5. Before tokenizing, the reader checks for merge conflict markers (§3.9.1).

### 3.2.2 Structure

A lock consists of a **header line**, a **root block**, and zero or more **entry blocks**:

```sai-lock
lock 1

root acme:app @ 0.3.0
    targets { wasm32-wasi }
    requires acme:json @ ^1.6 -> 1.8.0

package acme:json @ 1.8.0
    source git "https://github.com/acme/json"
    commit "0095142504f87ba0c83406017d3637a65db29876"
    checksum "sai1:495a0e04c51e574dc6fbd7ac377914a1e76031b8df86acf72c4aea8630318e26"
    compiler ^0.1
    capabilities { alloc }
```

1. The header is `lock` followed by the format version, an `INT` (§3.10).
2. The root block describes the root package and its edges. It is not an entry: the root is not locked content.
3. Each entry block describes one resolved node ([02 §2.1.1](02-resolver.md)). Its first line is `package <id> @ <version>`; the following lines are its fields.
4. Blocks are recognized by their first keyword (`root`, `package`). Indentation and blank lines are not significant to the parser; they are part of the canonical layout only.

### 3.2.3 Canonical layout

The toolchain writes exactly the following bytes (N-G-36):

1. Lines end with LF (U+000A). There is no CR, no trailing whitespace on any line, and no byte order mark.
2. Line 1 is `lock 1`. Line 2 is empty. Then follows the root block, then each entry block in the order of §3.3.4. Blocks are separated by exactly one empty line.
3. The first line of a block starts in column 1. Every field line is indented by exactly four spaces (U+0020).
4. Within a line, tokens are separated by exactly one space, except that no space precedes `,` and no space follows a comparison operator (`^`, `~`, `=`, `>=`, `<`, `<=`). Thus `@` and `->` have one space on each side, a list is written `{ a, b, c }`, and an empty list is `{ }`. A list is always written on one line.
5. A version requirement is written with its comparators in source order, each as the operator immediately followed by the version in canonical form (§3.3.6), joined by `, ` (`>=1.2, <2.0`). Partial versions stay partial; they are not completed, because completion changes the meaning of `~` ([02 §2.3.1](02-resolver.md)).
6. Strings are written between `"` with `\"` for `"` and `\\` for `\`. All other characters are written literally. Strings in a lock never contain control characters (§3.3.6).
7. The file ends with the LF of its last line. There is no empty line at the end.
8. Nothing else is written: no comments, no timestamps, no toolchain version, and no host-dependent data.

A lock that parses and validates (§3.3) but whose bytes differ from the canonical serialization of its content is **not canonical**. Reading it is the warning `LOCK_NOT_CANONICAL`; its content is used as is (§3.7.2).

---

## 3.3 Grammar and fields

### 3.3.1 Grammar

The notation is that of [01 §1.3](01-package-contract.md). `VersionReq`, `OnlyClause`, and `CapName` are the productions of 01.

```ebnf
LockFile      ::= Header NL RootBlock ( NL EntryBlock )* nl EOF
Header        ::= "lock" INT
RootBlock     ::= "root" PKG_PATH '@' VERSION ( NL RootField )*
RootField     ::= TargetsField | RootEdge
TargetsField  ::= "targets" '{' nl List<WORD, Sep> nl '}'
RootEdge      ::= "requires" "test"? PKG_PATH '@' VersionReq '->' VERSION OnlyClause?
EntryBlock    ::= "package" PKG_PATH '@' VERSION ( NL EntryField )*
EntryField    ::= SourceField | DirField | CommitField | ChecksumField | WitField
                | ArtifactField | CompilerField | CapsField | ImportsField | EntryEdge
SourceField   ::= "source" ( "git" STRING | "path" STRING )
DirField      ::= "dir" STRING
CommitField   ::= "commit" STRING
ChecksumField ::= "checksum" STRING
WitField      ::= "wit" STRING
ArtifactField ::= "artifact" WORD STRING
CompilerField ::= "compiler" VersionReq
CapsField     ::= "capabilities" '{' nl List<CapName, Sep>? nl '}'
ImportsField  ::= "imports" WORD '{' nl List<CapName, Sep>? nl '}'
EntryEdge     ::= "requires" PKG_PATH '@' VersionReq '->' VERSION OnlyClause?
```

The reader accepts any layout that this grammar and the lexical rules allow (for example lists split over lines); the writer produces only the canonical layout of §3.2.3.

### 3.3.2 The root block

| Field | Occurs | Meaning |
|---|---|---|
| `root <id> @ <version>` | once | identity and version of the root package |
| `targets { .. }` | once | the root's `targets`, sorted by bytes |
| `requires [test] <id> @ <req> -> <v> [only { .. }]` | once per root edge | an included edge of the root ([02 §2.5](02-resolver.md)): the required identity, the requirement in canonical form, the selected version of its node, and its `only` set sorted by bytes; `test` marks a `requires test` edge |

Root edges are recorded so that the resolver can decide, without fetching anything, whether the lock still matches the root contract ([02 §2.10.2](02-resolver.md) rule 3). Source clauses and `grant` sets are not recorded: they are read from `sai.package` (N-G-44).

### 3.3.3 Entry fields

| Field | Git entry | Path entry | Meaning |
|---|---|---|---|
| `source git "<url>"` | required | absent | canonical URL of the repository ([04 §4.2.2](04-distribution-and-security.md)) |
| `source path "<p>"` | absent | required | directory of a local path package, relative to the root's directory, normalized, with `/` separators; it may start with `..` |
| `dir "<d>"` | if the package is in a subdirectory | absent | normalized package directory inside the repository |
| `commit "<hex>"` | required | absent | the commit the content was taken from: 40 lowercase hexadecimal digits (SHA-1 repositories) or 64 (SHA-256 repositories) |
| `checksum "sai1:<hex>"` | required | absent | source tree checksum of the package directory (§3.4) |
| `wit "sai1:<hex>"` | if the package declares `wit` | absent | WIT checksum (§3.5) |
| `artifact <target> "sai1:<hex>"` | one per declared artifact | absent | artifact checksum (§3.6) |
| `compiler <req>` | if declared | if declared | the package's `compiler` requirement ([01 §1.5](01-package-contract.md)) |
| `capabilities { .. }` | source and hybrid packages | absent | capability footprint of the source ([spec/08 §8.12.8](../spec/08-effects-and-capabilities.md)), normalized as in item 1 and sorted by bytes |
| `imports <target> { .. }` | one per declared artifact | absent | the capabilities imported by the artifact for that target, sorted by bytes |
| `requires <id> @ <req> -> <v> [only { .. }]` | once per edge | once per edge | an included edge of the package ([02 §2.5](02-resolver.md)) |

1. The **resolved capabilities** of D-PKG-03 are the `capabilities` and `imports` fields. They are the footprint of spec/08 §8.12.8, not the result of the build-time reachability check, which depends on the root's code, the target, and the profile. The `capabilities` field is the union of the source footprints for the targets that the package declares ([01 §1.8](01-package-contract.md)), without any capability that another capability of the set covers (the *covers* relation of [spec/08 §8.12.1](../spec/08-effects-and-capabilities.md); `wasi:http/types` is omitted when `wasi:http` is present). This normalization ([impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.10 items 1 and 2) does not change which capabilities the field covers, so the escalation rule of [02 §2.12.3](02-resolver.md) is unaffected.
2. Path entries carry no commit, checksums, or footprint (N-G-44): a local path package is the user's working copy, read anew at every build ([02 §2.7.1](02-resolver.md) rule 6), and it is not subject to capability review. A path package inside a Git package ([02 §2.7.2](02-resolver.md)) is a Git entry.
3. An entry has no field for its compatibility line, its form (source, artifact, or hybrid), or the toolchain version: the line follows from the version, the form is chosen per build ([04 §4.5.1](04-distribution-and-security.md)), and the toolchain is not part of the resolution result.

### 3.3.4 Order

1. Entries are sorted by the bytes of their identity, then by version precedence ([02 §2.2.2](02-resolver.md)).
2. Within an entry, fields appear in the order of the table in §3.3.3. `artifact` and `imports` fields are sorted by target name; `requires` fields are sorted by the bytes of the required identity.
3. Within the root block, `targets` comes first, followed by the edges sorted by the bytes of the required identity (identities are unique across `requires` and `requires test`, [01 §1.10.4](01-package-contract.md)).
4. The reader accepts fields in any order. A different order makes the lock non-canonical (§3.2.3).

### 3.3.5 Validation

A lock that violates any of the following rules is `LOCK_CORRUPT`. The diagnostic names the line and the rule; the help suggests deleting `sai.lock` and running `sai lock`.

1. The header is the first line, and its version is supported (otherwise `UNSUPPORTED_LOCK_VERSION`, §3.10).
2. There is exactly one root block, directly after the header.
3. Every field allowed once occurs at most once; required fields are present; fields marked absent in §3.3.3 are absent. Unknown keywords are errors: format 1 has no extension fields (§3.10).
4. Identities, versions, requirements, and capability names are valid per [01](01-package-contract.md). Versions carry no build metadata.
5. No two entries have the same identity and the same compatibility line ([02 §2.2.3](02-resolver.md)).
6. A commit is 40 or 64 lowercase hexadecimal digits. A checksum is the string `sai1:` followed by exactly 64 lowercase hexadecimal digits.
7. URLs are canonical ([04 §4.2.2](04-distribution-and-security.md)); paths in `source path` and `dir` are normalized ([01 §1.6.1](01-package-contract.md)).
8. Targets are known target names; `artifact` and `imports` fields occur for the same set of targets.

Consistency between entries (for example an edge whose recorded version has no entry) is not corruption: it makes the lock out of date, and the resolver handles it by Mode B ([02 §2.10.3](02-resolver.md)).

### 3.3.6 Values

1. **Versions** are written in the canonical form of [02 §2.2.1](02-resolver.md): numeric identifiers without leading zeros, pre-release identifiers as in the source, no build metadata. Within a requirement, a partial version keeps its number of components (`^1.6` stays `^1.6`), but each present component is written without leading zeros.
2. **Requirements** are written as the package author wrote them, normalized by rule 1 and by the spacing of §3.2.3. Two requirements with the same comparators in the same order are the same requirement; the resolver compares requirements in this canonical form ([02 §2.10.2](02-resolver.md) rule 3).
3. **Strings.** No string in a lock contains a control character (U+0000–U+001F, U+007F–U+009F) or a bidirectional control character (U+202A–U+202E, U+2066–U+2069). URLs are ASCII ([04 §4.2](04-distribution-and-security.md)); paths satisfy [01 §1.6.1](01-package-contract.md); digests and commits are lowercase hexadecimal. A lock string that violates this is `LOCK_CORRUPT`.
4. **Names** in lists (targets, `only` sets, capabilities, and each `imports` capability set) are written as `WORD` or `PKG_PATH` tokens exactly as in `sai.package`, sorted by their bytes, without duplicates. These lists have set semantics: a reader treats a repeated member as one value, and canonical serialization removes repetitions. Repeated `artifact` or `imports` fields for the same target remain corrupt because each target has at most one field of each kind.
5. **Repository directory.** `dir "."` is accepted as a noncanonical spelling of the repository root ([02 §2.6.1](02-resolver.md)); canonical serialization omits the `dir` field for that entry. The resolver treats the value as an empty directory for tag lookup and tree hashing.

---

## 3.4 The `sai1:` tree hash

The **tree hash** is the checksum of a directory of a Git commit. The source checksum of an entry (`checksum`) is the tree hash of the package directory; the WIT checksum (`wit`, §3.5) is the tree hash of the WIT directory. D-PKG-03 fixes the principle: SHA-256 over the concatenation, in path order, of `sha256(content)  path\n`. This section fixes every detail (N-G-37).

### 3.4.1 Input

1. The input is a **Git tree object**: the tree at directory `D` of the locked commit `C`. `D` is the entry's `dir` (empty for the repository root) for the source checksum, and `D` followed by the package's `wit` path for the WIT checksum.
2. The hash is computed from Git objects, not from a working-tree checkout. Checkout behaviour of the host (`core.autocrlf`, `.gitattributes`, filters, sparse checkout) therefore has no effect.
3. The materialized copy in the cache ([04 §4.4.2](04-distribution-and-security.md)) contains exactly the selected files with exactly their blob bytes, so recomputing the hash over the cached files yields the same digest.
4. Local path packages are never hashed (§3.3.3 rule 2).

### 3.4.2 File selection

The **selected files** are the entries reachable from the input tree by recursive descent, each with its **path** relative to the input tree (components joined by `/`), filtered as follows:

1. **Regular files** (Git modes `100644` and `100755`) are selected. The two modes are hashed identically: the executable bit is not part of the hash, and cached files are never executable.
2. **Directories** (mode `040000`) are descended into. They contribute no line of their own; an empty directory cannot exist in Git and contributes nothing.
3. **Symbolic links** (mode `120000`) are not selected. Each one is reported once per fetch as the warning `SYMLINK_IGNORED`, naming its path. A symbolic link could point outside the package, and its meaning depends on the host.
4. **Gitlinks** (mode `160000`, submodules) are not selected and are never fetched (D-PKG-02). Each one is reported as the warning `SUBMODULE_IGNORED`.
5. Any path with a component equal to `.git` is not selected. (Git itself refuses such paths, but crafted trees can contain them.)
6. For the source checksum only, paths whose first component is `.sai` or `out` are not selected: these directories hold tool state and build outputs (D-TOOL-03, [01 §1.6.1](01-package-contract.md) rule 6).
7. Everything else is selected, including `.gitignore`, `.gitattributes`, the package's own `sai.lock` ([02 §2.10.1](02-resolver.md)), and nested packages (a subdirectory with its own `sai.package` is part of the enclosing directory's content).
8. No Git attribute is applied: no line-ending conversion, no `filter` (in particular no LFS smudge), no `export-ignore`, no `export-subst`. A Git LFS pointer file is hashed as the pointer text stored in the commit; LFS objects are never fetched (D-PKG-02).

### 3.4.3 Path validation

Every selected path MUST satisfy the following rules. A path that violates one is the error `INVALID_PACKAGE_FILE_PATH`, naming the path and the rule; the content is rejected and nothing is written to the cache.

1. The path is valid UTF-8. (Git paths are byte strings.)
2. No component is empty, `.`, or `..`.
3. The path contains no control character (U+0000–U+001F, U+007F–U+009F), no bidirectional control character (U+202A–U+202E, U+2066–U+2069), and none of `\ < > : " | ? *`.
4. No component is a reserved device name of Windows: `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, compared case-insensitively in ASCII, either alone or followed by `.` and further characters (`nul.txt`, `Com1.sai`).
5. No component ends with `.` or a space.
6. A component is at most 255 bytes, and the path is at most 4096 bytes.

These rules guarantee that the content can be materialized identically on every supported host (P4) and displayed unambiguously in diagnostics ([04 §4.11](04-distribution-and-security.md)).

### 3.4.4 Collisions

Two selected paths that are different byte strings but become equal after Unicode 16.0.0 normalization form NFC followed by Unicode 16.0.0 default full case folding are the error `PATH_COLLISION`, naming both paths. The same comparison applies to every component prefix: a file path cannot collide with a differently spelled directory prefix of another selected path, and a file cannot occupy a path another selected file needs as a directory. The implementation uses the Unicode Character Database 16.0.0 `CaseFolding.txt` C and F mappings (excluding Turkic-only mappings), with unmapped code points mapping to themselves. File systems that normalize or ignore case (the defaults on macOS and Windows) would merge them, and the materialized package would differ between hosts.

### 3.4.5 Order

The selected files are sorted by their path bytes (UTF-8), compared as unsigned bytes, with `/` as the byte 0x2F. This is the order of the Git index and of `LC_ALL=C sort`. A depth-first walk of Git trees, which Git stores in its own tree order, produces the same sequence.

### 3.4.6 Byte stream and digest

1. For each selected file, in the order of §3.4.5, the **line** is:
   - the SHA-256 digest of the file's content, as 64 lowercase hexadecimal digits;
   - two spaces (0x20 0x20);
   - the path bytes;
   - one LF (0x0A).
2. The content of a file is the exact bytes of its Git blob (after decompression), without any conversion.
3. The **stream** is the concatenation of the lines. Paths contain no LF (§3.4.3 rule 3), so the stream is unambiguous.
4. The **digest** is SHA-256 of the stream. It is written as `sai1:` followed by 64 lowercase hexadecimal digits (N-G-38). Uppercase digits, other lengths, and other prefixes are invalid in format 1 (§3.3.5 rule 6).
5. An empty selection has the empty stream. This cannot happen for a source checksum, because the package directory contains `sai.package`.

Informative: on a host with GNU coreutils and findutils, the stream equals the output of `sha256sum` applied to the selected files in sorted order, run from the input directory, provided no path needs `sha256sum`'s escaping (§3.4.3 excludes `\` and LF):

```text
cd <input-directory>
<list the selected paths, one per line> | LC_ALL=C sort | xargs -d '\n' sha256sum | sha256sum
```

### 3.4.7 Test vector

A package directory with these five files (`\n` is LF):

| Path | Content |
|---|---|
| `sai.package` | `package acme:hello @ 0.1.0\nsource "src"\ntargets { wasm32-core }\n` |
| `README.md` | `Hello\n` |
| `src/lib.sai` | `pub fn answer() -> i32 {\n    42\n}\n` |
| `src/empty.txt` | (empty) |
| `src/a.b` | `x\n` |

The stream is the following 386 bytes (five lines, each ending with LF):

```text
66a045b452102c59d840ec097d59d9467e13a3f34f6494e539ffd32c1bb35f18  README.md
97b18bd8c2d82630d9ae22caa012f739d24ce94402afbb5f7623cd592aff525c  sai.package
73cb3858a687a8494ca3323053016282f3dad39d42cf62ca4e79dda2aac7d9ac  src/a.b
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  src/empty.txt
a39be25cb7e57b5cbc17de00157a42dd64bf7a4ba83d2bbb52aa9a40658e2544  src/lib.sai
```

`README.md` precedes `sai.package` because `R` (0x52) is less than `s` (0x73). The digest is:

```text
sai1:9b4c1b8a5126574063c4c23803fe7eaa86a3d012b8d61b398f0980cc8abe8672
```

---

## 3.5 The WIT checksum

1. An entry has a `wit` field when the package's contract declares `wit` ([01 §1.6.3](01-package-contract.md)). This is REQUIRED for artifact and hybrid packages and optional for source packages.
2. The WIT checksum is the tree hash (§3.4) whose input is the WIT directory in the locked commit. Paths are relative to the WIT directory. The rules of §3.4.2 apply except rule 6, which concerns only the package directory.
3. The WIT files are also covered by the source checksum, because the WIT directory lies inside the package directory ([01 §1.6.1](01-package-contract.md) rule 4). The separate checksum identifies the **interface** independently of the implementation: two versions with equal WIT checksums have byte-identical interface files. The review shows whether the interface of an updated package changed ([04 §4.7.1](04-distribution-and-security.md)), and an artifact is verified against the interface it claims ([04 §4.5.2](04-distribution-and-security.md)).
4. The WIT checksum is verified together with the source checksum, whenever content is fetched (§3.7.3).

## 3.6 Artifact checksums and imports

1. An entry has one `artifact <target> "sai1:<hex>"` field per target in the package's `artifact component` declaration ([01 §1.7](01-package-contract.md)), sorted by target name.
2. The artifact checksum is SHA-256 of the bytes of the artifact file, written as `sai1:` followed by 64 lowercase hexadecimal digits (N-G-39). It is not a tree hash: an artifact is a single file. The field name determines how a `sai1:` value was computed; the prefix names the hash function and the encoding, which are the same for both.
3. The artifact file lies in the package directory, so its bytes are covered by the source checksum as well. The separate checksum lets a build that uses the artifact form verify exactly the bytes it links ([04 §4.5.2](04-distribution-and-security.md) step 1), and lets the review show which artifact changed.
4. The artifact file MUST be a regular file selected by §3.4.2. A path that names a symbolic link or a submodule is `ARTIFACT_NOT_FOUND` ([01 §1.7](01-package-contract.md)). A Git LFS pointer is not a component and fails artifact verification (`INVALID_ARTIFACT`, [04 §4.5.2](04-distribution-and-security.md)).
5. For every artifact, the entry has an `imports <target> { .. }` field: the capabilities that the artifact imports, which is the artifact's footprint ([spec/08 §8.12.8](../spec/08-effects-and-capabilities.md)). An artifact package has no `capabilities` field; a hybrid package has both (§3.3.3).

---

## 3.7 Lifecycle

### 3.7.1 Creation

1. Every command that resolves ([02 §2.10.4](02-resolver.md)) creates `sai.lock` when it does not exist, unless `--locked` or `--frozen` is given. Under those flags a missing lock is the error `LOCK_MISSING`.
2. A package without dependencies still gets a lock, consisting of the header and the root block (N-G-46). Commands under `--locked` therefore behave the same for every package.
3. When the lock is created, there is no previous approval: the command prints a summary of all footprints ([02 §2.12.3](02-resolver.md) rule 6).

### 3.7.2 Writing

1. The content to be written is the resolution result of [02 §2.10.4](02-resolver.md), serialized canonically (§3.2.3, §3.3.4).
2. The file is written only if the serialized bytes differ from its current bytes. Content is compared **semantically**: two locks are equal when their canonical serializations are byte-identical (N-G-40). Thus a lock that is valid but not canonical (`LOCK_NOT_CANONICAL`) is equal to its canonical form; it is rewritten in canonical form by any writing command, and it is left untouched under `--locked` and `--frozen`.
3. The write is **atomic**:
   1. write the new bytes to a new file in the same directory, with a name that starts with `.sai.lock.` and ends with a random suffix;
   2. flush the file to stable storage;
   3. rename it over `sai.lock`;
   4. flush the directory to stable storage where the host supports it.
   An interruption at any point leaves either the old or the new `sai.lock`, never a partial file. A leftover temporary file is removed by the next writing command.
4. Commands that change both `sai.package` and `sai.lock` (`sai add`, `sai remove`, structured fixes) write `sai.package` first, then `sai.lock`, each atomically. An interruption between the two writes leaves a lock that is merely out of date; the next resolution repairs it in Mode B ([02 §2.10.3](02-resolver.md)).
5. Toolchain processes in the same package directory are serialized by the workspace lock `.sai/lock` of [impl/13 §13.5.4](../impl/13-cli.md) (D-TOOL-03): a command that writes `sai.lock` holds it exclusively. Because writes are atomic, a reader never observes a partial lock file. The global package cache has its own lock ([04 §4.4.5](04-distribution-and-security.md)).

### 3.7.3 Verification

Content is verified against the lock before it is used (N-G-45):

1. **On fetch.** After a commit has been fetched ([04 §4.3.2](04-distribution-and-security.md)), the source checksum is computed from its Git objects (§3.4), then the WIT checksum (§3.5) and the artifact checksums (§3.6). Each is compared with the entry. Only content that matches is inserted into the cache.
2. **In the cache.** A cache directory is named by its checksum and is read-only ([04 §4.4](04-distribution-and-security.md)). A build uses a cached entry without hashing it again; `sai fetch` re-hashes the cached content of every locked entry ([04 §4.4.2](04-distribution-and-security.md)). A local attacker who can modify the cache can also modify the toolchain, so re-hashing on every build adds cost without protection.
3. **Tags.** When a release tag is fetched for a locked version, it MUST point to the locked commit ([04 §4.3.3](04-distribution-and-security.md), `TAG_MOVED`).

A mismatch in rule 1 is the error `CHECKSUM_MISMATCH`. Its message names the kind of checksum (`source`, `wit`, or `artifact <target>`), the identity and version, the URL, the commit, and the expected and actual digests. The fetched content is discarded.

1. The lock is **never** updated to match fetched content, under any flag or command. Draft §73: content that does not match the lock is rejected.
2. Because the commit is fixed, a mismatch means that the lock was edited, or that the Git objects for that commit differ from those seen when the lock was written: a corrupted or malicious server, or a hash collision attack on a SHA-1 repository. The help says so and recommends investigating before anything else.
3. If the lock itself is wrong (for example after a bad manual edit or merge), the fix is to restore it from version control. Deleting `sai.lock` and running `sai lock` also recovers, with the effects of [02 §2.10.4](02-resolver.md) rule 3: versions return to the minimal selection, and every footprint is shown again.

---

## 3.8 `--locked`, `--frozen`, and `--offline`

The flags are accepted by every command that resolves. Their spelling and their availability on each command are owned by [impl/13](../impl/13-cli.md); their meaning is defined here (N-G-41).

### 3.8.1 `--locked`

1. The lock MUST exist (`LOCK_MISSING`) and MUST be usable in Mode A ([02 §2.10.2](02-resolver.md)). If a condition of Mode A fails, the command fails with `LOCK_OUT_OF_DATE` without running selection and without listing or fetching any tag. The message names the failed condition and the entries concerned (for example "root requirement `acme:json @ ^1.7` differs from the locked `^1.6`").
2. This is equivalent to "the lock would change": if any Mode A condition fails, a Mode B resolution changes at least one block of the lock ([02 §2.10.3](02-resolver.md)).
3. The network is still used to fetch locked commits that are not in the cache. Their content is verified as usual (§3.7.3).
4. `sai lock`, `sai update`, `sai add`, and `sai remove` exist to change the lock; under `--locked` they fail with `LOCK_OUT_OF_DATE` as soon as a change would be written. `sai lock --locked` therefore checks that the lock is up to date and writes nothing ([impl/13 §13.4.3](../impl/13-cli.md)).
5. Nothing is ever written to `sai.lock` under `--locked`. A non-canonical lock is reported with `LOCK_NOT_CANONICAL` and used as is.
6. Continuous integration SHOULD build with `--locked`, so that a pull request that changes requirements without updating the lock fails.

### 3.8.2 `--offline`

1. No network access is made. Content and tag information come only from the cache, including the toolchain's bundled packages ([04 §4.4.3](04-distribution-and-security.md)).
2. In Mode A, every active node ([02 §2.5.3](02-resolver.md)) MUST be in the cache. A node that is not is `OFFLINE_FETCH_REQUIRED`, naming the node, its URL, and its commit.
3. In Mode B, selection needs the commit of the release tag of every named version. It is taken from the cache's record of previously fetched tags ([04 §4.4.1](04-distribution-and-security.md)) or from the bundled index. If a needed tag or its content is not available, the command fails with `OFFLINE_FETCH_REQUIRED`. It never selects a different version because of what happens to be cached (P8): offline resolution either produces the same result as an online one or fails.
4. Listing tags is impossible offline, so `sai update` and `sai add` fail with `OFFLINE_FETCH_REQUIRED`, except `sai update id@w` when everything needed is cached ([02 §2.10.5](02-resolver.md) rule 7).
5. `--offline` does not prevent writing the lock.

### 3.8.3 `--frozen`

`--frozen` is `--locked` together with `--offline`: the lock MUST NOT change and no network access is made. A build under `--frozen` uses exactly the locked content from the cache, or fails.

### 3.8.4 Summary

| Situation | No flag | `--locked` | `--offline` | `--frozen` |
|---|---|---|---|---|
| no `sai.lock` | resolve, create | `LOCK_MISSING` | resolve from cache, create | `LOCK_MISSING` |
| Mode A holds | use the lock | use the lock | use the lock | use the lock |
| Mode A fails | Mode B, write | `LOCK_OUT_OF_DATE` | Mode B from cache, write | `LOCK_OUT_OF_DATE` |
| lock not canonical | rewrite | warning only | rewrite | warning only |
| locked content not cached | fetch | fetch | `OFFLINE_FETCH_REQUIRED` | `OFFLINE_FETCH_REQUIRED` |
| capability escalation | approval ([02 §2.12.3](02-resolver.md)) | not reached | approval | not reached |
| merge conflict markers | `LOCK_MERGE_CONFLICT` (§3.9) | `LOCK_MERGE_CONFLICT` | `LOCK_MERGE_CONFLICT` | `LOCK_MERGE_CONFLICT` |

A build in Mode A never needs approval, so `--locked` and `--frozen` builds never stop for review.

---

## 3.9 Merging

The layout is designed so that most concurrent changes merge without conflicts: every node has its own block, blocks are sorted, and nothing global (counters, hashes of the whole file, timestamps) changes on every write. Two branches that add different dependencies usually touch different blocks and merge cleanly. A clean textual merge can still produce a lock that does not match the merged `sai.package`; the next resolution detects this (Mode A fails) and repairs it in Mode B, and `--locked` reports it as `LOCK_OUT_OF_DATE`. The supported workflow after any merge is: merge `sai.package`, then run `sai lock` (N-G-42).

### 3.9.1 Conflict markers

1. Before tokenizing, the reader scans for **conflict marker lines**: a line that starts with seven `<`, seven `|`, or seven `>` characters followed by a space or the end of the line, or a line that consists of exactly seven `=` characters.
2. A lock that contains one is the error `LOCK_MERGE_CONFLICT` for every command except `sai lock`. The diagnostic points at the first marker and suggests `sai lock`.

### 3.9.2 Resolution by `sai lock`

`sai lock` resolves a lock with conflict markers as follows:

1. **Reconstruct the sides.** *Ours* is the text outside the conflict regions plus, for each region, the lines between the `<<<<<<<` line and the next `|||||||` or `=======` line. *Theirs* is the text outside the regions plus the lines between the `=======` line and the `>>>>>>>` line. A base section (between `|||||||` and `=======`, produced by the `diff3` style) is ignored. Nested or unbalanced markers are `LOCK_CORRUPT`.
2. **Parse both sides** (§3.3). If the parser can read either side's format version and it is greater than the highest format the toolchain supports, the command fails with `UNSUPPORTED_LOCK_VERSION` (§3.10.2), before syntax or semantic validation. Otherwise, if either side is not a valid lock, the command fails with `LOCK_CORRUPT` and suggests deleting `sai.lock` and running `sai lock`.
3. **Floors.** For every node `(id, line)` that is not pinned in either side, the floor is the higher of the two versions ([02 §2.10.3](02-resolver.md) rule 1).
4. **Known content.** A version whose entry appears on both sides with the same source MUST have the same commit and checksums on both. Otherwise the command fails with `LOCK_MERGE_CONFLICT`, naming the version and both commits: the two branches locked different content for one version, which is a tag that moved between the two fetches ([04 §4.3.3](04-distribution-and-security.md)). The user decides which content to keep by taking one side of the file, then runs `sai lock` again.
5. **Resolve.** The current root contract is resolved in Mode B with these floors. Known commits and checksums from either side are reused as in [02 §2.10.3](02-resolver.md) rule 3.
6. **Approval baseline.** For the capability review ([02 §2.12.3](02-resolver.md)), the previous footprint of a node is the union of its footprints on both sides, and a source is approved if either side records it. Both branches were reviewed when their locks were written.
7. **Write** the result (§3.7.2).

The result does not depend on which side is *ours*: floors are maxima and known content is equal on both sides, so the procedure is symmetric.

### 3.9.3 Duplicate entries after a clean merge

A clean textual merge can leave two entries for one node, for example when both branches added the same new dependency at different versions and other entries separate the two blocks. Such a lock is `LOCK_CORRUPT` (§3.3.5 rule 5) for every command except `sai lock`, which treats the duplicate entries as the two sides of a conflict and applies §3.9.2 rules 3 to 7. The format-version rule in §3.9.2 rule 2 applies before syntax, semantic, and duplicate-count checks. For each duplicated node, the first entry in file order is assigned to *ours* and the second to *theirs*; entries for nodes that are not duplicated are present on both sides. This ordering makes side-specific diagnostics deterministic and does not affect the symmetric resolution result. More than two entries for one node are `LOCK_CORRUPT` because they cannot be reconstructed as exactly two sides without guessing.

---

## 3.10 Format versioning

1. The header `lock <n>` names the format version. `<n>` is an `INT` from [01 §1.2.3](01-package-contract.md) and must fit in an unsigned 64-bit integer; an out-of-range value is lexically invalid. This document defines **format 1**.
2. A lock whose format is greater than the highest format the toolchain supports is the error `UNSUPPORTED_LOCK_VERSION`; the help suggests upgrading the toolchain. A format of 0 is `LOCK_CORRUPT`.
3. Within a format, the set of fields, the canonical layout, and the checksum algorithms are fixed. There is no rule that unknown fields are ignored: an unknown field could carry security-relevant data, such as a new checksum, which an older toolchain would silently drop when it rewrites the file. Any additional field therefore requires a new format.
4. The checksum prefix names the algorithm. Format 1 defines only `sai1:`. A new algorithm gets a new prefix (for example `sai2:`), introduced by a new format.
5. A toolchain that supports several formats keeps the format of an existing lock, and writes new locks in the highest format it supports. It never lowers the format of an existing lock. Raising it is a write (§3.7.2) and does not happen under `--locked` or `--frozen`.
6. The format version is independent of the toolchain version and of the `compiler` requirements of the packages (N-G-43).

---

## 3.11 The draft example, corrected

Draft §33 names the file with two embedded U+200B characters; the name is `sai.lock` (R-11). It gives no concrete syntax, only a conceptual list of contents. Each item maps to a field of format 1:

| Draft §33 item | Field | Notes |
|---|---|---|
| package | `package <id>` | first line of an entry |
| version | `@ <version>` | the selected version |
| source URL | `source git "<url>"`, `dir` | canonical URL; `dir` for packages in a subdirectory; `source path` for local packages |
| commit | `commit` | the commit of the release tag at the time of locking |
| source checksum | `checksum` | tree hash of the package directory (§3.4) |
| Wasm artifact checksum | `artifact <target>` | one per artifact (§3.6) |
| WIT/interface checksum | `wit` | tree hash of the WIT directory (§3.5) |
| resolved capabilities | `capabilities`, `imports <target>` | footprints ([spec/08 §8.12.8](../spec/08-effects-and-capabilities.md)) |
| compiler compatibility | `compiler` | the package's `compiler` requirement |
| (not listed) | root block, `requires .. -> <version>` | the edges, which draft §33's goal of reproducing the same dependency graph from the lock requires |

The following lock belongs to the complete example of [01 §1.19](01-package-contract.md), `acme:photo-service`, after a first `sai lock`:

```sai-lock
lock 1

root acme:photo-service @ 1.3.0
    targets { wasm32-core, wasm32-wasi }
    requires acme:exif @ >=0.7.1, <0.8 -> 0.7.1
    requires test acme:fixtures @ ^0.2 -> 0.2.0
    requires acme:image @ ^1.2 -> 1.2.0
    requires acme:photo-model @ ^1.0 -> 1.0.2
    requires sai:collections @ ^0.3 -> 0.3.1
    requires sai:http @ ^1.4 -> 1.4.0
    requires sai:wasi @ ^0.2 -> 0.2.1 only { wasm32-wasi }

package acme:exif @ 0.7.1
    source git "https://github.com/acme/exif"
    commit "f4d658773ce22592e6fe1f0897c15929524de092"
    checksum "sai1:ca1e163fc57eebd27d83be27c071d468a160243e34a9b04bfbad7390aab160e2"
    compiler ^0.1
    capabilities { }

package acme:fixtures @ 0.2.0
    source git "https://github.com/acme/fixtures"
    commit "34d01e95e8543e5076e42a6babd81aefa2ed5e90"
    checksum "sai1:f5449a083d93c696ad75e4ac9fec32b07aaad3ea5d27dca99f5ee07a3b0eb405"
    compiler ^0.1
    capabilities { alloc }

package acme:image @ 1.2.0
    source git "https://github.com/acme/media"
    dir "image"
    commit "d9c4883e750754ef26d2d6be5b8f5f9a76c0269e"
    checksum "sai1:bc4ddd7c0bb2d717c5ef6b2dcd86020d1eda8e5b31483d67a6b28c33237d6266"
    wit "sai1:a925ba9e542ad927c6b3d3d154f6d6ca9ce3403e166e21d153db6bfdcec42fa5"
    artifact wasm32-wasi "sai1:9cee7fb967c2d93db4eab23f626cb419fa73de8830db7910ba3380a08aa1fcd1"
    compiler ^0.1
    capabilities { alloc }
    imports wasm32-wasi { }

package acme:photo-model @ 1.0.2
    source path "../photo-model"
    compiler ^0.1
    requires sai:collections @ ^0.3 -> 0.3.1

package sai:collections @ 0.3.1
    source git "https://github.com/sai-lang/collections"
    commit "19d5e3b4dbadc1d7fd2ed554c0b2b20866e9287e"
    checksum "sai1:ce9a6cd353135f3c540805220c467df2cad144bcb1ebf9142829a365b3589f65"
    compiler ^0.1
    capabilities { alloc }

package sai:http @ 1.4.0
    source git "https://github.com/acme/http-fork"
    commit "6f52f4877c512efbd545768676d8548f6c8732a4"
    checksum "sai1:cef5219b5c1c24bf99cce8fd65e0fbfdaaed681348a4e5696a67a320a7e5820a"
    compiler ^0.1
    capabilities { alloc, wasi:http }
    requires sai:collections @ ^0.3.1 -> 0.3.1
    requires sai:wasi @ ^0.2.1 -> 0.2.1 only { wasm32-wasi }

package sai:wasi @ 0.2.1
    source git "https://github.com/sai-lang/wasi"
    commit "d98153863424a27ea6c89d1a8a4cc3b9476f8326"
    checksum "sai1:2f3ac1982c56204302a59b9710a8d2c8dda6d0ff37ec70ae8f55d61009f5e985"
    compiler ^0.1
    capabilities { wasi:cli, wasi:clocks, wasi:filesystem, wasi:http, wasi:io }
```

Notes:

- The root requires `sai:collections @ ^0.3`, whose minimum is `0.3.0`, but `sai:http` requires `^0.3.1`. MVS selects the maximum of the minimums, `0.3.1`, and every edge to the node records `-> 0.3.1` ([02 §2.4](02-resolver.md)). `sai:wasi` is raised to `0.2.1` the same way.
- `sai:http` comes from the fork named by the root's override ([02 §2.8.1](02-resolver.md) rule 1). The fork must carry the release tag `v1.4.0`.
- `acme:image` lives in the monorepo `acme/media`; its release tag is `image/v1.2.0` ([02 §2.6.1](02-resolver.md)). It is a hybrid package: the entry records its source footprint and, for its single artifact, the artifact's imports. The root's `acme:image use artifact` override does not appear in the lock; it selects the form per build ([04 §4.5.1](04-distribution-and-security.md)).
- `acme:photo-model` is a local path package: no commit, checksum, or footprint (§3.3.3 rule 2). Its version `1.0.2` is the one declared in `../photo-model/sai.package`.
- `acme:exif` has an empty footprint, which the root's `grant {}` covers ([01 §1.10.8](01-package-contract.md)).
- The footprint of `sai:wasi` contains `wasi:filesystem`, which the root denies. This is a review item, not an error: only build time rejects a program, and only if code that needs `wasi:filesystem` is reachable ([spec/08 §8.12.8](../spec/08-effects-and-capabilities.md), [02 §2.12](02-resolver.md)).
- `sai:wasi` is resolved and locked although the `wasm32-core` build never activates it: resolution covers all of the root's targets, and `only` restricts activation ([02 §2.5.2](02-resolver.md), [02 §2.5.3](02-resolver.md)).
- The commits and digests are illustrative.

---

## 3.12 Decisions introduced by this chapter

These decisions are provisional, pending their entry in [decisions.md](../design/decisions.md). They refine D-PKG-02 and D-PKG-03 and re-decide neither.

| ID | Rule | Rationale |
|---|---|---|
| N-G-36 | `sai.lock` has exactly one canonical byte form: LF only, `lock 1` header, sorted blocks separated by one empty line, four-space field indentation, single spaces, one-line lists, `\"` and `\\` as the only escapes, no comments, timestamps, or toolchain version (§3.2.3). | P8: identical resolutions give identical bytes on every host; P9: diffs show only real changes. |
| N-G-37 | The `sai1:` tree hash is computed from the Git tree of the locked commit; it selects regular files only (both file modes hashed alike), skips symbolic links, submodules, `.git` components, and top-level `.sai` and `out`, applies no Git attributes or LFS, validates every path, rejects case- and normalization-insensitive collisions, and sorts by path bytes (§3.4). | P4: the same digest and the same materialized tree on every host; P8: host checkout settings cannot change the result; P9: one fully specified algorithm with a test vector. |
| N-G-38 | Digests are written as `sai1:` followed by 64 lowercase hexadecimal digits; no other encoding is valid (§3.4.6). D-PKG-03 fixes the prefix and the hash but not the encoding. | P8: one spelling per digest; P9: matches the output of common tools. |
| N-G-39 | An artifact checksum is SHA-256 of the artifact file's bytes with the same `sai1:` encoding; the WIT checksum is the tree hash of the WIT directory (§3.5, §3.6). | P3: interface and artifact changes are visible separately from implementation changes; P9: one hash function throughout. |
| N-G-40 | Locks are compared semantically, through their canonical serialization. A valid but non-canonical lock is a warning, is used as is, and is rewritten only by a writing command outside `--locked` (§3.7.2). | P8: formatting accidents never change a build or fail CI; P9: no special repair command is needed. |
| N-G-41 | `--locked` requires Mode A and fails without fetching tags; `--offline` uses only the cache and the bundled index and never selects differently because of what is cached; `--frozen` is both (§3.8). | P2, P8: CI detects stale locks deterministically; offline results equal online results or fail. |
| N-G-42 | After a merge, `sai lock` reconstructs both sides of a conflicted lock, takes per-node maximum floors, rejects differing content for one version, uses the union of approvals, and resolves the merged `sai.package` in Mode B. The procedure is symmetric (§3.9). | P8: merge results do not depend on merge direction; P5: content conflicts are never resolved silently. |
| N-G-43 | Format 1 has a fixed field set with no ignorable extensions; new fields or checksum algorithms need a new format; existing locks keep their format and are never downgraded (§3.10). | P5: an older toolchain cannot drop security-relevant data on rewrite; P8. |
| N-G-44 | The lock records the root block with the root's edges, and every entry's edges with their resolved versions. Footprints are the resolved capabilities. Path entries carry no commit, checksum, or footprint. No line, form, or toolchain field is stored (§3.3). | P1, P8: the graph can be reproduced and checked against `sai.package` without network access; P7: nothing derivable is stored. |
| N-G-45 | Content is verified when it is fetched, before it enters the cache; builds trust the read-only, checksum-named cache; a mismatch is never resolved by updating the lock (§3.7.3). | P5: unverified content never reaches a build; P8: no per-build cost; draft §73. |
| N-G-46 | Every package, application or library, SHOULD commit its lock; a lock is created even without dependencies; `sai.package` is written before `sai.lock`, each atomically, and concurrent writers are serialized (§3.1.1, §3.7). | P8: reproducible CI for libraries too; P9: a crash never leaves a partial or inconsistent pair that cannot be repaired. |

---

## 3.13 Diagnostics

Codes defined in another document are marked with that document.

| Code | Severity | Description |
|---|---|---|
| `LOCK_MISSING` | error | `--locked` or `--frozen` and there is no `sai.lock` (§3.7.1, §3.8) |
| `LOCK_OUT_OF_DATE` | error | `--locked` or `--frozen` and the lock does not satisfy Mode A or would change; names the failed condition (§3.8.1) |
| `LOCK_CORRUPT` | error | the lock does not parse or violates §3.3.5; its recorded edges do not describe the locked content ([02 §2.10.2](02-resolver.md)); a side of a merge conflict is invalid (§3.9.2) |
| `LOCK_NOT_CANONICAL` | warning | the lock is valid but not in canonical form (§3.2.3, §3.7.2) |
| `UNSUPPORTED_LOCK_VERSION` | error | the lock's format is newer than the toolchain supports (§3.10) |
| `LOCK_MERGE_CONFLICT` | error | the lock contains conflict markers, or both sides of a merge lock different content for one version (§3.9) |
| `CHECKSUM_MISMATCH` | error | fetched content does not match the source, WIT, or artifact checksum of the lock (§3.7.3) |
| `OFFLINE_FETCH_REQUIRED` | error | `--offline` or `--frozen` and needed content or tag information is not cached (§3.8.2) |
| `INVALID_PACKAGE_FILE_PATH` | error | a file path in fetched content violates §3.4.3 |
| `PATH_COLLISION` | error | two file paths differ only by case or Unicode normalization (§3.4.4) |
| `SYMLINK_IGNORED` | warning | a symbolic link in fetched content is not part of the package (§3.4.2) |
| `SUBMODULE_IGNORED` | warning | a submodule in fetched content is not part of the package (§3.4.2) |
| `INVALID_UTF8` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; lexical error (§3.2.1) |
| `INVALID_LINE_ENDING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; lexical error (§3.2.1) |
| `FORBIDDEN_CHARACTER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; lexical error (§3.2.1) |
| `UNTERMINATED_STRING` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.4; lexical error (§3.2.1) |
| `INVALID_ESCAPE` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.6.3; lexical error (§3.2.1) |
| `ARTIFACT_NOT_FOUND` | error | defined in [01](01-package-contract.md) §1.7; an artifact path that is not a regular file of the package (§3.6) |
| `INVALID_ARTIFACT` | error | defined in [04](04-distribution-and-security.md) §4.5.2; an artifact file is not a valid component (§3.6) |
| `TAG_MOVED` | error | defined in [04](04-distribution-and-security.md) §4.3.3; a release tag points to another commit than the lock records (§3.7.3) |
| `CAPABILITY_ESCALATION` | error | defined in [02](02-resolver.md) §2.12.3; a lock change adds capabilities to a node and was not approved (§3.8.4, §3.9.2) |
