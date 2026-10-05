# Implementation Decisions for Review

**Status:** Informative. This log records implementation choices that need human review; it does not amend the normative language design.

## Open for review

### IR-001: Build the reference implementation incrementally from Stage 0

- **Decision:** Start with source locations and diagnostics, then implement the Stage 0 frontend and tools in dependency order. Do not create placeholder crates for later phases before they have a concrete implementation.
- **Reason:** The repository initially contained only specifications. A working, testable vertical slice gives each later phase a real foundation and avoids a workspace that appears complete while containing empty compiler phases.
- **Review:** Confirm this incremental crate rollout is the preferred interpretation of the fixed workspace architecture in `docs/impl/01-architecture.md` §1.3.

### IR-002: Use Unicode data crates for normative identifier handling

- **Decision:** Pin `unicode-ident` 1.0.18, `unicode-normalization` 0.1.24, and `unicode-general-category` 1.1.0, whose tables all use Unicode 16.0.0. The lexer uses them for XID properties, NFC checks, and Lu/Lt case classification. Permit these crates and `unicode-normalization`'s `tinyvec` dependency in the shipped dependency list.
- **Reason:** Rust's standard library does not expose the required XID, NFC, and General Category operations. Pinning one Unicode data version makes identifier behavior portable and reproducible; implementing partial tables locally could accept or reject names inconsistently.
- **Review:** Confirm the Unicode 16.0.0 baseline for v0.1. Future data upgrades must update all three crates together, amend the lexical spec and dependency allowlist, and rerun Unicode-focused conformance tests.

### IR-003: Use crate tests as the development gate before conformance exists

- **Decision:** Until the conformance suite and harness exist, gate each implementation milestone on its focused crate tests, workspace tests, formatting, and an independent adversarial review. Do not make a language conformance claim during this period.
- **Reason:** The documented merge gate requires a complete Stage 0–4 claim, but the repository has no test suite, harness, compiler, or executable yet. The full gate cannot be meaningful for the initial implementation commits.
- **Review:** Decide whether the eventual CI should add an explicit bootstrap gate and at what point it should switch to the normative conformance gate in `docs/impl/15-conformance-and-testing.md` §15.12.6.

### IR-004: Defer Stage 0 package discovery semantics until the CLI milestone

- **Decision:** Implement the frontend independently of package discovery first. Before implementing `sai check` or `sai new`, reconcile the Stage 0 command requirements with the Stage 3 package contract and Stage 4 default target.
- **Reason:** The architecture needs package identity and a source directory to load files, while the roadmap assigns the full package contract to Stage 3. Implementing package resolution early would either duplicate the later package system or silently create an incompatible subset.
- **Review:** Define the Stage 0 behavior for reading `sai.package`, external dependencies, and the `sai new` template before claiming Stage 0 conformance.

### IR-005: Keep diagnostic JSON values internal and deterministic

- **Decision:** Represent diagnostic data with an internal JSON value enum and ordered maps; do not add `serde_json` to the shipped compiler.
- **Reason:** `docs/impl/11-diagnostics.md` models `data` with `serde_json::Map`, while `docs/impl/01-architecture.md` §1.3.2 rule 4 limits shipped dependencies and explicitly places `serde_json` among test-only dependencies. Ordered maps also make output ordering explicit and deterministic.
- **Review:** Confirm the diagnostic chapter should describe this internal value model, with the specified deterministic JSON form produced by the compiler's own writer.

### IR-006: Split structural fix validation from syntax-aware planning

- **Decision:** `sai_diagnostics` validates operation schemas and structural edit constraints. A `FixOperationPlanner` must provide both the operation's resulting file bytes and the minimal text edits; `Fix::new` applies those edits to the snapshot and rejects the fix unless both results match exactly. Parsing, node containment, CST rewriting, and format-unit formatting belong to the syntax-aware planner. Non-built-in effect sets require a `CanonicalEffectSet` populated from resolved identities; duplicate atom identities and ambiguous output spellings are rejected.
- **Reason:** The fixed crate graph places `sai_diagnostics` below `sai_syntax` and `sai_semantic`; adding either dependency here would create a cycle. The planner boundary lets the lower diagnostics layer check semantic/text parity without duplicating CST logic. Resolved effect identity is a trusted compiler-internal input because the resolver is above this crate.
- **Review:** Confirm the eventual semantic planner is the only production implementation of `FixOperationPlanner`, derives identities from actual resolution, and tests each operation against both result channels before exposing fixes.

### IR-007: Bind fixes and diagnostics to immutable source snapshots

- **Decision:** A `Fix` captures its complete primary `Location` and an opaque VFS snapshot ID. Construction checks the primary and every edit's file identity, canonical path and package identity, per-document version, raw-file bounds, UTF-8 boundaries, and CR LF boundaries against the same immutable source set. A `Diagnostic` retains that source snapshot, validates all primary, secondary, related, lint-level, and cause spans when bound, and cannot be rebound after fixes are attached.
- **Reason:** Diagnostic spans and edits use raw file byte offsets, while the frontend uses decoded-text offsets; the VFS retains raw bytes and provides explicit decoded-to-original range conversion. Document versions are per-file, so only the opaque ID can bind a multi-file set without comparing unrelated version numbers.
- **Review:** This VFS value currently represents the consistent file/document set only. Extend its session identity to include the package graph, lock state, target, profile, test options, and other §12.1.2 inputs when the package and query layers are implemented; the driver must create a fresh identity whenever any such input changes. Verify the producer also includes all required package and manifest documents, and keep path/version parsing aligned with the package crate.

### IR-010: Keep effect identities as trusted semantic inputs

- **Decision:** `CanonicalEffectSet` validates uniqueness and ordering from the identities and output spellings supplied to it; the type does not independently resolve source names or authenticate identity strings.
- **Reason:** Name resolution and WIT identity are owned by higher crates, and importing those crates into `sai_diagnostics` would violate the dependency graph. This value is an internal handoff from the semantic resolver, not a parser for untrusted external identity claims.
- **Review:** Keep construction at the resolver/operation-planner boundary and ensure production callers never synthesize identity strings from source spelling or aliases.

### IR-008: Start with a copy-only stable-address arena

- **Decision:** Keep `Arena<T: Copy>` as thread-safe stable storage and add `TypeArena<T: Copy + Eq + Hash>` for structural interning. `TypeArena` returns the same stable address for equal values with consistent `Hash`/`Eq`, including concurrent calls; a separate append-only Box owner keeps every returned address live even if a key violates that trait contract.
- **Reason:** The type arena needs pointer equality to mean structural equality and must allow query workers to intern in parallel. Both types restrict entries to `Copy` to avoid destructor-order hazards for values that may refer to other arena allocations; the mutex-backed implementation favors a simple safety argument over allocation throughput.
- **Review:** Benchmark contention and memory use against compiler workloads. If future HIR/type values require destructors, design and test a destructor-safe arena before storing them.

### IR-009: Use a safe radix tree for lock-free interner reads

- **Decision:** The append-only symbol vector uses a sparse radix tree of `OnceLock` slots. Interning initializes a slot once; resolving a published symbol reads initialized slots without taking a lock.
- **Reason:** This satisfies the documented lock-free read behavior without an external atomic-pointer crate or unsafe append-only vector access. Insertions still serialize briefly while assigning dense session IDs.
- **Review:** Measure memory and lookup costs under the symbol counts and concurrency expected by the reference compiler; retain this representation only if it meets the stated compilation targets.

### IR-011: Require dependency source references to agree on package identity

- **Decision:** A `dep:<identity>/<path>` source path is accepted only when the separate `package` field is exactly the same `<identity>`.
- **Reason:** The diagnostic source-reference contract carries both fields, and allowing them to name different packages can misattribute a diagnostic while still passing snapshot validation.
- **Review:** Confirm this equality rule matches every dependency and embedded-library source reference produced by the driver.

### IR-012: Bound numeric prerelease identifiers by `u64`

- **Decision:** Treat a purely numeric SemVer prerelease identifier as `Num` for the resolver rule, and reject it if it exceeds `u64`; clarify the package-contract wording to match.
- **Reason:** The resolver grammar says every `Num` fits in `u64`, while the package lexer summary previously omitted the bound for prerelease components.
- **Review:** Confirm the package parser, resolver, and lock reader all share this same version rule.

### IR-013: Keep source text and offset metadata immutable

- **Decision:** Make `SourceFile` identity, decoded text, BOM state, and line index private; expose read-only accessors. Reject decoded text that starts with an unrecorded BOM.
- **Reason:** Decoded-to-original offset conversion depends on those fields matching the retained raw bytes. Publicly replaceable fields and an unrecorded leading BOM allowed callers to invalidate that mapping after construction.
- **Review:** Check downstream crates use the read-only API and do not need to construct or mutate partial source snapshots.

### IR-014: Keep lexical query diagnostics independent of opaque snapshot IDs

- **Decision:** Lexical diagnostics carry the source path, package, document version, and validated raw span, but remain unbound to the VFS snapshot object. `lex()` preserves that query value; `lex_with_fixes()` binds at the snapshot-aware boundary and plans token-level fixes for normalization, unnecessary raw prefixes, lone `!`, and semicolons. Before binding, it confirms the candidate file's raw bytes exactly match the bytes lexed.
- **Reason:** The opaque snapshot ID is a freshness token, not lexical input. Retaining it in a query value would make equal source text produce unequal diagnostics across otherwise identical snapshots and defeat deterministic incremental equality. Comparing immutable source bytes at the later boundary prevents stale fix intents from being rebound to changed content.
- **Review:** Ensure the diagnostic collection boundary binds or validates every location before applying snapshot-sensitive operations, and that future full-session snapshots cover all semantic inputs. Extend token-level planning to the remaining lexical fixes as their parser operations are implemented.

### IR-015: Keep forbidden-character names in a closed local table

- **Decision:** Map every code point admitted by the three forbidden-character classes to its Unicode name in `sai_syntax`; include both `U+XXXX` and the name in a diagnostic note.
- **Reason:** The language defines a finite forbidden set, and a closed table avoids a runtime Unicode-name dependency while keeping diagnostic spelling stable across Rust toolchains.
- **Review:** Check the table against the Unicode 16.0.0 source data and retain coverage for every code point admitted by `forbidden_class`.

### IR-016: Preserve literal boundaries for malformed UTF-8

- **Decision:** When a decoded U+FFFD replacement comes from malformed UTF-8 inside a quote-delimited literal, keep the enclosing `STRING`, `BYTE_STRING`, `CHAR`, or `BYTE` token and set `HAS_ERROR`. Outside literals, emit an `ERROR` token. Report `INVALID_UTF8` once from decoding and do not add a second literal diagnostic for the same bytes.
- **Reason:** Splitting a quote-delimited literal around a replacement character would destroy the token boundary the parser uses to recover it as one expression. The literal token's existing `HAS_ERROR` contract lets later phases suppress value-dependent diagnostics while preserving the source structure.
- **Review:** Confirm the parser, AST literal accessors, and HIR lowering all honor `HAS_ERROR` and never interpret a replacement-containing literal as valid. Revisit if conformance requires a distinct `ERROR` token inside literals.

### IR-017: Check indentation on whitespace-only multiline-string lines

- **Decision:** Treat every non-empty multiline-string content line, including a line containing only spaces or tabs, as subject to the base-indentation prefix check. Use the exact bytes before the closing delimiter as the required prefix; additional indentation after that prefix is allowed.
- **Reason:** The lexical specification exempts empty lines and requires every non-empty line to start with the base indentation. Applying that wording literally avoids silently exempting lines that contain source bytes and keeps indentation removal deterministic.
- **Review:** Confirm whitespace-only lines should follow the same prefix rule as lines with content, and confirm that base indentation is a required prefix rather than an exact count of leading whitespace.

### IR-018: Keep green-tree operations safe for deep syntax

- **Decision:** Compute each green node's structural hash from its children, compare nodes and find their last token iteratively, and empty uniquely owned child lists iteratively during destruction. Intern nodes with at most three children by structural child values; leave larger nodes uncached.
- **Reason:** Left-associative expressions can produce trees whose depth grows with token count even when parser nesting is bounded. Recursive hashing or destruction can overflow the native stack. Address-only cache keys avoid recursive work but fail to intern equal parents around structurally equal, uncached larger children; cached child hashes keep ordinary construction linear while structural equality resolves collisions exactly.
- **Review:** Retain the 10,000-level equality, hashing, token lookup, and drop regression test. Profile the cached-hash and small-node-key costs on representative source files before expanding interning to larger nodes.

### IR-019: Preserve lexer token discriminants when adding syntax nodes

- **Decision:** Append CST node kinds after `EOF` in the `#[repr(u16)]` `SyntaxKind` enum, and treat `ERROR` as both a raw token kind and a recovery node kind.
- **Reason:** Existing lexer token discriminants remain unchanged for consumers that store or compare their numeric representation. `ERROR` is emitted for malformed source and is also the documented wrapper for parser recovery.
- **Review:** Keep `is_token()` tied to the token range ending at `EOF` and keep `is_node()` explicitly admitting `ERROR`; update the boundary and error-tree tests when adding token kinds or recovery nodes.

### IR-020: Permit zero-child nodes only for recovery errors

- **Decision:** Reject every zero-source-width green node except `ERROR` and the root `SOURCE_FILE` of an empty file. Delimited list and block nodes must contain at least one real opening or closing delimiter token.
- **Reason:** An empty file necessarily has a zero-width root and EOF, while a semantically empty delimited list still has source delimiters. Synthetic NEWLINE or EOF tokens cannot stand in for the list's source text.
- **Review:** Keep coverage for the empty-file root, a zero-width `ERROR`, an empty `ARG_LIST` with and without unrelated text, and a correctly delimited empty list. When parser recovery is added, ensure missing delimiters are represented without manufacturing source text.

### IR-021: Index red tokens by source text range

- **Decision:** Build a root-wide green-node/token interval index lazily on the first `token_at_offset` call, then use binary search within the queried node's token interval. Create red node handles only for the returned token's parent path. Ignore zero-width synthetic `NEWLINE` tokens but keep zero-width `EOF` addressable.
- **Reason:** Several synthetic tokens can share one source offset. Returning the first two inclusive matches can hide EOF, while scanning descendants from the start makes repeated editor lookups quadratic. NEWLINE has no source text to point into; EOF is useful at the end-of-file position.
- **Review:** Keep tests for adjacent token boundaries, trivia gaps, an EOF sharing an offset with NEWLINE, lookup scoped to a descendant, and sorted index construction. Measure the one-time memory cost on large files.

### IR-022: Make red-tree diagnostics and teardown shallow

- **Decision:** Format red nodes as a shallow summary and iteratively release parent chains. The offset index stores only green metadata until lookup; it then caches strong red handles by weak-free node IDs, with no cache back-reference in `NodeData`.
- **Reason:** Derived debug output recursively follows green children or red parents, and a strong red-parent chain can recurse during destruction. Caching lazily materialized red paths avoids repeated deep reconstruction without making the entire red tree eager; keeping `NodeData` independent of its cache prevents a reference cycle.
- **Review:** Retain deep debug/drop regression tests and verify that token-indexed red nodes release cleanly both when handles outlive the root wrapper and when the cache is dropped.

### IR-023: Reject invalid parser token streams and marker lifecycles

- **Decision:** Validate that parser input contains only raw token kinds followed by one final EOF; require ordered, in-bounds UTF-8 token spans, require NEWLINE and EOF to be zero-width, and require EOF to be at the end of the source. Require every marker to complete or abandon in LIFO order, accept only node kinds at completion, undo a child's forward-parent link when its wrapper is abandoned, and return an error if events leave tokens unconsumed.
- **Reason:** The tree builder consumes a lossless, structurally valid event stream. Catching invalid inputs and marker misuse at the parser boundary prevents false progress, mismatched node spans, and successful event extraction that can only fail during tree construction.
- **Review:** Keep regression tests for composite kinds in raw input, zero-width ordinary tokens, nonempty NEWLINEs, malformed or out-of-order spans, out-of-order completion, abandoning a preceding marker, invalid completion kinds, unfinished markers, and EOF consumption. The parser validates token range order and UTF-8 boundaries; the tree builder remains responsible for proving that tokens and trivia partition the source exactly.

### IR-024: Bind expected-token errors to snapshot files

- **Decision:** Build `EXPECTED_TOKEN` diagnostics only when a parser is created from a `SnapshotFile`. Convert the current decoded token span back to original-file byte offsets through `Location::from_decoded_source`; emit no detached diagnostic when snapshot context is missing, and report at most once per token position.
- **Reason:** The parser reads decoded text and token ranges, while diagnostics and edits use original bytes. Snapshot-file context supplies the file path, package, document version, and decoding map required to attribute the error correctly, including BOM and malformed-UTF-8 cases.
- **Review:** Keep tests for token retention after an expectation fails, one diagnostic for repeated expectations at the same position, alternative-token messages, empty/invalid expected sets, composite-token consumption, and decoded-to-original location conversion. Revisit the one-per-position rule when implementing `UNEXPECTED_NEWLINE` and recovery diagnostics so their precedence and suppression remain deterministic.

### IR-025: Omit absent module headers and reject unimplemented item forms

- **Decision:** Do not create a zero-width `MODULE_HEADER` when a file has neither inner docs nor an `unsafe module` declaration. The current source-file parser handles module headers and use declarations; other item kinds return `UnsupportedItem` instead of being wrapped as apparently valid syntax.
- **Reason:** Green-tree invariants reject empty nodes, and treating unimplemented declarations as arbitrary tokens would create a misleading CST. The typed boundary lets the grammar grow incrementally without misclassifying valid future forms as user syntax errors.
- **Review:** Confirm the omission of an absent `MODULE_HEADER` matches AST consumers, and decide whether `UnsupportedItem` should remain an implementation error or become a diagnostic once all Stage 0 item forms are dispatched.

### IR-026: Keep attribute parsing generic and syntax-only

- **Decision:** Parse item attributes with the recursive `AttrArg` grammar, retain identifiers as `NAME_REF` nodes, and defer attribute-name, target, and argument validation to later compiler stages.
- **Reason:** The grammar explicitly makes attribute arguments generic, while the implementation plan assigns the closed name set and per-attribute rules to validation. This allows the parser to preserve unknown attributes for tooling without embedding semantic policy in syntax parsing.
- **Review:** Confirm `NAME_REF` is the expected AST representation for attribute names, keys, and word values before adding typed attribute accessors.

### IR-027: Synchronize a missing use-list close at the next line-start item

- **Decision:** When a selective-use list reaches a top-level item after a physical line break, leave that item unconsumed and report the missing `}`. Recognize the boundary through `AFTER_LINE_BREAK` when the preceding comma prevents the lexer from producing a `NEWLINE` token.
- **Reason:** Commas are not newline-ending tokens in the lexer, but they can precede a malformed list followed by a valid top-level declaration. Preserving the following declaration limits recovery to the malformed list and keeps later syntax available, including item forms whose parser is not implemented yet.
- **Review:** Check recovery with comments, attributes, visibility, and newly added top-level item starters; retain coverage for both emitted `NEWLINE` and `AFTER_LINE_BREAK` synchronization paths.

### IR-028: Demote misplaced doc comments at their parser context

- **Decision:** Emit `MISPLACED_DOC_COMMENT` warnings over the original source bytes and demote misplaced outer and inner doc tokens to line-comment trivia. Keep outer docs in `DOCS` only when attached before an item and inner docs in the module header only at file start.
- **Reason:** Placement depends on grammar context, and demotion preserves exact source text while preventing invalid documentation from appearing as attached API docs.
- **Review:** Extend placement handling as block, field, variant, and expression grammars are added; each context must distinguish valid attachment from a misplaced comment.

### IR-029: Bound the first function-parser slice and recursive grammar depth

- **Decision:** Parse top-level function signatures, generic and associated type bounds, function clauses, effectful function-pointer types, and empty bodies. Restrict associated-type bindings to immediate generic arguments of trait-reference bounds. Emit `MISSING_FN_BODY` for a free function without a body. Preserve valid syntax in `FN_DECL` when clauses are duplicated or out of order, while reporting `DUPLICATE_CLAUSE` or `CLAUSE_ORDER`; leave an unparenthesized `where` bound's following `effects` clause attached to the function. Accept `pub export` before a function as redundant visibility, retain both tokens in `VISIBILITY`, and report `REDUNDANT_PUB_EXPORT`. Accept attributes on parameters and generic parameters to retain their syntax, but report `INVALID_ATTRIBUTE_TARGET` once at the first attribute of each sequence. Until block and expression grammar exists, return `UnsupportedSyntax` at a non-empty body or a `requires`/`ensures` clause. Limit recursive type and attribute-argument parsing to 512 levels, skip the over-deep construct into an `ERROR` node, preserve a current synchronization token and the following function body, and report `IMPLEMENTATION_LIMIT_EXCEEDED` once per file.
- **Reason:** The parser should build truthful nodes for implemented syntax and avoid presenting unparsed body or contract tokens as valid expressions. The depth limit follows the frontend resource-limit contract and keeps hostile nesting from exhausting the native stack while preserving later source for recovery.
- **Review:** Add block and expression grammar before removing the `UnsupportedSyntax` boundaries; parameterize body requirements when trait, interface, and extern contexts are parsed; attach the specified machine-applicable clause-reordering fix and the delete-`pub` fix for `REDUNDANT_PUB_EXPORT` after parser diagnostics receive the workspace snapshot and CST node identity needed by the fix planner; and verify the shared depth counter covers every recursive grammar routine added later. The diagnostics crate defines the `reorder_clauses` operation, but this parser slice cannot yet attach a snapshot-validated fix to its diagnostic.
