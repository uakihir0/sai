# 11. Diagnostics

**Status:** Normative for the reference implementation (see README §2). The diagnostic codes and their stability policy (§11.2, §11.12), the span encoding (§11.5), the JSON wire format (§11.9), and the `sai explain` record (§11.13) are normative for all conforming tools (D-TOOL-01, N-H-1).
**Stage:** 0 (model, codes, human and JSON output); fixes that refer to semantic node IDs: 0; related why-chains: 3 (effects, capabilities) and 2 (borrows)
**Related:** [decisions.md](../design/decisions.md) D-TOOL-01, D-TOOL-02, D-TOOL-03; [02-lexical-structure.md](../spec/02-lexical-structure.md) §2.1; [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md); [appendix-a-diagnostic-codes.md](../spec/appendix-a-diagnostic-codes.md); [02-frontend-ast-hir.md](02-frontend-ast-hir.md); [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md); [12-semantic-tooling.md](12-semantic-tooling.md); [13-cli.md](13-cli.md); [14-formatter.md](14-formatter.md)

This chapter defines what a diagnostic is and how tools emit it. It covers the code naming rules and severities, the data model (primary span, secondary labels, notes, help, structured fixes, related information), the span encoding, the versioned JSON wire format and its JSON Schema, the human renderer, ordering, deduplication and limits, cascade suppression, the code stability policy, and the `sai explain` command. Which diagnostics exist, and when each is raised, is defined by the chapter that owns the rule. [appendix-a-diagnostic-codes.md](../spec/appendix-a-diagnostic-codes.md) collects all of them. Lint levels and their precedence are defined by [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md); this chapter defines only how the resulting level becomes an emitted severity.

---

## 11.1 Overview

### 11.1.1 Producers and the diagnostic pipeline

Every component of the toolchain reports problems through one interface, the **diagnostic sink**. Producers include the lexer, the parser, name resolution, the type checker, the borrow checker, effect and capability analysis, the Standard Fact Checker, the package resolver, the linker, the formatter, the semantic protocol, and the CLI itself.

A diagnostic travels through these stages, in this order:

1. **Creation.** A producer creates a `Diagnostic` (§11.4) with a code, a message, spans, and optional fixes and related information.
2. **Level resolution.** If the code is a lint, its effective level is computed (§11.3). A diagnostic whose level is `allow` is dropped here.
3. **Cascade suppression.** Diagnostics caused by an earlier error are dropped (§11.11.4).
4. **Deduplication** (§11.11.2).
5. **Ordering** (§11.11.1).
6. **Limiting.** The `--max-errors` limit is applied (§11.11.3).
7. **Rendering** as human text (§11.10) or as JSON records (§11.9).

Stages 2–6 are deterministic functions of the set of created diagnostics. They do not depend on the order in which producers ran, the number of threads, or caching. Two runs over the same inputs, with the same compiler version and options, MUST produce byte-identical output in both formats (determinism principle, decisions.md §0.1). The one exception is the `tool.version` field of the JSON header, which records the compiler version itself.

### 11.1.2 Normativity

- **For all conforming tools** (N-H-1): the code namespace and naming rules (§11.2), the severities (§11.3.1), the code stability policy (§11.12), the span encoding (§11.5), the JSON wire format (§11.9), and the `sai explain` JSON record (§11.13.2). Any tool that reads or writes SAI diagnostics in JSON MUST use this format, so agents and editors work the same with every implementation (P3, P4).
- **For the reference implementation only:** the human renderer layout and colors (§11.10), the exact message wording, the default limits, and the precise set of cascades suppressed (§11.11.4). Another implementation MAY word messages differently, but it MUST use the same codes for the same conditions.

---

## 11.2 Diagnostic codes

### 11.2.1 One namespace

Every diagnostic has exactly one **code**. Lint names and diagnostic codes are one namespace (D-TOOL-01): the code of a lint is the name used in `@allow(..)`, `@warn(..)`, `@deny(..)`, in the CLI lint flags ([13-cli.md](13-cli.md) §13.4.4), and in `sai explain`. No code is ever assigned two meanings (§11.12).

### 11.2.2 Naming rules

A code MUST satisfy all of the following (N-H-2):

1. It matches the regular expression `^[A-Z][A-Z0-9]*(_[A-Z][A-Z0-9]*)+$`. It is UPPER_SNAKE_CASE ASCII with at least two words, and every word starts with a letter. For example `INVALID_UTF8` is valid, while `UTF_8_INVALID` is not, because the word `8` starts with a digit.
2. It has at most 6 words and at most 40 characters.
3. It **names the problem, not the fix**: `UNKNOWN_FIELD`, not `USE_KNOWN_FIELD`. One historical exception is allowed: codes that report a construct SAI deliberately lacks start with `NO_` and name that construct (`NO_SEMICOLON`, `NO_NULL`, `NO_DOUBLE_COLON`). The one other exception, `USE_AND_OR` from [02-lexical-structure.md](../spec/02-lexical-structure.md) §2.12, is kept as is, because codes are never renamed (§11.12).
4. It uses a fixed vocabulary of leading words where one applies:

| Leading word | Meaning | Examples |
|---|---|---|
| `UNKNOWN_` | a name or key that does not resolve | `UNKNOWN_FIELD`, `UNKNOWN_ATTRIBUTE`, `UNKNOWN_ABI` |
| `MISSING_` | something required is absent | `MISSING_FN_BODY`, `MISSING_EFFECTS_DECLARATION` |
| `DUPLICATE_` | something appears twice | `DUPLICATE_BINDING`, `DUPLICATE_PACKAGE_LINE` |
| `INVALID_` | a construct is present but ill-formed for its position | `INVALID_SELF_TYPE`, `INVALID_ESCAPE` |
| `UNEXPECTED_` / `EXPECTED_` | parser-level token expectations | `UNEXPECTED_NEWLINE`, `EXPECTED_TOKEN` |
| `UNUSED_` | something is never used | `UNUSED_VARIABLE`, `UNUSED_IMPORT` |
| `UNTERMINATED_` | an unclosed lexical construct | `UNTERMINATED_STRING` |
| `NO_` | a construct SAI deliberately does not have | `NO_NULL`, `NO_EXCEPTIONS` |

5. A lint code reads naturally after `@allow(`: it names the flagged thing or condition (`UNUSED_VARIABLE`, `AMBIENT_IMPORT`, `CONTRACT_RUNTIME_CHECK`). The leading word never decides the class of a code: the registry of [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.17.2 decides which codes are lints (N-A-49). For example, `UNUSED_MUST_USE` is a hard error and `UNUSED_OVERRIDE` is a hard warning.
6. Abbreviations are allowed only from this closed list: `ABI`, `AST`, `CST`, `DCE`, `FFI`, `FN`, `HIR`, `ID`, `IO`, `LSP`, `MIR`, `NFC`, `SFC`, `UTF8`, `UTF16`, `WASI`, `WASM`, `WIT`. Adding to the list requires a revision of this chapter.
7. Codes carry no numeric suffixes, no phase prefixes (`E0`, `PARSE_`), and no package prefixes. A code describes the problem the user faces, not the compiler phase that found it (P8).

The prefixes `INTERNAL_` and `SAI_` are reserved for the toolchain. Only `INTERNAL_COMPILER_ERROR` (§11.4.6) uses them in v0.1.

### 11.2.3 Code classes

Every code belongs to exactly one class:

- **Hard diagnostic.** It has a fixed severity that no attribute, policy, or flag can change. Every rule of the specification that says "is an error" produces a hard error. Some hard diagnostics are warnings or info, for example `MISPLACED_DOC_COMMENT` in [02-lexical-structure.md](../spec/02-lexical-structure.md) §2.12, unless the owning document declares them lints.
- **Lint.** It has a **default level**, and its effective level is computed per occurrence by the rules of [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md). A lint never reports a condition that makes a program ill-formed. With a level of `allow`, the program still compiles.

The owning document states each code's class. In the code tables of the spec chapters, a severity written `warning (lint)` or `info (lint)` marks a lint whose default level is `warn` or `info`. A bare `error`, `warning`, or `info` marks a hard diagnostic.

### 11.2.4 Registry entries

The diagnostic registry is the list of all codes. It is published as [appendix-a-diagnostic-codes.md](../spec/appendix-a-diagnostic-codes.md) and compiled into the toolchain for `sai explain`. Each entry has these fields:

| Field | Content |
|---|---|
| `code` | the code (§11.2.2) |
| `class` | `hard` or `lint` |
| `severity` | for hard diagnostics: `error`, `warning`, or `info` |
| `default_level` | for lints: `allow`, `info`, `warn`, or `deny` |
| `stage` | the stage that introduces the condition (README §4) |
| `owner` | the document that defines when the code is raised |
| `since` | the toolchain version that first assigned the code (`0.1.0` for all v0.1 codes) |
| `status` | `active`, `deprecated`, or `removed` (§11.12) |
| `superseded_by` | for deprecated and removed codes: the list of replacement codes, possibly empty |
| `summary` | one line, at most 80 characters, no trailing period |
| `data_schema` | the JSON Schema of the code's `data` object (§11.4.5), or `null` |
| `explanation` | the text of `sai explain` (§11.13) |

**Current implementation slice.** `sai_diagnostics` compiles a sorted snapshot of all 492 v0.1 code names from Appendix A into the non-exhaustive `Code` enum and exposes `Code::ALL`, `Code::from_name`, and `Code::as_str` for the complete registry. The enum preserves the pre-existing variant order first, keeping derived `Ord` results stable; newly added codes follow the snapshot's name order. It also exposes `lookup_lint_name`, which returns a lint entry with its default level and stage, a hard diagnostic with its fixed severity, or unknown. The 28-entry `LINT_REGISTRY` follows spec/15 §15.17.2; tests compare the checked-in code-name snapshot, hard warning/info classifications, and lint metadata against the workspace specifications. The enum is generated from the checked-in name snapshot, and the build rejects malformed, unsorted, duplicate, or colliding names and Rust variants. The full `sai explain` metadata record, registry lifecycle, and callers for package `lints { }` and CLI flags remain unimplemented.

---

## 11.3 Severities and lint levels

### 11.3.1 Severities

An emitted diagnostic has exactly one of three severities (N-H-3):

| Severity | Rank | Effect on the command |
|---|---|---|
| `error` | 0 | The command fails: no artifact is written, and the exit code is 1 ([13-cli.md](13-cli.md) §13.3.4). |
| `warning` | 1 | None. The command still succeeds. |
| `info` | 2 | None. It reports a fact the user may want to know, for example an obligation that became a runtime check (`CONTRACT_RUNTIME_CHECK`). |

The rank is used for ordering only (§11.11.1). `note` and `help` are not severities. They are kinds of child message inside a diagnostic (§11.6). There is no `fatal` severity: an unrecoverable condition is an `error` after which the command stops. An internal compiler error is the hard error `INTERNAL_COMPILER_ERROR` (§11.4.6).

### 11.3.2 Lint levels

A lint occurrence has an **effective level** from the totally ordered set `allow < info < warn < deny`. The level sources and their precedence (attributes such as `@allow(CODE)`, `@warn(CODE)`, `@deny(CODE)`, the `lints { }` block of `sai.package` ([package/01-package-contract.md](../package/01-package-contract.md) §1.13), package `policy` entries such as `deny runtime_contracts` (§1.12), and the CLI flags of [13-cli.md](13-cli.md) §13.4.4) are defined by [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md). This chapter relies on two properties of that definition: (a) for every occurrence it yields exactly one level, and (b) the result depends only on the source, `sai.package`, and the command line.

`info` is a valid default level, but no attribute or flag sets a lint to `info`. A user can only raise an info-default lint (`@warn`, `@deny`) or silence it (`@allow`).

### 11.3.3 From level to severity

After the effective level is resolved, a lint occurrence becomes an emitted diagnostic as follows:

| Effective level | Emitted? | Severity |
|---|---|---|
| `allow` | no (dropped at pipeline stage 2) | — |
| `info` | yes | `info` |
| `warn` | yes | `warning` |
| `deny` | yes | `error` |

Two post-processing rules then apply, in this order:

1. **Dependency cap** (N-H-4). A lint occurrence whose primary span lies in a source file of a package other than the root package (or the workspace members, [13-cli.md](13-cli.md) §13.2.2) is dropped, whatever its level. Hard diagnostics in dependencies are never dropped. A new compiler version that adds a lint, or changes a lint's default, therefore cannot break the build of a package that depends on unchanged code (P6, P8). The cap does not apply to a lint occurrence whose level source is `policy` (§11.3.4): a `policy { deny .. }` item of the package that contains the occurrence is a promise that package makes about itself, and it is enforced when the package is compiled as a dependency ([package/01-package-contract.md](../package/01-package-contract.md) §1.12, rule 3). The cap is consistent with the root-only `lints { }` block (§1.13, rule 4, N-G-14).
2. **`--deny-warnings`** ([13-cli.md](13-cli.md) §13.4.4). Every remaining diagnostic of severity `warning` becomes an `error`, whether it is a lint or a hard warning. Info diagnostics are not affected.

### 11.3.4 Recording the level decision

Every emitted lint diagnostic records how its severity was reached in the field `level` (§11.9.4):

- `level.value`: the effective level (`info`, `warn`, or `deny`).
- `level.source`: `default`, `attribute`, `manifest` (the `lints { }` block), `policy`, `cli`, or `deny_warnings`. `deny_warnings` is used when the severity was changed by `--deny-warnings`.
- `level.location`: the location of the attribute, of the `lints` entry, or of the `policy` entry that set the level, or `null` for `default`, `cli`, and `deny_warnings`.

The human renderer prints a note for every source other than `default`, for example `= note: `UNUSED_VARIABLE` is denied by `@deny(UNUSED_VARIABLE)` at src/lib.sai:3:1`. Hard diagnostics have `level: null`.

**Current implementation slice.** `sai_diagnostics::resolve_lint_occurrence` resolves a registered lint from prevalidated CLI and manifest overrides, source-ordered attribute scopes, the occurrence package's local policy, and the root package's graph-wide `deny duplicate_lines` policy. `sai_package` extracts package policies and root `lints { }` entries from VFS-bound manifests, deriving their locations from the same snapshot. The MVS duplicate-line diagnostic applies those root settings automatically, while leaving CLI settings and `--deny-warnings` as explicit inputs. The resolver selects the innermost applicable attribute by the smallest containing syntax range in the same file, applies policy > attribute > CLI > manifest > default, suppresses `allow`, applies the dependency cap, and then applies `--deny-warnings`. Its result contains the emitted severity and `LevelInfo`, or reports that the code is hard or unknown. `apply_deny_warnings` also handles hard warnings; callers leave their `level` field null. `LevelInfo.value` remains the resolved lint level when `--deny-warnings` raises a warning, while its source becomes `deny_warnings` and its location becomes null.

The API deliberately accepts resolved package membership and CLI settings. `sai_driver::RootDatabase::mvs_graph_diagnostics` binds the root manifest and every MVS-visited dependency manifest to one workspace snapshot, validates the root's session file key, and checks dependency candidate IDs against distinct snapshot entries with the exact package version and `dep:` source path. It collects the source-bound conflict, cycle, and duplicate-line diagnostics and accepts the duplicate-line CLI level and `--deny-warnings` decision from its caller. `sai_package::resolver_contract` converts syntax-valid and semantically valid manifest parses into MVS contracts while retaining source, `only`, `grant`, test-edge, and span metadata for later graph and lock processing. Requirement metadata is exposed read-only so callers cannot change source clauses independently from the validated manifest; the adapter can verify the remaining MVS edge metadata. It binds target filtering and root overrides to the exact root parse and accepts concrete path-package versions from its caller; it does not access the filesystem or fetch packages. `compare_resolver_contract_edges` compares the target-included dependency edges after root overrides, using canonical Git URLs and retaining path provenance. Its `compare_with_resolved_paths` callback lets the caller resolve each path against the expected or actual requiring package's source tree before declaring equivalence; it collects every lookup failure with its side and edge provenance. `sai_package::resolver_source` chooses the source for each MVS contract request using root overrides, path pins, the least requirer among named edges, and a lock source for floor-only requests. Each selection retains the winning override span, path-pin edge, least-requirer edge, or exact lock-floor version for later diagnostics, and returns every path-pin source so the loader can normalize and compare paths, including local symlinks. `validate_resolver_path_pins` compares caller-produced keys for the complete resolved source, reports both pin provenances and keys on conflict, and associates any canonicalization error with its pin. Local keys must identify joined paths after lexical and symlink resolution; Git keys must include the canonical repository URL, requiring package commit, and normalized tree path. The loader must reject Git paths that escape the repository. `sai_package::resolver_source_provider` connects that selector to dequeue-time MVS loading, requires a source canonicalizer, validates the path pins known at dequeue before invoking the loader, then validates the complete path-pin set from every discovered named edge after the queue drains. This final pass catches pins added later for an already-read version; if loading failed, it validates the accumulated edges from successfully read contracts only. It validates returned dependency-role contracts against their requested versions and root context and retains successfully read contracts and their selected sources when another branch fails. Its `plan_resolver_final_sources` API validates retained MVS contracts and read-time sources, recomputes each read choice in dequeue order using the earlier read contracts and same lock-source map, then records read-time and final source choices only for selected graph dependencies. It uses the fully accumulated named edges and retains final source-selection failures per node. Range conflicts and dependency-to-dependency cycles remain in the graph; root-cycle findings are retained separately while the resolved branches are still planned. `plan_resolver_source_reloads` compares these choices using source identities captured by the loader when the sources were actually read, rather than resolving read-time selections again from a potentially changed filesystem. The identity-aware loader API returns each dependency contract paired with the complete identity captured from the tree it read, and pairs the root contract with its own identity. It retains identities for unselected candidates whose contracts contributed edges. A final-source plan is bound to that identified load; identities from another load cannot be substituted even when its contract and source values are equal. Path-base provenance determines the identity source: root overrides and lock-file path entries use the captured root identity (lock paths are relative to the root directory under package/03 §3.3.3), selected-dependency-relative sources use that dependency's final identity after resolution, and unselected dependency bases use their captured read-time identity. Missing read identities, final-source selection failures, unavailable selected bases, source-identity cycles, and final canonicalization failures are retained per node instead of falling back to a possibly stale tree. The API returns separate same-source and reload sets. These APIs perform no filesystem or network access. `audit_resolver_reloaded_contracts` compares every identity-planned reload using the exact read-time contract retained in the final-source plan and the caller's reloaded contract and final context. It validates that identity results match the source plan and cover the selected graph, rejects missing or extra reload output, and preserves comparison failures and source provenance. Its path resolver receives the captured read identity for Expected, the selected base's final identity for Actual, and the captured identity for root or unselected bases; it attempts every path lookup even when another edge differs or lookup fails. `is_complete` requires no identity-plan failures, audit failures, or path lookup failures; `is_equivalent` additionally requires no edge or resolved-path difference. `sai_driver::RootDatabase::mixed_source_divergent_diagnostics` turns divergent comparisons into source-bound diagnostics after checking load identity and matching snapshot manifest parses to the contracts retained by that load. `mixed_source_identity_diagnostics` reports classified final identity-canonicalization failures from the same bound load with the exact manifest snapshot and requirement chain. Internal and unclassified identity failures and final reload failures remain on the audit plan for the package workflow to handle. CLI parsing, construction of full attribute syntax ranges from HIR owners, general diagnostic-pipeline integration, the `lint_levels(OwnerId)` query, human/JSON review formatting, and lock generation remain follow-up work because this workspace has no concrete package loader or command-line crate.

---

## 11.4 Data model

### 11.4.1 The `Diagnostic` structure

The reference implementation represents a diagnostic as follows (Rust notation, D-TOOL-05). The JSON form (§11.9) is a direct serialization of it.

```rust
pub struct Diagnostic {
    pub code: Code,                    // §11.2
    pub severity: Severity,            // §11.3.1: Error | Warning | Info
    pub level: Option<LevelInfo>,      // §11.3.4; None for hard diagnostics
    pub message: String,               // §11.6.1
    pub primary: Option<Location>,     // §11.4.2; None only as allowed by §11.4.3
    pub labels: Vec<Label>,            // secondary labels, §11.6.2
    pub children: Vec<Child>,          // notes and help, in order, §11.6.3
    pub fixes: Vec<Fix>,               // §11.7
    pub related: Vec<Related>,         // §11.8
    pub data: serde_json::Map<String, Value>, // code-specific payload, §11.4.5
    pub node: Option<NodeId>,          // node ID of the primary node, D-TOOL-02
    pub symbol: Option<SymbolId>,      // the symbol most directly concerned, impl/12 §12.2
    pub package: Option<PackageId>,    // e.g. "acme:user@1.0.0"; None for CLI-level diagnostics
    cause: CauseKey,                   // internal; used for cascade suppression, §11.11.4
}

pub struct Location {
    pub file: SourceRef,               // §11.5.4
    pub span: Span,                    // half-open byte range [start, end), §11.5.1
}

pub struct Label { pub location: Location, pub message: String }   // message may be empty

pub struct Child {
    pub kind: ChildKind,               // Note | Help
    pub message: String,
    pub location: Option<Location>,    // a note may point at a location without a snippet
}
```

### 11.4.2 Primary location

The **primary location** is the single byte range that best identifies the problem. The producer chooses it with these rules:

1. It is the smallest range that contains the offending construct and identifies it without ambiguity. For an unknown field in `user.username`, the range is the field name `username`, not the whole expression.
2. For a missing construct, the range is empty (`start == end`) and is placed where the construct would have to be inserted, for example directly after `)` for a missing return type. The renderer shows an empty range as a single caret (§11.10.4).
3. For a declaration-level problem, the range is the declared name, for example `create_user` in `fn create_user(...)`, not the whole item.
4. For a problem at end of input (`EXPECTED_TOKEN` at EOF), the range is the empty range at the end of the file.

### 11.4.3 Diagnostics without a location

`primary` MAY be `null` only if the condition cannot be attributed to any byte range in any file. Examples are an unknown CLI option, an unreadable directory, or a network failure while fetching a URL given on the command line. A condition that comes from `sai.package`, `sai.lock`, or a `.wit` file MUST point into that file. A version conflict, for example, points at the requirement in `sai.package` that the resolver rejected.

### 11.4.4 Node and symbol references

- `node` is the node ID (`<kind>:<hex>`, D-TOOL-02) of the innermost syntax node whose range equals the primary range. If no node has exactly that range, it is the innermost node that contains it. Node IDs are valid only for the document snapshot named in `primary.file.snapshot` (§11.5.4). Their allocation is defined in [12-semantic-tooling.md](12-semantic-tooling.md) §12.3.
- `symbol` is the symbol ID ([12-semantic-tooling.md](12-semantic-tooling.md) §12.2) of the declaration the diagnostic is about. For `UNKNOWN_FIELD` it is the type whose field was looked up, for `MISSING_EFFECTS_DECLARATION` the function, and for a borrow conflict the enclosing function. It is `null` when no declaration is concerned, as for lexical errors.

### 11.4.5 Code-specific data

`data` is a JSON object whose shape is fixed per code by the registry field `data_schema` (§11.2.4). Agents read the facts of a diagnostic from it without parsing the message text (P3). The owning chapter defines the schema. Producers MUST NOT put information in `data` that is not also stated, in some form, by the message, labels, or children. `data` is never the only carrier of a fact a human needs.

The fields of draft §60 belong under `data`:

```json
{
  "code": "UNKNOWN_FIELD",
  "message": "`User` has no field `username`",
  "data": {
    "type_name": "User",
    "field": "username",
    "candidates": ["name", "display_name"]
  }
}
```

Candidate lists in `data` (and in help messages) are ordered by increasing edit distance (the Damerau–Levenshtein distance over Unicode scalar values), with ties broken by declaration order. A list contains only names whose distance from the written name is at most `max(1, n / 3)`, where `n` is the number of scalar values in the written name (integer division). If the set of candidates (fields, variants, or items in scope) has at most 5 members, all of them are listed whatever their distance, still in distance order. In the example above, both fields of `User` are listed for this reason. A list has at most 10 entries (N-H-5). `UNKNOWN_LINT` is an exception to the relative distance threshold: spec/15 §15.15.4 admits registered lint names within distance 2, so its candidate list uses that fixed limit and still follows the ordering and length limit here.

`UNKNOWN_LINT` has this `data` shape, including when there are no candidates:

```json
{
  "name": "UNUSED_VARIABLES",
  "candidates": ["UNUSED_VARIABLE"]
}
```

`name` is the exact spelling the user wrote. `candidates` contains at most 10 registered lint codes within Damerau–Levenshtein distance 2, ordered by distance and then by their declaration order in spec/15 §15.17.2. A case-only correction is represented by the diagnostic's machine-applicable fix, not by adding a case variant to `candidates`.

### 11.4.6 Internal compiler errors

When a component of the toolchain detects a violated internal invariant or panics, the toolchain reports the hard error `INTERNAL_COMPILER_ERROR` (defined in [01-introduction.md](../spec/01-introduction.md) §1.10; this section defines only how it is reported) and stops the command with exit code 101 ([13-cli.md](13-cli.md) §13.3.4). Its message is `internal compiler error: <description>`. Its `data` object has the fields `component` (string, the crate name), `panic_message` (string), `query_stack` (array of strings, innermost first), and `compiler_version` (string). It has a `primary` location if the failing query concerned a source range, and `null` otherwise. Diagnostics produced before the failure are still emitted, in the order of §11.11.1, before the `INTERNAL_COMPILER_ERROR` record.

---

## 11.5 Spans and positions

### 11.5.1 Byte spans

A span is a half-open range `[start, end)` of byte offsets, with `0 <= start <= end <= file length` ([02-lexical-structure.md](../spec/02-lexical-structure.md) §2.1 item 6). Offsets are counted over **the bytes of the file exactly as read**, including a leading BOM and the CR of every CR LF pair (N-H-6). A text edit computed from a span can therefore be applied to the file without first normalizing it. The byte span is the canonical position. Every other representation in this section is derived from it.

Producers MUST NOT create a span boundary that falls inside a UTF-8 sequence or between the CR and the LF of a CR LF pair. In a file with invalid UTF-8 (`INVALID_UTF8`), each byte of an ill-formed sequence counts as one character for the derived positions below.

The front end works on decoded text, in which the BOM is removed and every maximal ill-formed subsequence is replaced by U+FFFD ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.2.1). Its internal `TextRange` offsets therefore differ from the offsets defined here, by 3 bytes after a BOM and by `3 - k` bytes after each replaced subsequence of `k` bytes. The diagnostic sink converts internal offsets to file offsets before rendering, using the BOM shift recorded in the line index and the list of replaced subsequences (their decoded offsets and raw lengths) that decoding records. Every replaced subsequence is itself reported as `INVALID_UTF8`.

### 11.5.2 Lines and UTF-8 columns

- **Line terminators** for position purposes are LF, CR LF taken as a single unit, and a lone CR. A lone CR is reported as `INVALID_LINE_ENDING` and still ends a line, as in the line index of [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.2.2.
- **Line** numbers are 1-based. The line of an offset is 1 plus the number of line terminators that end strictly before it.
- **Column** (`col`) is 1-based and counts UTF-8 bytes from the start of the line: `col = offset - line_start + 1`. On line 1 the line start is taken to be after the BOM, if one is present, so the first character after a BOM is at column 1. The BOM itself has no column of its own. Offset 0, the offset of the BOM, is reported as line 1, column 1, the same as the first character after it.
- A TAB counts as one byte. Display expansion of TABs is a renderer concern (§11.10.4).
- The position of a line terminator, or of the end of the file, is the column one past the last byte of the line's content. For a line `ab` followed by CR LF, the CR is at column 3.

### 11.5.3 UTF-16 columns and LSP positions

LSP clients count positions in UTF-16 code units (the LSP default `positionEncoding` `utf-16`). Every position therefore also carries `utf16_col`: 1 plus the number of UTF-16 code units of the characters between the line start and the offset. A character in U+0000..U+FFFF counts 1 and a supplementary character counts 2.

The mapping to an LSP `Position` is `{ line: line - 1, character: utf16_col - 1 }`. If the client announces `positionEncoding` `utf-8` during initialization, the server uses `{ line: line - 1, character: col - 1 }` instead ([12-semantic-tooling.md](12-semantic-tooling.md) §12.10.2).

For documents opened over LSP, positions are computed over the text the client sent. If that text starts with U+FEFF, the lexer treats it as the BOM ([02-lexical-structure.md](../spec/02-lexical-structure.md) §2.1 item 2), but for LSP positions it counts as one UTF-16 code unit at character 0, because the client counts it that way.

Reverse mapping (LSP position to byte offset) clamps a `character` beyond the end of the line to the line's end, as LSP requires. A position that falls inside a surrogate pair is rounded down to the start of the character.

### 11.5.4 Source references

`file` in a `Location` is a **source reference**:

| Field | Type | Meaning |
|---|---|---|
| `path` | string | the file's path in one of the forms below |
| `package` | string or null | the package identity and version that owns the file (`acme:user@1.0.0`), or `null` for files outside every package |
| `snapshot` | integer or null | the document version the span refers to (LSP and semantic protocol). For the CLI it is `null`, meaning the file content on disk when the command read it. |

`path` takes one of these forms (N-H-7):

1. A file inside the workspace root ([13-cli.md](13-cli.md) §13.2): the path relative to the workspace root, with `/` as the separator and no `.` or `..` segments, for example `src/user/model.sai` or `sai.package`.
2. A file of a dependency in the package cache, including the toolchain-shipped `sai:core` and `sai:alloc`: `dep:<ns>:<pkg>@<version>/<path inside the package>`, for example `dep:sai:http@1.4.2/src/client.sai`.
3. Standard input: `<stdin>` (only for `sai fmt -`, [13-cli.md](13-cli.md) §13.6.7).

The WIT files of the WASI packages shipped with the toolchain are in none of these forms. A location in them is not reported: a chain step whose location would be there has `location` `null` and is rendered without ` at <location>` ([08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.6.4).

JSON output MUST NOT contain absolute file system paths, so it is identical across machines (draft §72). The human renderer uses the same strings, except that form 1 paths are shown relative to the current directory when the current directory is inside the workspace (§11.10.3).

### 11.5.5 Position objects

In JSON, a position is an object with four fields, all REQUIRED:

```json
{ "byte": 214, "line": 9, "col": 18, "utf16_col": 18 }
```

A range is `{ "start": <position>, "end": <position> }`. The producer computes every field. A consumer MAY use whichever representation suits it, and it MUST obtain the same location from any of them.

---

## 11.6 Messages, labels, notes, and help

### 11.6.1 The main message

The main message states the problem in one line:

1. It is a single line (no LF), at most 100 characters as a SHOULD, and it has no trailing period.
2. It starts with a lowercase letter, unless it starts with a code fragment in backticks, as in "`` `User` has no field `username` ``".
3. Every name, type, effect, keyword, or other piece of source text is written in backticks. Types are printed in SAI syntax, using the shortest path that is unambiguous in the module containing the primary location. Effect sets are printed as `{ a, b }` in canonical effect order (§11.7.6).
4. It does not repeat the severity or the code. The renderer adds both.
5. It does not tell the user what to do. Suggestions go in `help` children or in fixes.

### 11.6.2 Secondary labels

A label marks a further range that helps explain the problem, such as the declaration of the field list, or the earlier borrow. Labels follow these rules:

- A label message is a short phrase with no trailing period, or empty. It is printed next to the underline.
- A label MAY be in a different file from the primary location.
- Two labels of one diagnostic MUST NOT have the same range. Producers merge their messages with `; `.
- A label MUST NOT have the same range as the primary location. A message for the primary range goes in the field `primary_label`, a string that is empty by default (§11.9.4). It is drawn under the primary caret line.

### 11.6.3 Notes and help

Children are kept in the order the producer added them, and rendered in that order:

- A **note** states a fact that explains the diagnostic: where a level was set (§11.3.4), why a NEWLINE was generated (`UNEXPECTED_NEWLINE`, [02-lexical-structure.md](../spec/02-lexical-structure.md) §2.10.3), or which rule applies.
- A **help** gives a suggestion. A help that a fix implements is not added as a child. The renderer prints the fix's title as a help line instead (§11.10.6).
- A child message MAY contain LF characters, to list items, for example. Each line after the first is rendered indented under the first (§11.10.5). Such a message SHOULD NOT exceed 12 lines.
- A child MAY carry a location. The renderer prints it as `at <path>:<line>:<col>`, after the message and with no snippet.

### 11.6.4 Example

The draft §60 diagnostic, as the reference implementation produces it:

```text
error[UNKNOWN_FIELD]: `User` has no field `username`
  --> src/user.sai:12:22
   |
12 |     let label = user.username
   |                      ^^^^^^^^ unknown field
   |
  ::: src/model.sai:3:6
   |
 3 | type User {
   |      ---- `User` is declared here
   |
   = help: available fields:
             name
             display_name
   = help: replace with `name`
   |
12 |     let label = user.name
   |                      ~~~~
```

The draft shows a bare `error[UNKNOWN_FIELD]` heading, the message on a separate line, and no location. This chapter replaces that with a single heading line followed by a location, so one line of output identifies the code, the message, and the place (N-H-8, §11.10).

---

## 11.7 Structured fixes

### 11.7.1 Model

A fix is a concrete repair that a tool can apply without interpreting the message text (draft §61, D-TOOL-01). It has two equivalent descriptions. The **operation** names the change on semantic nodes, and the **edits** spell the same change as text edits on the snapshot the diagnostic refers to.

```rust
pub struct Fix {
    pub title: String,                  // e.g. "replace with `name`"; same style as a help message
    pub applicability: Applicability,
    pub operation: FixOperation,        // §11.7.3
    pub edits: Vec<TextEdit>,           // §11.7.4
    pub placeholders: Vec<Placeholder>, // non-empty if and only if applicability == HasPlaceholders
}

pub enum Applicability { MachineApplicable, MaybeIncorrect, HasPlaceholders }

pub struct FixOperation {
    pub name: String,                   // from the closed set of §11.7.3
    pub node: NodeId,                   // the node the operation applies to, e.g. "expr:82A1"
    pub args: serde_json::Map<String, Value>,
}

pub struct TextEdit { pub file: SourceRef, pub range: Span, pub new_text: String }

pub struct Placeholder {
    pub edit: usize,                    // index into `edits`
    pub start: usize,                   // byte range inside `new_text`
    pub end: usize,
    pub label: String,                  // what the user must provide, e.g. "value"
}
```

A diagnostic MAY have several fixes. They are alternatives, ordered from most to least likely. At most one fix of a diagnostic is ever applied.

### 11.7.2 Applicability levels

Each fix has exactly one of three levels (N-H-9). In JSON they are spelled `machine-applicable`, `maybe-incorrect`, and `has-placeholders`.

| Level | Meaning | Applied by |
|---|---|---|
| `machine-applicable` | The fix is the only correct repair of the reported condition. Applying it removes this diagnostic and changes nothing else about the program's meaning. | `sai check --fix` automatically; editors and agents without asking |
| `maybe-incorrect` | The edits produce valid syntax, but the fix makes a choice the user has to confirm, for example one of several candidate names. It may also lead to other diagnostics. | only on explicit request: an LSP code action, or the semantic protocol's `sai/applyFix` |
| `has-placeholders` | The edits contain placeholders the user must fill in. The text is not valid until then. | only interactively. Editors render the placeholders as snippet tab stops. |

A producer MUST NOT mark a fix `machine-applicable` if it chose among two or more plausible alternatives, or if the fix can change run-time behavior. Typical machine-applicable fixes:

- removing `;` (`NO_SEMICOLON`)
- replacing `::` with `.` (`NO_DOUBLE_COLON`), and `&&` / `||` with `and` / `or` (`USE_AND_OR`)
- NFC-normalizing an identifier (`IDENT_NOT_NFC`)
- reordering signature clauses (`CLAUSE_ORDER`)
- inserting `borrow` at a call site where the parameter type requires it (D-OWN-02)
- inserting the inferred effect set (`MISSING_EFFECTS_DECLARATION`, D-EFF-01)

Typical maybe-incorrect fixes:

- choosing a field name from a candidate list (`UNKNOWN_FIELD`)
- suggesting `borrow x` for a move out of a borrow (D-EXPR-04), because the user may prefer `clone()`

### 11.7.3 Fix operations

`operation.name` is taken from a closed set. It is either a node edit primitive from the table below, or a structured operation of [12-semantic-tooling.md](12-semantic-tooling.md) §12.6 (for example `rename_symbol` for a `NAMING_CONVENTION` fix). In the latter case `args` has exactly the parameters of that operation, and `node` is the declaration node of the target symbol.

| Name | `args` | Effect on `node` |
|---|---|---|
| `replace_node` | `text: string` | replaces the node's text |
| `insert_before` | `text: string` | inserts text before the node |
| `insert_after` | `text: string` | inserts text after the node |
| `remove_node` | — | removes the node, and the separator that belongs to it in its list |
| `replace_field` | `field: string` | replaces the field name of a field access, record argument, or record pattern (draft §61) |
| `insert_borrow` | `mutable: bool` | prefixes the expression with `borrow` or `borrow mut` |
| `set_effects` | `effects: [string]` | inserts or replaces the function's effects clause with the given set, in canonical order (§11.7.6) |
| `reorder_clauses` | — | rewrites the signature clauses in D-SYN-06 order |
| `remove_token` | — | removes a token that is not a node by itself (`;`); `node` is the token's node ID |

The edits of a fix MUST be exactly the edits that applying `operation` to the snapshot would produce. A semantic client MAY apply the operation, and a text client MAY apply the edits. Both MUST arrive at the same text.

### 11.7.4 Edit rules

1. All edits of one fix refer to the same snapshot as the diagnostic's primary location.
2. The edits are sorted by `(file.path, range.start)`, and for any two consecutive edits `a`, `b` in the same file, `a.range.end <= b.range.start`. Two insertions at the same offset are merged into one edit.
3. `new_text` uses LF line terminators. When a tool applies edits to a file whose first line terminator is CR LF, it replaces every LF in `new_text` with CR LF.
4. **Formatting guarantee** (N-H-10). The producer computes the edits by applying the operation to the CST, formatting the enclosing **format unit** (a statement, a match arm, a field, a variant, or an item) with the unit formatting of [14-formatter.md](14-formatter.md) §14.3.6, which starts from the smallest such unit and widens it only when the layout of an enclosing node changes, and taking the minimal difference between the old and the new text of that unit. A fix never changes text outside that unit. If the file was formatter-canonical before the fix, it is canonical after it.
5. Edits never change comments, except comments inside a node that the operation removes or replaces as a whole.

### 11.7.5 Applying fixes in bulk

`sai check --fix` ([13-cli.md](13-cli.md) §13.6.2) and the LSP "fix all" action apply machine-applicable fixes as follows:

1. Collect every emitted diagnostic, before the `--max-errors` limit, that has a `machine-applicable` fix. Take the first such fix of each diagnostic.
2. Visit these fixes in diagnostic order (§11.11.1). Accept a fix unless one of its edits overlaps an edit of an already accepted fix, or touches the same offset. Fixes that are not accepted are left for the next round.
3. Apply the accepted edits per file in decreasing order of `range.start`, and write each changed file atomically (to a temporary file in the same directory, then rename).
4. Re-run the check and repeat from step 1. Stop when no machine-applicable fix remains, or after 8 rounds. The final round's diagnostics are the command's output.

The process is deterministic: given the same input, it applies the same fixes in the same order.

### 11.7.6 Canonical effect order

When a fix, a message, or `data` prints an effect set, the effects appear in the canonical order of [08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.2.3 (N-D-2): built-in effects in the table order of §8.3, then interface effects by identity in byte order, then user effects by spelling in byte order. The atoms are separated by `, `, and the empty set is `{}`.

The formatter never reorders effects written by the user ([14-formatter.md](14-formatter.md) §14.7.5, N-H-11). The canonical order applies only to text that tools generate.

---

## 11.8 Related information

### 11.8.1 Purpose

Some diagnostics are explained by a chain of facts that live far from the primary location: why a function has an effect, why a capability is reachable, why a borrow is still live, or why two requirements conflict. A tool must be able to follow such a chain without parsing prose (P3), so these facts are recorded in `related`, a list of typed entries (N-H-12). The owning analyses decide **which** chain is reported: [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) for effects and capabilities, [05-borrow-checker.md](05-borrow-checker.md) for borrows, and [package/02-resolver.md](../package/02-resolver.md) for requirements. This section defines only their representation and rendering.

### 11.8.2 Chain steps

Chain-shaped entries are lists of steps:

```rust
pub struct ChainStep {
    pub name: String,                 // display name, e.g. "build_message", "String.concat"
    pub symbol: Option<SymbolId>,     // impl/12 §12.2
    pub location: Option<Location>,   // the call site, instantiation, or declaration that links this step to the next
    pub relation: Relation,           // how this step leads to the next one
}

pub enum Relation {
    Calls,              // this function calls the next one at `location`
    Instantiates,       // this function instantiates a type parameter with the next step (latent effect, D-EFF-02)
    Awaits,             // this function awaits a future whose body is the next step (D-ASYNC-01)
    Covers,             // this user-defined effect covers the next effect (D-EFF-03)
    Imports,            // this function is an import with the next interface effect
    Declares,           // last step: the effect is declared or intrinsic here
    Requires,           // package requirement chain: this package requires the next one
}
```

Step 0 is the declaration the diagnostic is about. The last step is the origin, and its relation is `Declares` (or `Requires` for requirement chains, where the last step is the conflicting requirement). Chains never contain cycles. A chain has at most 32 steps. If it is longer, the producer keeps the first 16 and the last 15 and puts a step with `name` `…`, no symbol, no location, and relation `Calls` between them.

### 11.8.3 Entry kinds

| `kind` | Fields | Used by |
|---|---|---|
| `effect_chain` | `effect: string`, `steps: [ChainStep]` | `MISSING_EFFECTS_DECLARATION`, effects-clause violations, `PUBLIC_EFFECTS_INFERRED` |
| `capability_chain` | `capability: string`, `effect: string`, `steps: [ChainStep]`, `policy: Location \| null` (the `allow`, `deny` or `grant` entry that the reporting check compares against, or `null` when there is none: a capability absent from `allow`, the target check, and the test check; [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.6.5) | capability policy, grant, target and test violations (D-CAP-01) |
| `borrow_conflict` | `place: string`, `loan: Location`, `loan_kind: "shared" \| "mutable"`, `access: Location`, `access_kind: "read" \| "write" \| "move" \| "borrow" \| "borrow_mut" \| "storage_dead"`, `later_use: Location \| null` | borrow errors (the three-point explanation) |
| `use_after_move` | `place: string`, `moved_at: Location`, `move_kind: "assign" \| "argument" \| "return" \| "capture" \| "pattern"`, `used_at: Location`, `in_loop: bool` | use-after-move errors |
| `requirement_chain` | `steps: [ChainStep]` | `VERSION_CONFLICT` (one entry per conflicting requirement, D-PKG-02) |
| `declared_here` | `location: Location`, `what: string` | any diagnostic that points at a declaration, such as the original definition for `DUPLICATE_BINDING` |

The **three-point explanation** of a borrow conflict is `loan` (where the borrow was created), `access` (the conflicting access, which is also the diagnostic's primary location), and `later_use` (a point where the loan is still live, which makes the access a conflict). `later_use` is `null` only when the loan is live because it is returned from the function. In that case the producer adds a note naming the return.

### 11.8.4 Rendering related entries

The human renderer handles the kinds in two ways:

- `borrow_conflict` and `use_after_move` are rendered as **labels** in the snippet: `` `<place>` is borrowed here `` (`mutably borrowed` for a mutable loan) at `loan`, `borrow later used here` at `later_use`, `` `<place>` moved here `` at `moved_at`. They are not rendered if a label with the same range already exists. Producers MUST NOT add labels that duplicate them.
- Chain kinds (`effect_chain`, `capability_chain`, `requirement_chain`) and `declared_here` are rendered as **notes**, after all other children:

```text
   = note: `create_user` has effect `alloc` because:
             `create_user` calls `build_message` at src/user.sai:14:17
             `build_message` calls `String.concat` at src/message.sai:3:9
             `String.concat` declares `alloc` at dep:sai:alloc@0.1.0/src/string.sai:88:5
```

The line text for each relation is fixed:

| Relation | Line |
|---|---|
| `Calls` | `` `<name>` calls `<next>` at <loc> `` |
| `Instantiates` | `` `<name>` instantiates a type parameter with `<next>` at <loc> `` |
| `Awaits` | `` `<name>` awaits `<next>` at <loc> `` |
| `Covers` | `` `<name>` covers `<next>` at <loc> `` |
| `Imports` | `` `<name>` is imported from `<next>` at <loc> `` |
| `Declares` | `` `<name>` declares `<effect>` at <loc> `` |
| `Requires` | `` `<name>` requires `<next>` at <loc> `` |

The heading line is `` `<step 0>` has effect `<effect>` because: `` for effect chains, `` `<capability>` is reached through effect `<effect>`: `` for capability chains, and `conflicting requirement:` for requirement chains. A step without a location omits ` at <loc>`.

---

## 11.9 JSON wire format

### 11.9.1 Stream form

With `--format json` ([13-cli.md](13-cli.md) §13.3.3), every command writes **newline-delimited JSON** (NDJSON) to standard output: one JSON object per line, each line terminated by LF (N-H-13). An NDJSON stream can be consumed incrementally, and if the producer is interrupted, every complete line is still valid. A single JSON array has neither property.

A stream consists of:

1. exactly one `header` record, first
2. zero or more `diagnostic` records, in the order of §11.11.1
3. zero or more `result` records, whose shapes are defined per command by [13-cli.md](13-cli.md) and [12-semantic-tooling.md](12-semantic-tooling.md). They follow all diagnostic records, except where a command defines interleaving, such as the per-test results of `sai test`.
4. exactly one `summary` record, last

With `--format json`, a command writes nothing to standard error, unless writing to standard output fails. In that case it writes one human-readable line to standard error and exits with code 4.

### 11.9.2 Serialization

Every record is serialized deterministically, so that identical runs produce byte-identical streams:

- The encoding is UTF-8, with no BOM.
- There is no whitespace outside strings.
- Object members appear in the order in which the schema (§11.9.5) lists the properties. A member whose value is `null` or an empty array is still written, with the exception of `data`, which is written as `{}` when the code has no data.
- In strings, only `"`, `\`, and U+0000–U+001F are escaped: U+0008, U+0009, U+000A, U+000C, and U+000D as `\b`, `\t`, `\n`, `\f`, `\r`, and the others as `\u00XX` with lowercase hex digits. All other characters, including `/` and non-ASCII characters, are written unescaped.
- All numbers are non-negative integers written in decimal without leading zeros. Floating-point numbers never appear.

### 11.9.3 Versioning

The header carries `"format": "sai-diagnostics"` and an integer `version`. This chapter defines version **1**.

- Within one version, new optional object members and new enum values for `kind`, `relation`, and `operation.name` MAY be added. Consumers MUST ignore unknown members, and MUST treat an unknown enum value as opaque rather than as an error.
- Removing a member, renaming it, or changing its type or meaning requires a new version number.
- A toolchain emits exactly one version. It does not negotiate.

### 11.9.4 Record contents

The `header` record contains `type` (`"header"`), `format`, `version`, `tool` (`{ "name": "sai", "version": "<semver>" }`), `command` (the subcommand path, such as `"check"` or `"component build"`), and `root`: the root package as `<ns>:<pkg>@<version>`, or `null` outside a package.

The `diagnostic` record serializes `Diagnostic` (§11.4.1). Its members, in order, are: `type`, `code`, `severity`, `level`, `message`, `primary`, `primary_label`, `labels`, `children`, `fixes`, `related`, `data`, `node`, `symbol`, and `package`.

The `summary` record contains:

- `errors`, `warnings`, `infos`: the counts **before** the `--max-errors` limit is applied
- `shown_errors`: the number of error records in the stream
- `suppressed`: an object with `cascade`, `duplicate`, and `dependency`, counting the diagnostics dropped by cascade suppression, deduplication, and the dependency cap (§11.3.3, §11.11)
- `exit_code`: the process's exit code ([13-cli.md](13-cli.md) §13.3.4)

Lints dropped at level `allow` are not counted anywhere.

### 11.9.5 JSON Schema

Every record of a version 1 stream MUST validate against this schema (JSON Schema 2020-12). The schema's property order is the serialization order of §11.9.2.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "urn:sai:schema:diagnostics:1",
  "title": "SAI diagnostic stream record, version 1",
  "oneOf": [
    { "$ref": "#/$defs/header" },
    { "$ref": "#/$defs/diagnostic" },
    { "$ref": "#/$defs/result" },
    { "$ref": "#/$defs/summary" }
  ],
  "$defs": {
    "code": { "type": "string", "pattern": "^[A-Z][A-Z0-9]*(_[A-Z][A-Z0-9]*)+$", "maxLength": 40 },
    "uint": { "type": "integer", "minimum": 0 },
    "pos1": { "type": "integer", "minimum": 1 },
    "symbol_id": { "type": "string", "pattern": "^symbol://[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*/.*#[0-9A-F]{6}$" },
    "node_id": { "type": "string", "pattern": "^[a-z][a-z_]*:[0-9A-F]{4,16}$" },
    "position": {
      "type": "object",
      "required": ["byte", "line", "col", "utf16_col"],
      "properties": {
        "byte": { "$ref": "#/$defs/uint" },
        "line": { "$ref": "#/$defs/pos1" },
        "col": { "$ref": "#/$defs/pos1" },
        "utf16_col": { "$ref": "#/$defs/pos1" }
      }
    },
    "source_ref": {
      "type": "object",
      "required": ["path", "package", "snapshot"],
      "properties": {
        "path": { "type": "string", "minLength": 1 },
        "package": { "type": ["string", "null"] },
        "snapshot": { "type": ["integer", "null"], "minimum": 0 }
      }
    },
    "location": {
      "type": "object",
      "required": ["file", "start", "end"],
      "properties": {
        "file": { "$ref": "#/$defs/source_ref" },
        "start": { "$ref": "#/$defs/position" },
        "end": { "$ref": "#/$defs/position" }
      }
    },
    "opt_location": { "oneOf": [{ "$ref": "#/$defs/location" }, { "type": "null" }] },
    "level": {
      "type": ["object", "null"],
      "required": ["value", "source", "location"],
      "properties": {
        "value": { "enum": ["info", "warn", "deny"] },
        "source": { "enum": ["default", "attribute", "manifest", "policy", "cli", "deny_warnings"] },
        "location": { "$ref": "#/$defs/opt_location" }
      }
    },
    "label": {
      "type": "object",
      "required": ["location", "message"],
      "properties": { "location": { "$ref": "#/$defs/location" }, "message": { "type": "string" } }
    },
    "child": {
      "type": "object",
      "required": ["kind", "message", "location"],
      "properties": {
        "kind": { "enum": ["note", "help"] },
        "message": { "type": "string" },
        "location": { "$ref": "#/$defs/opt_location" }
      }
    },
    "edit": {
      "type": "object",
      "required": ["file", "start", "end", "new_text"],
      "properties": {
        "file": { "$ref": "#/$defs/source_ref" },
        "start": { "$ref": "#/$defs/position" },
        "end": { "$ref": "#/$defs/position" },
        "new_text": { "type": "string" }
      }
    },
    "placeholder": {
      "type": "object",
      "required": ["edit", "start", "end", "label"],
      "properties": {
        "edit": { "$ref": "#/$defs/uint" },
        "start": { "$ref": "#/$defs/uint" },
        "end": { "$ref": "#/$defs/uint" },
        "label": { "type": "string" }
      }
    },
    "fix": {
      "type": "object",
      "required": ["title", "applicability", "operation", "edits", "placeholders"],
      "properties": {
        "title": { "type": "string" },
        "applicability": { "enum": ["machine-applicable", "maybe-incorrect", "has-placeholders"] },
        "operation": {
          "type": "object",
          "required": ["name", "node", "args"],
          "properties": {
            "name": { "type": "string", "pattern": "^[a-z][a-z_]*$" },
            "node": { "$ref": "#/$defs/node_id" },
            "args": { "type": "object" }
          }
        },
        "edits": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/edit" } },
        "placeholders": { "type": "array", "items": { "$ref": "#/$defs/placeholder" } }
      }
    },
    "chain_step": {
      "type": "object",
      "required": ["name", "symbol", "location", "relation"],
      "properties": {
        "name": { "type": "string" },
        "symbol": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "type": "null" }] },
        "location": { "$ref": "#/$defs/opt_location" },
        "relation": { "enum": ["calls", "instantiates", "awaits", "covers", "imports", "declares", "requires"] }
      }
    },
    "steps": { "type": "array", "minItems": 1, "maxItems": 32, "items": { "$ref": "#/$defs/chain_step" } },
    "related": {
      "type": "object",
      "required": ["kind"],
      "properties": { "kind": { "type": "string" } },
      "oneOf": [
        { "properties": { "kind": { "const": "effect_chain" }, "effect": { "type": "string" }, "steps": { "$ref": "#/$defs/steps" } }, "required": ["effect", "steps"] },
        { "properties": { "kind": { "const": "capability_chain" }, "capability": { "type": "string" }, "effect": { "type": "string" }, "steps": { "$ref": "#/$defs/steps" }, "policy": { "$ref": "#/$defs/opt_location" } }, "required": ["capability", "effect", "steps", "policy"] },
        { "properties": { "kind": { "const": "borrow_conflict" }, "place": { "type": "string" }, "loan": { "$ref": "#/$defs/location" }, "loan_kind": { "enum": ["shared", "mutable"] }, "access": { "$ref": "#/$defs/location" }, "access_kind": { "enum": ["read", "write", "move", "borrow", "borrow_mut", "storage_dead"] }, "later_use": { "$ref": "#/$defs/opt_location" } }, "required": ["place", "loan", "loan_kind", "access", "access_kind", "later_use"] },
        { "properties": { "kind": { "const": "use_after_move" }, "place": { "type": "string" }, "moved_at": { "$ref": "#/$defs/location" }, "move_kind": { "enum": ["assign", "argument", "return", "capture", "pattern"] }, "used_at": { "$ref": "#/$defs/location" }, "in_loop": { "type": "boolean" } }, "required": ["place", "moved_at", "move_kind", "used_at", "in_loop"] },
        { "properties": { "kind": { "const": "requirement_chain" }, "steps": { "$ref": "#/$defs/steps" } }, "required": ["steps"] },
        { "properties": { "kind": { "const": "declared_here" }, "location": { "$ref": "#/$defs/location" }, "what": { "type": "string" } }, "required": ["location", "what"] }
      ]
    },
    "header": {
      "type": "object",
      "required": ["type", "format", "version", "tool", "command", "root"],
      "properties": {
        "type": { "const": "header" },
        "format": { "const": "sai-diagnostics" },
        "version": { "const": 1 },
        "tool": {
          "type": "object",
          "required": ["name", "version"],
          "properties": { "name": { "const": "sai" }, "version": { "type": "string" } }
        },
        "command": { "type": "string" },
        "root": { "type": ["string", "null"] }
      }
    },
    "diagnostic": {
      "type": "object",
      "required": ["type", "code", "severity", "level", "message", "primary", "primary_label", "labels",
                   "children", "fixes", "related", "data", "node", "symbol", "package"],
      "properties": {
        "type": { "const": "diagnostic" },
        "code": { "$ref": "#/$defs/code" },
        "severity": { "enum": ["error", "warning", "info"] },
        "level": { "$ref": "#/$defs/level" },
        "message": { "type": "string", "minLength": 1 },
        "primary": { "$ref": "#/$defs/opt_location" },
        "primary_label": { "type": "string" },
        "labels": { "type": "array", "items": { "$ref": "#/$defs/label" } },
        "children": { "type": "array", "items": { "$ref": "#/$defs/child" } },
        "fixes": { "type": "array", "items": { "$ref": "#/$defs/fix" } },
        "related": { "type": "array", "items": { "$ref": "#/$defs/related" } },
        "data": { "type": "object" },
        "node": { "oneOf": [{ "$ref": "#/$defs/node_id" }, { "type": "null" }] },
        "symbol": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "type": "null" }] },
        "package": { "type": ["string", "null"] }
      }
    },
    "result": {
      "type": "object",
      "required": ["type", "kind"],
      "properties": {
        "type": { "const": "result" },
        "kind": { "type": "string", "pattern": "^[a-z][a-z_.]*$" }
      }
    },
    "summary": {
      "type": "object",
      "required": ["type", "errors", "warnings", "infos", "shown_errors", "suppressed", "exit_code"],
      "properties": {
        "type": { "const": "summary" },
        "errors": { "$ref": "#/$defs/uint" },
        "warnings": { "$ref": "#/$defs/uint" },
        "infos": { "$ref": "#/$defs/uint" },
        "shown_errors": { "$ref": "#/$defs/uint" },
        "suppressed": {
          "type": "object",
          "required": ["cascade", "duplicate", "dependency"],
          "properties": {
            "cascade": { "$ref": "#/$defs/uint" },
            "duplicate": { "$ref": "#/$defs/uint" },
            "dependency": { "$ref": "#/$defs/uint" }
          }
        },
        "exit_code": { "$ref": "#/$defs/uint" }
      }
    }
  }
}
```

Enum values of the Rust model (§11.4, §11.7, §11.8) are serialized in lowercase, with words joined by `_` (`Calls` becomes `calls`, `HasPlaceholders` becomes `has-placeholders` as the only hyphenated exception, following the level names of §11.7.2). The `result` record leaves its other members to the command that defines it. Each command's chapter gives the schema of its result kinds.

### 11.9.6 Example stream

`sai check --format json` on a package with one unknown field (pretty-printed here; on the wire each record is one line):

```json
{"type":"header","format":"sai-diagnostics","version":1,"tool":{"name":"sai","version":"0.1.0"},"command":"check","root":"acme:user@1.0.0"}
{
  "type": "diagnostic",
  "code": "UNKNOWN_FIELD",
  "severity": "error",
  "level": null,
  "message": "`User` has no field `username`",
  "primary": {
    "file": { "path": "src/user.sai", "package": "acme:user@1.0.0", "snapshot": null },
    "start": { "byte": 262, "line": 12, "col": 22, "utf16_col": 22 },
    "end": { "byte": 270, "line": 12, "col": 30, "utf16_col": 30 }
  },
  "primary_label": "unknown field",
  "labels": [
    {
      "location": {
        "file": { "path": "src/model.sai", "package": "acme:user@1.0.0", "snapshot": null },
        "start": { "byte": 41, "line": 3, "col": 6, "utf16_col": 6 },
        "end": { "byte": 45, "line": 3, "col": 10, "utf16_col": 10 }
      },
      "message": "`User` is declared here"
    }
  ],
  "children": [
    { "kind": "help", "message": "available fields:\nname\ndisplay_name", "location": null }
  ],
  "fixes": [
    {
      "title": "replace with `name`",
      "applicability": "maybe-incorrect",
      "operation": { "name": "replace_field", "node": "expr:82A1", "args": { "field": "name" } },
      "edits": [
        {
          "file": { "path": "src/user.sai", "package": "acme:user@1.0.0", "snapshot": null },
          "start": { "byte": 262, "line": 12, "col": 22, "utf16_col": 22 },
          "end": { "byte": 270, "line": 12, "col": 30, "utf16_col": 30 },
          "new_text": "name"
        }
      ],
      "placeholders": []
    }
  ],
  "related": [],
  "data": { "type_name": "User", "field": "username", "candidates": ["name", "display_name"] },
  "node": "name:82A6",
  "symbol": "symbol://acme:user/model/User#3C09D1",
  "package": "acme:user@1.0.0"
}
{"type":"summary","errors":1,"warnings":0,"infos":0,"shown_errors":1,"suppressed":{"cascade":0,"duplicate":0,"dependency":0},"exit_code":1}
```

The human renderer prints the lines of a multi-line child message with indentation (§11.10.5). In JSON the message holds plain LF-separated lines without indentation.

In this example `node` is the name node `username` (`name:82A6`), the innermost node with exactly the primary range (§11.4.4). The fix operation targets the enclosing field access expression `expr:82A1`, as in draft §61.

---

## 11.10 Human renderer

### 11.10.1 Destination and scope

With `--format human` (the default), diagnostics are written to standard error, in the order of §11.11.1, followed by the summary line (§11.10.8). The layout in this section is normative for the reference implementation (N-H-8). The test suite of [15-conformance-and-testing.md](15-conformance-and-testing.md) compares rendered output byte for byte with colors disabled.

### 11.10.2 Layout of one diagnostic

Let `W` be the number of decimal digits of the largest line number displayed anywhere in the diagnostic, with a minimum of 1. Write `G` for `W` spaces. A diagnostic is rendered as:

```text
<severity>[<CODE>]: <message>
G--> <path>:<line>:<col>
G |
<snippet of the primary file>
G::: <path>:<line>:<col>            (one block per further file with labels)
G |
<snippet of that file>
G |                                 (only if children or fixes follow)
G = <kind>: <child message>
G = help: <fix title>
G |                                 (only if the fix has a suggestion snippet)
<suggestion snippet>
<empty line>
```

1. The **heading** is the severity (`error`, `warning`, or `info`), the code in square brackets, a colon, a space, and the message.
2. The **location line** shows the start of the primary location. `<col>` is the UTF-8 byte column of §11.5.2. If `primary` is `null`, the location line and all snippet lines are omitted, and the children use `G` = two spaces.
3. **Further files**: the labels whose file differs from the primary file are grouped per file, in order of the first label of each file in `labels`. Each group starts with a `:::` line at the start of its first label.
4. **Children** follow in this order: the children in producer order; the level note (§11.3.4); and the notes rendered from related entries (§11.8.4).
5. **Fixes**: at most 3 fixes are rendered, in order, each as a `help` line with its title, optionally followed by a suggestion snippet (§11.10.6).
6. Each diagnostic ends with one empty line.

### 11.10.3 Paths

The path is the source reference path of §11.5.4. When the current directory is inside the workspace root, a form 1 path is shown relative to the current directory, using `..` segments if needed, with `/` as the separator on every platform. Otherwise it is shown exactly as in JSON. Paths are never absolute.

### 11.10.4 Snippets

For one file, the renderer collects the **annotations**: the primary location (if it is in this file) and the labels in this file, with the borrow and move points of §11.8.4.

1. **Display lines.** These are the start line and the end line of every annotation. An unannotated line that lies between two display lines with exactly that one line between them is also displayed. A larger gap is shown as a single row `...` with no gutter.
2. **Source rows.** Each display line is printed as `<line number right-aligned in W> | <text>`. In `<text>`, TABs are expanded to the next multiple of 4 display columns, the line terminator is removed, and every character of the classes that make up `FORBIDDEN_CHARACTER`, as well as U+FEFF, is shown as `<U+XXXX>` with 4 to 6 uppercase hex digits. Source text is never colored.
3. **Display width.** Characters with East Asian Width `W` or `F` are 2 columns wide. Characters of General Category `Mn`, `Me`, or `Cf` other than those replaced in item 2 are 0 columns wide, and all others are 1 column wide. Underlines are positioned and sized in display columns.
4. **Annotation rows.** After a source row come one row for each annotation that ends on that line, then one row for each annotation that only starts there. The primary annotation comes first. Secondary annotations follow in order of start column, and annotations with the same start column keep their order in `labels`. A row is `G | ` followed by spaces up to the annotation's start column, then the marks, then a space and the message if the message is non-empty.
5. **Marks.** The primary annotation uses `^`, and secondary annotations use `-`. A mark covers the display width of the annotated text on that line, and is at least 1 column wide. An empty range is drawn as one mark at its column. A range that starts at the end of a line is drawn as one mark just after the line's last character.
6. **Multi-line ranges.** On its start line, a range spanning several lines is drawn from its start column to the end of the line, with no message. On its end line, it is drawn from the first non-whitespace character to its end column, followed by the message. Lines strictly between the start and the end are displayed if there are at most two of them, and replaced by `...` otherwise.
7. **Long lines.** If a source row's text is wider than 140 display columns, the renderer shows a window of 140 columns. The window starts 20 columns before the leftmost annotation start on that line (or at column 1 if that is less than 21), and is shifted left as needed so that the rightmost annotation end is visible. Each cut side is marked with `...`, and the annotation rows are shifted with the window.

### 11.10.5 Children

A child is rendered as `G = note: <first line>` or `G = help: <first line>`, followed by ` at <path>:<line>:<col>` if it carries a location. Every further line of the message is rendered as `G` followed by `kind.len() + 7` spaces and the line, so continuation lines are indented 2 columns more than the first line's text.

### 11.10.6 Suggestion snippets

A fix gets a suggestion snippet if all of its edits are in one file, at least one edit has non-empty `new_text`, and the affected region after the fix spans at most 6 lines. The region runs from the line of the first edit's start to the line of the last edit's end, both measured in the text after the fix. Otherwise only the `help` line is printed.

The snippet shows the lines of that region after the fix, as source rows with their new line numbers. In every maximal run of characters that came from a `new_text`, leading and trailing whitespace is trimmed. Each non-empty trimmed run is underlined with `~` in an annotation row below its line. Placeholders are underlined with `?` instead of `~`.

### 11.10.7 Colors

Colors are used when the color mode is `always`, or when it is `auto` and the conditions of [13-cli.md](13-cli.md) §13.3.2 hold. Only these SGR sequences are used, and each colored span ends with `ESC[0m`:

| Element | SGR |
|---|---|
| heading severity and code, for `error` | `ESC[1;31m` (bold red) |
| heading severity and code, for `warning` | `ESC[1;33m` (bold yellow) |
| heading severity and code, for `info` | `ESC[1;36m` (bold cyan) |
| heading message | `ESC[1m` (bold) |
| `-->`, `:::`, gutter `\|`, line numbers, `...`, `=` | `ESC[1;34m` (bold blue) |
| primary marks `^` and the primary label | the heading's severity color |
| secondary marks `-` and label messages | `ESC[1;34m` (bold blue) |
| the word `note` | `ESC[1m` (bold) |
| the word `help`, suggestion marks `~` and `?` | `ESC[1;32m` (bold green) |
| `failed` in the summary line | `ESC[1;31m` (bold red) |
| `finished` in the summary line | `ESC[1m` (bold) |

Paths, source text, and child message text are never colored. With colors disabled, the output contains no escape sequences.

### 11.10.8 Summary line

After all diagnostics, if at least one was rendered, the renderer prints one line:

```text
<command> <status>: <counts>[; <n> more errors not shown (--max-errors <limit>)]
```

- `<command>` is the subcommand path (`check`, `build`, `component build`).
- `<status>` is `failed` if the error count is non-zero, and `finished` otherwise.
- `<counts>` joins with `, ` the non-zero entries among `<n> error`/`<n> errors`, `<n> warning`/`<n> warnings`, and `<n> info`, in that order.
- The counts are taken before the limit is applied, as in the JSON summary.

### 11.10.9 Complete example

A missing effects declaration (D-EFF-01) with an effect chain and a machine-applicable fix:

```sai
// rejected: MISSING_EFFECTS_DECLARATION
export fn greet(count: I32) -> I32 {
    let message = build_message(count)
    message.len().trunc_to<I32>()
}
```

```text
error[MISSING_EFFECTS_DECLARATION]: exported function `greet` does not declare its effects
 --> src/lib.sai:2:11
  |
2 | export fn greet(count: I32) -> I32 {
  |           ^^^^^ inferred effects: `{ alloc }`
  |
  = note: `greet` has effect `alloc` because:
            `greet` calls `build_message` at src/lib.sai:3:19
            `build_message` calls `String.concat` at src/message.sai:4:5
            `String.concat` declares `alloc` at dep:sai:alloc@0.1.0/src/string.sai:88:5
  = help: declare the inferred effects
  |
2 | export fn greet(count: I32) -> I32
3 |     effects { alloc }
  |     ~~~~~~~~~~~~~~~~~
4 | {

check failed: 1 error
```

The fix replaces the single space before `{` with `"\n    effects { alloc }\n"`. Its formatter-canonical result puts the clause on its own line ([14-formatter.md](14-formatter.md) §14.7.4).

---

## 11.11 Ordering, deduplication, limits, and cascades

### 11.11.1 Ordering

Emitted diagnostics are sorted by the following key, compared component by component (N-H-14):

1. **Location class**: diagnostics with `primary: null` come first.
2. **Package rank**: the root package; then the workspace members ([13-cli.md](13-cli.md) §13.2.2) in byte order of identity; then dependencies in byte order of `<ns>:<pkg>@<version>`.
3. **File rank** within the package: `sai.package` first, then `sai.lock`, then every other file in byte order of its path.
4. `primary.start.byte`, ascending.
5. `primary.end.byte`, ascending.
6. Severity rank (§11.3.1): errors before warnings, and warnings before info.
7. `code`, in byte order.
8. `message`, in byte order.
9. The byte-wise serialization of the whole record (§11.9.2), as a final tiebreaker.

The order does not depend on the order in which producers reported diagnostics. Producers running in parallel report into a sink that sorts before rendering. The reference implementation buffers all diagnostics of a command and renders them once the command's analyses are complete. For `sai test`, the per-test result records ([13-cli.md](13-cli.md) §13.6.6) are an exception: they are streamed as each test finishes.

### 11.11.2 Deduplication

Two diagnostics are **duplicates** if they have the same code, severity, primary location, and message. Of a set of duplicates, only the one whose serialization sorts first is kept, and the others are counted in `suppressed.duplicate`. Duplicates arise mainly when one generic body is checked for several instantiations, or when one condition is found by two analyses. Producers SHOULD avoid them, but correctness does not depend on it.

### 11.11.3 Limits

`--max-errors <N>` ([13-cli.md](13-cli.md) §13.4.5) limits how many **error** diagnostics are rendered. The default is 50, and 0 means no limit (N-H-15).

- The rendered errors are the first `N` errors in the order of §11.11.1. Warnings and info diagnostics are not limited.
- The limit affects output only. The toolchain runs every analysis it would run without the limit. The counts in the summary, the exit code, and the fixes applied by `--fix` are the same as without the limit. An implementation MAY stop early only if it can guarantee identical output.
- The LSP server ([12-semantic-tooling.md](12-semantic-tooling.md) §12.10) applies no limit.

### 11.11.4 Cascade suppression

An error often causes further diagnostics that add no information. The toolchain drops them with the following rules. They are normative for the reference implementation and RECOMMENDED for others (N-H-16). Each diagnostic carries an internal **cause key**: the set of poison facts its condition depends on. A diagnostic is dropped if its cause key contains a fact that was poisoned by another diagnostic that is kept. A diagnostic never poisons a fact in its own cause key, so the first diagnostic of a cascade is always kept.

| Rule | Poisoned fact | Poisoned by | Dropped diagnostics |
|---|---|---|---|
| C1 | the CST error node `n` | a lexical or syntax error that created a recovery node ([02-frontend-ast-hir.md](02-frontend-ast-hir.md)) | every non-syntax diagnostic whose primary location lies inside `n` |
| C2 | the error type | any error that leaves an expression or declaration with the error type `{error}` ([03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md)) | every type, trait, ownership, effect, or contract diagnostic whose condition involves a type containing `{error}` |
| C3 | an unresolved spelling `s` in item `i` | the first unresolved-name error for `s` in `i` | later unresolved-name errors for `s` in `i` |
| C4 | the signature of symbol `f` | an error in `f`'s signature (parameters, return type, clauses) | call-site diagnostics that compare arguments, results, or effects with `f`'s signature |
| C5 | the body of item `i` | any error from name resolution or type checking in `i`'s body | all lints in `i`, plus `MISSING_EFFECTS_DECLARATION` and `PUBLIC_EFFECTS_INFERRED` for `i` |
| C6 | the package `p` | a resolution, fetch, checksum, or lock error for `p` ([package/02-resolver.md](../package/02-resolver.md), [package/03-lock-file.md](../package/03-lock-file.md)) | diagnostics about `use` of `p`'s modules and items |

Two rules prevent diagnostics from being created at all, so they are not counted as suppressed:

- **Phase gating.** Borrow checking, effect checking against declared clauses, the Standard Fact Checker, and capability checking run on a function body only if name resolution, type checking, and match checking (`check_match`, [03-name-resolution-and-type-checking.md](03-name-resolution-and-type-checking.md) §3.12.2, §3.16) of that body produced no error.
- **Package gating.** Source checking does not start for a package that rule C6 poisoned.

Every diagnostic dropped by C1–C6 is counted in `suppressed.cascade`.

---

## 11.12 Code stability

### 11.12.1 Rules

Codes are part of the toolchain's public interface. Scripts, CI configurations, editor settings, and `@allow(..)` attributes in published packages name them. The following rules are normative for all conforming tools (D-TOOL-01, N-H-1):

1. Once a code has appeared in a released toolchain, its meaning MUST NOT change. Its message, labels, notes, help, fixes, and `data` fields MAY be improved, and new `data` fields MAY be added. The condition it reports MUST stay the same, apart from fixing false positives and false negatives.
2. A code MUST NOT be renamed. If a better name is needed, the old code is deprecated and a new code is introduced with the old one as `superseded_by`.
3. A code MUST NOT be reused. A removed code keeps its registry entry with status `removed`, and no later condition gets that code.
4. A hard code MUST NOT become a lint, or a lint a hard code, within one major language version (the version that `compiler` requirements refer to, [package/01-package-contract.md](../package/01-package-contract.md) §1.5; there are no editions in v0.1, N-G-5). Within a major version, a lint's default level MAY be raised only from `allow` or `info` to `warn` (P6). Raising a default to `deny` requires a new major version.
5. A hard warning MUST NOT become a hard error within a major version, unless the condition is newly found to be unsound. That case is a language change recorded in [design/decisions.md](../design/decisions.md).

### 11.12.2 Statuses

| Status | Meaning | Behavior |
|---|---|---|
| `active` | The code is reported. | normal |
| `deprecated` | The code is still reported, but a newer code covers the condition, or the condition is planned for removal. | Naming it in a level attribute or a CLI lint flag produces `DEPRECATED_DIAGNOSTIC_CODE`, and the level still applies to it. If `superseded_by` is set, the level also applies to the new code. |
| `removed` | The code is never reported. | Naming it produces `DEPRECATED_DIAGNOSTIC_CODE`, and the attribute or flag has no effect. |

A code stays `deprecated` for at least one minor toolchain release before it becomes `removed`.

### 11.12.3 Naming a code that is not a lint

Level attributes, the `lints { }` block of `sai.package`, and CLI lint flags accept only lint codes. `UNKNOWN_LINT` and `NOT_A_LINT` are defined in [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.15.4; this section defines how the registry decides between them. These diagnostics apply:

- `UNKNOWN_LINT` (error) if the name is not in the registry. The candidates of §11.4.5 are offered from the registered lint codes. The code is shared with [package/01-package-contract.md](../package/01-package-contract.md) §1.13 and [02-frontend-ast-hir.md](02-frontend-ast-hir.md). For a CLI flag, the error makes the command exit with code 2 ([13-cli.md](13-cli.md) §13.3.4).
- `NOT_A_LINT` (error) if the name is a hard code. Hard diagnostics cannot be allowed or denied. The help says so, and for a hard warning it points to `--deny-warnings`. The code is shared with [02-frontend-ast-hir.md](02-frontend-ast-hir.md).
- `DEPRECATED_DIAGNOSTIC_CODE` (warning) as in §11.12.2, with a machine-applicable fix that replaces the name with `superseded_by` when it is set.

Whether these are reported for attributes is owned by [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md). This chapter fixes the codes and the form of the diagnostics. A code introduced by a newer toolchain is unknown to an older one, so a package that names it fails to build there. This is intended: a package that silences a diagnostic depends on the toolchain version that defines it (P8). Such a package SHOULD state that version in its `compiler` requirement ([package/01-package-contract.md](../package/01-package-contract.md) §1.5).

---

## 11.13 `sai explain`

### 11.13.1 Behavior

`sai explain <CODE>` ([13-cli.md](13-cli.md) §13.6.20) prints the registry entry of a code. The command needs no workspace and reads nothing but its arguments. Its output is the same on every target and every host, and changes only with the toolchain version.

- The code is matched exactly. Case is significant, and `unknown_field` is not `UNKNOWN_FIELD`.
- Unknown codes produce `UNKNOWN_DIAGNOSTIC_CODE` with candidates (§11.4.5) and exit code 1. This is an error in the command's input, not in its usage, so the exit code is 1 and not 2.
- Deprecated and removed codes are explained normally, and a notice line follows the metadata table.
- `sai explain --list` prints every registered code, one per line, in byte order, followed by a space, the class (`hard` or `lint`), a space, and the summary.

### 11.13.2 Explanation record

The explanation is CommonMark. Its structure is normative for all conforming tools, so that editors and documentation generators can process it:

````markdown
# UNKNOWN_FIELD

| Class | Severity | Stage | Owner | Since | Status |
|---|---|---|---|---|---|
| hard | error | 1 | spec/06-expressions-and-statements.md | 0.1.0 | active |

## Summary

A field access names a field that the record type does not have.

## Explanation

<one or more paragraphs>

## Example

```sai
// rejected: UNKNOWN_FIELD
<code>
```

## Fix

```sai
<corrected code>
```

## See also

- <link or code>
````

Rules:

1. The sections appear in exactly this order. `Summary`, `Explanation`, and `Example` are always present. `Fix` is present if the condition has a source-level correction. `See also` is present if there are related codes or sections.
2. The heading is the code. The metadata table has exactly the columns shown. For a lint, the `Severity` column holds the default level (`allow`, `info`, `warn`, or `deny`) instead of a severity.
3. For a deprecated or removed code, the line `> **Deprecated:** use \`NEW_CODE\` instead.` or `> **Removed** in <version>.` follows the table, separated by one empty line.
4. The `Example` block is a complete module that, compiled on its own (with `sai.package` defaults), produces the code at least once. The `Fix` block is the same module corrected, and it produces no diagnostic. The conformance suite ([15-conformance-and-testing.md](15-conformance-and-testing.md)) compiles both. Both blocks are formatter-canonical ([14-formatter.md](14-formatter.md)).
5. Codes about manifests, lock files, or the command line use ```` ```text ```` blocks that show the input and the command, instead of ```` ```sai ````.

The human output is this Markdown text. A terminal renderer MAY add styling, but MUST NOT change the text when standard output is not a terminal. With `--format json`, the stream has one result record (§11.9.4):

```json
{"type":"result","kind":"explain","data":{"code":"UNKNOWN_FIELD","class":"hard","severity":"error","default_level":null,"stage":1,"owner":"spec/06-expressions-and-statements.md","since":"0.1.0","status":"active","superseded_by":null,"summary":"A field access names a field that the record type does not have.","markdown":"# UNKNOWN_FIELD\n..."}}
```

---

## 11.14 Mapping to the Language Server Protocol

The LSP server ([12-semantic-tooling.md](12-semantic-tooling.md) §12.10) publishes the same diagnostics as `sai check`, after the same level resolution, dependency cap, cascade suppression, and deduplication. The `--max-errors` limit does not apply. Each diagnostic maps to an LSP `Diagnostic` as follows:

| LSP field | Value |
|---|---|
| `range` | the primary location, converted per §11.5.3. A diagnostic with `primary: null` is published on `sai.package` at range `0:0–0:0`. |
| `severity` | `error` → 1, `warning` → 2, `info` → 3. Hint (4) is not used. |
| `code` | the code string |
| `codeDescription.href` | `https://sai-lang.org/explain/<CODE>`. Offline clients get the same text through the `sai/explain` request ([12](12-semantic-tooling.md) §12.5). |
| `source` | `"sai"` |
| `message` | the main message. If `primary_label` is non-empty, it is appended after one LF. Each child follows after one LF, as `<kind>: <message>`. |
| `tags` | `[1]` (Unnecessary) for `UNUSED_VARIABLE` and the other `UNUSED_` lints; `[2]` (Deprecated) for uses of deprecated items |
| `relatedInformation` | one entry per label, in label order, with the label's message. Then one entry per child that has a location. Then one entry per chain step of `related` that has a location, with the message of its rendered line (§11.8). |
| `data` | `{ "node": ..., "symbol": ..., "fixes": [...], "related": [...] }`, using the JSON forms of §11.9 |

Code actions are computed from `data.fixes`. Each fix becomes one `CodeAction` with `kind` `quickfix`, `title` equal to the fix title, and `isPreferred` true for the first machine-applicable fix of a diagnostic. Its `edit` is the fix's edits as a `WorkspaceEdit`. A fix with placeholders uses snippet syntax (`${1:label}`) if the client supports snippet edits, and otherwise inserts the label text. The "fix all" action (`source.fixAll.sai`) applies machine-applicable fixes with the algorithm of §11.7.5, restricted to one document.

---

## 11.15 Decisions introduced by this chapter

These rules fill gaps that [design/decisions.md](../design/decisions.md) leaves open. They use provisional IDs until they are merged into the decision record.

| ID | Rule | Rationale |
|---|---|---|
| N-H-1 | Codes, their stability rules (§11.2, §11.12), the span encoding (§11.5), the JSON stream format (§11.9), and the explanation record (§11.13.2) are normative for every conforming tool. The human layout, colors, candidate selection, and cascade rules are normative only for the reference implementation. | Tools in an ecosystem exchange machine-readable diagnostics. The exchanged form must be fixed (P4, P8), while presentation can vary without harm (P7). |
| N-H-2 | Codes match `^[A-Z][A-Z0-9]*(_[A-Z][A-Z0-9]*)+$`, have 2–6 words and at most 40 characters, name the problem rather than the fix (except `NO_`), use the leading-word vocabulary of §11.2.2 and only the listed abbreviations, and carry no numeric, phase, or package prefix. `INTERNAL_` and `SAI_` are reserved. | Codes are read by people and searched in code and logs. A uniform scheme makes them predictable (P8) and boring (P9). Phase prefixes would leak the compiler's structure into a stable interface (P6). |
| N-H-3 | There are three severities, `error`, `warning`, and `info`. Notes and help are child kinds, not severities. There is no fatal severity, and a crash is `INTERNAL_COMPILER_ERROR` with exit code 101. | A small closed set (P7). Separating children from severities keeps counts meaningful (P8). |
| N-H-4 | After level resolution, lint occurrences in packages other than the root package and the workspace members are dropped. Hard diagnostics and package policy violations are never dropped. | A new lint must not break a downstream build of unchanged code (P6). The package author, not the user, is the one who can act on a lint (P1). |
| N-H-5 | Candidates are names at Damerau–Levenshtein distance at most `max(1, ⌊n/3⌋)` from a name of `n` characters. If the full set has at most 5 names, all are listed. At most 10 are shown, in distance order, with ties broken by declaration order. | Deterministic and simple (P8, P9), and close to established practice. |
| N-H-6 | Byte offsets count the raw bytes of the file, including a BOM and CR characters. | Offsets must index the file a tool actually reads (P9). Normalized offsets would make edits off by one in CRLF files. |
| N-H-7 | Locations name files by a `source_ref` with a workspace-relative path, a `dep:<ns>:<pkg>@<version>/<path>` path for dependencies, or `<stdin>`. Absolute paths never appear. | Output must not depend on where the workspace or the cache is on disk, so that it can be compared across machines (P8). |
| N-H-8 | The human layout of §11.10 replaces the layout of draft §60. The draft fields move into `data`, and the draft's `fix` text becomes a structured fix. | The draft layout mixed machine fields into human text. Separating them keeps both forms stable (P3, P8). |
| N-H-9 | Fixes have one of three applicabilities: `machine-applicable`, `maybe-incorrect`, or `has-placeholders`. Only the first is applied by `--fix` and "fix all". | Automatic changes must preserve meaning (P9). The user decides when the meaning may change (P1). |
| N-H-10 | A fix's edits are computed by formatting the smallest enclosing format unit, and they change nothing outside it. | Fixes must keep canonical files canonical (D-TOOL-04) without rewriting unrelated text (P8). |
| N-H-11 | Effect sets in generated text use the canonical order of spec/08 §8.2.3 (N-D-2). The formatter never reorders effects written by the user, and a fix reorders them only if reordering is its stated purpose. | The order of a written set carries no meaning (spec/08 §8.2.3), so changing it is not formatting (D-TOOL-04). Generated text must be deterministic (P8). |
| N-H-12 | `related` carries typed explanations (`effect_chain`, `capability_chain`, `borrow_conflict`, `use_after_move`, `requirement_chain`, `declared_here`) with at most 32 chain steps, shortened to the first 16, a marker, and the last 15. | Why-chains are central to SAI's predictability (P8). A typed form lets tools render them, and a bound keeps output finite. |
| N-H-13 | The JSON form is an NDJSON stream on standard output of exactly one header, diagnostic records, result records, and one summary. It is serialized deterministically, with schema member order and minimal escaping, and nothing is written to standard error. It is versioned by an integer, and consumers ignore unknown members. | Streams can be consumed incrementally and survive interruption. Byte-identical output makes golden tests possible (P8, P9). |
| N-H-14 | Diagnostics are ordered by location class, package rank, file rank, start byte, end byte, severity, code, message, and serialization. | Output must not depend on scheduling (P8). |
| N-H-15 | `--max-errors` defaults to 50 and limits rendered errors only. 0 means no limit. Counts, exit codes, and fixes do not depend on the limit. | Limiting output must not change results (P8). 50 keeps a terminal usable without hiding much. |
| N-H-16 | Cascade suppression uses poison facts (rules C1–C6 of §11.11.4) and phase gating. Suppressed diagnostics are counted in the summary. | The first error is the actionable one (P9). Counting what was suppressed keeps it visible (P1). |

---

## 11.16 Diagnostics

Codes defined by this chapter, and codes from other documents that this chapter gives a role. Codes marked "owned by" are defined elsewhere and only referenced here.

| Code | Severity | Description |
|---|---|---|
| `INTERNAL_COMPILER_ERROR` | error | defined in [01-introduction.md](../spec/01-introduction.md) §1.10; an internal invariant of the toolchain was violated or a component panicked. This chapter defines how it is reported, with exit code 101 (§11.4.6). |
| `UNKNOWN_DIAGNOSTIC_CODE` | error | `sai explain` was given a name that is not a registered code (§11.13.1) |
| `NOT_A_LINT` | error | defined in [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.15.4: a level attribute, a `lints` entry, or a CLI lint flag names a hard code. This chapter defines the registry lookup (§11.12.3). Also used by [02-frontend-ast-hir.md](02-frontend-ast-hir.md). |
| `DEPRECATED_DIAGNOSTIC_CODE` | warning | a level attribute, a `lints` entry, or a CLI lint flag names a deprecated or removed code (§11.12.2, §11.12.3) |
| `UNKNOWN_LINT` | error | defined in [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.15.4: a level attribute, a `lints` entry, or a CLI lint flag names a code that is not registered. This chapter defines the registry lookup (§11.12.3). Also used by [package/01-package-contract.md](../package/01-package-contract.md) §1.13 and [02-frontend-ast-hir.md](02-frontend-ast-hir.md). |
| `UNKNOWN_FIELD` | error | defined in [spec/06](../spec/06-expressions-and-statements.md) §6.6.1; used in the examples of §11.6.4 and §11.9.6 |
| `MISSING_EFFECTS_DECLARATION` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; used in the example of §11.10.9 and in cascade rule C5 |
| `PUBLIC_EFFECTS_INFERRED` | warning (lint) | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.9.3; used in cascade rule C5 |
| `UNUSED_VARIABLE` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.18.1; used in the examples of §11.3.4 and §11.14 |
| `NAMING_CONVENTION` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.18.5; its fix is a structured `rename_symbol` operation (§11.7.3) |

### Implementation update: IR-251

`sai_package::mvs_diagnostics::divergent_sources_diagnostic` now builds `DIVERGENT_SOURCES` from an audit-produced reload comparison, and `divergent_sources_diagnostics` collects all divergent selected versions. The diagnostic shows both source clauses and their selecting requirement edges or root overrides, labels the read-time and final choices as distinct resolved source trees, includes both contract edge sets, and lists the exact relative path requests whose resolved identities differed. Manifest source clauses and override values are retained with their spans and checked before those spans are displayed, preventing stale selections from pointing at changed source text. Root override provenance also carries the root package version, so a same-span override in another root cannot be mistaken for the original selection. A shared comparison token keeps each path result bound to its exact edge comparison. The diagnostic omits canonical identity values to avoid exposing absolute local paths. Final source-plan entries now retain source clauses that did not select each selected dependency's final source for package reviews. `sai_driver::RootDatabase::mixed_source_divergent_diagnostics` binds those comparisons to one MVS load and checks that every workspace manifest parse matches the contract used by that load. `mixed_source_identity_diagnostics` uses the same load and manifest binding to report classified final-source canonicalization failures with the selecting source location and requirement chain; `mixed_source_reload_diagnostics` reports direct selected-package identity/version mismatches with their original MVS requirement chains. Other reload failures remain on the audit plan until final-manifest source locations can be bound safely. Human/JSON review formatting, CLI integration, and lock generation remain pending.

`sai_package::mvs_diagnostics::mixed_source_identity_failure_diagnostics` reports final-side canonicalization failures that have a package diagnostic classification. It omits read-time and internal consistency failures, and keeps unclassified local filesystem errors available through the audit plan. Local missing-path and missing-manifest failures use `PATH_PACKAGE_NOT_FOUND`; invalid paths and Git identity/fetch errors retain their resolver classifications. The driver binds these diagnostics to the exact mixed-source load and the original root and dependency manifest bytes before attaching source spans.

`mixed_source_reload_failure_diagnostics` and `RootDatabase::mixed_source_reload_diagnostics` report classified final reload failures for already-selected packages. Direct package identity/version mismatches use `PACKAGE_IDENTITY_MISMATCH`; safely classified local path, package-size, path-source conflict, and Git path/fetch failures use context-appropriate resolver codes. Exact-commit Git path loads map release-tag-only error kinds to `FETCH_FAILED`. For local `PathRequirementDiscovery` failures and classified Git path-discovery failures, the audit plan retains the exact final `sai.package` bytes and the originating package, path, and requirement span. The driver adds that file to an augmented workspace snapshot under a diagnostic-only synthetic dependency path, then validates the parsed package, path, and exact requirement span before using it as the primary location. The original MVS chain remains attached to its original manifest entries in the same snapshot. `RootDatabase::mixed_source_reload_diagnostics` returns `MixedSourceReloadDiagnostics`, which carries both the diagnostics and the augmented snapshot so callers can resolve every file ID. Messages use bounded typed details or fixed text and omit absolute paths, Git URLs, commits, checksums, and the failing relative path. Final syntax, header, semantic contract, and contract-audit failures whose useful location is inside a changed final manifest remain on the audit plan until their exact source snapshots can be bound safely.
