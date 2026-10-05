# SAI Documentation

This directory contains the language specification, the implementation specification, the package system specification, and the design records of **SAI**, a Wasm-first systems language.

The original language draft (`specification.txt`, v0.1 Draft, in Japanese) is not part of this repository. The documents here turn that draft into a precise, implementable specification. Where the draft was ambiguous or incomplete, the decisions are recorded in [design/decisions.md](design/decisions.md). Every place where this specification deliberately departs from the draft is listed in [design/draft-reconciliation.md](design/draft-reconciliation.md).

---

## 1. Document map

### Design records

| Document | Contents |
|---|---|
| [design/decisions.md](design/decisions.md) | **Design Decision Record.** Every open question of the draft, decided under a stable ID (`D-<AREA>-<NN>`) with its rationale. Highest authority among these documents. |
| [design/draft-reconciliation.md](design/draft-reconciliation.md) | Every difference between the draft and this specification, with the reason and the corrected form. |
| [design/decision-index.md](design/decision-index.md) | Generated index of the detailed decisions (`N-<group>-<n>`) that the other documents record in their decisions sections, with the defining document and section. |

### Language specification (`spec/`)

| Chapter | Contents |
|---|---|
| [01-introduction.md](spec/01-introduction.md) | Scope, conformance, notation, normative terms, stages |
| [02-lexical-structure.md](spec/02-lexical-structure.md) | Source text, tokens, literals, NEWLINE generation |
| [03-grammar.md](spec/03-grammar.md) | Complete EBNF grammar |
| [04-modules-and-names.md](spec/04-modules-and-names.md) | Packages, modules, `use`, visibility, name resolution |
| [05-types.md](spec/05-types.md) | Type system: primitives, compound types, declarations, generics, traits, coercions, inference |
| [06-expressions-and-statements.md](spec/06-expressions-and-statements.md) | Typing and evaluation of every expression and statement; patterns; match |
| [07-ownership-and-borrowing.md](spec/07-ownership-and-borrowing.md) | Moves, copies, borrows, regions, elision, borrow checking rules, drop, arenas |
| [08-effects-and-capabilities.md](spec/08-effects-and-capabilities.md) | Effect system, latent effects, inference, capability policy |
| [09-contracts-and-refinements.md](spec/09-contracts-and-refinements.md) | Refined types, `requires` / `ensures`, invariants, the Standard Fact Checker |
| [10-errors-and-panics.md](spec/10-errors-and-panics.md) | `Result`, `?`, panics, traps |
| [11-async-and-concurrency.md](spec/11-async-and-concurrency.md) | `async`, `Future`, `Stream`, structured concurrency (Stage 5) |
| [12-unsafe-and-ffi.md](spec/12-unsafe-and-ffi.md) | `unsafe`, raw pointers, `extern "wasm"`, `extern "C"` |
| [13-components-and-wit.md](spec/13-components-and-wit.md) | Interfaces, worlds, resources, WIT mapping (Stage 4) |
| [14-testing.md](spec/14-testing.md) | `test`, `property`, generators |
| [15-attributes-and-lints.md](spec/15-attributes-and-lints.md) | The closed set of attributes; lints and lint levels |
| [16-core-library.md](spec/16-core-library.md) | `core` and `alloc`: prelude, lang items, traits, desugarings |
| [appendix-a-diagnostic-codes.md](spec/appendix-a-diagnostic-codes.md) | Catalog of every diagnostic code |

### Implementation specification (`impl/`)

| Chapter | Contents |
|---|---|
| [01-architecture.md](impl/01-architecture.md) | Compiler pipeline, crates, query system, incrementality |
| [02-frontend-ast-hir.md](impl/02-frontend-ast-hir.md) | Lexer, parser, lossless CST, AST, HIR lowering |
| [03-name-resolution-and-type-checking.md](impl/03-name-resolution-and-type-checking.md) | Resolver, type inference, trait solving, method resolution |
| [04-mir.md](impl/04-mir.md) | MIR data structures, construction, phases, invariants |
| [05-borrow-checker.md](impl/05-borrow-checker.md) | Borrow checking algorithm on MIR |
| [06-wasm-lowering-and-layout.md](impl/06-wasm-lowering-and-layout.md) | Memory layout, calling convention, MIR to Wasm |
| [07-component-lowering.md](impl/07-component-lowering.md) | Canonical ABI lifting and lowering, component assembly |
| [08-effect-and-capability-analysis.md](impl/08-effect-and-capability-analysis.md) | Effect inference and capability checking algorithms |
| [09-optimization-and-size.md](impl/09-optimization-and-size.md) | Optimization passes, DCE, size attribution (`sai size`) |
| [10-runtime.md](impl/10-runtime.md) | Allocators, panic handling, shadow stack, async runtime support |
| [11-diagnostics.md](impl/11-diagnostics.md) | Diagnostic model, JSON format, structured fixes |
| [12-semantic-tooling.md](impl/12-semantic-tooling.md) | Symbol identity, semantic graphs, semantic operations, LSP, semantic diff |
| [13-cli.md](impl/13-cli.md) | The `sai` command line |
| [14-formatter.md](impl/14-formatter.md) | Canonical formatting rules |
| [15-conformance-and-testing.md](impl/15-conformance-and-testing.md) | Conformance suite and compiler testing strategy |
| [16-interfaces-and-wit.md](impl/16-interfaces-and-wit.md) | WIT loading, SAI views, interface, impl and world checks, flags, generated WIT |

### Package system (`package/`)

| Chapter | Contents |
|---|---|
| [01-package-contract.md](package/01-package-contract.md) | The `sai.package` language |
| [02-resolver.md](package/02-resolver.md) | Version resolution (MVS) |
| [03-lock-file.md](package/03-lock-file.md) | The `sai.lock` format and checksums |
| [04-distribution-and-security.md](package/04-distribution-and-security.md) | Git fetching, artifacts, hybrid packages, cache, dependency security |

### Targets

| Document | Contents |
|---|---|
| [targets/targets-and-profiles.md](targets/targets-and-profiles.md) | Targets, target capabilities, build profiles, entry points |

### Other

| Document | Contents |
|---|---|
| [roadmap.md](roadmap.md) | Implementation stages and milestones |
| [glossary.md](glossary.md) | Terms used across the documents |

---

## 2. Normativity

- Documents marked **Status: Normative** define requirements. Documents marked **Status: Informative** explain, give examples, or describe a recommended implementation.
- Within a normative document, examples, notes, and rationale paragraphs are informative unless stated otherwise.
- Order of authority when documents conflict: `design/decisions.md` > `spec/*` > `package/*` and `targets/*` > `impl/*`. A conflict is a bug in the documents; report it and fix the lower-authority document, or amend the decision record.
- The implementation specification (`impl/*`) is normative for the reference implementation where it says so, for example the MIR definition, the memory layout of a given compiler version, or the diagnostic JSON format. Other implementations only have to match the language specification's observable behavior.

## 3. Requirement keywords

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**, and **OPTIONAL** are to be interpreted as described in RFC 2119 and RFC 8174 when, and only when, they appear in all capitals.

When a rule says something "is an error" and names a diagnostic code (for example `CLAUSE_ORDER`), a conforming compiler MUST reject the program and SHOULD report that code.

## 4. Stages

Features are tagged with the implementation stage of draft §84:

| Stage | Contents |
|---|---|
| 0 | lexer, parser, AST, basic type checker, formatter |
| 1 | HIR, MIR, wasm32-core backend, integers, booleans, functions, structs, enums, `Option` / `Result` |
| 2 | ownership, borrow checker, generics, traits, dead-code elimination, tiny profile |
| 3 | effects, capabilities, package system, `sai.package`, `sai.lock`, Git resolution, tests |
| 4 | WIT generation and import, Component Model, resources, worlds, WASI |
| 5 | async, `Future`, `Stream`, WASI async mapping |
| 6 | semantic protocol, structured edits, impact analysis, semantic diff |
| 7 | native backend, C FFI |

**v0.1 conformance** covers Stages 0 through 4, plus structured diagnostics (draft §85). Features of Stages 5–7 are fully specified, so that the v0.1 design does not have to change later, but a v0.1 compiler is not required to implement them. A compiler that does not implement a staged feature MUST reject programs that use it with `FEATURE_NOT_IMPLEMENTED`, naming the stage ([spec/01](spec/01-introduction.md) §1.4.5).

## 5. Notation

- Grammar rules use the EBNF notation defined in [spec/03-grammar.md §3.1](spec/03-grammar.md).
- SAI code examples are in fenced blocks tagged `sai`. Examples that are meant to be rejected say so in a comment and name the diagnostic code.
- Typing rules use the notation `Γ ⊢ e : T ! E`: in environment Γ, expression `e` has type `T` and effects `E`. Each chapter that uses judgments defines its own forms.
- Regions are written `'a`, `'b` in rules and explanations. They never appear in SAI source.
- Section references to the draft are written "draft §N".

## 6. Language of the documents

All documents are written in English. The draft remains in Japanese and is the historical source. This specification replaces it.
