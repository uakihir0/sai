# Package Lock Auditor

Package Lock Auditor checks a small, fixed package graph before a hypothetical
build. It verifies that each requested package is present, that its resolved
major and minor version fits every incoming range, and that the graph has a
dependency-first build order.

A caller could render the fixed audit results like this:

```text
clean lock
packages: 6; requirements: 8
missing package references: 0
version range conflicts: 0
duplicate package ID pairs: 0
dependency cycle: no
build order: palette -> glyphs -> renderer -> ui -> audit -> app

incompatible range fixture
missing package references: 0
version range conflicts: 1
duplicate package ID pairs: 0
dependency cycle: no
build order: unavailable (lock is invalid)

missing requester with incompatible target
missing package references: 1
version range conflicts: 1
duplicate package ID pairs: 0
dependency cycle: no
build order: unavailable (lock is invalid)

missing target fixture
missing package references: 1
version range conflicts: 0
duplicate package ID pairs: 0
dependency cycle: no
build order: unavailable (lock is invalid)

cycle fixture
missing package references: 0
version range conflicts: 0
duplicate package ID pairs: 0
dependency cycle: yes
build order: unavailable (lock is invalid)

duplicate ID fixture
missing package references: 2
version range conflicts: 0
duplicate package ID pairs: 1
dependency cycle: unknown (package IDs are ambiguous)
build order: unavailable (lock is invalid)
```

The project is a pure `wasm32-core` library. `model.sai` defines resolved
packages, version requirements, and audit results. `fixtures.sai` provides a
valid shared-dependency graph plus missing-target, incompatible-range, and
cycle variants. `lookup.sai` resolves package IDs, `versions.sai` checks the
range policy, `graph.sai` computes a stable dependency-first order, and
`audit.sai` combines their results. `lib.sai` contains fixed regression tests.

The graph uses numeric package IDs so the example can focus on graph semantics
instead of parsing manifests or names. A requirement accepts one major version
and a half-open minor window: `minimum_minor <= locked.minor <
maximum_minor_exclusive`. This is an intentionally small teaching constraint,
not full Semantic Versioning compatibility. Patch versions are recorded but
do not affect this range check. Both requester and target IDs are checked;
each missing endpoint contributes one to the reported count. When the target
exists, its version is checked against the requirement even if the requester
is missing.

The order is stable: on each pass, choose the first not-yet-built package in
fixture order whose known dependencies are already built. A shared dependency
is built once. Missing edges are reported separately and ignored for cycle
detection, so a missing package does not hide a cycle elsewhere in the known
graph. Repeated package-ID pairs invalidate the lock and make cycle status
unknown because the graph nodes cannot be resolved unambiguously. If a unique,
fully referenced graph cannot be completely ordered, the remaining graph
contains a cycle. Any missing endpoint, range conflict, duplicate ID, or cycle
withholds the order.

This is a read-only auditor over in-memory fixtures. It does not parse a
manifest, fetch packages, verify checksums, write a lockfile, or model
features, optional dependencies, target conditions, or full SemVer rules.
The fixture outcomes and stable order above are hand-checked. The current
reference compiler's `sai check --target wasm32-core` reports the Stage 0
`sai check` and `@derive` feature gates; `sai test --target wasm32-core`
reports that the Stage 3 test command is not implemented. Formatting succeeds,
but the package's semantic checks and SAI tests have not executed.
