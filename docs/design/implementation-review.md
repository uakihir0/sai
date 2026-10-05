# Implementation Decisions for Review

**Status:** Informative. This log records implementation choices that need human review; it does not amend the normative language design.

## Open for review

### IR-001: Build the reference implementation incrementally from Stage 0

- **Decision:** Start with source locations and diagnostics, then implement the Stage 0 frontend and tools in dependency order. Do not create placeholder crates for later phases before they have a concrete implementation.
- **Reason:** The repository initially contained only specifications. A working, testable vertical slice gives each later phase a real foundation and avoids a workspace that appears complete while containing empty compiler phases.
- **Review:** Confirm this incremental crate rollout is the preferred interpretation of the fixed workspace architecture in `docs/impl/01-architecture.md` §1.3.

### IR-002: Use Unicode data crates for normative identifier handling

- **Decision:** The lexer will use `unicode-ident` for UAX #31 XID properties and `unicode-normalization` for NFC checks.
- **Reason:** Rust's standard library does not expose the required XID and NFC operations. Implementing partial Unicode tables locally would risk accepting invalid identifiers or rejecting valid ones, contrary to the explicit and portable behavior required by P1, P2, and P8.
- **Review:** `docs/impl/01-architecture.md` §1.3.2 rule 4 currently limits shipped third-party crates to an enumerated set. Before these dependencies are added, amend that list and consider recording the Unicode data version used by the reference implementation.

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

- **Decision:** The first `sai_intern::Arena` stores each `Copy` value in a stable `Box` and returns a reference tied to the arena borrow. It does not accept values with destructors.
- **Reason:** This gives the query/type infrastructure stable addresses while avoiding destructor-order hazards for values that can refer to other allocations in the same arena. A per-value allocation is slower than a bump allocator but is straightforward to validate.
- **Review:** Check whether the HIR/type arena values can be represented as `Copy` handles and references. If not, design and test a destructor-safe arena before using non-`Copy` values.

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
