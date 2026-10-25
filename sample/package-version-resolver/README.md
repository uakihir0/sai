# Package Version Resolver

This sample resolves a deliberately small package graph from fixed candidate
lists. It first removes candidates that have no compatible value in a
neighboring package domain, then uses deterministic backtracking when local
consistency still leaves a globally incompatible choice. Failed branches are
discarded by copying the bounded solver state, so trying an older candidate
cannot leak removals into the next branch.

The satisfiable fixture has three packages and two candidates per package.
Candidate order is preferred-first:

| Package | Candidate order |
| --- | --- |
| Renderer | 2.0.0, 1.0.0 |
| Parser | 2.1.0, 1.8.0 |
| UnicodeTable | 2.0.0, 1.9.0 |

The highest Renderer candidate requires Parser 2.x and UnicodeTable 1.x.
Parser 2.1.0 requires UnicodeTable 2.x. Each individual pair still has a
supporting candidate before a choice is made, so propagation alone does not
spot the three-way conflict. Search tries Renderer 2.0.0 first, discovers the
conflict after propagation, rolls the branch back, and returns the hand-checked
assignment Renderer 1.0.0, Parser 1.8.0, UnicodeTable 1.9.0. The second fixture
adds a graph-wide UnicodeTable 2.x rule, making the whole problem
unsatisfiable.

## Scope

This teaching resolver models exactly three named packages, at most two
versions in each candidate domain, and at most six dependency rules. A rule
applies when its source candidate is selected; `source_version: None` means it
applies to every candidate of that source package. Rules constrain a target
using numeric three-part versions and a half-open interval:
`minimum_inclusive <= version < maximum_exclusive`. Candidate arrays already
encode deterministic preference order; the solver does not parse or sort
version strings. An input whose rule count exceeds the fixed array is rejected
with `rule_count_valid: false` before any rule indexing.

This is not the repository's package resolver. It does not implement Minimal
Version Selection, SemVer prereleases or build metadata, package identities,
sources, capability checks, lockfiles, registry lookup, transitive discovery,
or explanations for rejected candidates. It solves the fixed binary
constraints in these in-memory fixtures only.

## Files

- `src/model.sai` declares the fixed capacities and solver data.
- `src/version.sai` compares numeric versions and checks half-open ranges.
- `src/fixtures.sai` defines the backtracking example and a separate
  unsatisfiable example.
- `src/propagate.sai` enforces pairwise arc consistency to a fixed point.
- `src/search.sai` picks the smallest unresolved domain, tries candidates in
  fixture order, and reports the selected versions and search counts.
- `src/lib.sai` exports fixture entry points and contains source-level
  regression assertions.

## Review required

- Confirm that a package sample using a generic binary constraint solver is
  useful alongside the repository's actual Minimal Version Selection design.
  The sample explicitly does not claim to model that production contract.
- Confirm the chosen teaching semantics: dependencies are conditional on the
  selected source candidate, and the resolver returns one deterministic
  solution rather than enumerating all solutions or explaining conflicts.
- The algorithm is complete only for the stated fixed binary-constraint model.
  Any future extension to conditional groups, optional dependencies, package
  sources, or package-discovery rules needs a new correctness review.

## Formatting and compiler status

Run the formatter from the repository root:

```sh
sai fmt --check sample/package-version-resolver
```

Select this package for a compiler check with:

```sh
sai -C sample/package-version-resolver check --offline --target wasm32-core
```

The fixture assignment and its one failed branch are hand-checked. The
reference `sai check` command parses the package and source, then stops at the
existing Stage 0 `sai check` and `@derive` feature gates. The Stage 3 `sai test`
command is not implemented, so the embedded source-level assertions have not
executed. Formatter success does not establish semantic or runtime
correctness.
