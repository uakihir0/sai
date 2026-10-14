# 02. Dependency Resolution

**Status:** Normative
**Stage:** 3
**Related:** [01-package-contract.md](01-package-contract.md), [03-lock-file.md](03-lock-file.md), [04-distribution-and-security.md](04-distribution-and-security.md), [decisions.md](../design/decisions.md) (D-PKG-01 to D-PKG-05, D-CAP-01), [spec/08](../spec/08-effects-and-capabilities.md), [impl/08](../impl/08-effect-and-capability-analysis.md), [impl/11](../impl/11-diagnostics.md), [impl/13](../impl/13-cli.md), [targets/targets-and-profiles.md](../targets/targets-and-profiles.md)

This chapter defines how the toolchain turns the requirements of a root package into a **resolved graph**: one selected version and one source for every package node that the build can use. It defines version syntax and ordering (SemVer 2.0.0), compatibility lines (D-PKG-02), the meaning of version requirements, Minimal Version Selection without backtracking, Git tags, path dependencies, the implicit packages `sai:core` and `sai:alloc`, root overrides, cycles, conflicts and their explanations, the interaction with `sai.lock`, the commands `sai add` and `sai update`, the capability checks that run at resolution time, and the determinism and complexity guarantees. The syntax of the requirements is in [01-package-contract.md](01-package-contract.md); the byte format of the lock file is in [03-lock-file.md](03-lock-file.md); fetching, caching, artifacts, and review are in [04-distribution-and-security.md](04-distribution-and-security.md). Command-line flags are owned by [impl/13](../impl/13-cli.md); this chapter defines what the commands compute.

---

## 2.1 Overview

### 2.1.1 Inputs and output

Resolution is a function

```text
resolve(root contract, lock file or none, mode, fetched contents) -> resolved graph | diagnostics
```

1. The **root package** is the package whose `sai.package` the command operates on. Exactly one package is the root of a resolution. How a command selects the root (the current directory, a path argument, or a workspace member) is defined by [impl/13](../impl/13-cli.md); every selected root is resolved independently with this chapter's algorithm.
2. A **node** is a pair (package identity, compatibility line), written `acme:json#1` for line 1 or `acme:json#0.4` for line 0.4. Two versions of the same package in different lines are different nodes (D-PKG-02).
3. An **edge** is a requirement from a package version to a node, carrying the version requirement, the requiring contract's original source clause, the `only` set, and the optional `grant` set of the requirement ([01 §1.10](01-package-contract.md)). The original source clause and grant are retained unchanged; a root override changes only the effective source used by resolution (§2.7.3).
4. The **resolved graph** assigns to every node reachable from the root a selected version and a source, together with the edges of the selected versions. It is the content of `sai.lock` ([03-lock-file.md](03-lock-file.md)).
5. The **mode** is one of: *build* (any command that needs a resolved graph: `sai check`, `sai build`, `sai test`, `sai run`, and similar), *lock* (resolve and write the lock without building, §2.10.4), *update* (§2.10.5), and *add* (§2.11). Flags such as `--locked`, `--offline`, and `--frozen` restrict what a mode may do ([03-lock-file.md](03-lock-file.md) §3.8).

### 2.1.2 Properties

The resolver is designed for these properties (P8, P9):

1. **Determinism.** The result depends only on the inputs of §2.14.1. It does not depend on the time, the order of network replies, the host operating system, the contents of the cache beyond their checksums, or the order of files in a directory.
2. **No backtracking.** Each node gets the maximum of the minimum versions requested for it (D-PKG-02). Upper bounds are checked afterwards; a violation is an error with an explanation, never a trigger for a search (§2.4, §2.13).
3. **No index.** The resolver fetches exactly the versions that some requirement names as its minimum, plus the versions recorded in the lock file. Only `sai update` and `sai add` list the tags of a repository (§2.10.5, §2.11; N-G-31).
4. **Stability.** A build with an up-to-date lock file uses exactly the locked versions and fetches nothing that is not locked (§2.10.2).
5. **No code execution.** Resolution reads `sai.package` files and hashes trees. It never runs code from a package (D-PKG-02, [04 §4.10](04-distribution-and-security.md)).

### 2.1.3 Phases

A resolution runs these phases in order. A phase that reports an error stops the resolution; the lock file is not written.

| Phase | Section | Result |
|---|---|---|
| 1. Parse and validate the root contract | [01](01-package-contract.md) | the root's requirements |
| 2. Decide whether the lock can be used as is | §2.10.2 | Mode A (use the lock) or Mode B (select) |
| 3. Selection (Mode B only) | §2.4 | selected version per node |
| 4. Range check | §2.4.3 | `VERSION_CONFLICT` or success |
| 5. Source selection and fetch | §2.6 to §2.8 | contents, verified against the lock |
| 6. Graph checks | §2.4.4, §2.9 | cycles, duplicate lines |
| 7. Resolution-time capability checks | §2.12 | validated policies, footprints, review items |
| 8. Write the lock (when the mode allows it) | [03 §3.7](03-lock-file.md) | `sai.lock` |

Phases 3 and 5 interleave in Mode B: selection reads the `sai.package` of every version it visits, which requires fetching it.

---

## 2.2 Versions

### 2.2.1 Syntax

Package versions are SemVer 2.0.0 versions without build metadata (N-G-4):

```ebnf
Version    ::= Major "." Minor "." Patch ( "-" PreRelease )?
Major      ::= Num
Minor      ::= Num
Patch      ::= Num
Num        ::= "0" | [1-9] [0-9]*
PreRelease ::= PreIdent ( "." PreIdent )*
PreIdent   ::= Num | [0-9A-Za-z-]* [A-Za-z-] [0-9A-Za-z-]*
```

1. Each `Num` MUST fit in an unsigned 64-bit integer. A numeric pre-release identifier MUST NOT have a leading zero (SemVer 2.0.0 §9). Violations are `INVALID_VERSION` ([01 §1.2.3](01-package-contract.md)).
2. Build metadata (`+...`) is not part of a SAI version. In `sai.package` and `sai.lock` it is `BUILD_METADATA_NOT_ALLOWED`. A Git tag whose version part contains `+` is not a release tag (§2.6.1).
3. The **core** of a version is its `Major.Minor.Patch` triple. A version with a pre-release part is a **pre-release**; otherwise it is a **release**.
4. A version is written in canonical form everywhere the toolchain writes one: decimal numbers without leading zeros, and the pre-release part exactly as written. Two versions are **equal** when they are byte-identical in canonical form.

### 2.2.2 Precedence

Versions are totally ordered by SemVer 2.0.0 precedence:

1. Compare `Major`, then `Minor`, then `Patch` numerically.
2. If the cores are equal, a pre-release has lower precedence than the release (`1.0.0-rc.1 < 1.0.0`).
3. If both are pre-releases with the same core, compare the pre-release identifiers from left to right:
   - two numeric identifiers compare numerically;
   - two alphanumeric identifiers compare by the bytes of their ASCII text;
   - a numeric identifier has lower precedence than an alphanumeric one;
   - if all compared identifiers are equal, the version with fewer identifiers has lower precedence.

Because build metadata does not exist, precedence equality coincides with byte equality of canonical forms. This is why build metadata is excluded (N-G-4): with it, two different versions could have equal precedence, and "the maximum of the minimums" would not be unique.

```text
0.9.0 < 0.10.0 < 1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-alpha.beta < 1.0.0-beta
      < 1.0.0-beta.2 < 1.0.0-beta.11 < 1.0.0-rc.1 < 1.0.0 < 1.0.1 < 1.10.0 < 2.0.0
```

### 2.2.3 Compatibility lines

The **compatibility line** of a version (D-PKG-02) is computed from its core:

```text
line(M.m.p[-pre]) = "M"          if M >= 1
                  = "0.m"        if M = 0 and m >= 1
                  = "0.0.p"      if M = 0 and m = 0
```

1. A pre-release belongs to the line of its core: `2.0.0-rc.1` is in line `2`, not in line `1`.
2. Within one line, a later version is expected to be a compatible replacement for an earlier one (SemVer). The resolver relies on this: it may select a later version of a line than a requirement asked for, but never a version of another line (D-PKG-02).
3. Lines are ordered by the precedence of their smallest possible member. Diagnostics and `sai inspect` write a node as `<identity>#<line>` (`acme:json#1`, `acme:json#0.4`, `acme:json#0.0.7`). `sai.lock` does not store lines; they are recomputed from the locked versions.
4. A requirement's line is the line of its minimum ([01 §1.10.2](01-package-contract.md) rule 8). The minimum of a requirement is defined in §2.3.1.

---

## 2.3 Version requirements

### 2.3.1 Comparators as intervals

The syntax and the validation rules of requirements are in [01 §1.10.2](01-package-contract.md). This section defines the set of versions each comparator denotes. A partial version (`1`, `1.4`) is first **completed** with zero components (`1.0.0`, `1.4.0`). Let `V = M.m.p[-pre]` be the completed version. Intervals are taken in precedence order (§2.2.2); `[a, b)` contains every version `x` with `a <= x < b`.

| Comparator | Interval |
|---|---|
| `^V`, `M >= 1` | `[V, (M+1).0.0)` |
| `^V`, `M = 0`, `m >= 1` | `[V, 0.(m+1).0)` |
| `^V`, `M = 0`, `m = 0` | `[V, 0.0.(p+1))` |
| `~M.m.p[-pre]` or `~M.m` | `[V, M.(m+1).0)` |
| `~M` | `[M.0.0, (M+1).0.0)` |
| `=V` | `[V, V]` (V only) |
| `>=V` | `[V, +inf)` |
| `<V` | `(-inf, V)` |
| `<=V` | `(-inf, V]` |

1. A requirement denotes the intersection of the intervals of its comparators. Because a requirement has exactly one lower bound and at most one upper bound ([01 §1.10.2](01-package-contract.md) rules 3 to 5), the intersection is a single interval `[min, upper)` or `[min, upper]`.
2. The **minimum** `min(r)` of a requirement `r` is the lower end of the interval of its lower-bound comparator. The requirement's **node** is `(id, line(min(r)))`.
3. `^V` therefore always means "`V` or a later version of the line of `V`": its interval is exactly the part of `line(V)` at or above `V`. For `M >= 1` and for `0.m` with `m >= 1` this agrees with Cargo and npm.
4. A caret comparator with a partial version whose given components are all zero (`^0`, `^0.0`) is `INVALID_VERSION_REQUIREMENT` (N-G-22). Its completed reading (`^0.0.0`, the single line `0.0.0`) and the reading of other ecosystems (`>=0.0.0, <1.0.0`) disagree; the diagnostic offers both `^0.0.0` and `>=0.0.0, <0.1.0`.
5. A comparator with a pre-release (`^1.0.0-rc.1`, `>=2.0.0-beta`) uses the full version, including the pre-release, as its bound. `~` and `^` with a pre-release compute the upper bound from the core (`^1.0.0-rc.1` is `[1.0.0-rc.1, 2.0.0)`).
6. The empty-requirement check of [01 §1.10.2](01-package-contract.md) rule 7 is a comparison of the interval ends: `[a, b)` is empty when `b <= a`, and `[a, b]` when `b < a`.

### 2.3.2 Satisfaction

1. A version `x` **satisfies** a requirement `r` when `x` lies in the interval of `r`.
2. For resolution, a requirement `r` can only be satisfied by a version of its own line: the resolver assigns the edge to the node `(id, line(min(r)))` and checks the selected version of that node (§2.4.3). Stable versions of other lines that lie in the interval (as with `>=1.2` or `~0`) are never used; when the interval contains the stable start of the next line, this is the reason for the warning `VERSION_REQ_SPANS_LINES` ([01 §1.10.2](01-package-contract.md) rule 9). A boundary pre-release can lie in the interval without triggering the warning when its stable boundary is excluded.
3. The selected version of a node is always at least the minimum of every edge to it (§2.4.2). A range check can therefore fail only through an upper bound (`<`, `<=`, the upper end of `~`, or `=`), or through a path pin (§2.7.1).

### 2.3.3 Pre-releases

1. **Range checks use plain precedence** (N-G-21). A pre-release satisfies a requirement exactly when it lies in its interval: `^1.2.0` is satisfied by `1.4.0-beta.1`, and `^1.2.0` is not satisfied by `1.2.0-rc.1`, which is below `1.2.0`.
2. **Selection never introduces a pre-release by itself.** MVS selects, for each node, one of the versions named by a requirement minimum or by the lock file (§2.4.2). A pre-release is therefore selected only if some requirement names it as its minimum, or a lock entry records it.
3. **Candidate choice** in `sai update` (§2.10.5) and `sai add` (§2.11) excludes pre-releases, with one exception: when the currently selected version of a node is a pre-release, the candidates for that node also include the pre-releases with the same core. This lets `1.0.0-rc.1` advance to `1.0.0-rc.2` or to `1.0.0`, but never to `1.1.0-alpha`.
4. A package whose own version is a pre-release has no special rules. Its `pub` functions follow D-EFF-01 according to the core of its version.

Examples:

| Requirement | Satisfied by | Not satisfied by |
|---|---|---|
| `^1.4` | `1.4.0`, `1.9.3`, `1.5.0-rc.1` | `1.4.0-rc.1`, `2.0.0`, `2.0.0-rc.1` |
| `^0.4.2` | `0.4.2`, `0.4.9` | `0.5.0`, `0.4.1` |
| `^0.0.3` | `0.0.3` | `0.0.4`, `0.0.3-rc.1` |
| `^1.0.0-rc.1` | `1.0.0-rc.1`, `1.0.0-rc.2`, `1.0.0`, `1.3.1` | `1.0.0-beta`, `2.0.0` |
| `~1.4.2` | `1.4.2`, `1.4.9` | `1.5.0`, `1.5.0-alpha` |
| `>=1.2, <1.8` | `1.2.0`, `1.7.9`, `1.8.0-rc.1` | `1.8.0`, `1.1.9` |
| `=1.4.2` | `1.4.2` | `1.4.3` |

The row `>=1.2, <1.8` shows that `1.8.0-rc.1` lies below `1.8.0` and is admitted by precedence. It is never chosen unless a requirement or the lock names it (rule 2).

---

## 2.4 Minimal Version Selection

### 2.4.1 Definitions

1. A **package version** is a pair `(id, v)`. Its **contract** is the `sai.package` of that version, read from its source (§2.6, §2.7). The root package version is written `root`.
2. `Edges(u, c)` is the ordered list of active requirements of the package version `u` with contract `c` (§2.5), sorted by the identity bytes of the required package.
3. For an edge `e`, `Node(e) = (e.id, line(min(e.req)))` (§2.3.1). The **named version** of `e` is:
   - for an edge to a node with a root `use path` override (§2.7.3), the version declared by the override's package;
   - for a `from path` edge, the version declared by the path package (§2.7.1);
   - otherwise `min(e.req)`.
4. **Floors** `F` map nodes to versions. In Mode B they are the versions of the lock entries (§2.10.3), adjusted by `sai update` (§2.10.5).
5. **Pins** `P` map nodes to a path source and its declared version (§2.7). Root-override pins `P₀` are known before selection; pins from `from path` edges are added when the edge is processed.

### 2.4.2 Phase 1: selection

```text
procedure Select(root, F, P):
    sel     := empty map Node -> Version
    named   := empty map (Identity, Version) -> set of (requirer, edge)
    read    := empty map (Identity, Version) -> (Source, Contract)
    touched := empty set of Node
    queue   := empty min-priority queue of (Identity, Version)
               -- ordered by identity bytes, then version precedence

    procedure Name(id, v, requirer, e):
        n := (id, line(v))
        if n = (root.id, line(root.version)):
            report DEPENDENCY_CYCLE (requirer reaches the root)        -- §2.4.4
        if e is a path edge:
            add its pin to P[n]                                        -- §2.7
        if n not in touched:
            add n to touched
            if n in F and n not in P₀:
                Name(id, F[n], LOCK, none)                             -- lock floor, N-G-24
        if requirer != LOCK:
            add (requirer, e) to named[(id, v)]
        if n not in sel or sel[n] < v:
            sel[n] := v
        if (id, v) not in read and (id, v) not in queue:
            queue.push((id, v))

    for e in Edges(root, contract(root)):
        Name(e.id, NamedVersion(e), root, e)
    while queue is not empty:
        (id, v) := queue.pop_min()
        s := ReadSource(id, v, named[(id, v)])     -- §2.8.2
        c := FetchContract(id, v, s)               -- §2.6.2, §2.7: fetch, identity check
        read[(id, v)] := (s, c)
        for e in Edges((id, v), c):
            Name(e.id, NamedVersion(e), (id, v), e)
    for n in nodes pinned by a from-path edge:
        sel[n] := P[n].version                                    -- exclude its floor
    return sel, named, read
```

1. Selection visits every package version that is named by an edge of a visited version, starting at the root, including versions that will not be selected (Go's MVS, D-PKG-02). It also visits the floor version of each reached node that is not pinned by a root override. A `from path` pin discovered later does not remove an already queued or read floor version from this candidate closure; its edges can therefore name versions of other nodes. This rule makes the closure independent of queue order.
2. For an unpinned node, `sel[n]` is the maximum of its named versions and floor: "the maximum of the minimums" of D-PKG-02. For a node pinned by a `from path` edge, `sel[n]` is exactly the declared path version; the floor version can still contribute edges to the candidate closure but does not select that node.
3. Each package version is read at most once. The queue order fixes which edges are already known when a version is read, which matters only for the choice of the source to read it from (§2.8.2).
4. A floor is added only when its node is reached by an edge (N-G-24). This refines "lock entries are additional root requirements" of D-PKG-02: entries for nodes that nothing requires any more are dropped instead of being kept alive forever. A root-override pin suppresses the floor before traversal begins; a `from path` pin overrides the node's final selected version after candidate traversal.
5. A path package always has exactly its declared version. If another edge's minimum is above that version or an upper bound excludes it, phase 2 reports `VERSION_CONFLICT` (§2.7.1, §2.4.3). Floors of reached candidate versions are still visited as specified above.

### 2.4.3 Phase 2: the selected graph and the range check

```text
procedure Check(root, sel, read, P):
    G       := empty set of (PackageVersion, Edge, Node)
    visited := { root }
    pending := [ root ]
    while pending is not empty:
        u := pending.pop()
        for e in Edges(u, contract(u)):
            n := Node(e)
            add (u, e, n) to G
            w := (n.id, sel[n])
            if w not in visited:
                add w to visited
                pending.push(w)
    for (u, e, n) in G:
        if not satisfies(sel[n], e.req):
            report VERSION_CONFLICT(n, e)                              -- §2.13
    for n in P where n is a node of G:
        if sel[n] != P[n].version:
            report VERSION_CONFLICT(n, pin of n)                       -- §2.7.1
    return G
```

1. `G` contains only edges of **selected** versions. The nodes of `G` are the **resolved nodes**; they are exactly the entries of the lock file. Nodes that were touched in phase 1 but are not in `G` are dropped.
2. Every edge of `G` is checked against the selected version of its node, including `<`, `<=`, `~`, and `=` (D-PKG-02). Edges of versions that were visited but not selected are not checked: they are not part of the build.
3. All violations are reported, each with its own explanation (§2.13), sorted by the identity bytes of the node and then by line. There is no backtracking: the resolver does not try other versions to avoid a conflict (D-PKG-02, P8).
4. `G` does not depend on the order in which `pending` is processed.

### 2.4.4 Cycles

1. A cycle is a sequence of edges in `G` that leads from a selected package version back to itself. A cycle is the error `DEPENDENCY_CYCLE` (N-G-28). The diagnostic lists the package versions of one cycle in order, starting at the one with the least identity, and the `sai.package` location of each edge.
2. A requirement of a package on its own identity is `PACKAGE_REQUIRES_ITSELF` ([01 §1.10.1](01-package-contract.md)) when it is direct. An indirect requirement on the root's own node is reported during selection (the check in `Name`), because the root is not a selected node.
3. A requirement on the root's identity in a **different** line is not a cycle: it is a different node, fetched like any other, and it triggers `DUPLICATE_PACKAGE_LINE` (§2.9).
4. Rationale: a package is compiled after its dependencies, and its public API may mention their types. A cycle has no compilation order (P8, P9).

### 2.4.5 Properties

1. **Existence and uniqueness.** `Select` always terminates with a unique `sel`: every visited version is read once, the number of visited versions is bounded by the versions named in fetched contracts plus the floor version of each reached node, and the maximum for an unpinned node is unique because precedence is a total order (§2.2.2).
2. **Minimality.** For an unpinned node, `sel[n]` is the least version that is at least every version named for `n` by a visited package version or a floor, and it is one of those named versions. For a path-pinned node, `sel[n]` is exactly the declared path version. Selection never invents a version (§2.3.3 rule 2).
3. **Reproducibility.** For the same inputs (§2.14.1), `Select` and `Check` produce the same `G`. When a newer release of a dependency is published, the result does not change until a requirement or the lock names it (high-fidelity builds).
4. **Failure is explained, not searched around.** For an unpinned node, a conflict means that some upper bound of an edge in `G` is below the maximum of the minimums of its node. That maximum may come from a version that was visited but not selected (Go's MVS counts every visited version); the explanation then marks the requirer as "not selected" (§2.13.1). For a pinned node, a conflict means that the pin's version fails any requirement of an edge in `G`. A different assignment might satisfy every edge of `G`, but finding it would need search, which D-PKG-02 excludes. The fix is a change of requirements, an update of the package that sets the upper bound, or an override (§2.13.3).

---

## 2.5 Requirement collection

### 2.5.1 Edges of a package version

1. The edges of the **root** are its `requires` entries followed by its `requires test` entries ([01 §1.10.6](01-package-contract.md)). Test edges are part of every resolution, so that one lock file serves `sai build`, `sai test`, and `sai check --tests` alike; they are marked as test edges in the lock (N-G-23).
2. The edges of a **dependency** version are its `requires` entries only. Its `requires test` entries are never read for resolution (N-G-11, N-G-16).
3. An edge carries the required identity, the version requirement, the original source clause from the requiring contract before root overrides, the `only` set, the optional `grant` set, and whether it is a test edge. The original clause and grant remain part of the edge metadata; root overrides determine the effective source separately (§2.7.3, §2.8).
4. The order of `requires` entries in a file has no meaning. `Edges` sorts them by identity bytes; an identity appears at most once per package ([01 §1.10.4](01-package-contract.md)).

### 2.5.2 Target filtering

1. An edge without `only` is **included** in resolution.
2. An edge with `only { T }` is included if `T` contains at least one target of the **root's** `targets` declaration; otherwise it is ignored for resolution and locking (N-G-23). This applies to edges of the root and of dependencies alike.
3. A dependency's `only` set is validated against the dependency's own `targets` ([01 §1.10.5](01-package-contract.md)), but filtered against the root's targets, because only the root's targets are ever built.
4. Consequence: one `sai.lock` covers every target of the root. Adding a target to the root may add nodes to the lock; removing one may remove nodes.

### 2.5.3 Activation for a build

Resolution produces one graph for all targets. A build for one target `t` and one command uses a subgraph:

1. An edge of `G` is **active** for `t` if it has no `only` set or its `only` set contains `t`, and, for a build without test analysis, it is not a test edge. Test analysis is performed by `sai test` and `sai check --tests` ([spec/14](../spec/14-testing.md) §14.2.4, N-I-39); every other build, including `sai check` without `--tests`, uses no test edge.
2. A resolved node is active if it is reachable from the root through active edges. Only active nodes are fetched, compiled, and checked by the build. Inactive nodes stay in the lock.
3. Every active dependency MUST list `t` in its `targets`; otherwise the build for `t` fails with `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` ([01 §1.8](01-package-contract.md) rule 4). Resolution itself does not reject a graph for this reason; `sai add` and `sai update` show it in the review ([04 §4.7](04-distribution-and-security.md)).

### 2.5.4 Implicit packages

1. `sai:core` and `sai:alloc` ship with the toolchain and are always available (D-PKG-04). They are not nodes: they are never fetched, never selected, never written to the lock, and have no compatibility line (N-G-35).
2. Their version is the toolchain's. The `compiler` requirement of each package ([01 §1.5](01-package-contract.md)) is the only constraint that relates a package to them; it is checked for every selected version (`COMPILER_VERSION_MISMATCH`) and recorded in the lock ([03-lock-file.md](03-lock-file.md)).
3. A requirement on `sai:core` or `sai:alloc` is `IMPLICIT_PACKAGE_REQUIRED` ([01 §1.10.1](01-package-contract.md)).
4. The other standard packages (`sai:collections`, `sai:fmt`, ...) are ordinary nodes with Git sources. The toolchain's pre-populated cache only avoids network access ([04 §4.4.3](04-distribution-and-security.md)); it never changes which version is selected.

---

## 2.6 Git sources

### 2.6.1 Release tags

1. For a package with the source `from git "<url>"`, the **release tag** of version `v` is the Git ref `refs/tags/v<v>`, where `<v>` is the canonical form of `v` (§2.2.1). With `dir "<d>"`, it is `refs/tags/<d>/v<v>`, where `<d>` is the normalized directory path ([01 §1.6.1](01-package-contract.md)) (D-PKG-02, N-G-34). The spelling `dir "."` denotes the repository root and is normalized to an empty directory, so it selects the same `v<v>` tag as an omitted `dir`.
2. When the resolver needs version `v`, it requests exactly that tag. It never derives a version from a tag name in any other way.
3. When tags are listed (`sai update`, `sai add`), a ref is a release tag of the package only if its name is exactly `v` (or `<d>/v`) followed by a canonical version. Other refs are ignored silently: `v1.2` (incomplete), `v01.2.3` (leading zero), `1.2.3` (no `v`), `v1.2.3+build` (build metadata), `V1.2.3`, and tags of other directories.
4. A tag may be lightweight or annotated. An annotated tag is peeled to the commit it points to, subject to the chain-depth limit in [04 §4.4.4](04-distribution-and-security.md). A tag that does not point to a commit is `INVALID_TAG` ([04 §4.3](04-distribution-and-security.md)).

### 2.6.2 Fetching a contract and the identity check

`FetchContract(id, v, git(url, d))` performs these steps:

1. Fetch the release tag of `v` from `url` as described in [04 §4.3](04-distribution-and-security.md). If the repository has no such tag, the error is `VERSION_NOT_FOUND`, naming the URL, the tag name, and the chain of the edge that named `v` (§2.13.4). Network and protocol failures are `FETCH_FAILED`.
2. If the lock has an entry for `(id, v)` with the same source, the commit of the tag MUST equal the locked commit; otherwise see `TAG_MOVED` ([04 §4.3.3](04-distribution-and-security.md)). The fetched tree MUST match the locked checksum (`CHECKSUM_MISMATCH`, [03 §3.4](03-lock-file.md)).
3. The package directory is `<d>` in the tree of the commit (the repository root when `dir` is omitted or `"."`). It MUST contain a file `sai.package`; otherwise the error is `MISSING_PACKAGE_FILE`.
4. The contract is parsed and validated as a dependency contract ([01](01-package-contract.md); root-only declarations are ignored, N-G-16). Errors in it are reported against that package; the dependent's diagnostics about uses of it are suppressed ([impl/11](../impl/11-diagnostics.md)).
5. The declared identity and version MUST equal `(id, v)` exactly. Otherwise the error is `PACKAGE_IDENTITY_MISMATCH`, which shows the requested and the declared identity and version (D-PKG-02). This catches repositories that were renamed, tags that point at the wrong commit, and URLs that point at a different package.

### 2.6.3 No branch or commit sources

There are no branch, revision, or commit sources (N-G-27). A requirement always names a version, and a Git source always provides versions through release tags.

1. MVS compares versions; branches and commits have no order and no compatibility line.
2. A branch head moves without a change to any file of the dependent, which defeats reproducibility (P8).
3. Commits are pinned by the lock file ([03-lock-file.md](03-lock-file.md)), which is the only place where a commit appears.
4. For unreleased work, the root uses `overrides { <id> use path "<dir>" }` with a local checkout, or `overrides { <id> use git "<fork>" }` with a fork that has release tags (§2.7.3, [01 §1.16](01-package-contract.md)).

---

## 2.7 Path sources

### 2.7.1 Path dependencies and pins

1. `from path "<dir>"` names a directory relative to the directory of the requiring package ([01 §1.10.3](01-package-contract.md)). For the root and for packages that are themselves local path packages, it is a directory of the local file system. For a package fetched from Git, see §2.7.2.
2. The directory MUST contain `sai.package` (`PATH_PACKAGE_NOT_FOUND`). Its declared identity MUST equal the required identity (`PACKAGE_IDENTITY_MISMATCH`).
3. Let `pv` be the declared version of the path package. `pv` MUST satisfy the requirement, and `line(pv)` MUST equal `line(min(r))`. Otherwise the error is `PATH_VERSION_MISMATCH`, showing the requirement and the declared version (N-G-25).
4. The edge **pins** its node: `P[Node(e)] := (path, pv)`. A pinned node is selected at exactly `pv`. If another edge to the same node has a minimum above `pv`, or an upper bound below it, `Check` reports `VERSION_CONFLICT` with the pin as one side of the explanation (§2.4.3).
5. Two pins of the same node by different directories are `CONFLICTING_PATH_SOURCES`. Directories are compared after lexical normalization of the joined path and, for local directories, after resolving symbolic links of the host file system; the comparison result is used only for this check.
6. A local path package is read from the file system at every resolution and build. It has no commit and no checksum in the lock ([03 §3.3](03-lock-file.md)), and changes to its files do not make the lock out of date unless its contract changes (§2.10.2).

### 2.7.2 Path sources inside a Git package

A package fetched from Git may use `from path` to refer to another package in the same repository (a monorepo).

1. Let the requiring package come from `git(url, d)` at commit `c`. The target directory is the lexical normalization of `<d>/<dir>` (or `<dir>` without `d`), interpreted in the tree of commit `c`.
2. The normalized path MUST NOT leave the repository: a `..` that would go above the repository root is `PATH_ESCAPES_REPOSITORY` (N-G-25).
3. The node is pinned as in §2.7.1 with the source `git(url, normalized dir)` **at commit `c`**. It is not fetched through a release tag: the requiring package was published together with this exact sibling. The lock records the commit and the checksum like for any Git entry.
4. The edge's requirement and `pv` are checked as in §2.7.1 rule 3.

When validating a path edge from a lock, use the requiring package's source as the base, not the edge's path string. For a root edge, the base is the selected root package directory. For a local path package, resolve its entry's root-relative `source path` from the selected root and canonicalize the requiring directory and joined target through the host's symbolic links. For a Git package, use that entry's exact locked repository URL, commit, and package directory, then lexically normalize the joined path and reject any path that leaves the repository. Never use the process working directory or fall back to the root directory for a non-root requirer. These rules apply before activation pruning as well as during selection.

### 2.7.3 Root overrides

Overrides are root-only ([01 §1.16](01-package-contract.md)). Their effect on resolution:

1. `<id> use path "<dir>"`: the package at `<dir>` (relative to the root) is read; let `pv` be its declared version and `n = (<id>, line(pv))`. Every edge to `n` has its source replaced by the path and names `pv` (§2.4.1 rule 3); `n` is pinned. Edges to other lines of `<id>` are not affected (N-G-17). Each edge's requirement is still checked against `pv` in phase 2; a violation is `VERSION_CONFLICT` with the override as one side.
2. `<id> use git "<url>" [dir "<d>"]`: every edge to any line of `<id>` has its source replaced by the override's source. Versions are selected normally and fetched from the override's release tags.
3. `<id> use artifact`: no effect on resolution. It selects the artifact form in the cases of [04 §4.5.1](04-distribution-and-security.md).
4. An override that matches no edge of `G` is the warning `UNUSED_OVERRIDE`. An override of a package that is a node only through edges that are not included (§2.5.2) is also unused.
5. Overrides never change which versions are acceptable: every selected version satisfies every requirement of `G` (D-PKG-02, N-G-17).

---

## 2.8 Source selection

### 2.8.1 The source of a resolved node

The source of a resolved node `n` with selected version `v` is chosen by the first rule that applies (D-PKG-02, N-G-26):

1. A root override of `n.id` (`use path` for the line of `n`, or `use git`).
2. The pin of `n` (§2.7).
3. The effective source clause of an edge that **named** `v` for `n` (`named[(n.id, v)]` in §2.4.2), after applying any current root override to its identity. If several edges named `v`, the edge whose requirer is least by identity bytes, then by version precedence, is used. The root's identity takes part in this order like any other identity; a root that wants its own source to win uses an override. The source clause retained on each edge is the original one from its requiring contract; source clauses are compared after URL canonicalization ([04 §4.2.2](04-distribution-and-security.md)).
   Only an individual edge that named this exact selected version is a candidate. If no edge named it, there is no requirement-source candidate; use rule 4. Do not synthesize a candidate from another incoming edge.
4. The source recorded in the lock entry for `(n.id, v)`, if `v` was named only as a floor.

Every selected version is named by an edge, a floor, or a pin, so one of the rules applies. In Mode A (§2.10.2), `named` contains only the edges of `G`, and rule 4 covers versions that were originally named by a version that is not selected.

### 2.8.2 The source used to read a contract

During selection, the contract of `(id, v)` is read from the source given by the same rules, applied to the edges that have named `(id, v)` at the moment it leaves the queue (§2.4.2). Because the queue order is fixed, this choice is deterministic.

### 2.8.3 Divergent sources

1. After `Check`, for every resolved node, the final source (§2.8.1) is compared with the source used to read its selected version (§2.8.2). If they differ, the version is fetched from the final source, and its included edges (identity, requirement, source clause, `only`, `grant`) MUST equal those read during selection.
2. Otherwise the error is `DIVERGENT_SOURCES`: two sources provide different contracts for the same package version. The diagnostic names both sources and the edges that named them, and suggests an override that makes the choice explicit (§2.7.3).
3. Different sources that provide equal contracts are accepted. Each package review that is shown lists the source clauses that did not select that node's final source ([04 §4.7](04-distribution-and-security.md)). This includes clauses on incoming edges of the selected graph and on any other candidate edge that named the exact selected version. It omits the final winning named-requirement clause and every path-pin clause in the final path-pin set, because all such pins participate in the source-identity check. A root override or lock-floor source does not select a requirement clause, so all included incoming requirement clauses are listed. Edges excluded by target filtering and edges to other, non-selected candidates are omitted.

---

## 2.9 Duplicate lines

1. When the resolved graph contains two or more nodes with the same identity, the lint `DUPLICATE_PACKAGE_LINE` (default level `warn`, [spec/15](../spec/15-attributes-and-lints.md) §15.17.2) is reported once per identity (D-PKG-02). It lists every line with its selected version and one explanation chain per line (§2.13.1).
2. If the root declares `policy { deny duplicate_lines }` ([01 §1.12](01-package-contract.md)), the same condition is an error with the same code: the policy item sets the level of the lint to `deny` ([spec/15](../spec/15-attributes-and-lints.md) §15.16.5).
3. Nodes that are not included (§2.5.2) are not counted. A node reachable only through test edges counts.
4. The root's own identity counts: a dependency on another line of the root package is a duplicate line (§2.4.4 rule 3).
5. Different lines are independent packages for the compiler: their items and types are distinct, and each package's `use` refers to the node of its own edge ([spec/04](../spec/04-modules-and-names.md)).

---

## 2.10 Interaction with the lock file

### 2.10.1 What the resolver reads from the lock

The format is defined in [03-lock-file.md](03-lock-file.md). The resolver uses:

1. the root block: the root identity and version, the root's targets, and the root's edges (identity, requirement, original source clause, test flag, `only` set, optional `grant`, and the version each resolved to);
2. one entry per resolved node: identity, version, source (URL and `dir`, or path), commit, checksums, footprint, `compiler` requirement, and the entry's edges (identity, requirement, original source clause, `only` set, optional `grant`, and resolved version).

Only the root's lock file is used. A `sai.lock` inside a dependency's directory is part of that dependency's content (it is hashed like any other file) but is never read.

### 2.10.2 Mode A: using the lock as is

A resolution uses the lock unchanged (**Mode A**, N-G-29) when all of the following hold:

1. The lock parses, its format version is supported, and it contains no merge conflict ([03 §3.9](03-lock-file.md)). A format-1 lock is usable in Mode A only when it is root-only: it has no root edges and no package entries. A dependency-bearing format-1 lock cannot satisfy conditions 5 and 6 because it does not record complete edge metadata.
2. The root identity, version, and `targets` equal those of the root contract.
3. The root block's edges equal the included edges of the root contract (§2.5): the same identities, requirements in canonical form, original source clauses, test flags, `only` sets, and optional grants. An omitted grant differs from an explicit empty grant.
4. For every edge in the lock (root edges, and the edges recorded in entries), the lock has an entry for `Node(e)` whose version satisfies the edge's requirement and equals the version recorded on the edge.
5. For every pinned node (root `use path` overrides and `from path` edges), the entry's version equals the currently declared version of the path package, and its recorded edges equal the included edges of its current contract for every declared field: identity, canonical requirement, original source clause, `only` set, and optional `grant`. Each edge's recorded resolved version must equal the corresponding lock entry version and satisfy its requirement (condition 4). An omitted grant differs from an explicit empty grant.
6. For every entry, its recorded source equals the source chosen by §2.8.1, computed over the complete format-2 lock graph, including edges that are inactive for the requested target or test mode, and the root's current overrides. This check happens before activation prunes the graph.
7. Every entry is reachable from the root block through recorded edges, and the recorded edges have no cycle.

If all conditions hold, the resolved graph is the lock's graph; no selection runs and no tag is fetched. Only after validating the complete graph is the active subgraph derived for the requested target and test mode (§2.5.3). Active contents are fetched by commit when they are not in the cache, and verified against the lock ([03 §3.7.3](03-lock-file.md)). Condition 5 also requires the current contracts of every path-pinned node, including inactive nodes; a Git-backed path pin is read from its exact locked commit when needed, without listing tags or fetching an inactive requiring package solely to authenticate the edge. If that pinned package's verified content is not cached, it must be fetched by the locked commit (or fail with `OFFLINE_FETCH_REQUIRED` under `--offline`). When the contract of a fetched entry is read, its included edges, including the original source clauses and optional grants, MUST equal the edges recorded in the entry; otherwise the lock is `LOCK_CORRUPT`, because the checksum matched but the recorded edges do not describe the content.

### 2.10.3 Mode B: selection with lock floors

If any of conditions 1 to 7 fails for an otherwise valid lock, the resolver runs selection (**Mode B**):

1. Floors are taken from the lock: for every entry that is not a path entry, `F[(id, line(v))] := v`. Without a lock, `F` is empty.
2. `Select` and `Check` run with these floors (§2.4). A floor applies only when its node is reached (N-G-24), so entries for requirements that were removed disappear.
3. Entries whose identity, version, and source are unchanged keep their commit and checksums: the fetched content is verified against them. Entries with a new version or a new source are fetched by release tag and recorded with the new commit and checksums.
4. Under `--locked` or `--frozen`, any change to the lock file is `LOCK_OUT_OF_DATE`, and nothing is written ([03 §3.8](03-lock-file.md)).

Mode A and Mode B agree whenever the lock was written by a resolution of the same root contract. They can differ only after the root has relaxed a requirement in a way that keeps the lock consistent (for example, lowering a minimum). Mode A then keeps the locked versions, while a fresh Mode B run could select a higher version for some node, because it also visits the newly named lower version (§2.4.5 rule 4). Keeping the lock is the intended, stable outcome (P8): a change that only relaxes a requirement never changes the build.

### 2.10.4 Writing the lock

1. Every command that resolves (build mode, `sai lock`, `sai add`, `sai update`, `sai remove`) writes the lock when the result differs from the file, unless `--locked`, `--frozen`, or a failed approval (§2.12.3) prevents it. The write is atomic ([03 §3.7.2](03-lock-file.md)).
2. `sai lock` resolves (Mode A if possible, otherwise Mode B with floors) and writes the lock without building. On a lock file with merge conflict markers, it resolves the conflict as defined in [03 §3.9](03-lock-file.md).
3. Deleting `sai.lock` and running `sai lock` produces the minimal resolution: Mode B without floors.
4. Tools that edit `sai.package` (`sai add`, `sai remove`, structured fixes) preserve comments and the order of unrelated declarations, insert new requirements in the canonical layout of the formatter ([impl/14](../impl/14-formatter.md)), and then resolve as above.

### 2.10.5 `sai update`

`sai update` raises selected versions within their compatibility lines (N-G-30). It is the only command, besides `sai add`, that lists tags (§2.6.1 rule 3). The command-line spelling is owned by [impl/13](../impl/13-cli.md); the semantics are:

```text
procedure Update(root, L, targets):        -- targets: ALL, or a set of identities and id@version pairs
    G0 := resolve(root, L)                  -- Mode A or Mode B, §2.10.2, §2.10.3
    F  := floors of L (§2.10.3)
    for every id@w in targets:
        n := (id, line(w))
        require n is a node of G0                             else UPDATE_CHANGES_LINE
        require release tag of w exists                       else VERSION_NOT_FOUND
        F[n] := w; mark n as probed
    (sel, G) := Select and Check with F
    loop:
        S := resolved nodes n of G, in identity-then-line order, such that
               n is not probed, n is not pinned, and (targets = ALL or n.id in targets)
        if S is empty: exit loop
        for n in S:
            mark n as probed
            T := release tags of the source of n (§2.8.1)                  -- one listing per URL and dir
            C := { x in T : line(x) = line(n), x >= sel[n],
                   x satisfies every edge e of G with Node(e) = n,
                   x is a release, or x is a pre-release with the core of sel[n]
                   if sel[n] is a pre-release }                          -- §2.3.3 rule 3
            if C is not empty: F[n] := max(C)
        (sel, G) := Select and Check with F
    for every id@w in targets: require sel[(id, line(w))] = w             else UPDATE_VERSION_NOT_SELECTED
    return G
```

1. With no arguments, every node is updated, including nodes that appear only because an update requires them (the loop probes each node once, so it terminates).
2. With identities, only the nodes of those identities are probed. Other nodes keep their floors and are raised only as far as the new versions require (MVS).
3. `id@w` sets the floor of the node of `w` to exactly `w`, which may lower it. If another requirement forces a higher version, the command fails with `UPDATE_VERSION_NOT_SELECTED`, showing that requirement. If `w` is in a line that the graph does not contain, the command fails with `UPDATE_CHANGES_LINE`: changing lines is an edit of `sai.package`.
4. `sai update` never edits `sai.package`, never changes lines, and never updates pinned nodes (they are listed as skipped).
5. If `Check` reports a conflict in any round, the command fails, reports the conflict (§2.13), and leaves the lock unchanged. There is no backtracking.
6. The command reports, for every node, the old and the new version, the added and the removed nodes, and the capability review (§2.12.3, [04 §4.7](04-distribution-and-security.md)).
7. Under `--offline`, listing tags is impossible: `sai update` fails with `OFFLINE_FETCH_REQUIRED` unless every argument has the form `id@w` and the needed content is cached.

### 2.10.6 `sai remove`

`sai remove <id>` deletes the requirement on `<id>` from the root's `requires` or `requires test`. If no such requirement exists, the command fails with `NOT_REQUIRED`. It then resolves; entries that are no longer reachable disappear (§2.10.3 rule 2).

---

## 2.11 `sai add`

`sai add` adds a requirement to the root's `sai.package` (N-G-31). Its input is a Git URL, an optional `dir`, an optional version, and whether the requirement is a test requirement; the spelling of these arguments is owned by [impl/13](../impl/13-cli.md). A local directory is added with a `from path` source in the same way, without tag listing.

1. **Canonicalize** the URL ([04 §4.2](04-distribution-and-security.md)). Insecure or unsupported URLs are rejected there.
2. **Choose the version.**
   - With a version `w`: its release tag MUST exist (`VERSION_NOT_FOUND`).
   - Without a version: list the release tags of the URL and `dir` (§2.6.1 rule 3) and choose the highest **release**; pre-releases are not chosen (§2.3.3 rule 3). If there is no release tag, the command fails with `NO_RELEASE_TAGS`, and the help shows how to name a pre-release explicitly.
   - Listing tags here refines D-PKG-02, which allows only `sai update` to list tags: `sai add` is the other command that must discover a version. Resolution itself never lists tags.
3. **Read the contract** of the chosen version (§2.6.2). Its declared identity becomes the identity of the requirement; the identity check of §2.6.2 rule 5 compares the declared version with the tag.
4. **Check the root.** If the root already requires that identity (in `requires` or `requires test`), the command fails with `ALREADY_REQUIRED` and suggests `sai update` or editing the requirement. If the identity is the root's own, it fails with `PACKAGE_REQUIRES_ITSELF`.
5. **Name check.** The identity is compared with the identities of the resolved graph and of the toolchain's bundled packages for confusable names ([04 §4.11.1](04-distribution-and-security.md), `CONFUSABLE_PACKAGE_NAME`).
6. **Write the requirement** `<id> @ ^<w> from git "<canonical url>" [dir "<d>"]`, using the canonical layout of the formatter (§2.10.4 rule 4). For `0.0.z` versions, `^0.0.z` is exact, which is the meaning of a `0.0.z` line (§2.3.1).
7. **Resolve** with the current lock as floors (Mode B, §2.10.3), and show the review: identity, version, form (source, artifact, or hybrid), supported and unsupported targets of the root, capability footprint, and estimated size ([04 §4.7](04-distribution-and-security.md), draft §75).
8. **Commit.** If resolution succeeds and the review is approved, `sai.package` and `sai.lock` are both written, each atomically. If resolution fails or approval is refused, neither file changes.

---

## 2.12 Capability checks: resolution time and build time

Capability semantics are owned by [spec/08 §8.12](../spec/08-effects-and-capabilities.md) and the analysis by [impl/08](../impl/08-effect-and-capability-analysis.md). This section states which checks run during resolution and how their results are used. The rule of spec/08 §8.12.8 holds: **only build time rejects a program for capability reasons**. Resolution validates policies, computes footprints, and produces review items.

### 2.12.1 At resolution time

For every resolved node, after its content has been fetched:

1. **Policy validation.** Every `allow`, `deny`, and `grant` name in the graph is validated: its syntax ([01 §1.11.2](01-package-contract.md), `INVALID_CAPABILITY_NAME`), its existence (`UNKNOWN_CAPABILITY`, which needs the WIT packages of the resolved graph), and the conflict rule (`CAPABILITY_ALLOW_DENY_CONFLICT`). These are errors of the package's contract and stop the resolution.
2. **Footprint.** The capability footprint of the package is computed as defined in spec/08 §8.12.8: for a source package, the capabilities of all its bodies (not only reachable ones); for an artifact, all of its imports. A hybrid package has both, and the lock records the source footprint and, per target, the artifact's imports ([03 §3.3](03-lock-file.md)). The footprint is recorded in the lock as the package's resolved capabilities (D-PKG-03). The source footprint is computed for each target that the package declares, and the lock records their normalized union ([03 §3.3.3](03-lock-file.md) item 1). If the effect analysis of the package reports an error for one of these targets, the package has no footprint ([impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.10 item 4): the resolver reports the diagnostics, and they stop the resolution as in rule 1, so that the lock never records a footprint smaller than the package's code.
3. **Review items.** The following are shown in the review ([04 §4.7](04-distribution-and-security.md)) and never stop the resolution:
   - a footprint that is not granted by the package's own policy, or by the root's package-level policy `(allow, deny)`: resolution has no profile, so no effective policy applies here ([targets](../targets/targets-and-profiles.md) §5.5 item 6);
   - a footprint of a dependency `Q`, or of a package reachable from `Q`, that the `grant` of an edge to `Q` does not grant ([01 §1.10.8](01-package-contract.md));

   "Grant" includes the `wasi:io` implication of [targets](../targets/targets-and-profiles.md) §3.3 in both items.
   - an artifact import that its world does not import (this one is also an artifact verification error, [04 §4.5.2](04-distribution-and-security.md));
   - a target of the root that an active dependency does not support (§2.5.3).
4. **Escalation.** Capability changes against the previous lock are detected (§2.12.3).

Footprints are computed when an entry is created or its content changes (new version, new source, or new checksum). An entry whose content is unchanged keeps its recorded footprint, so that a lock is not rewritten only because a toolchain was upgraded.

### 2.12.2 At build time

For every build (`sai check`, `sai build`, `sai test`, `sai run`), on the active subgraph (§2.5.3):

1. the checks of [spec/08 §8.12.6](../spec/08-effects-and-capabilities.md): every package's required capabilities against its own policy, the union against the root's effective policy, and against the target (`CAPABILITY_NOT_ALLOWED`, `CAPABILITY_DENIED`, `TARGET_CAPABILITY_UNAVAILABLE`);
2. the `grant` check of [01 §1.10.8](01-package-contract.md) (`CAPABILITY_NOT_GRANTED`), with the same reachability;
3. target support (`TARGET_NOT_SUPPORTED_BY_DEPENDENCY`).

Build-time checks use reachability on the actual program, so a dependency whose footprint exceeds its policy is accepted as long as the exceeding code is not reachable (spec/08 §8.12.8). An artifact's imports are always reachable (D-CAP-01), so for a package used in artifact form the review items of §2.12.1 rule 3 become build errors.

### 2.12.3 Capability escalation and approval

When a command would write a lock that differs from the existing one, the resolver compares capabilities per node (N-G-32):

1. For a node `n` present in both the old and the new lock (same identity and line), let `Old` and `New` be the footprints, including, for hybrid packages, the artifact imports of every target. The node **escalates** if some capability in `New` is not covered by any capability in `Old` (the *covers* relation of [spec/08 §8.12.1](../spec/08-effects-and-capabilities.md)). This compares names, and the `wasi:io` implication of [targets](../targets/targets-and-profiles.md) §3.3 does not apply, because the lock, which is the record of approval (item 5), records names.
2. A node that is new in the lock escalates if its footprint is not empty.
3. A node whose source changed (a different canonical URL or `dir`) is a source change, reviewed as defined in [04 §4.7.2](04-distribution-and-security.md) (`SOURCE_CHANGED`).
4. Escalations and source changes require **approval**. An interactive command shows the review and asks. A non-interactive command, or an interactive one whose user refuses, fails with `CAPABILITY_ESCALATION` (or `SOURCE_CHANGED`) and writes nothing. The way to approve non-interactively (a command-line option) is owned by [impl/13](../impl/13-cli.md).
5. The lock file is the record of approval: a capability that is in the footprint of a lock entry has been approved for that node. A build in Mode A never needs approval.
6. When there is no previous lock, nothing escalates. The command prints a summary of all footprints and writes the lock.
7. Removing capabilities, removing nodes, and lowering versions never need approval.

---

## 2.13 Conflicts and explanations

### 2.13.1 Explanation chains

Every resolution error about a node carries one or more **requirement chains** (N-G-33). A chain is a path of edges from the root to the edge in question, or from a lock-floor entry when the exact candidate version is reachable only through that floor:

```text
root -> (p1, v1) -> (p2, v2) -> ... -> edge e
```

1. Chains are taken in the **visited graph**: the package versions visited by `Select` and their edges (in Mode A, the lock graph). Root-reachable paths take precedence. A candidate branch reached only through a lock floor starts at that floor. A package version on a chain that is not selected is marked `(not selected)`.
2. The chain of an edge `e` with requirer `u` is a **shortest** path from the root, or from the applicable lock-floor entry, to `u` (fewest edges), followed by `e`. Among shortest paths, the one whose sequence of package versions is least is used, comparing elementwise by identity bytes and then by version precedence. It is found by breadth-first search from each origin that expands edges in `Edges` order and keeps the first predecessor found; root-origin chains win when both origins reach the same version.
3. A floor-only chain is explained as `lock entry acme:json 1.9.0 (sai.lock)`. A pin is explained by the chain of the edge or override that created it.

A `VERSION_CONFLICT` on node `n` with selected version `s` and violated edge `e` shows:

1. the node, the selected version `s`, and the requirement of `e` that `s` does not satisfy;
2. **why `s` was selected**: the chain of the edge that named `s` (the least requirer as in §2.8.1 rule 3), or the floor, or the pin;
3. **why the requirement exists**: the chain of `e`;
4. a help section (§2.13.3).

Example (rendering per [impl/11](../impl/11-diagnostics.md); the content is normative here):

```text
error[VERSION_CONFLICT]: acme:json#1 is selected at 1.9.0, but acme:cache 1.2.0 requires `>=1.4, <1.8`
  = 1.9.0 is the highest minimum of acme:json#1:
        acme:app 0.3.0 (root)          sai.package:9:5      acme:http @ ^2.4
     -> acme:http 2.4.0                sai.package:12:5     acme:json @ ^1.9
  = the conflicting requirement:
        acme:app 0.3.0 (root)          sai.package:10:5     acme:cache @ ^1.2
     -> acme:cache 1.2.0               sai.package:8:5      acme:json @ >=1.4, <1.8
  = help: a newer release of acme:cache may accept acme:json 1.9; try `sai update acme:cache`
  = help: or override acme:cache with a patched copy: `overrides { acme:cache use path "..." }`
```

Locations refer to the `sai.package` of each requirer; for fetched packages, the path is shown relative to the package directory, prefixed with the identity and version.

### 2.13.2 Structured data

The chains are part of the diagnostic's structured data ([impl/11](../impl/11-diagnostics.md)): each link records the requirer's identity and version, whether it is selected, the edge's requirement, and the source location. Diagnostics caused by a failed node (for example uses of its modules) are suppressed in favour of the resolution error (impl/11, cascade class C6).

For `VERSION_CONFLICT`, `data.selected_reason` is `root_override_pin`, `path_pin`, `requirement`, or `lock_floor`. `data.selected_chain` and `data.requirement_chain` each contain an `origin` (`root` or `lock_floor`, with package and version) and `steps`. Each step records the requirer identity and version, whether it is the root and selected, the named target identity and version, the requirement, and its `sai.package` location. A lock-floor selection has an empty `selected_chain.steps`.

For `DEPENDENCY_CYCLE`, `data.nodes` lists the package versions in cycle order and repeats the first node at the end. `data.edges[i]` records the requiring package version, selected target version, version named by the requirement, requirement, and source location. In a selection-time cycle to the root, the visited candidate path can differ from the selected target versions; both are recorded. `data.requirement_chain` gives the shortest provenance chain to the cycle-closing edge. Its origin is the root or a lock-floor entry, following §2.13.1. A cycle that reaches the root's own line starts and ends at the selected root package version; the closing edge separately records the version named in its requirement.

### 2.13.3 Help

The resolver offers only help that does not need network access or search:

1. If the conflicting upper bound comes from a dependency, suggest `sai update <that dependency>`.
2. If an ordinary root requirement named the selected version and a conflicting upper bound rejects it, suggest lowering that root requirement to a version accepted by the conflicting dependency.
3. Suggest an override (`use path` or `use git`) for a patched copy. When the selected version exceeds a dependency's upper bound, patch the requiring dependency; when a pin is below a requirement's minimum, patch the selected target package.

No structured fix is applied automatically: which side should change is a decision of the user (P1).

### 2.13.4 Other resolution errors

`VERSION_NOT_FOUND`, `FETCH_FAILED`, `PACKAGE_IDENTITY_MISMATCH`, `MISSING_PACKAGE_FILE`, `PATH_PACKAGE_NOT_FOUND`, `PATH_VERSION_MISMATCH`, `CONFLICTING_PATH_SOURCES`, `PATH_ESCAPES_REPOSITORY`, `DIVERGENT_SOURCES`, and `DEPENDENCY_CYCLE` carry the chain of the edge that caused them (§2.13.1). A known root cycle remains reportable when an independent branch fails to provide its contract; that branch stops traversal, while the cycle chain must still consist entirely of visited, source-backed edges. All errors found in one resolution are reported, sorted by node, as long as they are independent.

---

## 2.14 Determinism and complexity

### 2.14.1 Inputs

The resolved graph is a function of exactly these inputs:

1. the root's `sai.package` and the `sai.package` files of local path packages;
2. the root's `sai.lock`, if present;
3. the content of the Git commits that are fetched: for locked entries, the locked commits; otherwise, the commits that the requested release tags point to at the time of the fetch;
4. the toolchain: its language version (for `compiler` checks), the WASI packages it knows, and its bundled package index ([04 §4.4.3](04-distribution-and-security.md));
5. for `sai update` and `sai add` only, the lists of tags of the listed repositories;
6. the command and its arguments.

It does not depend on the time, the order in which network replies arrive, parallelism, environment variables (those that select credentials or the cache location never influence the result), the locale, the host operating system, the order of entries in directories, or what else the cache contains. Input 3 without a lock depends on the tags at fetch time; this is why the lock exists, and why a moved tag is detected ([04 §4.3.3](04-distribution-and-security.md)).

### 2.14.2 Parallel fetching

An implementation MAY fetch contents speculatively and in parallel. The result MUST be identical to the sequential algorithm of §2.4 with the read sources of §2.8.2. Speculative fetches whose result is not used have no effect, except on the cache, which is content-addressed ([04 §4.4](04-distribution-and-security.md)).

### 2.14.3 Complexity

Let `N` be the number of visited package versions and `E` the number of their edges.

| Step | Time | Network |
|---|---|---|
| `Select` | `O((N + E) log N)` (priority queue and ordered maps) | one fetch per visited version |
| `Check` and cycle detection | `O(N + E)` | none |
| Divergent-source check | `O(N + E)` | one extra fetch per node whose final source differs (rare) |
| Mode A validation | `O(entries + edges)` of the lock | none; active nodes are fetched by commit only when not cached |
| One explanation chain | `O(N + E)` (breadth-first search) | none |
| `sai update` | one `Select`/`Check` per round; at most one round per probed node, in practice two or three | one tag listing per distinct (URL, `dir`) |

There is no search over version combinations, so the cost is linear in the size of the visited graph up to logarithmic factors. Resource limits on fetched content are defined in [04 §4.4.4](04-distribution-and-security.md).

---

## 2.15 Worked example

The root `acme:app 0.3.0` has `targets { wasm32-wasi }` and:

```sai-package
requires {
    acme:http @ ^2.4
        from git "https://github.com/acme/http"
    acme:json @ ^1.6
        from git "https://github.com/acme/json"
    acme:log @ ^0.4.1
        from git "https://github.com/acme/log"
        only { wasm32-wasi }
}
```

The contracts at the named tags contain these requirements (sources omitted):

| Package version | Requirements |
|---|---|
| `acme:http 2.4.0` | `acme:json @ ^1.8`, `acme:log @ ^0.4.3` |
| `acme:json 1.6.0` | none |
| `acme:json 1.8.0` | none |
| `acme:log 0.4.1` | `acme:json @ ^1.2` |
| `acme:log 0.4.3` | `acme:json @ ^1.2`, `acme:time @ ~0.9.2` |
| `acme:time 0.9.2` | none |

There is no lock. Selection (§2.4.2) visits, in queue order:

1. `acme:http 2.4.0`, `acme:json 1.6.0`, `acme:log 0.4.1` (named by the root);
2. from `acme:http 2.4.0`: `acme:json 1.8.0` and `acme:log 0.4.3`;
3. from `acme:log 0.4.1`: `acme:json 1.2.0` is named, so `acme:json 1.2.0` is visited too (it has no requirements);
4. from `acme:log 0.4.3`: `acme:time 0.9.2`.

The maxima are `acme:http#2 = 2.4.0`, `acme:json#1 = 1.8.0`, `acme:log#0.4 = 0.4.3`, `acme:time#0.9 = 0.9.2`. `Check` builds `G` from the selected versions: the root, `acme:http 2.4.0`, `acme:json 1.8.0`, `acme:log 0.4.3`, `acme:time 0.9.2`. Every edge is satisfied: `~0.9.2` admits `0.9.2`, and every `^` edge is satisfied by construction. `acme:log 0.4.1` and `acme:json 1.2.0` were visited but are not part of `G`; they are not locked.

The source of `acme:json#1` at `1.8.0` comes from the only edge that named `1.8.0`, the one in `acme:http 2.4.0` (§2.8.1 rule 3).

If `acme:time 0.9.2` had required `acme:json @ >=1.0, <1.7`, `Check` would report `VERSION_CONFLICT` on `acme:json#1`: `1.8.0` was named by `acme:http 2.4.0`, and the conflicting edge is reached by `acme:app -> acme:log 0.4.3 -> acme:time 0.9.2`. No other version of any package would be tried.

---

## 2.16 Decisions introduced by this chapter

| ID | Rule | Rationale |
|---|---|---|
| N-G-21 | Range checks treat pre-releases by plain SemVer precedence. Selection never introduces a pre-release that no requirement or lock entry names; `sai update` and `sai add` choose pre-releases only when the current version is a pre-release with the same core. | P8: one ordering everywhere; pre-releases enter a graph only by an explicit request. |
| N-G-22 | `^V` denotes exactly the part of `line(V)` at or above `V` (partial versions are completed with zeros); `^0` and `^0.0` are `INVALID_VERSION_REQUIREMENT`. | P8, P9: the caret always means "this line"; the ambiguous forms are rejected instead of guessed. |
| N-G-23 | The root's `requires` and `requires test` edges are resolved together; dependencies contribute `requires` only. An `only` edge is included when its target set meets the root's targets. One lock covers every target of the root; each build activates a subgraph. | P8: one lock per root; switching targets or running tests never re-resolves. |
| N-G-24 | A lock floor applies only to a node that is reached by an edge during selection. This refines D-PKG-02's "lock entries are additional root requirements". | P8: removed requirements disappear from the lock instead of being kept alive by it. |
| N-G-25 | A path package pins its node at its declared version, which must satisfy the requirement and be in its line (`PATH_VERSION_MISMATCH`). Two different directories for one node are `CONFLICTING_PATH_SOURCES`. Inside a Git package, a path source refers to the same commit and must not leave the repository. | P6, P8: a path package has exactly one version; monorepo siblings are used as published. |
| N-G-26 | The source of a node is chosen by: root override, pin, the least requirer (identity bytes, then version) among edges that named the selected version, the lock entry. Contracts read from another source than the final one must have equal edges, or the error is `DIVERGENT_SOURCES`. | P8: realizes D-PKG-02 deterministically; never builds from a source whose content was not the one resolved. |
| N-G-27 | There are no branch, revision, or commit sources; commits appear only in the lock. Unreleased code is used through `use path` or `use git` overrides. | P8: MVS needs ordered versions; moving refs defeat reproducibility. |
| N-G-28 | A cycle among selected package versions is `DEPENDENCY_CYCLE`; another line of the root's own package is not a cycle. | P8, P9: packages need a compilation order. |
| N-G-29 | Mode A: when the lock is consistent with the root contract (§2.10.2), it is used unchanged and no selection runs. Otherwise Mode B runs MVS with lock floors. | P8: stable builds that need only locked content; relaxing a requirement never changes the build. |
| N-G-30 | `sai update` raises versions within their lines using tag listings, probes each node once, never edits `sai.package`, never changes lines or pins, and fails without writing on a conflict. `id@w` sets an exact floor. | P1, P8: predictable updates without search. |
| N-G-31 | `sai add` lists tags to find the highest release, checks the identity, writes `^<version>` with an explicit source in canonical layout, resolves, reviews, and writes both files only on success. This adds `sai add` to the commands that may list tags (D-PKG-02). | P1: the added requirement is fully explicit; P5: review before any change. |
| N-G-32 | Capability escalation is detected per node by comparing footprints with the previous lock under the covers relation; escalations and source changes require approval, and the lock is the approval record. | P5: no new authority without a visible decision. |
| N-G-33 | Explanation chains are shortest paths from the root in the visited graph, with ties broken by identity bytes and version; non-selected versions are marked. | P8: the same conflict always gets the same explanation. |
| N-G-34 | The release tag of `v` is exactly `v<canonical v>` or `<dir>/v<canonical v>`; when listing, other refs are ignored. | P8: one tag name per version. |
| N-G-35 | `sai:core` and `sai:alloc` are not graph nodes and are not locked; the `compiler` requirement relates packages to them. | P7: the implicit packages are part of the toolchain (D-PKG-04). |

---

## 2.17 Diagnostics

Codes defined in another document are marked with that document.

| Code | Severity | Description |
|---|---|---|
| `VERSION_CONFLICT` | error | a selected version does not satisfy a requirement of the selected graph, or a pin; reported with both chains (§2.4.3, §2.13.1; named by D-PKG-02) |
| `DUPLICATE_PACKAGE_LINE` | warning (lint) | several lines of one package in the graph (§2.9, D-PKG-02); an error under `policy { deny duplicate_lines }` |
| `DEPENDENCY_CYCLE` | error | selected package versions form a cycle, or a dependency requires the root's own line (§2.4.4) |
| `VERSION_NOT_FOUND` | error | the release tag of a requested version does not exist (§2.6.2, §2.10.5, §2.11) |
| `FETCH_FAILED` | error | a network or protocol failure while fetching from a Git source (§2.6.2) |
| `MISSING_PACKAGE_FILE` | error | the package directory of a fetched commit has no `sai.package` (§2.6.2) |
| `PACKAGE_IDENTITY_MISMATCH` | error | a fetched or path package declares another identity or version than requested (§2.6.2, §2.7.1) |
| `PATH_VERSION_MISMATCH` | error | a path package's declared version does not satisfy the requirement or is in another line (§2.7.1) |
| `CONFLICTING_PATH_SOURCES` | error | two different directories pin the same node (§2.7.1) |
| `PATH_ESCAPES_REPOSITORY` | error | a path source inside a Git package leaves the repository (§2.7.2) |
| `DIVERGENT_SOURCES` | error | two sources provide different contracts for the same package version (§2.8.3) |
| `UPDATE_CHANGES_LINE` | error | `sai update id@w` names a line that the graph does not contain (§2.10.5) |
| `UPDATE_VERSION_NOT_SELECTED` | error | `sai update id@w` cannot select `w` because a requirement forces a higher version (§2.10.5) |
| `NO_RELEASE_TAGS` | error | `sai add` found no release tag in the repository (§2.11) |
| `ALREADY_REQUIRED` | error | `sai add` for an identity that the root already requires (§2.11) |
| `NOT_REQUIRED` | error | `sai remove` for an identity that the root does not require (§2.10.6) |
| `CAPABILITY_ESCALATION` | error | a lock change adds capabilities to a node and was not approved (§2.12.3) |
| `INVALID_VERSION` | error | defined in [01](01-package-contract.md) §1.2; malformed version |
| `BUILD_METADATA_NOT_ALLOWED` | error | defined in [01](01-package-contract.md) §1.2; build metadata in a version |
| `INVALID_VERSION_REQUIREMENT` | error | defined in [01](01-package-contract.md) §1.10.2; malformed requirement, including `^0` and `^0.0` (§2.3.1) |
| `VERSION_REQ_SPANS_LINES` | warning | defined in [01](01-package-contract.md) §1.10.2; a requirement interval contains the stable start of the next compatibility line |
| `PACKAGE_REQUIRES_ITSELF` | error | defined in [01](01-package-contract.md) §1.10; a package requires its own identity |
| `IMPLICIT_PACKAGE_REQUIRED` | error | defined in [01](01-package-contract.md) §1.10.1; `sai:core` or `sai:alloc` required |
| `COMPILER_VERSION_MISMATCH` | error | defined in [01](01-package-contract.md) §1.5; a selected version's `compiler` requirement is not met |
| `PATH_PACKAGE_NOT_FOUND` | error | defined in [01](01-package-contract.md) §1.10.3; a path source without `sai.package` |
| `UNUSED_OVERRIDE` | warning | defined in [01](01-package-contract.md) §1.16; an override that matches no edge |
| `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` | error | defined in [01](01-package-contract.md) §1.8; an active dependency does not list the build target (§2.5.3) |
| `INVALID_CAPABILITY_NAME` | error | defined in [01](01-package-contract.md) §1.11.2; malformed capability name |
| `CAPABILITY_NOT_GRANTED` | error | defined in [01](01-package-contract.md) §1.10.8; build-time `grant` violation (§2.12.2) |
| `UNKNOWN_CAPABILITY` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.1; capability name unknown to the build |
| `CAPABILITY_ALLOW_DENY_CONFLICT` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.5; the same name in `allow` and `deny` |
| `CAPABILITY_NOT_ALLOWED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; build-time policy violation |
| `CAPABILITY_DENIED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; build-time deny violation |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; capability not provided by the target |
| `CHECKSUM_MISMATCH` | error | defined in [03](03-lock-file.md) §3.7.3; fetched content does not match the lock |
| `LOCK_CORRUPT` | error | defined in [03](03-lock-file.md) §3.3.5; the lock cannot be parsed, or its recorded edges do not describe the locked content |
| `LOCK_OUT_OF_DATE` | error | defined in [03](03-lock-file.md) §3.8.1; `--locked` or `--frozen` and the lock would change |
| `OFFLINE_FETCH_REQUIRED` | error | defined in [03](03-lock-file.md) §3.8.2; network access needed under `--offline` |
| `INVALID_TAG` | error | defined in [04](04-distribution-and-security.md) §4.3.2; a release tag does not point to a commit |
| `TAG_MOVED` | error | defined in [04](04-distribution-and-security.md) §4.3.3; a release tag points to another commit than the lock records |
| `SOURCE_CHANGED` | error | defined in [04](04-distribution-and-security.md) §4.7.2; a lock change moves a node to another source and was not approved |
| `CONFUSABLE_PACKAGE_NAME` | warning | defined in [04](04-distribution-and-security.md) §4.11.1; an added identity is confusable with a known one |
