# 12. Semantic Tooling

**Status:** Normative for the reference implementation (see README §2)
**Stage:** 0 for node IDs, symbol IDs, the symbol index, and the base language server; 6 for the semantic graphs, the semantic protocol, semantic operations, impact analysis, and semantic diff ([spec/01-introduction.md](../spec/01-introduction.md) §1.4). An operation that a Stage 0 fix names (§12.6) is available from the stage of that fix.
**Related:** [decisions.md](../design/decisions.md) D-TOOL-01, D-TOOL-02, D-TOOL-03, D-TOOL-05; draft §62–§67; [spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.6, §4.9; [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.2.3, §8.12; [spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.11; [spec/13-components-and-wit.md](../spec/13-components-and-wit.md); [spec/14-testing.md](../spec/14-testing.md); [spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md); [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6, §2.8; [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md); [09-optimization-and-size.md](09-optimization-and-size.md); [11-diagnostics.md](11-diagnostics.md); [13-cli.md](13-cli.md); [14-formatter.md](14-formatter.md)

This chapter defines how tools see a SAI program as a set of semantic objects rather than as text (draft §62). It defines node IDs and symbol IDs (D-TOOL-02), the persistent symbol index in `.sai/index/`, the six semantic graphs and their queries, the JSON-RPC semantic protocol, the eleven semantic operations of draft §63, impact analysis (draft §66), semantic diff (draft §67), and the language server. It defines the JSON documents that [13-cli.md](13-cli.md) prints for `sai graph`, `sai diff`, and `sai edit`, and their JSON Schema. Canonical paths are defined by [spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9, the CST by [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6, diagnostics and fixes by [11-diagnostics.md](11-diagnostics.md), command lines by [13-cli.md](13-cli.md), and formatting by [14-formatter.md](14-formatter.md); this chapter uses them and does not redefine them.

---

## 12.1 Overview

### 12.1.1 Components

| Component | Crate (impl/01 §1.3) | Section |
|---|---|---|
| node IDs | `sai_syntax` | §12.3 |
| symbol IDs and the symbol index | `sai_semantic` | §12.2, §12.9 |
| semantic graphs and queries | `sai_semantic` | §12.4 |
| semantic operations and impact analysis | `sai_semantic` | §12.6, §12.7 |
| semantic diff | `sai_semantic` | §12.8 |
| semantic protocol and language server | `sai_lsp` | §12.5, §12.10 |

All components read the results of the ordinary compiler queries (impl/01): the CST, the HIR, name resolution, types, the signature effect sets and the capability footprint of [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md), the generated WIT of [spec/13-components-and-wit.md](../spec/13-components-and-wit.md), and, for sizes, the size attribution of [09-optimization-and-size.md](09-optimization-and-size.md). They add no language rules. None of their results is an input to code generation, so no artifact depends on them (draft §64, D-TOOL-03).

### 12.1.2 Snapshots

1. A **document snapshot** is one version of the text of one file. Over LSP, the version is the `version` number that the client sends with `textDocument/didOpen` and `textDocument/didChange`. For a file that is not open in the client, and for the CLI, the snapshot is the content on disk when it was read; its version is `null` ([11-diagnostics.md](11-diagnostics.md) §11.5.4).
2. A **workspace snapshot** is a consistent set of document snapshots: one for every source file and `sai.package` of the root package, of the workspace members ([13-cli.md](13-cli.md) §13.2), and of the dependencies, together with `sai.lock`, the target, and the profile. Every query, operation, and diff of this chapter is evaluated on one workspace snapshot, and its result refers to the document snapshots of that workspace snapshot. A workspace snapshot is analyzed as `sai check --tests` analyzes the workspace (N-H-79) ([13-cli.md](13-cli.md) §13.6.2): tests and test-only code are part of every graph and are updated by every operation. The capability facts (`Req`, the `requires` edges of §12.4.2, and `FootprintChanged` of §12.8.2) are those of the build without tests, as `sai build` computes them, so that they describe the artifact. The index analyzes the tests view for navigation and queries; this does not change the diagnostics that `sai build` and `sai check` report ([spec/14-testing.md](../spec/14-testing.md) N-I-39).
3. The language server creates a new workspace snapshot on every change of a document, of the target, or of the profile (§12.10.4). A request that names a document version that is not the current one is answered with `STALE_SNAPSHOT` (§12.3.3).

### 12.1.3 Guarantees

1. **Determinism.** Every result of this chapter is a function of the workspace snapshot and of the content of `.sai/index/` (§12.9). It does not depend on the order of requests, on the number of threads, or on the time. Lists in results are sorted by the rules given for each result.
2. **Source is the truth.** Operations change source files only. The symbol index records identities for tools; it never changes what a program means, and deleting it never changes a build ([13-cli.md](13-cli.md) §13.5.2).
3. **No partial edits.** An operation either produces all of its edits or none (§12.6.2).
4. **Text output is canonical.** Code that tools print uses the canonical one-line text of [14-formatter.md](14-formatter.md) §14.13, and effect sets use the canonical order of [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.2.3 ([11-diagnostics.md](11-diagnostics.md) §11.7.6).

---

## 12.2 Symbol identity

### 12.2.1 Symbols

A **symbol** is an entity that has a canonical path ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9.1) and is not a package or a module (N-H-63). The symbols are:

- module-level items: functions, record types, aliases, refined types, enums, distinct types, resources, traits, constants, user effects, interfaces, worlds, and extern functions;
- members: enum variants, WIT flags constants, fields and tuple-variant payloads, inherent associated functions and methods, trait methods and associated types, trait impls (including derived impls, impl/02 §2.9.17) and their items, interface impls, interface members and interface resource methods;
- generic parameters, `test` declarations, and `property` declarations;
- the items of WIT packages (Stage 4), with the canonical paths of spec/04 §4.9.1.

Packages, modules, local bindings, closures, `async` blocks, `use` declarations, invariant declarations, and world items are not symbols. A module is identified by its canonical path, because a module is a file (D-SYN-08) and has no identity beyond its path. An invariant declaration and the items of a world are parts of the type or world they belong to (§12.8.2).

Every symbol has a **kind**, used in JSON as one of these strings:

| Kind | Entities |
|---|---|
| `fn` | module-level function |
| `assoc_fn` | inherent associated function or method, trait method, impl item function, interface member function, resource method |
| `extern_fn` | function of an `extern` block |
| `record`, `alias`, `refined`, `enum`, `distinct`, `resource` | the type declarations of the same name ([spec/05-types.md](../spec/05-types.md)) |
| `variant`, `field` | enum variant; record, resource, or record-variant field, or tuple-variant payload |
| `trait`, `assoc_type`, `impl` | trait; associated type declaration or definition; trait impl or interface impl |
| `const` | module constants and each WIT flags constant `F.X` at `ns:pkg/iface.F.X` (spec/13 §13.9.4) |
| `effect`, `interface`, `world` | the declarations of the same name |
| `generic_param` | type or const generic parameter |
| `test`, `property` | `test` and `property` declarations ([spec/14-testing.md](../spec/14-testing.md)) |

The **declaration node** of a symbol is the CST node that declares it: the item, member, field, variant, or generic parameter node, or, for a derived impl, the argument of `@derive` that names the trait. A symbol from a dependency has a declaration node in the dependency's source, or none for items of WIT packages and of binary artifacts.

### 12.2.2 Syntax of symbol IDs

A symbol ID is built from the symbol's canonical path by spec/04 §4.9.2, with these precisions: the `PackageId` authority is serialized as `<namespace>%3A<package>` so the package separator is a URI percent-encoded byte, and in step 3 the text of each `Component` (after its leading `.` is removed) is **percent-encoded** (N-H-64). Every byte of a component's UTF-8 encoding that is not one of `A`–`Z`, `a`–`z`, `0`–`9`, and ``- . _ ~ ! $ & ' ( ) * + , ; = : @`` is written as `%` followed by two uppercase hexadecimal digits. Module segments match `[a-z][a-z0-9_]*` (spec/04 §4.2.3) and are never encoded.

```text
SymbolId ::= 'symbol://' PackageAuthority ( '/' Segment )+ '#' HEX6
PackageAuthority ::= Namespace '%3A' Package
Segment  ::= ( pchar )+          (* RFC 3986 pchar: unreserved, pct-encoded, sub-delims, ':', '@' *)
HEX6     ::= [0-9A-F] [0-9A-F] [0-9A-F] [0-9A-F] [0-9A-F] [0-9A-F]
```

| Canonical path | Symbol ID |
|---|---|
| `acme:user.User.create` | `symbol://acme%3Auser/User/create#8F21A7` |
| `acme:shop/model.Order.field[total]` | `symbol://acme%3Ashop/model/Order/field%5Btotal%5D#3C0E91` |
| `acme:shop/model.Shape.Rect.field[width]` | `symbol://acme%3Ashop/model/Shape/Rect/field%5Bwidth%5D#E1D2C3` |
| `acme:shop/model.impl[sai:core/fmt.Format for acme:shop/model.Order].fmt` | `symbol://acme%3Ashop/model/impl%5Bsai:core%2Ffmt.Format%20for%20acme:shop%2Fmodel.Order%5D/fmt#D41B07` |
| `acme:image/decode.test["truncated header"]` | `symbol://acme%3Aimage/decode/test%5B%22truncated%20header%22%5D#0A77E2` |

The hexadecimal parts in examples are illustrative. The path part of a symbol ID is descriptive: it shows the canonical path at the time the ID was printed, and it changes when the symbol is renamed or moved. The **identity** of a symbol is the pair of its package node (a package identity and a compatibility line, [package/02-resolver.md](../package/02-resolver.md) §2.1) and its hexadecimal part. Within one package node, no two symbols have the same hexadecimal part. The same canonical path is never printed with two different hexadecimal parts in one result. The URI string is a reference descriptor, not an identity key: callers resolve its hexadecimal part in a selected package node and must not use the descriptive path as identity.

Rationale for the encoding: a literal `:` in a URI authority introduces a port delimiter, so `<namespace>:<package>` is not a valid RFC 3986 authority form. Encoding the package separator as `%3A` makes it an unambiguous valid `reg-name`. Path components contain `[`, `]`, `"`, spaces, and `/` (inside `impl[…]`); encoding those keeps the path unambiguous. Together, these rules make every symbol ID a valid RFC 3986 URI while the canonical path, which every JSON record carries next to the symbol ID, stays the readable form.

**Parsing.** A string is parsed as a symbol ID by checking the prefix `symbol://`; reading the URI authority up to the first `/`; validating it as a URI `reg-name`; decoding its percent escapes and validating the resulting `PackageId`; reading one or more nonempty URI path segments up to the `#`; and reading exactly six uppercase hexadecimal digits that end the string. URI percent escapes may use either hexadecimal case, but the parsed ID is normalized to uppercase escapes. A raw `:` in the authority is invalid. A string that starts with `symbol://` and fails one of these steps is not a valid symbol ID (§12.2.5). Lowercase hexadecimal digits are not accepted, because the IDs that tools print are always uppercase.

### 12.2.3 Allocation of the hexadecimal part

The hexadecimal part is computed from the symbol's canonical path when the symbol is first seen, and persisted for the symbols of workspace packages (§12.9). The computation is deterministic (N-H-65):

1. The **seed** of a symbol `s` for probe `k = 0, 1, 2, …` is the 24 most significant bits of the 128-bit output of SipHash-1-3 with the all-zero key, applied to the bytes `sai-symbol` `0x00` `<ns>:<pkg>` `0x00` `<canonical path of s>` `0x00` followed by `k` as a little-endian `u32`. It is printed as six uppercase hexadecimal digits. The compatibility line is not part of the input, so a symbol has the same seed in every version of its package.
2. **Fresh allocation** of a set of symbols of one package node: the symbols are visited in byte order of their canonical paths. Each symbol takes its seed for the smallest `k` whose value is not taken by a symbol visited before it, and not reserved by the symbol index (§12.2.4).
3. The symbols of a **dependency** are always allocated fresh, without an index. The same version of a package therefore has the same symbol IDs in every workspace and on every machine.
4. The symbols of a **workspace package** (the root package or a workspace member, [13-cli.md](13-cli.md) §13.2) keep the hexadecimal part recorded in the symbol index. Symbols without an entry are allocated fresh, with every hexadecimal part recorded in the index reserved.

If all `2^24` values are already reserved or allocated, fresh allocation fails with `SymbolSpaceExhausted`; it does not wrap or reuse an ID. Without an index, a workspace package gets exactly the IDs it would get as a dependency. A probe `k > 0` is needed only for colliding seeds. A collision makes the later symbol's ID depend on the earlier one, so in a package without an index, adding a symbol can change the ID of a colliding symbol that sorts after it. The index prevents this for workspace packages.

### 12.2.4 Persistence across renames and moves

The symbol index (§12.9) records, for each symbol of each workspace package, its hexadecimal part, its canonical path, its `DefPathHash` ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.8.2), its kind, and its fingerprint. Every command that analyzes the workspace, and the language server after each analysis, **reconciles** the index with the current symbols of each workspace package, in this order:

1. **By key.** A current symbol whose `DefPathHash` equals that of an entry takes the entry's hexadecimal part. If the package's compatibility line changed, which changes every `DefPathHash`, the canonical path is used as the key instead.
2. **Recorded changes.** The operations that change canonical paths (`rename_symbol`, `move_symbol`, and the other operations of §12.6 that rename, move, or re-parent a symbol) update the index entries of the symbols they change, in the same command that writes the edited files (§12.6.2 rule 7). Step 1 then matches them, and their IDs survive the rename or move.
3. **Members follow their parent.** When a symbol is matched to an entry whose canonical path differs, the entries of its members (the symbols whose canonical path starts with the entry's old path followed by `.`) are rekeyed by replacing that prefix with the new path, and matched by step 1. Renaming a type thus keeps the IDs of its fields, variants, and methods.
4. **Rematching** (N-H-67). The current symbols and the entries that are still unmatched are compared by kind and fingerprint. A current symbol and an entry are matched if they have the same kind and the same fingerprint, and neither has another unmatched partner with that kind and fingerprint. Matches are made in byte order of the current canonical paths, and step 3 is applied after each match. This step recognizes renames and moves made with a text editor, outside the operations of §12.6.
5. **Allocation.** The current symbols that are still unmatched are allocated fresh (§12.2.3 rule 4).
6. **Removal.** Entries that are still unmatched are deleted.

The **fingerprint** of a symbol is the SipHash-1-3 128-bit hash, with the all-zero key, of its kind, a `0x00` byte, and the sequence of the kinds and texts of the tokens of its declaration node, excluding trivia, `NEWLINE` tokens, and the `DOCS` node, in which the text of the declaration's own name token (the `NAME` child, or the string of a `test` or `property`) is replaced by the empty string. A symbol that is renamed, or moved to another file without other changes, keeps its fingerprint. A symbol whose body also changed is not rematched and gets a new ID.

A WIT flag constant has no separate `VariantDef` or view `DefId`. Its fingerprint uses the source flag occurrence with its own name token masked, followed by the stable token pair `("WitFlagsBitIndex", <zero-based bit index>)`. Its inventory-only identity path extends the verified flags-parent view `DefPath` with `ValueNs(flag)` and that same bit index as the component disambiguator; `DefPathHash` of this path is the symbol key. Reordering flags therefore changes both the constant fingerprint and `DefPathHash`, even when the public path `ns:pkg/iface.F.X` remains the same. This derived path does not claim that HIR allocated a definition for the constant.

Consequences:

- An ID printed by a tool resolves to the same symbol after any number of renames and moves made by operations, as long as the index is kept (§12.2.5).
- Deleting `.sai/index/` loses only the identities that differ from fresh allocation, that is, those of symbols renamed or moved since their creation ([13-cli.md](13-cli.md) §13.5.2). Every other symbol gets the same ID again.
- A fresh clone of a repository has no index, because the index is not committed ([13-cli.md](13-cli.md) N-H-28). Its symbols get fresh IDs, which are the same on every machine.
- The IDs that appear in JSON output therefore depend on the index. No other result of a command depends on it, and no artifact contains a symbol ID (draft §64).

### 12.2.5 Resolving references

Commands and protocol methods accept a **symbol reference**: a symbol ID or a canonical path ([13-cli.md](13-cli.md) §13.6.16 to §13.6.18). A reference that starts with `symbol://` is a symbol ID; every other reference is a canonical path.

1. **Package node.** The package identity of the reference selects the package nodes of the workspace snapshot's resolved graph with that identity. If there is more than one (two compatibility lines, package/02 §2.1), the node is chosen in this order (N-H-66): the root package or a workspace member; the node that the root package requires directly; the node with the highest selected version. Results always name the package node with its version, so the choice is visible.
2. **Symbol ID.** A string that is not a valid symbol ID (§12.2.2) is the error `UNKNOWN_SYMBOL_ID`, with the message ``` `<string>` is not a valid symbol ID ```. Otherwise the hexadecimal part is looked up in the chosen package node. If no symbol has it, the error is `UNKNOWN_SYMBOL_ID` with the message ``` no symbol has the ID `<string>` ```, and, if the path part is the canonical path of a current symbol, the help ``` the symbol at this path now has the ID `<current ID>` ```. The path part is not used to find the symbol. If it differs from the symbol's current canonical path, the reference still resolves, and results carry the current ID.
3. **Canonical path.** The reference is parsed with the grammar of spec/04 §4.9.1. It resolves to the symbol or the module whose canonical path is byte-for-byte equal to it. A bare `<ns>:<pkg>` therefore names the package's root module. Package nodes, effect atoms, and capabilities are named by the node keys of §12.4.1 where a command accepts them (§12.4.3). `CanonType`s inside `impl[…]` are compared in the printed form of spec/04 §4.9.1, so `Int` must be written `I64`. A reference that does not parse, or that names nothing, is the error `UNKNOWN_SYMBOL_PATH` with the message ``` no item has the canonical path `<string>` ```. Its help lists candidates by the rules of [11-diagnostics.md](11-diagnostics.md) §11.4.5, taken from the symbols whose canonical path has the same parent (the same path without its last component).
4. A reference to a module is accepted where a command or method accepts graph nodes (§12.4.3). Where only symbols are accepted, such as the target of most operations, it is the error `INVALID_OPERATION_TARGET` (§12.6.2).

Both errors have `primary` `null` when the reference comes from the command line ([11-diagnostics.md](11-diagnostics.md) §11.4.3), and the `data` object `{ "reference": <string>, "candidates": [<canonical path>] }`.

---

## 12.3 Node IDs

A **node ID** names one element of the CST of one document snapshot ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6). Node IDs are how diagnostics, fixes, and protocol requests point at code that is not a symbol: an expression, a statement, a single token (D-TOOL-01, D-TOOL-02). Unlike symbol IDs, they are valid only for the snapshot they were computed on.

### 12.3.1 Groups

A node ID has the form `<group>:<hex>`. The group is derived from the element's `SyntaxKind` by the groups of [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6.5:

| impl/02 group | Node ID group | Example kinds |
|---|---|---|
| file | `file` | `SOURCE_FILE`, `MODULE_HEADER`, `DOCS`, `ATTRIBUTE`, `VISIBILITY` |
| items | `item` | `FN_DECL`, `PARAM`, `FIELD_DECL`, `VARIANT_DECL`, `IMPL_DECL`, `TEST_DECL` |
| statements | `stmt` | `BLOCK`, `LET_STMT`, `ASSIGN_STMT`, `EXPR_STMT` |
| expressions | `expr` | `CALL_EXPR`, `BIN_EXPR`, `MATCH_ARM`, `ARG` |
| patterns | `pat` | `BINDING_PAT`, `CTOR_PAT`, `OR_PAT` |
| types | `type` | `PATH_TYPE`, `BORROW_TYPE`, `FN_PTR_TYPE` |
| names | `name` | `NAME`, `NAME_REF` |
| errors | `error` | `ERROR` |
| (tokens) | `tok` | every token, including `NEWLINE` and `EOF` |

The group is redundant with the element it names. It exists so that a reader of a node ID knows what kind of code it points at, and so that a stale ID that now names an element of another group is detected (§12.3.3).

### 12.3.2 The hexadecimal part

The hexadecimal part is the **pre-order index** of the element among all elements of the file's tree (N-H-68):

1. The elements are the nodes and the tokens of the red tree. Trivia are not elements, because they are stored on tokens and have no node of their own ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6.3). Zero-width `NEWLINE` tokens, the `EOF` token, and zero-width `ERROR` nodes are elements.
2. The tree is traversed in pre-order with `children_with_tokens`: a node is numbered before its children, and children are numbered in order. The root `SOURCE_FILE` has index 0.
3. The index is written in uppercase hexadecimal, padded with leading zeros to at least four digits. An index that needs more digits is written with exactly as many as it needs. A file has fewer than 2^64 elements, so the hexadecimal part has at most 16 digits.

So the root of every file is `file:0000`, and an expression with the pre-order index 33,441 is `expr:82A1`. Node IDs are cheap to compute: a traversal that counts elements, without any table. Two trees that are equal (impl/02 §2.6.6 rule 6) have the same node IDs.

A node ID does not name its file. Every record that carries a node ID also carries the source reference of the file ([11-diagnostics.md](11-diagnostics.md) §11.5.4), either in the same object or in the enclosing diagnostic. In requests, a node is given as a **node reference**:

```json
{ "file": { "path": "src/service.sai", "package": "acme:user@1.0.0", "snapshot": 12 }, "node": "expr:82A1" }
```

`snapshot` is the document version over LSP and in the semantic protocol, and `null` in the CLI, where it means the content on disk (§12.1.2).

### 12.3.3 Resolution and staleness

A node reference is resolved in this order:

1. The node ID is parsed by the pattern `^[a-z][a-z_]*:[0-9A-F]{4,16}$` and the group must be one of the nine groups of §12.3.1. Otherwise it is the error `UNKNOWN_NODE_ID` with the message ``` `<string>` is not a valid node ID ```.
2. The file must be a file of the workspace snapshot. Otherwise it is the error `UNKNOWN_NODE_ID` with the message ``` no file `<path>` in the workspace ```.
3. If `snapshot` is an integer and is not the current version of the document in the server, the request is answered with `STALE_SNAPSHOT`, with the message ``` `<path>` has changed: version <current> is current, the request names version <snapshot> ```. Its `data` object is `{ "path": <string>, "requested": <integer>, "current": <integer> }`. The client recomputes the node ID from the current version, or re-requests the information it came from. Nothing is changed.
4. The element with the given index must exist and be in the given group. Otherwise it is the error `UNKNOWN_NODE_ID` with the message ``` `<node ID>` does not name a <group> node in `<path>` ```.

In the CLI, `snapshot` is `null`, and a file that has changed since a node ID was printed cannot be detected by step 3. Step 4 still rejects most stale IDs, and the operations that take node parameters check that the node lies inside the target's declaration node (§12.6.2 rule 2). Agents that edit files between commands SHOULD use symbol references for targets, and node IDs only within one snapshot (draft §64).

---

## 12.4 Semantic graphs

### 12.4.1 Model

A semantic graph is a directed graph whose nodes are semantic objects and whose edges are typed relations between them (draft §62). Its nodes have one of five **node kinds**:

| Node kind | Objects | Printed form | Node key in JSON |
|---|---|---|---|
| `symbol` | the symbols of §12.2.1 | canonical path | the symbol ID |
| `module` | modules | canonical path | `module:<canonical path>` |
| `package` | package nodes of the resolved graph | `<ns>:<pkg>@<version>` | `package:<ns>:<pkg>@<version>` |
| `effect` | built-in effect atoms and interface and world-import effect atoms ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.2) | the atom's spelling, such as `net.http` or `wasi:http/outgoing-handler` | `effect:<spelling>` |
| `capability` | capability names ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.1) | the capability name | `capability:<name>` |

User effects are symbols of kind `effect` (§12.2.1), not effect nodes. A node **belongs** to the package node whose source declares it: a symbol or module to its package, a package node to itself. Effect and capability nodes belong to no package. A node **is a dependency node** if it belongs to a package node that is neither the root package nor a workspace member.

Every graph is computed from one workspace snapshot, for the snapshot's target and profile, after name resolution, type checking, and effect inference. A graph contains only facts that the compiler has established. Where a fact is unknown because the program has errors (an unresolved name, a call whose callee did not type-check), the graph has no edge for it, and the result says so (§12.4.4, `complete`).

### 12.4.2 The six graphs

There are six graphs. Each has a fixed set of **edge kinds**, used as the edge labels in output and as the values of `--edge` ([13-cli.md](13-cli.md) §13.6.16). Every edge has a list of **sites**: the source locations that establish it, in position order. Each location is a location of [11-diagnostics.md](11-diagnostics.md) §11.9.5.

| Graph | Edge kind | From | To | Meaning | Sites |
|---|---|---|---|---|---|
| `symbols` | `contains` | module or symbol | module or symbol | the parent of the target in the tree of canonical paths (see below) | none |
| | `implements` | `impl` symbol | trait or interface symbol | the trait or the interface of a trait impl or interface impl | the impl header |
| | `for_type` | `impl` symbol | type symbol | the self type of a trait impl, if its head is a nominal type | the self type |
| | `supertrait` | trait symbol | trait symbol | a supertrait bound of the trait | the bound |
| | `reexports` | module | symbol or module | a `pub use` of the module re-exports the target (D-MOD-04) | the `USE_ITEM` |
| `references` | `references` | symbol or module | symbol or module | a resolved name occurrence in the source of the origin names the target | every occurrence |
| `calls` | `calls` | symbol with a body | function symbol | the origin's body calls the target (see below) | every call site |
| `effects` | `has_effect` | function symbol | effect node or `effect` symbol | the atom is in the function's signature effect set ([08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.2.6), declared or inferred | the atom in the effects clause, or none if inferred |
| | `covers` | `effect` symbol | effect node or `effect` symbol | an element of the user effect's `covers` clause (D-EFF-03) | the element |
| | `maps_to` | effect node | capability node | the capability is in `caps(atom)` ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.2) | none |
| `capabilities` | `depends_on` | package node | package node | a direct dependency in the resolved graph ([package/02-resolver.md](../package/02-resolver.md)) | the requirement in `sai.package` |
| | `requires` | package node | capability node | the capability is in `Req(Q)` of the build ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.4) | none |
| | `allows`, `denies` | package node | capability node | an entry of the package's `allow` or `deny` block; for the root package, of its effective policy (spec/08 §8.12.5) | the entry, or none for an entry added by the profile |
| `tests` | `tests` | `test` or `property` symbol | function symbol of a workspace package | the function is reachable from the test (see below) | none |

Precisions:

- **Containment** follows canonical paths (spec/04 §4.9.1): a module contains its submodules (the modules whose path adds one segment) and its module-level items, impls, tests, and properties; a type, trait, interface, or impl contains its members, so `acme:shop/model.Order` contains `acme:shop/model.Order.field[total]` and `acme:shop/model.Order.create`, wherever the inherent impl is written; an enum variant contains its fields; an item contains its generic parameters. Every symbol has exactly one incoming `contains` edge, and so does every module except a root module.
- **Origins of references.** The origin of an occurrence is the innermost symbol whose declaration node contains it. Occurrences in signatures, contract clauses, bounds, field types, and bodies all count. Occurrences in closures and `async` blocks belong to the enclosing symbol; occurrences in an invariant declaration belong to the type it constrains; occurrences in `use` declarations and in the module header belong to the module. An occurrence is a `NAME_REF`, a field name in a field access, record literal, or record pattern, a method name, an implicit member (`.Variant`), or a trait named by `@derive`. Occurrences that did not resolve have no edge.
- **Calls.** The origins of `calls` edges are the symbols with bodies: functions, tests, properties, and constants (their initializers), with the attribution rules of references. A call expression or method call whose callee resolves statically to one function gives an edge to that function: an inherent function, a module-level or extern function, or the impl item that the call selects. A call of a trait method on a type parameter or a projection gives an edge to the trait method itself (`Tr.x`). The site of a call is its callee expression for a call expression, and the method name for a method call. An operator, `for` loop, `.await`, or other construct that the desugaring of [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.9 turns into a call of a user-visible trait method gives an edge like an explicit method call, with the operator token as its site. A call through a function pointer or closure value gives no edge, because its callee is not known statically; the function that was turned into the value has a `references` edge instead. Drop glue gives no edges.
- **Reachability of tests.** A `tests` edge goes from a test or property to every function of a workspace package whose body is in the reachable set `Reach` of [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.3, computed for the snapshot's target with that test as the only root: `reach_from` of [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.7.1 with the one root `Test(t)`. Functions of dependencies get no `tests` edges: the graph answers which tests exercise the code of the workspace.
- A **function symbol** is a symbol of kind `fn`, `assoc_fn`, or `extern_fn`.

### 12.4.3 Queries

A **graph query** has the inputs `graph`, `starts` (a list, possibly empty), `direction` (`out`, `in`, or `both`), `depth` (1 to 64), `edges` (a set of edge kinds of the graph; by default all of them), and `deps` (a boolean). [13-cli.md](13-cli.md) §13.6.16 maps the options of `sai graph` onto these inputs, and the protocol method `sai/graph` takes them as parameters (§12.5.3). The query is evaluated as follows (N-H-69):

1. **Edges.** `E` is the set of edges of the graph whose kind is in `edges`. An edge is **followed forward** from its origin to its target, and **followed in reverse** from its target to its origin. `out` follows edges forward, `in` in reverse, and `both` in both ways.
2. **Starts.** Each start is resolved: a string that starts with `module:`, `package:`, `effect:`, or `capability:` is a node key (§12.4.1), and every other string is a symbol reference (§12.2.5). A `package:` key may omit `@<version>`, and the node is then chosen by §12.2.5 rule 1. A key that names no node of the snapshot is the error `UNKNOWN_SYMBOL_PATH`. A start that has no edge in `E` is still printed, as a tree of one node.
3. **Roots.** Without starts, the query starts at the **roots** of the graph, in byte order of their printed forms and then their node keys. Let `G` be `E` oriented in the direction of the query (forward for `both`). A node is a root if it is not a dependency node, has at least one edge in `E`, and either has no incoming edge in `G`, or belongs to a strongly connected component of `G` that has no incoming edge from outside the component, and is that component's non-dependency node with the smallest printed form and node key. So every node with an edge is reachable from some root.
4. **Trees.** Each start, or each root, gives one tree. The tree is built by a depth-first visit from the start, with an expanded set that is empty at the start of each tree. Visiting node `n` at distance `d` from the start:
   1. if `n` is a dependency node, `n` is not the start, and `deps` is false, `n` is printed with the mark `dep` and not expanded;
   2. otherwise, if `n` is in the expanded set, it is printed with the mark `seen` and not expanded;
   3. otherwise, if `d` equals `depth`, it is printed without a mark and not expanded;
   4. otherwise, `n` is added to the expanded set, printed without a mark, and its children are visited at distance `d + 1`. The children are the far ends of the edges of `E` that are followed from `n`, one child per edge and direction, sorted by: forward before reverse; edge kind, in byte order; printed form, in byte order; node key, in byte order.
5. **Labels.** The label of a child is its edge kind if the edge was followed forward, and `<-` followed by the edge kind if it was followed in reverse. So `sai graph calls --in acme:user/db.insert` prints `<-calls acme:user/service.create_user`: `create_user` calls `insert`.

The marks `seen` and `dep` are printed by [13-cli.md](13-cli.md) §13.6.16 as ` (*)` and ` (dep)`. Without starts the default depth is 64, so a query without starts prints every edge of the workspace part of the graph, each node expanded once per tree.

**Example.** For the `calls` graph of a package `acme:user` in which `api.handle_signup` calls `service.create_user`, `service.create_user` calls `db.insert` and `model.User.new`, and `db.insert` calls `sai:alloc/string.String.concat`:

```text
$ sai graph calls --in acme:user/db.insert --depth 2
acme:user/db.insert
└─ <-calls acme:user/service.create_user
   └─ <-calls acme:user/api.handle_signup
```

### 12.4.4 Query result

The result of a graph query is the following JSON object, the `data` of the `graph` record of [13-cli.md](13-cli.md) §13.6.16 and the result of `sai/graph` (§12.5.3). Its schema is `graph_result` in §12.11.

```json
{
  "graph": "calls",
  "direction": "out",
  "depth": 2,
  "edge_kinds": ["calls"],
  "deps": false,
  "complete": true,
  "trees": [
    {
      "node": "symbol://acme%3Auser/service/create_user#5B0C3E",
      "children": [
        { "edge": "calls", "node": "symbol://acme%3Auser/db/insert#91AF02", "mark": null,
          "children": [
            { "edge": "calls", "node": "symbol://sai%3Aalloc/string/String/concat#2D7E40", "mark": "dep", "children": [] }
          ] },
        { "edge": "calls", "node": "symbol://acme%3Auser/model/User/new#C4410B", "mark": null, "children": [] }
      ]
    }
  ],
  "nodes": [
    { "key": "symbol://acme%3Auser/db/insert#91AF02", "kind": "symbol", "symbol_kind": "fn",
      "path": "acme:user/db.insert", "package": "acme:user@0.4.0", "dependency": false,
      "location": { "file": { "path": "src/db.sai", "package": "acme:user@0.4.0", "snapshot": null },
                    "start": { "byte": 112, "line": 7, "col": 8, "utf16_col": 8 },
                    "end": { "byte": 118, "line": 7, "col": 14, "utf16_col": 14 } } },
    { "key": "symbol://acme%3Auser/model/User/new#C4410B", "kind": "symbol", "symbol_kind": "assoc_fn",
      "path": "acme:user/model.User.new", "package": "acme:user@0.4.0", "dependency": false,
      "location": { "file": { "path": "src/model.sai", "package": "acme:user@0.4.0", "snapshot": null },
                    "start": { "byte": 240, "line": 12, "col": 12, "utf16_col": 12 },
                    "end": { "byte": 243, "line": 12, "col": 15, "utf16_col": 15 } } },
    { "key": "symbol://acme%3Auser/service/create_user#5B0C3E", "kind": "symbol", "symbol_kind": "fn",
      "path": "acme:user/service.create_user", "package": "acme:user@0.4.0", "dependency": false,
      "location": { "file": { "path": "src/service.sai", "package": "acme:user@0.4.0", "snapshot": null },
                    "start": { "byte": 58, "line": 4, "col": 8, "utf16_col": 8 },
                    "end": { "byte": 69, "line": 4, "col": 19, "utf16_col": 19 } } },
    { "key": "symbol://sai%3Aalloc/string/String/concat#2D7E40", "kind": "symbol", "symbol_kind": "assoc_fn",
      "path": "sai:alloc/string.String.concat", "package": "sai:alloc@0.1.0", "dependency": true,
      "location": { "file": { "path": "dep:sai:alloc@0.1.0/src/string.sai", "package": "sai:alloc@0.1.0", "snapshot": null },
                    "start": { "byte": 4410, "line": 151, "col": 12, "utf16_col": 12 },
                    "end": { "byte": 4416, "line": 151, "col": 18, "utf16_col": 18 } } }
  ],
  "edges": [
    { "kind": "calls", "from": "symbol://acme%3Auser/db/insert#91AF02", "to": "symbol://sai%3Aalloc/string/String/concat#2D7E40",
      "sites": [{ "file": { "path": "src/db.sai", "package": "acme:user@0.4.0", "snapshot": null },
                  "start": { "byte": 160, "line": 8, "col": 22, "utf16_col": 22 },
                  "end": { "byte": 166, "line": 8, "col": 28, "utf16_col": 28 } }] },
    { "kind": "calls", "from": "symbol://acme%3Auser/service/create_user#5B0C3E", "to": "symbol://acme%3Auser/db/insert#91AF02",
      "sites": [{ "file": { "path": "src/service.sai", "package": "acme:user@0.4.0", "snapshot": null },
                  "start": { "byte": 131, "line": 6, "col": 8, "utf16_col": 8 },
                  "end": { "byte": 140, "line": 6, "col": 17, "utf16_col": 17 } }] },
    { "kind": "calls", "from": "symbol://acme%3Auser/service/create_user#5B0C3E", "to": "symbol://acme%3Auser/model/User/new#C4410B",
      "sites": [{ "file": { "path": "src/service.sai", "package": "acme:user@0.4.0", "snapshot": null },
                  "start": { "byte": 95, "line": 5, "col": 16, "utf16_col": 16 },
                  "end": { "byte": 103, "line": 5, "col": 24, "utf16_col": 24 } }] }
  ]
}
```

- `edge_kinds` is the effective set of edge kinds, in byte order. `depth` is the effective depth.
- `trees` has one entry per start or root, in order. An entry has `node` and `children`. A child also has `edge` (the label of §12.4.3 rule 5) and `mark` (`null`, `"seen"`, or `"dep"`). A child with a mark has no children.
- `nodes` has one entry for every node that appears in `trees`, sorted by `key` in byte order. `kind` is the node kind of §12.4.1. `symbol_kind` is the symbol kind of §12.2.1 for symbols and `null` otherwise. `path` is the printed form. `package` is the package node the node belongs to, written `<ns>:<pkg>@<version>`, or `null` for effect and capability nodes. `location` is the location of the name in the declaration node, or `null` for nodes without a declaration in source.
- `edges` has one entry for every edge of `E` that a tree follows, once, sorted by `from`, `kind`, and `to` in byte order. `sites` is sorted by position ([11-diagnostics.md](11-diagnostics.md) §11.11.1).
- `complete` is `false` if the analysis of a workspace package reported an error, so that edges may be missing (§12.4.1). The errors are reported as diagnostics. A graph is not computed at all if the analysis it needs did not run: if the package graph did not resolve, or, for `effects` and `capabilities`, if effect inference was stopped by an error (impl/08 §8.1.9). [13-cli.md](13-cli.md) §13.6.16 then exits with 1.

### 12.4.5 Typed AST access

Draft §62 asks for typed AST access. It is provided by the protocol method `sai/typedAst` (§12.5.3), not by a graph: the CST of [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6 with node IDs, annotated with the facts that analysis established for each node. Tools read structure from it and edit through operations (§12.6) or fixes, never by printing a modified tree themselves.

---

## 12.5 Semantic protocol

### 12.5.1 Transport and negotiation

The semantic protocol is a set of JSON-RPC 2.0 requests whose method names start with `sai/`. They are served by the language server `sai lsp` ([13-cli.md](13-cli.md) §13.6.21) on the same connection as LSP, so a client needs one process for both. The protocol is Stage 6.

1. **Negotiation.** The client MAY send `initializationOptions.sai.protocol`, an array of the protocol versions it supports. The server answers in `InitializeResult.capabilities.experimental.sai` with `{ "protocol": 1, "methods": [<method names>], "operations": [<operation names of §12.6>], "graphs": [<graph names>] }`, listing what the toolchain's stage implements. If the client lists versions and 1 is not among them, the server still answers, with `"protocol": null`, and every `sai/` request then fails with the JSON-RPC error `-32601` (method not found). This chapter defines version 1.
2. **Workspace.** Every request has an optional parameter `workspace`: the URI of a workspace folder (§12.10.3). It selects the workspace whose root package the request is about. It may be omitted if the server has exactly one workspace. Otherwise its absence is the JSON-RPC error `-32602` (invalid params).
3. **Snapshots.** A request is evaluated on the workspace snapshot that is current when the server starts processing it. Results that contain locations carry the document versions of that snapshot in `file.snapshot`. A request that names a document version is checked by §12.3.3 rule 3.
4. **Data model.** Parameters and results use the JSON forms of the CLI: locations, source references, edits, and diagnostics of [11-diagnostics.md](11-diagnostics.md) §11.9.5, node references of §12.3.2, symbol references of §12.2.5, and the result records of this chapter. Files are named by the paths of [11-diagnostics.md](11-diagnostics.md) §11.5.4, not by URIs. Positions in these records carry every representation of [11-diagnostics.md](11-diagnostics.md) §11.5.5, so they do not depend on the negotiated position encoding (§12.10.2).
5. **Cancellation and concurrency.** The client MAY cancel a request with `$/cancelRequest`. Before the server dispatches `workspace/applyEdit` or returns an edit, cancellation is answered with `-32800` (request cancelled); if a journal was prepared, the server first verifies the exact before state and durably clears it. After an edit is dispatched or returned, cancellation cannot retract the client's possible source change: the server must not answer `-32800` or claim that nothing was written. It completes the transaction when possible, otherwise preserves the journal and reports or recovers the unresolved operation. The server MAY evaluate read-only requests concurrently. Requests that change files (`sai/applyFix`, `sai/applyOperation`) are evaluated one at a time, in the order received.

### 12.5.2 Errors

A request that fails is answered with a JSON-RPC error:

| Condition | `code` | `message` | `data` |
|---|---|---|---|
| a condition with a diagnostic code, such as `UNKNOWN_SYMBOL_ID` or an operation precondition | `-32001` | the main message of the first diagnostic | `{ "diagnostics": [<diagnostic>] }` |
| `STALE_SNAPSHOT` (§12.3.3) | `-32801` (LSP `ContentModified`) | as above | as above |
| parameters that do not match the method's schema | `-32602` | a description of the first mismatch | `null` |
| an unknown method, or a method that the toolchain's stage does not implement | `-32601` | `method not found` | `null` |
| a request before `initialize` | `-32002` (LSP `ServerNotInitialized`) | `server not initialized` | `null` |
| `INTERNAL_COMPILER_ERROR` ([11-diagnostics.md](11-diagnostics.md) §11.4.6) | `-32603` | as for `-32001` | as for `-32001` |

The diagnostics in `data` are diagnostic records of [11-diagnostics.md](11-diagnostics.md) §11.9.5, with the codes of this chapter. They are not published with `textDocument/publishDiagnostics`, because they describe the request, not the program.

### 12.5.3 Methods

| Method | Parameters | Result |
|---|---|---|
| `sai/resolve` | `reference`: a symbol reference or node key | the node entry of §12.4.4 for the referenced object |
| `sai/symbol` | `reference` | the symbol record below |
| `sai/graph` | `graph`, `starts`, `direction`, `depth`, `edges`, `deps`, with the defaults of §12.4.3 | `graph_result` (§12.4.4) |
| `sai/typedAst` | `file` (a source reference), optional `node` (default the root), optional `depth` (default unlimited), optional `tokens` (default `false`) | the typed tree below |
| `sai/explain` | `code` | the explanation record of [11-diagnostics.md](11-diagnostics.md) §11.13.2 |
| `sai/applyFix` | `file` (a source reference with `snapshot`), `node`, `code`, `fix` (an index) | `{ "applied": <boolean>, "edits": [<edit>], "operation_id": <string>? }` |
| `sai/planOperation` | an operation request (§12.6.1), and `size` (default `false`, §12.7.2 rule 5) | `{ "operation", "target", "impact", "edits", "changed_files" }` as in [13-cli.md](13-cli.md) §13.6.18 |
| `sai/applyOperation` | an operation request, `size`, and `allow_breaking` (default `false`) | as `sai/planOperation`, plus `"applied": <boolean>` and optional `"operation_id"` |
| `sai/impact` | an operation request, and `size` | the impact record of §12.7.3 |
| `sai/diff` | `from` and `to` (a Git revision, or `null` for the current snapshot; defaults `"HEAD"` and `null`), `size` and `public` (default `false`) | the diff document of §12.8.5, extended by `from` and `to` as in [13-cli.md](13-cli.md) §13.6.17 |

The **symbol record** of `sai/symbol` is `{ "symbol", "path", "kind", "package", "visibility", "public", "location", "declaration", "signature", "docs", "effects", "members" }`: the symbol ID; the canonical path; the symbol kind; the package node; the visibility level of spec/04 §4.6 (`"package"`, `"pub"`, or `"export"`); whether the symbol is part of the package's public surface (spec/04 §4.6.7); the location of its name; the node reference of its declaration node, or `null`; the canonical one-line text of its declaration without its body and doc comments ([14-formatter.md](14-formatter.md) §14.13); the text of its `DOCS` node (impl/02 §2.6.4), or `null`; for function symbols, the signature effect set in canonical order, and `null` otherwise; and the symbol IDs of the symbols it contains (§12.4.2), in byte order of their canonical paths.

The **typed tree** of `sai/typedAst` is one entry per element, starting at `node`: `{ "node": <node ID>, "kind": <SyntaxKind name>, "range": <location>, "children": [...] }`, with the additional members `"text"` for tokens; `"type"` for expression and pattern nodes, the `CanonType` (spec/04 §4.9.1) of the node's type, or `null` if it is unknown; `"defines"` for `NAME` nodes, the symbol ID defined, or `null` for local bindings; and `"resolves_to"` for `NAME_REF` nodes and field and method names, the symbol ID or module key the name resolves to, or `null` for locals and unresolved names. Tokens are included only if `tokens` is `true`; `children` then lists nodes and tokens in order. Entries at `depth` below `node` have `"children": null` if they have children that were not included.

`sai/applyFix` recomputes the diagnostics of the named snapshot, takes the diagnostic with the given `code` whose `node` is the given node, and applies its fix with the given index. A missing diagnostic or fix index is the error `FIX_NOT_FOUND`. The fix is applied by §12.10.6. When `workspace/applyEdit` is supported, `applied` is the client's answer. When it is unsupported, `applied` is `false` because the server did not apply the returned edits; the client applies them itself. If the fix's operation is an operation of §12.6 and changes indexed symbols, its journal is committed before either sending or returning its edit, and the response includes the journal's `operation_id`. A fix that does not change indexed symbols needs no journal.

`sai/applyOperation` plans the operation and computes its impact like `sai edit` ([13-cli.md](13-cli.md) §13.6.18). If the impact is breaking and `allow_breaking` is `false`, the request fails with `OPERATION_BREAKS_PUBLIC_API` ([13-cli.md](13-cli.md) N-H-41) and nothing is sent to the client. Otherwise the edits are applied by §12.10.6. When `workspace/applyEdit` is supported, `applied` is the client's answer. When it is unsupported, `applied` is `false` because the server did not apply the returned edits, and the client can apply them itself. If the operation changes indexed symbols, the durable journal is committed before sending or returning the edit; the response includes its `operation_id`, and the journal follows the same pending and recovery rules as every directly returned `WorkspaceEdit`. The index is never reconciled past an unresolved journal.

---

## 12.6 Semantic operations

A **semantic operation** is a named, parameterized change of the program, stated in terms of symbols and nodes rather than text (draft §63, D-TOOL-01). The toolchain computes its text edits and its impact. Agents SHOULD prefer operations to text edits (draft §63).

### 12.6.1 Requests and planning

An **operation request** is a JSON object:

```json
{
  "operation": "add_parameter",
  "target": "acme:user/service.create_user",
  "arguments": { "name": "email", "type": "Email", "argument": "Email.empty()" }
}
```

- `operation` is one of the eleven names of §12.6.3 to §12.6.13. Any other name is the error `UNKNOWN_OPERATION`, with the help listing the names by the rules of [11-diagnostics.md](11-diagnostics.md) §11.4.5.
- `target` is a symbol reference (§12.2.5), or, for the operations whose table allows it, a module key or canonical path, or a node reference (§12.3.2). A fix whose operation is an operation of this section names its target by the declaration node ([11-diagnostics.md](11-diagnostics.md) §11.7.3); such a node reference resolves to the symbol it declares.
- `arguments` maps parameter names to values. A value is a JSON string, which is parsed by the parameter's type (§12.6.2 rule 1), or, for `int` and `bool` parameters, also a JSON number or boolean. `sai edit` builds the same object from its command line ([13-cli.md](13-cli.md) §13.6.18): every `<parameter>=<value>` becomes a string value.

**Planning** an operation is the following sequence. It reads the workspace snapshot and writes nothing.

1. **Target.** The target is resolved (§12.2.5, §12.3.3). Its kind must be one of the target kinds of the operation's table. Otherwise it is the error `INVALID_OPERATION_TARGET`, with the message ``` `<operation>` cannot be applied to <kind> `<canonical path>` ``` and a help that names the target kinds.
2. **Arguments.** Every required parameter must be present, every present parameter must belong to the operation, and every value must parse (§12.6.2 rule 1). Otherwise it is the error `INVALID_OPERATION_ARGUMENT`, with the `data` object `{ "parameter": <name>, "value": <string or null>, "expected": <description> }`.
3. **Preconditions.** The operation's preconditions are checked in the order of its table. The first that fails is reported with its code, and planning stops.
4. **Edits.** The edits are computed by the operation's rules and the common rules of §12.6.2, and checked by §12.6.2 rule 6.

The **plan** is the result of steps 1 to 4: the target's symbol ID (its module key, for a module target), the edits, and the files they change. The impact analysis of §12.7 is then computed from the plan. **Applying** a plan writes its edits and updates the symbol index (§12.6.2 rules 7 and 8). `sai edit` plans, analyzes, and applies ([13-cli.md](13-cli.md) §13.6.18); `sai/planOperation`, `sai/impact`, and `sai/applyOperation` do the same over the protocol (§12.5.3).

### 12.6.2 Common rules

These rules apply to every operation (N-H-70):

1. **Values.** A parameter has one of these types. `ident`: an identifier token of [spec/02-lexical-structure.md](../spec/02-lexical-structure.md) that is not a keyword or reserved word, whose first letter has the case required for the symbol kind (D-LEX-02: uppercase for types, traits, variants, constants, and type parameters; lowercase for functions, parameters, fields, interfaces, worlds, and effects), and that is ASCII where D-LEX-03 requires it. `string`: any text, used as a test name without quotes. `type`: text parsed by the `Type` rule of spec/03, and resolved in the scope where it is inserted. `expr`: text parsed by the `Expr` rule, and resolved and type-checked at every place where it is inserted. In `type` and `expr` values, a path may also start with a package path as in a `use` declaration (spec/04 §4.3), followed by `.` and an item name, such as `acme:user/model.Email` (N-H-71). Such a path names the item directly, whatever is in scope, and is written at each place by rule 5. A tool that does not know the scopes of the insertion points writes every item this way. `block`: text parsed by the `Block` rule. `params`: text parsed as the contents of a `PARAM_LIST`, without the parentheses. `effects`: text parsed as an `EFFECT_SET`, with braces. `path`: a symbol reference, resolved by §12.2.5. `module`: a module canonical path. `node`: a node ID in the file of the target's declaration, or a node reference. `int`: a decimal integer without sign. `bool`: `true` or `false`. A value that does not parse as its type, that has text after the parsed construct, or that contains a comment, is invalid.
2. **Nodes.** A `node` parameter must name a node inside the declaration node of the target, in the snapshot the operation reads. Otherwise it is the error `INVALID_OPERATION_ARGUMENT`.
3. **Editable files.** Only source files of the root package and of the workspace members are edited ([13-cli.md](13-cli.md) §13.2). A target whose declaration is in a dependency is the error `INVALID_OPERATION_TARGET` for every operation that edits the declaration. Files in the package cache are never edited.
4. **Syntax errors.** If a source file of a workspace package has an error of the lexer or the parser, planning fails with `FILE_HAS_SYNTAX_ERRORS`, with the message ``` `<path>` has syntax errors ``` and the first syntax error in a note. References inside an unparsable region cannot be found, so no operation can be planned safely. Other errors do not prevent planning. The operation then uses the facts that are known, and the impact record has `complete: false` (§12.7.3).
5. **Writing code.** Edits are made on the CST. Every changed format unit is formatted with the unit formatting of [14-formatter.md](14-formatter.md) §14.3.6, and the edits are the minimal differences, sorted and merged by [11-diagnostics.md](11-diagnostics.md) §11.7.4 rules 2, 3, and 5. Comments move with the tokens that carry them ([02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.6.3), so a moved or deleted item takes its doc comments, its attributes, and the comments on the lines above it along. A reference that an operation writes uses, at its position: the name already bound to the item there, if there is one; otherwise `<b>.<Item>`, if the item's module is bound to `b` by a `use`; otherwise the bare name, after adding `<Item>` to a `use` of the item's module (a new `use <module path>.{<Item>}` if there is none). New `use` declarations are placed in the order of [14-formatter.md](14-formatter.md) §14.7.2. A `use` item that the operation made unused is removed, and a `use` declaration left empty is removed. Types are written by the same rule, with the canonical form of spec/04 §4.9.1 for structural types.
6. **Checking the result.** The planned edits are applied to an in-memory copy of the workspace snapshot, and the copy is analyzed. An error of the copy that the original does not have, compared by code, primary file, and symbol, makes planning fail with `OPERATION_INTRODUCES_ERRORS`. Its `data` is `{ "errors": [<diagnostic>] }` with the new errors as diagnostics of the copy, and its first help lists up to five of them. The only exception is `change_type`: the errors it causes at use sites are expected, and are listed in the impact record instead (§12.6.7). The **refactorings** `rename_symbol`, `move_symbol`, `extract_function`, and `inline_function` also preserve meaning: in the copy, every reference resolves to the same symbol identity as before, every expression keeps its type, every function keeps its signature effect set and contracts, and evaluation order is unchanged. A refactoring whose edits cannot satisfy this fails with its own precondition code.
7. **Symbol index.** Applying a plan updates the symbol index of the changed packages in the same command (§12.9.3): entries of symbols whose canonical path changed get the new path and `DefPathHash`, symbols the operation created are allocated by §12.2.3 rule 4, and entries of symbols it deleted are removed. This update requires the durable operation journal of §12.9.3; until that journal and its crash recovery are implemented, index-backed refactorings are not available as complete operations.
8. **Atomicity.** Applying a plan holds the exclusive lock on `.sai/lock`, and writes each file atomically ([13-cli.md](13-cli.md) §13.5.4). If a file cannot be written, the files already written by the plan are restored to their previous content, the symbol index is not updated, and the command reports `OUTPUT_WRITE_FAILED` ([13-cli.md](13-cli.md)). The durable journal of §12.9.3 is required to recover the source/index transaction after process interruption; same-process rollback on a reported write failure is not a substitute. Over the protocol, the edits are sent to the client as one `workspace/applyEdit` (§12.10.6), and the index is updated only after the client reports that it applied them.
9. **Determinism.** The same request on the same snapshot and index gives the same plan, byte for byte.

Each operation below is given by its target kinds, a parameter table (type and default; a parameter without a default is required), its preconditions, and its edits. Preconditions are checked in the order listed, after the common rules.

### 12.6.3 `rename_symbol`

Targets: every symbol kind except `impl`, and except items of trait impls, which are renamed through the trait (precondition 1).

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `name` | `ident`, or `string` for `test` and `property` | | the new name |

Preconditions:

1. The target is not an item of a trait impl. Otherwise `INVALID_OPERATION_TARGET`, with the help ``` rename the trait item `<canonical path>` instead ```.
2. No other entity with the new name is declared in the same namespace and scope as the target: the module's items and `use` bindings for a module-level item; the members of the type, trait, interface, or resource for a member; the type's other variants or fields; the item's other generic parameters; the module's other tests or properties. Otherwise `OPERATION_NAME_CONFLICT`, with the message ``` `<name>` is already declared in `<scope path>` ``` and a label on the existing declaration.
3. At no reference to the target does the new name resolve to something else, and no existing reference to another entity would resolve to the target after the rename (for example, a local binding with the new name in scope at a reference site, or an unqualified reference that the new name would shadow). Otherwise `OPERATION_NAME_CONFLICT`, with a label on the first such site.

Edits: the target's `NAME` token, and every occurrence that resolves to the target (§12.4.2, references), including `use` items and `pub use` re-exports. A `use` item with an alias keeps its alias. Renaming a trait method or associated type also renames the corresponding item of every impl of the trait in the workspace, and every occurrence that resolves to those items. A rename to the current name is valid and has no edits.

Renaming changes the canonical paths of the target, of its members, and of the impls whose header mentions the target. The operation records these changes (§12.6.2 rule 7), so their symbol IDs are kept (§12.2.4 step 2).

### 12.6.4 `move_symbol`

Targets: module-level items with a name: `fn`, `record`, `alias`, `refined`, `enum`, `distinct`, `resource`, `trait`, `const`, `effect`, `interface`, and `world`.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `module` | `module` | | the destination module, in the target's package |

Preconditions:

1. The destination is a module of the target's package, or a module path that the package's module tree does not contain yet, whose segments are valid module names (spec/04 §4.2.3). A module of another package is `INVALID_OPERATION_ARGUMENT`: moving an item between packages changes its package identity and the dependency structure, and is not an operation in v0.1. Moving an item to its own module is `INVALID_OPERATION_ARGUMENT`.
2. The destination declares no item and binds no `use` name equal to the target's name. Otherwise `OPERATION_NAME_CONFLICT`.

Edits:

- The item's text, with its docs, attributes, and leading comments, is removed from its module and appended after the last item of the destination module, separated by one blank line. A destination module that does not exist is created as a new file at the path of spec/04 §4.2.3, containing only the item.
- References in the moved item to items of its old module, which were unqualified, are written by §12.6.2 rule 5 at the new position. So are all references to the moved item in the package: in the old module it now needs an import, in the destination module it no longer does, and elsewhere the path of `use` items and qualified references changes. `use` items that become unused are removed.
- Invariant declarations of a moved type move with it. Inherent impls and trait impls stay where they are. The canonical paths of inherent members and of impls whose header mentions the moved item change (spec/04 §4.9.1); the operation records these changes in the index (§12.6.2 rule 7), so their symbol IDs are kept.

### 12.6.5 `create_function`

Targets: a module (by module key or canonical path), for a module-level function; or a `record`, `enum`, `distinct`, or `resource` symbol, for an inherent associated function.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `name` | `ident` | | the function name |
| `parameters` | `params` | empty | the parameter list, which may start with a `self` parameter for an associated function |
| `returns` | `type` | none | the return type; none means `()` |
| `effects` | `effects` | none | the effects clause; none means inferred (D-EFF-01) |
| `visibility` | `package`, `pub`, or `export` | `package` | the visibility (spec/04 §4.6); `export` only for module-level functions (D-MOD-02) |
| `body` | `block` | `{ panic("not implemented") }` | the body |

Preconditions: the name is free in the scope (as `rename_symbol` precondition 2), and otherwise `OPERATION_NAME_CONFLICT`; an `export` associated function is `INVALID_OPERATION_ARGUMENT`.

Edits: a module-level function is appended after the last item of the module, separated by one blank line. An associated function is appended at the end of the item list of the last inherent impl of the type in the type's module, or, if there is none, in a new `impl <Type> { … }` placed directly after the type's declaration. Rule 6 applies, so a body that does not type-check, or an `export` function whose inferred effects must be declared (C-04), makes the operation fail with the errors in `data`.

### 12.6.6 `delete_function`

Targets: `fn`; `assoc_fn` that is an inherent associated function or a trait method; `extern_fn`; `test`; `property`.

This operation has no parameters.

Precondition: nothing outside the target's own declaration node refers to it, other than `use` items. Otherwise `FUNCTION_STILL_REFERENCED`, with the message ``` `<canonical path>` is still referenced in <n> places ```, a label on each of the first five references, and the `data` object `{ "references": [<location>] }` with all of them. For a trait method, references to the corresponding items of impls count too.

Edits: the declaration, with its docs, attributes, and leading comments, is removed. For a trait method, the corresponding item of every impl of the trait in the workspace is removed too. `use` items that named the target, and `use` items that only the deleted code used, are removed. An inherent impl or an `extern` block left empty is removed.

### 12.6.7 `change_type`

Targets: `field`; `const`; `alias` (the aliased type); `distinct` (the base type); `assoc_type` (a definition in an impl); and `fn` or `assoc_fn` with the `parameter` argument (a parameter type) or without it (the return type).

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `type` | `type` | | the new type |
| `parameter` | `ident` | none | for a function target, the parameter whose type changes; none means the return type |

Preconditions: `parameter` names a parameter of the function, and otherwise `INVALID_OPERATION_ARGUMENT`. For a trait method, the change applies to the trait and to every impl of the trait in the workspace.

Edits: the type node of the declaration is replaced by the new type, written by §12.6.2 rule 5, and `use` items are added as needed. Nothing else is changed. A function's return type `()` written as no `RET_TYPE` gets one; a new type `()` removes it.

`change_type` is not a refactoring. Rule 6 is applied only to the target's declaration node: an error there makes planning fail. Errors elsewhere, at the uses of the changed type, are expected. They are listed in the impact record's `new_errors` (§12.7.3), so that a tool sees what remains to be fixed before it applies the change.

### 12.6.8 `add_parameter`

Targets: `fn`, and `assoc_fn` other than items of trait impls. Adding a parameter to a trait method adds it to the trait and to every impl of the trait in the workspace.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `name` | `ident` | | the parameter name |
| `type` | `type` | | the parameter type |
| `position` | `int` | at the end | the index of the new parameter among the parameters other than `self`; 0 is first |
| `argument` | `expr` | none | the argument inserted at every call site |

Preconditions:

1. `position` is at most the number of parameters other than `self`. Otherwise `INVALID_OPERATION_ARGUMENT`.
2. If the target has call sites, `argument` is present. Otherwise `INVALID_OPERATION_ARGUMENT`, with the message ``` `argument` is required: `<canonical path>` has <n> call sites ```.
3. No parameter of the target has the name, and the body of the target (and of the impls, for a trait method) uses no binding of an enclosing scope with the name, which the new parameter would shadow. Otherwise `OPERATION_NAME_CONFLICT`.

Edits: the parameter `<name>: <type>` is inserted in the parameter list at `position`, and `argument` is inserted at the same position in the argument list of every call site in the workspace, in the form that the call uses for its other arguments. The argument is resolved and type-checked at each call site, in the scope of that site, so it may use the caller's local bindings. Its package-qualified paths (rule 1) are written by rule 5 at each site. An argument that does not resolve or type-check at some site makes planning fail by rule 6, with that site in `data`. Uses of the function as a value change type, and rule 6 reports them.

### 12.6.9 `remove_parameter`

Targets: as `add_parameter`.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `name` | `ident` | | the parameter to remove; `self` cannot be removed |

Preconditions:

1. The target has a parameter with that name. Otherwise `INVALID_OPERATION_ARGUMENT`.
2. The body of the target (and of every impl, for a trait method) does not use the parameter. Otherwise `PARAMETER_STILL_USED`, with a label on the first use.
3. At every call site, the argument for the parameter is a **removable** expression: a literal, a path to a local binding, constant, or function, an implicit member without arguments, `()`, a field access or tuple index on a removable expression, or `borrow` or `borrow mut` of one. Removing any other argument would remove its evaluation, which may have effects or panic. Otherwise `ARGUMENT_NOT_REMOVABLE`, with a label on the first such argument and the `data` object `{ "sites": [<location>] }`.

Edits: the parameter is removed from the declaration, and its argument from every call site. Rule 6 applies; uses of the function as a value change type, and rule 6 reports them.

### 12.6.10 `extract_function`

Targets: symbols with bodies: `fn`, `assoc_fn`, `test`, and `property`. The extracted code is part of the target's body.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `name` | `ident` | | the name of the new function |
| `from` | `node` | | an expression node, or the first statement to extract |
| `to` | `node` | `from` | the last statement to extract, in the same block as `from`, not before it |

The **region** is the expression `from` if `from` is an expression node and `to` is absent, and otherwise the statements from `from` to `to` of one block.

Preconditions (each failure is `EXTRACT_NOT_POSSIBLE`, with the reason in the message and in `data.reason`):

1. `not_a_region`: `from` and `to` are not statements of the same block in this order, or `from` alone is neither an expression nor a statement.
2. `control_flow`: the region contains `return`, `?`, or a `break` or `continue` whose loop is outside the region.
3. `generic`: the region uses a generic parameter of the target, or of its impl.
4. `unsafe`: the region is inside an `unsafe` block and is not the whole block.
5. `returns_borrow`: a value that the region produces, and that is used after it, has a borrow type.
6. The new name is free among the module's items (or the type's members, see below). Otherwise `OPERATION_NAME_CONFLICT`.

Edits:

- **Parameters.** The local bindings declared outside the region and used inside it, in the order of their first use, become parameters with their types. A binding that the region moves, or whose type is `Copy`, is passed by value; one that the region assigns or borrows mutably is passed as `borrow mut`; any other is passed as `borrow`. If the target is an associated function and the region uses `self`, the new function is an associated function of the same impl, with a `self` parameter of the same kind as the target's; otherwise it is a module-level function.
- **Result.** For an expression region, the result is the expression's value. For a statement region, the result is the bindings declared in the region and used after it: none gives `()`, one gives its value, several give a tuple in declaration order, which the call site destructures with `let`.
- **Declaration.** The new function is package-private, has no effects clause (its effects are inferred, D-EFF-01), is `async` if the region contains `.await`, and is placed directly after the module-level item that contains the target (a module-level function), or directly after the target in its impl (an associated function).
- **Call site.** The region is replaced by a call of the new function, with `.await` if it is `async`, with `borrow` and `borrow mut` written explicitly for borrowed arguments (D-OWN-02).

### 12.6.11 `inline_function`

Targets: `fn`, and `assoc_fn` that is an inherent associated function.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `call` | `node` | none | one call site to inline; none means every call site |
| `keep` | `bool` | `false` | when every call site is inlined, keep the function instead of deleting it |

Preconditions (each failure is `INLINE_NOT_POSSIBLE`, with `data.reason`):

1. `not_a_call`: `call` is not a call of the target. For this parameter, the node may be in any file of the workspace, and rule 2 does not apply.
2. `recursive`: the target calls itself, directly or through other functions.
3. `control_flow`: the body contains `return` other than as the last statement, or `?`.
4. `contracts`: the target has `requires` or `ensures` clauses. Inlining would remove the checks that they perform at run time (D-CONTRACT-01).
5. `visibility`: the body refers to an item that is not visible at a call site, such as a package-private item of another package.
6. `used_as_value`: every call site is inlined, `keep` is `false`, and the function is also used as a value.

Edits: each call is replaced by a block expression that binds the arguments to the parameters with `let`, in parameter order, and then contains the body's statements and tail. An argument that is a local binding or a literal, for a parameter that the body neither assigns nor borrows mutably, is substituted directly instead of bound. Local bindings of the body whose names are used in the enclosing scope of the call site are renamed by appending `_1`, `_2`, …, taking the smallest free suffix. References in the body are rewritten by §12.6.2 rule 5 for the call site's module. Type parameters are replaced by the type arguments of the call, written as types. A block that consists of a tail expression only is written without braces, in parentheses if needed. Unless `call` is given or `keep` is `true`, the function is then deleted as by `delete_function`.

### 12.6.12 `add_test`

Targets: function symbols of workspace packages, and modules of workspace packages.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `name` | `string` | | the test name ([spec/14-testing.md](../spec/14-testing.md) §14.2.3) |
| `body` | `block` | `{ panic("not implemented") }` | the test body |

Precondition: no test or property of the module has the name. Otherwise `OPERATION_NAME_CONFLICT`.

Edits: `test "<name>" <body>` is inserted in the module of the target, after the module-level item that contains the target and after the tests that directly follow that item, so that it is next to the code it tests (spec/14 §14.1.1); for a module target, after the module's last item. The default body fails, so that a test added without a body cannot pass by accident.

### 12.6.13 `implement_trait`

Targets: `record`, `enum`, `distinct`, and `resource` symbols of workspace packages.

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `trait` | `type` | | the trait, with its generic arguments, as a path in canonical form (spec/04 §4.9.1), such as `sai:core/fmt.Format` |
| `types` | `string` | none | definitions of the trait's associated types without a default, as `Name = Type` separated by `, ` |
| `derive` | `bool` | `false` | add the trait to the type's `@derive` attribute instead of writing an impl |

Preconditions:

1. `trait` names a trait. An interface or another item is `INVALID_OPERATION_ARGUMENT`.
2. The type has no impl of the trait with the same arguments, written or derived. Otherwise `TRAIT_ALREADY_IMPLEMENTED`, with a label on the existing impl.
3. With `derive`, the trait is derivable (D-TYPE-08). Otherwise `INVALID_OPERATION_ARGUMENT`.
4. Every associated type of the trait without a default has a definition in `types`, and `types` defines nothing else. Otherwise `INVALID_OPERATION_ARGUMENT`, naming the missing or extra names.

Edits: with `derive`, the trait is added to the type's `@derive` arguments, which are kept in their order with the new trait last, or a new `@derive(<Trait>)` attribute is added. Otherwise `impl <Trait> for <Type> { … }` is placed in the type's module, after the type's declaration and its existing impls. For a generic type, the impl has the type's generic parameters, without bounds. It contains the associated type definitions of `types`, and, for every method of the trait without a default body, a method with the trait's signature, with `Self` and the trait's parameters replaced, and the body `{ panic("not implemented") }`. Items are in the trait's declaration order. Rule 6 applies: a missing supertrait impl or an unsatisfied bound makes planning fail.

### 12.6.14 Example

The package `acme:user` has these two files:

```sai
use acme:user/model.{User}

pub fn create_user(name: Str) -> User {
    User.new(name)
}
```

```sai
use acme:user/model.{User}
use acme:user/service

pub fn handle_signup(name: Str) -> User {
    service.create_user(name)
}
```

The request

```json
{
  "operation": "add_parameter",
  "target": "acme:user/service.create_user",
  "arguments": { "name": "email", "type": "acme:user/model.Email", "argument": "acme:user/model.Email.empty()" }
}
```

is planned as follows. The target resolves to the function `create_user`. The arguments parse: `type` and `argument` use package-qualified paths (§12.6.2 rule 1). `position` defaults to the end. There is one call site, so `argument` is required and present. No binding named `email` is shadowed. The edits change `src/service.sai` to

```sai
use acme:user/model.{Email, User}

pub fn create_user(name: Str, email: Email) -> User {
    User.new(name)
}
```

and `src/api.sai` to

```sai
use acme:user/model.{Email, User}
use acme:user/service

pub fn handle_signup(name: Str) -> User {
    service.create_user(name, Email.empty())
}
```

In both files `Email` is added to the existing `use` of its module (§12.6.2 rule 5), and the `UseItems` stay sorted ([14-formatter.md](14-formatter.md) §14.7.2). Checking the copy (rule 6) finds no new error. It finds the new warning `UNUSED_VARIABLE` for `email`, which does not prevent the operation. Because `create_user` is `pub`, adding a parameter is a breaking change of the public API (§12.8.3), so `sai edit` without `--allow-breaking` stops with `OPERATION_BREAKS_PUBLIC_API` ([13-cli.md](13-cli.md) §13.6.18).

---

## 12.7 Impact analysis

### 12.7.1 Overview

The **impact** of a plan (§12.6.1) describes what the plan changes beyond the edited text, before anything is written (draft §66). It is computed for every plan: by `sai edit`, with or without `--dry-run` ([13-cli.md](13-cli.md) §13.6.18), and by `sai/planOperation`, `sai/impact`, and `sai/applyOperation` (§12.5.3).

The impact compares two versions of the workspace: the **before** version, which is the workspace snapshot that was planned on, and the **after** version, which is the in-memory copy of §12.6.2 rule 6 with the plan's edits applied. Both are analyzed for the same target and profile. The comparison is the semantic diff of §12.8, applied to these two versions with `public` false. Impact analysis reads both versions and writes nothing.

### 12.7.2 What is counted

A function symbol is **touched** by the plan if the diff of the two versions reports it as `Removed`, `Renamed`, `Moved`, or `FunctionChanged` (§12.8.2), or if one of the plan's edits lies inside its declaration node in the before version. The counts are computed on the graphs of §12.4.2, from the before version, except where stated (N-H-75).

1. **Call sites.** The sites of the `calls` edges whose target is a function symbol that the diff reports as `Removed`, `Renamed`, `Moved`, or `FunctionChanged`, together with the call sites that the plan replaces (`inline_function`, §12.6.11). Only sites in source files of workspace packages are counted. A site is counted once, even if it is the site of several edges, for example a call of a trait method whose impl item also changed.
2. **Tests.** The `test` and `property` symbols that have a `tests` edge to a touched function, the tests and properties whose own declaration node contains an edit, and the tests and properties that the plan adds (counted in the after version).
3. **Exported interfaces.** The interfaces of workspace packages that a world of the same package exports ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md)) and that are affected: the generated WIT of the interface differs between the two versions, or the reachable set `Reach` of [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.3, computed with the members of the interface's impls as the only roots (`reach_from` of [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.7.1 with one `Fn` root per member), contains a touched function.
4. **Component worlds.** The worlds of workspace packages whose generated WIT differs between the two versions, or that export an affected interface (rule 3).
5. **Size.** Only if the size is requested (`--size` of [13-cli.md](13-cli.md) §13.6.18, or `size: true` in the protocol request): the artifacts of the root package are built from both versions, with the selected target and profile, and the size change is the total size of the artifacts after, minus the total size before, in bytes, as measured by [09-optimization-and-size.md](09-optimization-and-size.md). Code is generated only for this purpose and is not written to `out/`. Without a size request, nothing is built and the size is `null`.
6. **Breaking.** The plan is breaking if the diff of the two versions reports a change whose `public_api` is `breaking` (§12.8.3).
7. **New errors.** The errors of the after version that the before version does not have (§12.6.2 rule 6). They are non-empty only for `change_type` (§12.6.7), because every other operation fails when it has any.

The counts are counts of distinct objects. A call site that lies in a test is counted as a call site, and the test is counted as a test. Interfaces, worlds, and tests of dependencies are never counted, because no edit of the workspace can change what a dependency's own tests or exports do.

If the before version has errors other than syntax errors (§12.6.2 rule 4), the counts use the facts that are known, and the impact is marked incomplete.

### 12.7.3 Impact record

The **impact record** is the JSON form of the impact. It is the `impact` member of the `edit` record of [13-cli.md](13-cli.md) §13.6.18 and the result of `sai/impact`. Its schema is `impact` in §12.11.

```json
{
  "operation": "add_parameter",
  "target": "symbol://acme%3Auser/service/create_user#5B0C3E",
  "path": "acme:user/service.create_user",
  "call_sites": [
    { "file": { "path": "src/api.sai", "package": "acme:user@0.4.0", "snapshot": null },
      "start": { "byte": 96, "line": 5, "col": 5, "utf16_col": 5 },
      "end": { "byte": 115, "line": 5, "col": 24, "utf16_col": 24 } }
  ],
  "tests": [
    { "symbol": "symbol://acme%3Auser/api/test%5B%22signup%20creates%20a%20user%22%5D#77B210",
      "path": "acme:user/api.test[\"signup creates a user\"]" }
  ],
  "exported_interfaces": [],
  "component_worlds": [],
  "breaking": true,
  "size_delta": null,
  "complete": true,
  "new_errors": [],
  "changes": [
    { "kind": "FunctionChanged", "path": "acme:user/service.create_user",
      "symbol": "symbol://acme%3Auser/service/create_user#5B0C3E", "before_path": null,
      "aspects": [
        { "aspect": "parameters", "added": ["email: Email"], "removed": [], "before": null, "after": null }
      ],
      "public_api": "breaking", "size_delta": null }
  ]
}
```

| Member | Content |
|---|---|
| `operation` | the operation name |
| `target` | the symbol ID of the target, or the module key for a module target |
| `path` | the canonical path of the target, in the before version |
| `call_sites` | the call sites of §12.7.2 rule 1, as locations of [11-diagnostics.md](11-diagnostics.md) §11.9.5 in the before version, sorted by position (11 §11.11.1) |
| `tests` | the tests and properties of rule 2, as `{ "symbol", "path" }` objects, sorted by `path` in byte order. The symbol ID of a test that the plan adds is the one it gets when the plan is applied (§12.6.2 rule 7). |
| `exported_interfaces`, `component_worlds` | the interfaces of rule 3 and the worlds of rule 4, as `{ "symbol", "path" }` objects, sorted by `path` |
| `breaking` | rule 6 |
| `size_delta` | rule 5: an integer number of bytes, negative for a decrease, or `null` |
| `complete` | `false` if the before version has errors (§12.7.2) |
| `new_errors` | rule 7: diagnostic records of [11-diagnostics.md](11-diagnostics.md) §11.9.5, located in the after version, with `snapshot` `null`, in the order of 11 §11.11.1 |
| `changes` | the `changes` of the diff document of §12.8.5 between the two versions |

### 12.7.4 Text rendering

The human form of an impact, printed by `sai edit` ([13-cli.md](13-cli.md) §13.6.18), is:

```text
<operation> <path>

affected
  <n> call site|call sites
  <n> test|tests
  <n> exported interface|exported interfaces
  <n> component world|component worlds

breaking public API: yes|no
estimated Wasm size: <size>
```

1. `<path>` is the canonical path of the target. The four counts are always printed, in this order, each indented by two spaces. The singular form is used for the count 1, and the plural form for every other count, including 0.
2. `breaking public API:` is followed by `yes` if `breaking` is true, and `no` otherwise.
3. The line `estimated Wasm size:` is printed only if the size was requested. `<size>` is the size change in the format of [13-cli.md](13-cli.md) §13.6.14 with an explicit sign: a change `d` with `|d| < 1024` is `+<d> bytes` or `-<|d|> bytes` (`+0 bytes` for no change), and a larger one is `+<x.y> KiB` or `-<x.y> KiB`, where `x.y` is `|d|` in KiB with one decimal, computed as in 13 §13.6.14.
4. If the impact is incomplete, the line `incomplete: the workspace has errors, so some effects of the change may be missing` follows, after an empty line.
5. If `new_errors` is not empty, an empty line, the line `new errors`, and one line per error follow: two spaces, the path, `:`, the line, `:`, the column of the primary location (as in [11-diagnostics.md](11-diagnostics.md) §11.10.3), two spaces, the code, `: `, and the main message. An error without a primary location is printed without the location and the two spaces after it.

The rendering of the plan in [13-cli.md](13-cli.md) §13.6.18 is:

```text
add_parameter acme:user/service.create_user

affected
  14 call sites
  3 tests
  2 exported interfaces
  1 component world

breaking public API: yes
estimated Wasm size: +420 bytes
```

---

## 12.8 Semantic diff

The **semantic diff** of two versions of a workspace lists the changes of its entities, their public API classification, and optionally their size changes (draft §67). It is printed by `sai diff` ([13-cli.md](13-cli.md) §13.6.17), returned by `sai/diff` (§12.5.3), and used by impact analysis (§12.7). It complements a text diff: it reports what changed for the users of the code, not how the text changed.

### 12.8.1 Versions, entities, and matching

1. **Versions.** A diff compares a `from` version and a `to` version. Each is a workspace snapshot, analyzed for the selected target and profile as [13-cli.md](13-cli.md) §13.6.17 rule 3 describes, or an in-memory copy (§12.7.1). The **diffed packages** are the workspace packages of either version, paired by package identity. A package present in one version only is reported as one `Added` or `Removed` change of its root module, with no changes for its contents. Dependencies are not compared item by item, except their unsafe inventories (§12.8.7); their effect on the workspace appears as `DependencyChanged`, `FootprintChanged`, and the changes of the workspace's own entities.
2. **Entities.** The entities of a diffed package are its modules and its symbols (§12.2.1), except fields, enum variants, and generic parameters, which are compared as aspects of the entity that contains them (§12.8.2). Function bodies, comments, formatting, and the order of declarations are not compared (N-H-73), except the `# Safety` section of an unsafe function or trait, which is the `safety` aspect (§12.8.2). A constant's initializer is compared as its `value` aspect (§12.8.2). A change of a body is visible only through the properties that depend on it, such as the inferred effect set or the size.
3. **Matching.** The entities of the two versions of a package are matched one to one, in this order (N-H-74):
   1. entities with the same canonical path and the same kind;
   2. among the unmatched, entities with the same kind and the same hexadecimal part of their symbol IDs. The IDs of the working tree come from the symbol index (§12.2.4), and those of a Git revision or an in-memory copy from fresh allocation (§12.2.3) or, for a copy, from the index updates that the plan would make (§12.6.2 rule 7). An operation that renamed a symbol after `from` was committed therefore lets the diff recognize the rename even if the body changed too;
   3. among the unmatched, entities with the same kind and the same fingerprint (§12.2.4), by the rule of §12.2.4 step 4.
   After each match by step 2 or 3 whose canonical paths differ, the members of the matched entities are matched by step 1 with the old path prefix replaced by the new one, as in §12.2.4 step 3.
4. Unmatched entities of `from` are `Removed`, unmatched entities of `to` are `Added`, and matched entities are compared by §12.8.2.

### 12.8.2 Change kinds and aspects

A matched pair of entities whose canonical paths differ is `Renamed` if only the last component of the path differs, and `Moved` otherwise. A path change that only follows the change of a parent's path is not reported for the member: renaming `User` to `Account` reports `Renamed` for the type, and `acme:user/model.Account.new` has a change only if one of its other aspects changed. A matched pair with the same path, or a member whose path change is not reported, has a change if one of its aspects differs. The change kind is then `FunctionChanged` for function symbols, `TypeChanged` for `record`, `alias`, `refined`, `enum`, `distinct`, and `resource` symbols, and `ItemChanged` for every other entity. Every entity has at most one change, which lists all of its changed aspects.

An **aspect** is a named property of an entity. A **value aspect** has a `before` and an `after` text. A **list aspect** has the lists of `added` and `removed` elements. Every text is the canonical one-line text of [14-formatter.md](14-formatter.md) §14.13 of the construct, with the names resolved in the version it comes from and printed by §12.6.2 rule 5 for the entity's module. The aspects are, in their output order:

| Aspect | Form | Entities | Compared value |
|---|---|---|---|
| `path` | value | `Renamed`, `Moved` | the canonical path |
| `visibility` | value | entities with a visibility (spec/04 §4.6.2) | `package`, `pub`, or `export` |
| `generics` | list | entities with generic parameters | each generic parameter with its bounds, and each `where` predicate |
| `parameters` | list | function symbols | each parameter as written in the declaration, such as `email: Email` or `borrow self` |
| `return` | value | function symbols | the return type, `()` if there is none |
| `effects` | value | function symbols | the signature effect set ([08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.2.6), declared or inferred, read from the `set` of the function's entry in `effect_interface(P)` (08 §8.9.3), printed in the canonical order of spec/08 §8.2.3 |
| `modifiers` | list | function symbols | the words `async` and `unsafe` that apply to the function |
| `requires`, `ensures` | list | function symbols | each contract clause, as its predicate |
| `fields` | list | `record`, `resource`, `enum` | each field as `<name>: <type>`, with the prefix `pub ` if it has one; for a record variant, as `<Variant>.<name>: <type>` |
| `variants` | list | `enum` | each variant, as its declaration text without its fields' `pub` markers |
| `type` | value | `alias`, `refined`, `distinct`, `const`, `assoc_type` | the aliased type; the base type and predicate; the base type, with `pub ` for `= pub B`; the constant's type; the associated type's definition or bounds |
| `invariants` | list | types with invariants | each invariant predicate |
| `value` | value | `const` | the initializer |
| `supertraits` | list | `trait` | each supertrait bound |
| `covers` | list | `effect` | each element of the `covers` clause |
| `imports`, `exports` | list | `world` | each world item, including `include` |
| `reexports` | list | modules | each `pub use` binding, as `<name> = <canonical path>` |
| `safety` | value | `unsafe fn` and `unsafe trait` symbols | the `# Safety` section of the doc comment as [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.10.4 item 5 extracts it, or `none` if the section is absent ([spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.11.2) |
| `deprecated` | value | every symbol | the text of its `@deprecated` attribute, or `none` |

The elements of a list aspect are compared as sequences. `removed` lists the elements of the `from` sequence that are not in its longest common subsequence with the `to` sequence, and `added` lists the elements of the `to` sequence that are not in it, both in sequence order. The longest common subsequence is the one found by the standard dynamic program over suffixes, which, when skipping either element gives the same length, skips the element of the `from` sequence. A changed element thus appears once in `removed` and once in `added`. For `fields` and `variants` of an enum, the sequence is the declaration order of the variants with their fields.

The **package-level changes** have the package's root module path `<ns>:<pkg>` as their path and no symbol:

| Kind | Aspect | Compared value |
|---|---|---|
| `WitChanged` | `wit` (list) | each top-level definition of each interface and world of the WIT generated for the package ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.11), as `<interface or world>.<definition text>`, with every whitespace run replaced by one space |
| `FootprintChanged` | `capabilities` (list) | the capability footprint of the package for the selected target ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.8): the normalized list `footprint` of its `EffectInterface` ([08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.7.10, §8.9.3), in byte order. A version whose analysis has an error has no footprint, and no `FootprintChanged` change is reported |
| `DependencyChanged` | `dependencies` (list) | each direct dependency in the resolved graph, as `<ns>:<pkg>@<selected version>`, in byte order |
| `PolicyChanged` | `allow`, `deny` (lists) | the capability entries of the package's effective policy (spec/08 §8.12.5), in byte order |
| `UnsafeChanged` | `unsafe` (list) | the entries of the unsafe inventory ([spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.11.1), as `<entry kind> <item>` with the `item` of [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.10.4 item 2 (the syntactic path of item 7 for an entry inside an item that `@target` removes, N-D-94), in byte order, with `×<n>` appended when `n > 1` entries have the same text |

Each package-level change is reported once per diffed package if its aspect differs. Its list aspects are compared as sets: `removed` is the elements only in `from`, `added` those only in `to`, both in byte order.

### 12.8.3 Public API classification

An entity is **public** in a version if code of another package can name it there (spec/04 §4.6.2): a module-level item that is `pub` or `export`; an inherent impl item that is `pub`, of a public type; a trait item of a public trait; an interface member of a public interface; a trait impl whose trait and self type can both be named from another package, and its items; and a module that contains a public entity or a `pub use` binding. Extern functions, tests, and properties are never public. A field is public if it is `pub` and its type is public. A variant is public if its enum is. `@doc(hidden)` does not change whether an entity is public ([spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.13 rule 4).

Every change has a `public_api` value (N-H-72):

- `none` if the entity is public in neither version. Package-level changes of kind `DependencyChanged`, `PolicyChanged`, and `UnsafeChanged` are always `none`: they describe how the package is built, not what other packages can use.
- `breaking` if the entity is public in `from` and the change matches a rule below, or if it is a `WitChanged` or `FootprintChanged` change that matches one.
- `compatible` otherwise.

A change is **breaking** if one of the following holds:

1. **Existence.** The entity is `Removed`, `Renamed`, or `Moved`, or it is no longer public in `to`. A change of visibility from `export` to `pub` is breaking, because it removes a core Wasm export of the root package (spec/04 §4.6.6).
2. **Added trait items.** An `Added` method or associated type of a public trait has no default: every impl of the trait in other packages lacks it.
3. **Generics.** The `generics` aspect changed, except when only bounds or `where` predicates were removed from a function symbol that is not a trait item, or from a type.
4. **Signature.** The `parameters` or `return` aspect changed, or `async` was added to or removed from `modifiers`, or `unsafe` was added.
5. **Effects.** An atom was added to the signature effect set. For a trait method, whose declared set bounds the effects of every impl (spec/08), any change of the set is breaking.
6. **Contracts.** A `requires` clause was added or changed, or an `ensures` clause was removed or changed. Removing a precondition and adding a postcondition are compatible.
7. **Fields and variants.** A public field was removed or changed; a field was added to a record or resource whose fields are all `pub` in `from`, so that other packages can construct it, or to a record variant; or a variant was added to, removed from, or changed in a public enum, which other packages may match exhaustively.
8. **Types.** The `type` aspect changed, except for the base type of a `distinct` type without `= pub B`, which other packages cannot see (spec/04 §4.6.4).
9. **Invariants and supertraits.** An invariant was added or changed, or the `supertraits` aspect changed.
10. **Worlds and re-exports.** A world export was removed or changed, a world import was added or changed, or a `pub use` binding was removed or changed.
11. **WIT.** A `WitChanged` change has a non-empty `removed` list: a definition of the generated WIT was removed or changed (spec/13).
12. **Footprint.** A `FootprintChanged` change has a non-empty `added` list. A package whose footprint grows can be rejected by the `allow` and `grant` policies of the packages that depend on it ([package/01-package-contract.md](../package/01-package-contract.md) §1.10.8, §1.11; spec/08 §8.12.6).

Changes of `value`, `covers`, `safety`, and `deprecated`, additions of public entities other than those of rule 2, and the removal of an `effects` atom (outside trait methods) are compatible. Rule 1 also covers a `Removed` trait impl.

With `public` (`--public` of [13-cli.md](13-cli.md) §13.6.17), the diff keeps only the changes whose `public_api` is not `none`.

### 12.8.4 Size changes

Sizes are compared only if requested (`--size` of [13-cli.md](13-cli.md) §13.6.17, or `size: true` in `sai/diff`). The artifacts of the root package are then built from both versions, with the selected target and profile, and their size attribution tables ([06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) §6.11.5, [09-optimization-and-size.md](09-optimization-and-size.md)) are read.

1. The **attributed size** of a symbol in a version is the total length of the attribution entries whose owner is the symbol or one of its instances: `Function`, `DataItem`, and `Global` of the symbol, and `Inlined` entries whose inlined symbol is the symbol. A symbol that has no code or data in the artifact has the attributed size 0.
2. Each change of a function symbol or a constant has a `size_delta`: its attributed size in `to` minus its attributed size in `from`, in bytes. Every other change has `size_delta` `null`. Without a size request, every `size_delta` is `null`.
3. The diff document has a `size` summary: the total size of the artifacts in each version, and their difference. A size change without any other change, such as the growth of a function whose body changed, is visible only in this summary and in `sai size` ([13-cli.md](13-cli.md) §13.6.14), because bodies are not compared (§12.8.1 rule 2).

Size changes never affect `public_api`, because a size is not part of the API.

### 12.8.5 Diff document

The **diff document** is the JSON form of a diff. It is the `data` of the `diff` record of [13-cli.md](13-cli.md) §13.6.17, which extends it by `from` and `to`, and the result of `sai/diff`. Its schema is `diff` in §12.11, and it is open: consumers ignore members they do not know (D-TOOL-01).

```json
{
  "changes": [
    { "kind": "FunctionChanged", "path": "acme:user/service.create_user",
      "symbol": "symbol://acme%3Auser/service/create_user#5B0C3E", "before_path": null,
      "aspects": [
        { "aspect": "parameters", "added": ["email: Email"], "removed": [], "before": null, "after": null },
        { "aspect": "effects", "added": [], "removed": [], "before": "{ db.write }", "after": "{ net.http, db.write }" }
      ],
      "public_api": "breaking", "size_delta": 1126 }
  ],
  "dependency_unsafe": [],
  "summary": { "changes": 1, "breaking": 1, "size": { "before": 48211, "after": 49337, "delta": 1126 } }
}
```

1. `changes` has one entry per change, sorted by `path` in byte order, then by `kind`. A package-level change sorts before the changes of the entities of the package, because its path is a prefix of theirs.
2. `kind` is a change kind of §12.8.2. `symbol` is the symbol ID in `to`, or in `from` for a `Removed` symbol, and `null` for modules and package-level changes. `path` is the canonical path in `to`, or in `from` for `Removed`. `before_path` is the canonical path in `from` for `Renamed` and `Moved`, and `null` otherwise.
3. `aspects` lists the changed aspects in the order of §12.8.2. A value aspect has `before` and `after` strings and empty `added` and `removed` lists; a list aspect has `before` and `after` `null`. `Added` and `Removed` changes have no aspects.
4. `public_api` is `breaking`, `compatible`, or `none` (§12.8.3). `size_delta` is §12.8.4 rule 2.
5. `summary.changes` is the number of entries, and `summary.breaking` the number with `public_api` `breaking`. `summary.size` is `null` without a size request, and otherwise `{ "before", "after", "delta" }` in bytes.
6. `dependency_unsafe` lists the unsafe changes of dependencies (§12.8.7).

### 12.8.6 Text rendering

The human form of a diff document, printed by `sai diff` ([13-cli.md](13-cli.md) §13.6.17), is one block per change, in the order of `changes`, separated by one empty line, followed by the blocks of §12.8.7, one empty line and the summary line. A diff without changes and without records of §12.8.7 is the single line `no semantic changes`. Each block is:

```text
<kind> <path>
  <aspect>
    - <removed element>
    + <added element>
  <aspect>
    before: <text>
    after:  <text>
  public API: breaking|compatible|none
  size: <size>
```

1. The first line is the change kind, one space, and `path`.
2. Each aspect of `aspects` follows, in order: its name indented by two spaces, then its lines indented by four spaces. A list aspect prints `- ` and each element of `removed`, then `+ ` and each element of `added`. A value aspect prints `before: ` and the `before` text, then `after:  ` (with two spaces, so that the texts line up) and the `after` text.
3. `public API: ` and the `public_api` value follows, indented by two spaces.
4. The line `size: <size>` follows only if `size_delta` is not `null`. `<size>` uses the signed format of §12.7.4 rule 3.
5. The summary line is `<n> change, <m> breaking` for `n = 1`, and `<n> changes, <m> breaking` otherwise, with the suffix of §12.8.7 when dependencies have unsafe changes.

Texts are the canonical one-line texts of §12.8.2, so effect sets are printed in the canonical order of spec/08 §8.2.3, with the empty set printed `{}`. A one-line text never contains a line break: string literals with line breaks are printed with escapes, and a block with more than one element is printed `{ … }` ([14-formatter.md](14-formatter.md) §14.13 rules 3 and 4).

The example of [13-cli.md](13-cli.md) §13.6.17, after a body change of `create_user` that added a call of a function with the effect `net.http`, is:

```text
FunctionChanged acme:user/service.create_user
  parameters
    + email: Email
  effects
    before: { db.write }
    after:  { net.http, db.write }
  public API: breaking
  size: +1.1 KiB

1 change, 1 breaking
```

A diff between a tag and the working tree after `create_user` was renamed from `make_user` by `rename_symbol`, a `pub` field was added to a record whose fields are all `pub`, and the package started to use `wasi:http`:

```text
$ sai diff v0.3.0
FootprintChanged acme:user
  capabilities
    + wasi:http
  public API: breaking

TypeChanged acme:user/model.User
  fields
    + pub email: Email
  public API: breaking

Renamed acme:user/service.create_user
  path
    before: acme:user/service.make_user
    after:  acme:user/service.create_user
  effects
    before: { db.write }
    after:  { net.http, db.write }
  public API: breaking

3 changes, 3 breaking
```

The rename is recognized by step 2 of §12.8.1 rule 3 if the index still holds the ID that `make_user` had in `v0.3.0`, and otherwise by step 3 only if the declaration's tokens are unchanged apart from the name. Here the body changed, so without the index the diff would report `Removed acme:user/service.make_user` and `Added acme:user/service.create_user`.

### 12.8.7 Unsafe code of dependencies

The diff compares the unsafe inventories of the dependencies whose lock entries changed, entry by entry ([spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.11.2, N-H-87). This is the only item-by-item comparison of dependencies (§12.8.1 rule 1). It compares what `sai inspect unsafe` ([13-cli.md](13-cli.md) §13.6.15) prints for the two versions.

1. **Inputs.** For each version, the **dependency nodes** are the package nodes ([package/02-resolver.md](../package/02-resolver.md) §2.1) of the active graph of the selected target ([08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.9.2 item 1), other than the workspace packages. `sai:core` and `sai:alloc` are not nodes. For each node `Q`, the version provides its lock entry ([package/03-lock-file.md](../package/03-lock-file.md) §3.3.3) and `unsafe_inventory(Q, t)` (08 §8.9.2) for the selected target `t`. The diff reads nothing else of a dependency.
2. **Pairs.** The dependency nodes of `from` and `to` are paired in two steps:
   1. nodes with the same package identity and compatibility line;
   2. among the unpaired nodes, a package identity with exactly one unpaired node in each version pairs these two nodes. A major upgrade, which changes the line, is thus one pair.

   An unpaired node of `to` is **new**. An unpaired node of `from` is **dropped** and is not compared: its code no longer runs, and `DependencyChanged` reports its removal.
3. **Compared pairs.** A pair is compared unless its two lock entries have the same selected version, the same source and the same `checksum`. Equal checksums mean byte-identical package directories ([package/03-lock-file.md](../package/03-lock-file.md) §3.4), so the inventories are equal, and they are not computed. A path entry has no checksum, so a pair of path entries is always compared. A new node is compared with an empty `from` inventory.
4. **Artifacts.** If the node is used in artifact form in either version, its inventory there has no entry (08 §8.9.2 item 4). The pair is then not compared entry by entry. It gets a record with `artifact` true and no entry changes, because the missing entries are unknown, not removed.
5. **Keys.** The key of an entry is `(kind, item, ordinal)`. `kind` and `item` are the fields of 08 §8.10.4. `ordinal` is the rank of the entry, counted from 1, among the entries of its inventory with the same `kind` and `item`, in the inventory order of 08 §8.10.4 item 9. Two entries of a compared pair match if their keys are equal. Symbol IDs, node IDs and locations are not part of the key:
   - a dependency's symbol IDs are allocated fresh from canonical paths (§12.2.3 rule 3), so they identify nothing that `item` does not, except after a seed collision, where an ID depends on other symbols;
   - modules and removed items have no symbol ID;
   - node IDs and locations change with every edit before the construct in its file.
6. **Changes.** Each compared pair produces these entry changes:
   - `added`: an entry of `to` without a match;
   - `removed`: an entry of `from` without a match;
   - `safety_changed`: a matched pair of `unsafe_fn` or `unsafe_trait` entries whose **safety texts** differ.

   The safety text is the `safety` value of 08 §8.10.4 item 5 with the trailing spaces and tabs of each line removed, or `null` if the section is absent. `null` differs from every string, including the empty string. Operations and `target` are not compared, because they follow the bodies and the predicates of the items. `sai inspect unsafe` of `to` prints them ([spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.11.1 item 2).
7. **Several entries of one item.** Entries with the same `kind` and `item` are matched by rank. When an item gains a second `unsafe` block, the change therefore names the last block of the item, which need not be the new one. The count per item is exact.

**Records.** The diff document has the member `dependency_unsafe`: one record per compared pair that has at least one entry change or `artifact` true.

```json
{
  "dependency_unsafe": [
    { "package": "acme:simd", "before": "acme:simd@1.0.1", "after": "acme:simd@1.1.0", "artifact": false,
      "entries": [
        { "change": "added", "kind": "unsafe_fn", "item": "acme:simd.load_aligned", "ordinal": 1,
          "symbol": "symbol://acme%3Asimd/load_aligned#4C1D09",
          "location": { "file": { "path": "src/lib.sai", "package": "acme:simd@1.1.0", "snapshot": null },
                        "start": { "byte": 610, "line": 24, "col": 1, "utf16_col": 1 }, "end": { "byte": 616, "line": 24, "col": 7, "utf16_col": 7 } },
          "safety_before": null, "safety_after": "`p` is readable for 16 bytes and aligned to 16." },
        { "change": "safety_changed", "kind": "unsafe_fn", "item": "acme:simd.load_unaligned", "ordinal": 1,
          "symbol": "symbol://acme%3Asimd/load_unaligned#E07A13",
          "location": { "file": { "path": "src/lib.sai", "package": "acme:simd@1.1.0", "snapshot": null },
                        "start": { "byte": 402, "line": 12, "col": 1, "utf16_col": 1 }, "end": { "byte": 408, "line": 12, "col": 7, "utf16_col": 7 } },
          "safety_before": "`p` is readable for 16 bytes.", "safety_after": "`p` is readable for 16 bytes.\nThe bytes are initialized." }
      ] }
  ]
}
```

1. `package` is the package identity. `before` and `after` are `<id>@<version>` of the two lock entries, as in the review of [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.7.1. `before` is `null` for a new node.
2. Each entry change has the key fields `kind`, `item` and `ordinal`. `symbol` and `location` are those of the entry in `to`, or in `from` for `removed`, printed as `sai inspect unsafe` prints them. `symbol` is `null` when the entry's `is_symbol` is false. `safety_before` and `safety_after` are the safety texts of the entry in `from` and in `to`. Each is `null` when the entry does not exist in that version, or when the entry has no `# Safety` section there.
3. **Order.** Records are ordered by the bytes of `package`, then by the selected version in `to`, in the precedence of [package/02-resolver.md](../package/02-resolver.md) §2.2.2. Entry changes are ordered by the bytes of `item`, then by `kind` in the order of 08 §8.10.4, then by `ordinal`. Two changes never have the same key, because equal keys match. The member depends only on the two lock entries, the two inventories and the selected target, so it is deterministic (P8).
4. With `public` (§12.8.3), `dependency_unsafe` is empty, like the package-level changes whose `public_api` is `none`.

**Text rendering.** The records are printed after the change blocks of §12.8.6 and before the summary line, one block per record, separated by one empty line:

```text
DependencyUnsafe acme:simd@1.0.1 -> 1.1.0
  + unsafe_fn acme:simd.load_aligned    src/lib.sai:24:1
    safety
      after:  `p` is readable for 16 bytes and aligned to 16.
  ~ unsafe_fn acme:simd.load_unaligned    src/lib.sai:12:1
    safety
      before: `p` is readable for 16 bytes.
      after:  `p` is readable for 16 bytes.\nThe bytes are initialized.
```

- The first line is `DependencyUnsafe `, then `<id>@<before> -> <after>` for a pair and `<id>@<after> (new)` for a new node, as in the review heading `Package`. It ends with ` (artifact: entries unknown)` when `artifact` is true.
- Each entry change is one line indented by two spaces: `+` for `added`, `-` for `removed`, `~` for `safety_changed`, then the kind, the item and the location as `<path>:<line>:<column>` of the file relative to its package. The columns are separated by four spaces.
- A `safety_changed` change, and an `added` or `removed` change of an `unsafe_fn` or `unsafe_trait` entry, are followed by `safety` and its `before: ` and `after:  ` lines, indented as the aspects of §12.8.6. A safety text is printed with each LF written as `\n`, and `null` is printed `none`, as for the `safety` aspect (§12.8.2). An `added` change prints only `after:  `, and a `removed` change only `before: `.

The summary line of §12.8.6 rule 5 gets the suffix `, <u> unsafe change in dependencies` for `u = 1`, and `, <u> unsafe changes in dependencies` for `u > 1`, where `u` is the number of entry changes. A diff prints `no semantic changes` only if it has no change and no record.

---

## 12.9 The symbol index

### 12.9.1 Layout

The symbol index is stored in `.sai/index/` below the package directory of the root package ([13-cli.md](13-cli.md) §13.5.2). It contains:

| File | Content |
|---|---|
| `index.json` | the header: `{ "format": "sai-index", "version": 1, "packages": [{ "file": <file name>, "blake3": <digest> }] }`, with unique records in UTF-8 byte order of `file` |
| `<ns>.<pkg>.<line>.jsonl` | the entries of one workspace package node, one JSON object per line |

`<line>` is the package's compatibility line ([package/02-resolver.md](../package/02-resolver.md) §2.1), such as `acme.user.0.4.jsonl` for the node `acme:user#0.4` or `acme.user.1.jsonl` for `acme:user#1`. Namespaces and package names contain no `.`, so the first two dots of the file name separate the namespace, the name, and the line, and the file name determines the package node. A package with no symbols has a zero-byte package file; otherwise each entry ends with one line feed, including the final entry. JSON is serialized as in [11-diagnostics.md](11-diagnostics.md) §11.9.2.

Each `blake3` is the lowercase 64-digit BLAKE3 digest of the exact bytes of its package file. Package filenames are untrusted input: parse and validate their namespace, package name, and compatibility line with the package grammar, reconstruct the canonical filename, and compare it with the recorded name before joining it to `.sai/index/`. Reject path separators, traversal, and absolute paths. Reject symlinks and reparse points in `.sai/index/`, at the fixed `index.json` path, and at every package-file or temporary-file path; use no-follow checks when opening and replacing files. The header has exactly the documented members, rejects duplicate JSON keys and duplicate filenames, and is serialized canonically with one trailing LF. For example, a header is serialized as `{"format":"sai-index","version":1,"packages":[{"file":"acme.user.1.jsonl","blake3":"<64 lowercase hex digits>}]}` followed by LF. The header therefore changes whenever any listed package file changes, even when the package set is unchanged.

### 12.9.2 Entries

Each nonempty line of a package file is one entry with exactly the following members:

```json
{"hex":"5B0C3E","def_path_hash":"9f3c0a7e51d2b84466a01c3e7f90b2d1","path":"acme:user/service.create_user","kind":"fn","fingerprint":"c41b07e2a9d35f6680e1b4c27d9a03f5"}
```

1. `hex` is the hexadecimal part of the symbol ID (§12.2.3), six uppercase digits.
2. `def_path_hash` is the `DefPathHash` of [02-frontend-ast-hir.md](02-frontend-ast-hir.md) §2.8.2, as 32 lowercase hexadecimal digits, most significant first.
3. `path` is the canonical path, `kind` the symbol kind of §12.2.1, and `fingerprint` the fingerprint of §12.2.4, as 32 lowercase hexadecimal digits.
4. Entries are sorted by `path` in byte order. No two entries of one file have the same `hex`, the same `path`, or the same `def_path_hash`.
5. Each line is the canonical JSON serialization of the entry followed by one LF. Duplicate or unknown object members, alternate JSON encodings, and a final line without LF are invalid.

A zero-byte package file contains no entries. A lone LF is an invalid blank record.

### 12.9.3 Reading, writing, and recovery

Journal recovery runs under the exclusive lock before the following rules, ordinary index validation or discard, and any symbol-ID allocation. If a journal is ambiguous or corrupt, affected packages cannot allocate IDs or perform index-backed edits until recovery is resolved.

1. **Reading.** A CLI command that reconciles or writes the index holds the exclusive lock from the start of the invocation, as §13.5.4 requires. The language server and genuinely read-only consumers hold a shared lock while reading. They read the index before allocating symbol IDs, read each package file into one byte snapshot, verify that snapshot's exact-byte BLAKE3 digest, and parse entries from those same bytes. This avoids a verification/parse time-of-check-to-time-of-use gap. A digest mismatch under the lock indicates an interrupted prior write or corruption, not a concurrent writer. Files not listed in the committed header are uncommitted orphans and are ignored. A missing `.sai/index/` is an empty index.
2. **Writing.** The index is written after reconciliation (§12.2.4) and after applying a plan (§12.6.2 rule 7), only if its content changed. A CLI command follows §13.5.4 and holds the exclusive lock for its whole duration. The language server holds a shared lock during analysis; before writing, it releases that lock, acquires the exclusive lock, rereads the index, and validates that the complete analyzed workspace snapshot is still current, including package inputs and open-document versions. If the snapshot is stale, it abandons the write and reanalyzes from a fresh snapshot and index; it never upgrades a shared lock in place. Under the exclusive lock, the writer reconciles the current symbols against the index it reread. It atomically writes each changed or new package file, flushes the file and containing directory, and then atomically replaces `index.json` with the exact BLAKE3 digests of all files in the new committed set; it flushes the header and directory before reporting success. It deletes files absent from the new header only after that header is durable, so a crash can leave an ignored orphan but cannot make a previously committed file disappear before the new header is published. A reader cannot observe an in-progress sequence while holding the shared lock; after an interrupted replacement of an already listed file, the old header's digest rejects the changed bytes instead of accepting a mixed old/new index. The exclusive-lock reread makes concurrent writers safe: a `sai edit` that ran while the server was analyzing has its recorded renames applied before the server writes.
3. **Packages.** The index has one file for every workspace package node of the current workspace snapshot. Files of package nodes that are no longer workspace packages are deleted when the index is written.
4. **Recovery.** If `index.json` cannot be read or parsed, has another `format`, or has a `version` other than 1, or if a package entry has an invalid filename or digest, duplicate or unordered package records, a listed package file is missing or its bytes do not match the recorded digest, or a package file has a line that is not a valid entry or violates §12.9.2, the whole index is discarded (N-H-76). The command reports the info diagnostic `SYMBOL_INDEX_DISCARDED`, with `primary` `null`, the message ``` the symbol index in `.sai/index/` was discarded: <reason> ```, and the note ``` symbols that were renamed or moved since they were created get new symbol IDs ```. Every symbol is then allocated fresh (§12.2.3), and the index is written anew by rule 2. A newer toolchain that writes another `version` is not compatible with this one; both discard each other's index.

The per-file digests protect the committed index-file set from a partial multi-file replacement; an unlisted new file is ignored until the new header commits. They do not make source edits and index updates one transaction. Before enabling index-backed refactorings, specify and test a durable operation journal before CLI source writes or before sending or returning edits to an LSP client. The journal must preserve exact before/after file identities and recorded symbol mappings so recovery can roll forward, roll back, or report an ambiguous external edit without silently losing a changed-body rename. LSP recovery must account for client-applied edits that do not honor `.sai/lock`. The in-memory pending-rename mechanism of §12.10.5 is only an in-session optimization and does not satisfy this requirement. Power-loss durability requires flushing package files and directories before publishing the header and flushing the committed header and directory; a flush failure is a failed write, not a successful index update.

The journal protocol is a ship prerequisite for index-backed edits, not an optional optimization. Its fixed path is `.sai/index/operation.json`, separate from the committed index header. It is canonical UTF-8 JSON with one trailing LF, exact known members, no duplicate keys, and a versioned envelope whose `format` is `sai-operation-journal` and `version` is `1`. The top-level object has exactly this ordered member set, and every member is required: `format`, `version`, `operation_id`, `origin`, `state`, `sources`, `index_before`, `index_after`, `symbol_mappings`. Unknown or duplicate members are invalid. `format` is `sai-operation-journal`; `version` is the integer `1`; `operation_id` is a canonical lower-case UUIDv4 string with the RFC variant; `origin` is one of `cli`, `lsp_apply_edit`, or `lsp_returned_edit`; and `state` is one of `prepared`, `awaiting_client`, `client_applied`, `client_rejected`, `sources_applied`, or `index_committed`.

The nested objects have these exact ordered members:

- Each `sources` item: `package_node_key`, `package_root`, `path`, `before`, `planned_after`. `sources` is nonempty; each `before` and `planned_after` is a `FileState`; every item describes a changed source (`before` differs from `planned_after`); and items are unique by `(package_node_key, path)` and sorted in ascending lexicographic order by the UTF-8 byte tuple `(package_node_key, package_root, path)`.
- Each of `index_before` and `index_after`: `header`, `package_files`, where `header` is a `FileState`. Each `package_files` item: `file`, `state`, where `state` is a `FileState`. Both arrays contain the exact same sorted set of affected basenames: every package file the operation may write, replace, or remove, including a pre-existing unlisted orphan if it is overwritten. This set MUST include every basename whose listed/unlisted membership or `blake3` digest differs between the canonical before and after headers. An absent before header denotes an empty committed package set. The planned after header MUST be a regular file with a nonempty package list: a journaled source edit operates on a workspace package, and every workspace package node has a package file even when that file has no symbol entries. The planned index generation MUST differ from the before generation as identified by the canonical header bytes; changing only an ignored orphan does not change the committed generation. Source-only changes that leave the index unchanged do not use this journal protocol. Each basename is a canonical `PackageIndexFileName`; items are unique and sorted by ascending UTF-8 byte order of `file`.
- A regular-file `header` contains canonical `sai-index` version 1 JSON as specified in §12.9.1, including its exact member set/order and trailing LF. For each recorded `package_files` basename listed by that header, `state` is `regular_file` and the BLAKE3 digest of its exact bytes equals the header's `blake3`. In `index_after`, every recorded basename omitted from the after header has `state: absent`. In `index_before`, a basename omitted from the before header may have either absent state or regular-file bytes: an unlisted regular file is an ignored orphan, and rollback MUST preserve those exact bytes if it overwrites that basename. Unchanged package files need not be embedded in `package_files`.
- `FileState` is exactly one of `{ "state": "absent" }` or `{ "state": "regular_file", "bytes": "<base64url>" }`, with no other members. `bytes` encodes the complete file bytes using the URL-safe Base64 alphabet without padding; decoding and re-encoding must reproduce the exact string.
- Each `symbol_mappings` item: `package_node_key`, `hex`, `before_path`, `after_path`. `before_path` and `after_path` are always present and each is a string or `null`; at least one is non-null, and equal before/after paths are omitted. The mapping identity is `(package_node_key, hex)`, with `hex` exactly six uppercase hexadecimal digits. Items are unique and sorted in ascending lexicographic order by the UTF-8 byte tuple `(package_node_key, hex)`; two mappings may not identify the same `(package basename, hex)`. Every non-null path is a canonical symbol path belonging to that package node. For each side whose header lists the corresponding package basename, the path claim equals the JSONL entry path for that `hex`, or is null exactly when that entry is absent. If the header omits the basename, including an ignored before-side orphan, that side has no committed entry and the mapping path is null. Every `(package basename, hex)` whose entry is created, deleted, or has a changed path between the affected before/after package-file snapshots MUST have exactly one matching mapping. The JSONL snapshots are authoritative for all other entry fields. The array may be empty when no symbol entry is created, deleted, or moved.

A `package_node_key` is `package:<namespace>:<name>@<version>`, where namespace and name are package segments (package/01 §1.2.3) and version is a canonical full SemVer 2.0.0 version without build metadata (package/02 §2.2.1). The package root is a canonical workspace-relative path, with `.` representing the workspace root; each source path is a nonempty canonical package-relative path. Both use UTF-8 `/` separators and reject absolute paths, empty components, `..` components, backslashes, and platform-specific absolute forms; `.` is allowed only as the exact workspace-root sentinel. The recorded package root must still identify the root of the recorded package node, and the package-relative source path must pass that package's source-path rules. A path whose before state is `absent` need not currently exist. A recovery observation of `unavailable` is distinct from the recorded `absent` state. Hashes alone are insufficient to restore partially written source or index files.

The complete record is canonical compact UTF-8 JSON with exactly one trailing LF, no other insignificant whitespace, and member order as specified above. Readers reject alternate encodings by parsing and re-serializing the record and requiring byte-for-byte equality; nested objects follow the member order stated above. Its complete encoded size, including JSON syntax and base64url payloads, is at most 256 MiB (268,435,456 bytes). A filesystem reader MUST reject an oversized opened journal before allocating its input buffer and MUST use a bounded read; the codec MUST reject an input byte slice over this limit before parsing. The cap bounds the full inline before/after snapshots while permitting large multi-file edits. Recovery may treat either snapshot's index as committed only after checking every basename in its header against the corresponding on-disk regular file and exact digest, including unchanged files that are not embedded in the journal. The codec validates the record grammar, canonical encodings, embedded index snapshots, and mapping cross-references; filesystem access, package-root correspondence, locking, state transitions, and recovery remain outside the codec. These member and ordering rules are a concrete v1 implementation choice that makes previously underspecified prose testable; the implementation owner records the exact decisions and rationale in `docs/design/implementation-review.md`.

Reject symlinks and non-regular journal files; use no-follow checks when opening or replacing the fixed path. A missing journal means there is no pending operation. An unreadable, noncanonical, incomplete, corrupt, or unsupported-version journal is preserved and blocks ID allocation and indexed edits with `SYMBOL_INDEX_RECOVERY_REQUIRED`; never reinterpret or discard it as an empty journal. Permit at most one unresolved operation journal per workspace; while one exists, reject every other index-writing or index-backed edit until it is settled. Durably write and flush the prepared record before changing CLI source files or exposing an LSP edit. Before every recovery read, replace, or remove, revalidate that the recorded package root still resolves to the recorded workspace package and traverse each path component relative to an open directory handle with no-follow semantics. Every intermediate component must be a real directory, and the final source target must be absent or a regular file. A symlink, non-regular target, escaped path, or changed package root is ambiguous: preserve the journal and block recovery without following or modifying that path. After every source file is at its planned contents, commit and flush the planned index state, then durably clear the journal and flush its containing directory.

On startup or before ordinary index recovery, recover any journal first. For a CLI transaction, compare every affected source path with its recorded before and planned-after state. If all paths contain only those recorded states and the planned index generation is not fully committed, restore every source and index file to the before state, removing paths recorded as absent; after verifying the exact before state, durably clear the journal and flush its containing directory. If rollback or verification fails, preserve the journal and block recovery. If all planned sources and the exact planned index generation are committed, finish by clearing the journal. An unexpected file state, or a planned index paired with sources that are not all planned-after, is ambiguous: preserve the journal and do not overwrite the observed source. For an LSP transaction that sent `workspace/applyEdit`, durably record the client's response as `client_applied` or `client_rejected` and flush the journal before changing the index or clearing the journal. `client_rejected` settles only if all files and the index still match the before state; `client_applied` settles only after all planned sources and the planned index are committed. If a crash occurs after the response but before its journal transition is durable, an `awaiting_client` record with all planned sources already present may idempotently commit the planned index; all-before remains pending because the response is unknown; any mixture or unavailable snapshot is ambiguous. For any directly returned `WorkspaceEdit`, including the fallback of `sai/applyFix` and `sai/applyOperation` when `workspace/applyEdit` is unsupported, compare all affected client snapshots with the exact before and planned-after states: if all are planned-after, idempotently commit the planned index state and clear the journal; if all are before, keep the journal pending because the client may still apply the returned edit. A mixture, unrelated edit, unavailable snapshot, corrupt journal, or contradictory index generation is ambiguous: preserve the journal, report the ambiguity, and block identity allocation and index-backed edits for affected packages. Ordinary index-discard recovery must never bypass an unresolved journal or allocate fresh IDs for its packages. Recovery after an index commit but before journal clear must recognize the committed generation and clear the journal without applying the mapping twice.
5. **Builds.** No build reads the index. Its content can change the symbol IDs in the JSON output of the commands of this chapter, and nothing else (§12.2.4).

---

## 12.10 The language server

`sai lsp` ([13-cli.md](13-cli.md) §13.6.21) runs the language server. It implements LSP 3.17 over standard input and output. Its base features are Stage 0, and the semantic protocol (§12.5) is added in Stage 6.

### 12.10.1 Capabilities

The server answers `initialize` with these capabilities. A feature whose stage the toolchain does not implement is left out.

| Capability | Value |
|---|---|
| `positionEncoding` | `utf-8` or `utf-16` (§12.10.2) |
| `textDocumentSync` | `{ "openClose": true, "change": 2, "save": { "includeText": false } }` (incremental) |
| `hoverProvider`, `definitionProvider`, `typeDefinitionProvider`, `referencesProvider`, `documentSymbolProvider`, `workspaceSymbolProvider` | `true` |
| `completionProvider` | `{ "triggerCharacters": [".", ":"], "resolveProvider": false }` |
| `signatureHelpProvider` | `{ "triggerCharacters": ["(", ","] }` |
| `renameProvider` | `{ "prepareProvider": true }` |
| `codeActionProvider` | `{ "codeActionKinds": ["quickfix", "source.fixAll.sai", "source.organizeImports", "refactor.extract", "refactor.inline"], "resolveProvider": true }` |
| `documentFormattingProvider`, `documentRangeFormattingProvider` | `true` |
| `workspace` | `{ "workspaceFolders": { "supported": true, "changeNotifications": true } }` |
| `experimental.sai` | the negotiation result of §12.5.1 |

### 12.10.2 Position encoding

The server uses `utf-8` if the client lists it in `general.positionEncodings` of its capabilities, and `utf-16` otherwise, including when the client sends no list (N-H-78). `utf-32` is never chosen. Positions are converted by [11-diagnostics.md](11-diagnostics.md) §11.5.3. The choice affects only LSP messages. The JSON records of this chapter carry all position representations (§12.5.1 rule 4).

### 12.10.3 Workspaces, targets, and profiles

1. **Folders.** The server serves one **workspace** per workspace folder that the client announces in `initialize` (`workspaceFolders`, or `rootUri` if there are none) and in `workspace/didChangeWorkspaceFolders`. The root package of a folder is discovered by [13-cli.md](13-cli.md) §13.2.1 with the folder as the start directory. A folder without a package has no workspace, and documents in it get only syntax diagnostics. Two folders with the same root package share one workspace.
2. **Documents.** A document belongs to the workspace whose root package or workspace member contains it. A document in no package gets the lexer and parser diagnostics only, and no semantic features.
3. **Target and profile.** Each workspace starts with the target and profile of the command line ([13-cli.md](13-cli.md) §13.6.21 rule 4). The client can override them in `initializationOptions.sai` as `{ "target": <name>, "profile": <name> }`, and later with `workspace/didChangeConfiguration`, whose `settings.sai` has the same members. A value that the command line would reject (`INVALID_OPTION_VALUE` or `TARGET_NOT_DECLARED`, [13-cli.md](13-cli.md) §13.4.1, §13.4.2) is reported with `window/showMessage`, with the main message of that diagnostic, and the previous value is kept. A change creates a new workspace snapshot (§12.1.2).
4. **Files on disk.** The server watches `sai.package`, `sai.lock`, and the source files of each workspace through `workspace/didChangeWatchedFiles` if the client supports dynamic registration, and otherwise rereads them on `textDocument/didSave` and before each request that needs a fresh analysis. A change of a file that is not open creates a new workspace snapshot.
5. **URIs.** LSP messages name files by URIs, unlike the records of this chapter (§12.5.1 rule 4). A workspace file is named by its `file` URI. A file of a dependency is named by the `file` URI of its copy in the package cache (N-H-80) ([package/04-distribution-and-security.md](../package/04-distribution-and-security.md)), so that a client can show it. The server never edits a file of the cache, and a `textDocument/didChange` of such a file is ignored for analysis.

### 12.10.4 Document synchronization

1. `textDocument/didOpen` makes the client's text the content of the document, replacing the content on disk, with the client's version as the document snapshot (§12.1.2). `textDocument/didChange` applies the content changes in order, with ranges in the negotiated position encoding (§12.10.2). `textDocument/didClose` makes the content on disk the content again.
2. Every change creates a new workspace snapshot. Analysis is incremental (impl/01): only the queries that depend on the changed input are recomputed. The server MAY wait for further changes before it starts the analysis of a new snapshot. Waiting changes when results are published, never what they are.
3. Every request is evaluated on one snapshot (§12.5.1 rule 3). If a document that a read-only request depends on changes before the answer is sent, the server MAY answer with `-32801` (`ContentModified`) instead. A result never mixes facts of two snapshots.

### 12.10.5 Features

Each feature is evaluated on the facts of §12.4 and §12.5 for the current snapshot. Results that list several items are sorted as stated, so that the same snapshot gives the same answer.

| Request | Result |
|---|---|
| `textDocument/hover` | On a name that resolves to a symbol: Markdown with a `sai` code block containing the symbol record's `signature` (§12.5.3), followed, for a function, by the line `effects { … }` with its signature effect set in canonical order and the word `inferred` if the set was inferred, then the doc text, then the canonical path. On an expression: its type, as a `CanonType` (spec/04 §4.9.1). On a local binding: `let <name>: <type>`. Elsewhere: `null`. |
| `textDocument/definition` | the location of the name in the declaration node of the symbol that the name resolves to; for a local, of its binding; for a `use` item or `pub use` binding, of the item it names; for a module path segment, the start of the module's file |
| `textDocument/typeDefinition` | the declaration of the nominal type at the head of the type of the expression or binding, or `null` |
| `textDocument/references` | the sites of the `references` edges to the symbol (§12.4.2) in workspace packages, plus its name if `includeDeclaration` is true; for a local, its uses in the enclosing body; sorted by file path and position |
| `textDocument/completion` | the candidates of the position (below), without `@doc(hidden)` declarations of other packages (spec/15 §15.13 rule 3) and without entities that are not visible there (spec/04 §4.6) |
| `textDocument/signatureHelp` | the signature of the callee of the enclosing call, as its one-line text without the body, with the active parameter given by the argument index, not counting `self` |
| `textDocument/prepareRename`, `textDocument/rename` | see below |
| `textDocument/codeAction` | see below |
| `textDocument/formatting` | the edits that make the document canonical ([14-formatter.md](14-formatter.md)), as minimal differences sorted by [11-diagnostics.md](11-diagnostics.md) §11.7.4 rule 2. `tabSize` and `insertSpaces` are ignored, because the format has no settings (D-TOOL-04). A document that the formatter refuses gets `[]`; its syntax diagnostics are already published. |
| `textDocument/rangeFormatting` | the edits of the unit formatting of [14-formatter.md](14-formatter.md) §14.3.6 for the range, or `[]` for a refused document |
| `textDocument/documentSymbol` | the hierarchy of the symbols declared in the document, by `contains` edges (§12.4.2), in source order, without generic parameters, with the kinds below |
| `workspace/symbol` | the symbols of workspace packages whose name contains the query as a subsequence, compared case-insensitively over ASCII letters; sorted by: the query is a prefix of the name first; then shorter names first; then canonical path bytes; at most 100 |

**Completion candidates.** After `.` that follows an expression: the visible fields of its type and the methods callable on it (inherent methods and the methods of traits in scope). After `.` that follows a module or type path: the items or members of the module or type. After `.` at the start of an implicit member (`.Variant`): the variants of the expected enum type. Otherwise: the local bindings in scope, the items of the module, the `use` bindings, the prelude, and the keywords valid at the position. The order is: locals, items of the module, imported names, prelude, keywords, each group by name bytes, expressed with `sortText` as a four-digit index. An item of another module that is not in scope is offered with `additionalTextEdits` that add the `use` of §12.6.2 rule 5. Completion never triggers a semantic operation.

**Symbol kinds.** `fn` and `extern_fn` are `Function` (12); `assoc_fn` is `Method` (6); `record` is `Struct` (23); `enum` is `Enum` (10); `variant` is `EnumMember` (22); `field` is `Field` (8); `trait` and `interface` are `Interface` (11); `resource`, `alias`, `refined`, and `distinct` are `Class` (5); `const` is `Constant` (14); `effect` is `Event` (24); `world` is `Namespace` (3); `impl` is `Object` (19); `assoc_type` is `TypeParameter` (26); `test` and `property` are `Function` (12), with the name `test "<name>"` or `property "<name>"`.

**Rename.** `prepareRename` returns the range of the name at the position if it is the `NAME` or a `NAME_REF` of a local binding, or of a symbol of a workspace package that `rename_symbol` accepts (§12.6.3). Otherwise it answers with the error `-32001` and the diagnostic `INVALID_OPERATION_TARGET`. `rename` of a symbol plans `rename_symbol` with the new name and returns the plan's edits as a `WorkspaceEdit`, or the error of the failed precondition. The plan is kept as a **pending rename** (N-H-77) only as an in-session optimization. A later snapshot containing exactly the planned text in every edited file permits its index update (§12.6.2 rule 7) and journal completion. If any snapshot differs from the planned text, the in-memory optimization is dropped, but the durable journal is preserved: an unchanged-before state remains pending, while an unexpected or mixed state is reported as ambiguous. Ordinary reconciliation and further indexed edits are blocked until that journal is resolved; a differing snapshot never silently discards the mapping. The durable operation journal required by §12.9.3 must be committed before returning the edit. Without the journal, symbol-changing rename is unavailable as a complete operation. `rename` of a local binding renames the binding and its uses in the enclosing body, with the conflict checks of `rename_symbol` precondition 3, and has no index update.

**Code actions.** `quickfix` and `source.fixAll.sai` are the actions of [11-diagnostics.md](11-diagnostics.md) §11.14. `source.organizeImports` applies the machine-applicable fixes of the `UNUSED_IMPORT` diagnostics of the document ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.3.6) and the `use` order of [14-formatter.md](14-formatter.md) §14.7.2. `refactor.extract` is offered for a selection that is exactly one expression or a sequence of statements of one block, and plans `extract_function` with the name `extracted`, or `extracted_<n>` for the smallest `n ≥ 2` that is free. `refactor.inline` is offered on a call of a function that `inline_function` accepts, for that call, and on the function's name, for every call. Refactor actions carry no edits when offered. `codeAction/resolve` plans the operation and adds its edits, or answers with the error of the failed precondition.

### 12.10.6 Applying edits

The server changes files only through the client. `sai/applyFix`, `sai/applyOperation`, and the fix-all and refactor code actions produce their edits as one `WorkspaceEdit`:

1. If the client supports `workspace.workspaceEdit.documentChanges`, the edit uses `documentChanges`: one `TextDocumentEdit` per changed file, in byte order of the paths, whose `textDocument.version` is the version of the document snapshot that was planned on (`null` for a file that is not open), preceded by a `CreateFile` with `ignoreIfExists: false` for each file that the plan creates (`move_symbol` to a new module, §12.6.4). Otherwise it uses `changes`; a plan that creates a file then fails with `INVALID_OPERATION_TARGET` and the help ``` the client cannot create files; create `<path>` and retry ```.
2. Text edits are the plan's edits (§12.6.2 rule 5) with their positions converted by §12.10.2.
3. For `sai/applyFix` and `sai/applyOperation` when the client supports `workspace/applyEdit`, the durable operation journal of §12.9.3 is committed before the server sends the request with the label `<operation> <canonical path>` or the fix title. The server waits for the answer. On `applied: true`, it durably records `client_applied`, verifies that every affected source has its planned contents, updates the symbol index by §12.6.2 rule 7 and §12.9.3 rule 2, then clears the journal. On `applied: false`, it durably records `client_rejected`; if every affected source and index file still matches the before state, it clears the journal without an index update and returns `"applied": false`. If either exact-state check fails, it preserves the journal, reports ambiguity, and blocks indexed operations. A crash while awaiting the answer is recovered under §12.9.3; an `awaiting_client` record with all planned contents can complete, while one with all before contents remains pending because the response is unknown. If the client does not support `workspace/applyEdit`, the server does not send this request: it commits an `lsp_returned_edit` journal before returning the edit, returns `"applied": false` and its `operation_id`, and follows item 4.
4. For `textDocument/rename`, resolved code actions, and the direct-return fallback in §12.5.3 when the client lacks `workspace/applyEdit`, the client applies the returned edit itself. Before returning any edit that changes indexed symbols, the server commits the durable operation journal of §12.9.3. For custom `sai/applyFix` and `sai/applyOperation` responses, the journal's `operation_id` is returned with the edit. The journal remains until client snapshots show the exact planned source contents and the matching index update is committed. If all snapshots remain at the before state, the journal stays pending because the client may still apply the edit; a timeout never cancels it. If a snapshot is mixed, unavailable, or contains unrelated text, recovery preserves the journal, reports ambiguity, and blocks indexed operations rather than silently reconciling away symbol identity. The in-memory pending-operation rule of §12.10.5 can accelerate same-session matching but is not recovery. Operations that do not change indexed symbols need no journal entry.

The server never writes a source file to disk. A file that is not open in the client is written by the client when it applies the edit.

### 12.10.7 Diagnostics

The server publishes diagnostics with `textDocument/publishDiagnostics`, mapped by [11-diagnostics.md](11-diagnostics.md) §11.14:

1. After the analysis of a snapshot completes, the server publishes the diagnostics of every file of the workspace packages that has diagnostics, with the `version` of the document snapshot they were computed on (absent for a file that is not open), and an empty list for every file that had diagnostics in the last publication and has none now.
2. The diagnostics are those of `sai check --tests` (§12.1.2) for the workspace's target and profile, with the same level resolution, dependency rules, cascade suppression, and deduplication ([11-diagnostics.md](11-diagnostics.md) §11.11).
3. A diagnostic whose primary location is in a dependency is published on the URI of the dependency's cache file (§12.10.3 rule 5). A diagnostic without a primary location is published on the root package's `sai.package` ([11-diagnostics.md](11-diagnostics.md) §11.14).
4. While an analysis runs, the previous publication stays. Syntax diagnostics of a changed document MAY be published before the semantic analysis of the snapshot completes; the full publication of rule 1 then replaces them.
5. The errors of requests (§12.5.2) are never published.

---

## 12.11 JSON Schema

The JSON documents of this chapter MUST validate against the following schema (JSON Schema 2020-12). It references the definitions of the diagnostics schema of [11-diagnostics.md](11-diagnostics.md) §11.9.5 by its `$id` `urn:sai:schema:diagnostics:1`, and is referenced by the CLI result schema of [13-cli.md](13-cli.md) §13.8. As in 11, the property order of each definition is the serialization order. The `diff` definition is open at its top level, so that [13-cli.md](13-cli.md) can extend it with `from` and `to`; every other object definition is closed. New change kinds, aspects, edge kinds, and symbol kinds are added only with a new schema version.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "urn:sai:schema:semantic:1",
  "title": "SAI semantic tooling documents, version 1",
  "$defs": {
    "symbol_id": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/symbol_id" },
    "node_id": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/node_id" },
    "source_ref": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/source_ref" },
    "location": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/location" },
    "opt_location": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/opt_location" },
    "edit": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/edit" },
    "diagnostic": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/diagnostic" },
    "strings": { "type": "array", "items": { "type": "string" } },
    "package_node": { "type": "string", "pattern": "^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*@[0-9]" },
    "module_key": { "type": "string", "pattern": "^module:[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*(/[a-z][a-z0-9_]*)*$" },
    "node_key": {
      "oneOf": [
        { "$ref": "#/$defs/symbol_id" },
        { "$ref": "#/$defs/module_key" },
        { "type": "string", "pattern": "^package:[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*@[0-9]" },
        { "type": "string", "pattern": "^(effect|capability):.+$" }
      ]
    },
    "symbol_kind": {
      "enum": ["fn", "assoc_fn", "extern_fn", "record", "alias", "refined", "enum", "distinct", "resource", "variant", "field",
               "trait", "assoc_type", "impl", "const", "effect", "interface", "world", "generic_param", "test", "property"]
    },
    "node_ref": {
      "type": "object", "required": ["file", "node"], "additionalProperties": false,
      "properties": { "file": { "$ref": "#/$defs/source_ref" }, "node": { "$ref": "#/$defs/node_id" } }
    },
    "named_symbol": {
      "type": "object", "required": ["symbol", "path"], "additionalProperties": false,
      "properties": { "symbol": { "$ref": "#/$defs/symbol_id" }, "path": { "type": "string" } }
    },
    "edge_kind": {
      "enum": ["contains", "implements", "for_type", "supertrait", "reexports", "references", "calls", "has_effect", "covers", "maps_to",
               "depends_on", "requires", "allows", "denies", "tests"]
    },
    "tree_child": {
      "type": "object", "required": ["edge", "node", "mark", "children"], "additionalProperties": false,
      "properties": {
        "edge": { "type": "string", "pattern": "^(<-)?[a-z_]+$" },
        "node": { "$ref": "#/$defs/node_key" },
        "mark": { "enum": [null, "seen", "dep"] },
        "children": { "type": "array", "items": { "$ref": "#/$defs/tree_child" } }
      }
    },
    "graph_result": {
      "type": "object",
      "required": ["graph", "direction", "depth", "edge_kinds", "deps", "complete", "trees", "nodes", "edges"],
      "additionalProperties": false,
      "properties": {
        "graph": { "enum": ["symbols", "references", "calls", "effects", "capabilities", "tests"] },
        "direction": { "enum": ["out", "in", "both"] },
        "depth": { "type": "integer", "minimum": 1, "maximum": 64 },
        "edge_kinds": { "type": "array", "items": { "$ref": "#/$defs/edge_kind" } },
        "deps": { "type": "boolean" },
        "complete": { "type": "boolean" },
        "trees": {
          "type": "array",
          "items": {
            "type": "object", "required": ["node", "children"], "additionalProperties": false,
            "properties": {
              "node": { "$ref": "#/$defs/node_key" },
              "children": { "type": "array", "items": { "$ref": "#/$defs/tree_child" } }
            }
          }
        },
        "nodes": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["key", "kind", "symbol_kind", "path", "package", "dependency", "location"],
            "additionalProperties": false,
            "properties": {
              "key": { "$ref": "#/$defs/node_key" },
              "kind": { "enum": ["symbol", "module", "package", "effect", "capability"] },
              "symbol_kind": { "oneOf": [{ "$ref": "#/$defs/symbol_kind" }, { "type": "null" }] },
              "path": { "type": "string" },
              "package": { "oneOf": [{ "$ref": "#/$defs/package_node" }, { "type": "null" }] },
              "dependency": { "type": "boolean" },
              "location": { "$ref": "#/$defs/opt_location" }
            }
          }
        },
        "edges": {
          "type": "array",
          "items": {
            "type": "object", "required": ["kind", "from", "to", "sites"], "additionalProperties": false,
            "properties": {
              "kind": { "$ref": "#/$defs/edge_kind" },
              "from": { "$ref": "#/$defs/node_key" },
              "to": { "$ref": "#/$defs/node_key" },
              "sites": { "type": "array", "items": { "$ref": "#/$defs/location" } }
            }
          }
        }
      }
    },
    "aspect": {
      "type": "object", "required": ["aspect", "added", "removed", "before", "after"], "additionalProperties": false,
      "properties": {
        "aspect": {
          "enum": ["path", "visibility", "generics", "parameters", "return", "effects", "modifiers", "requires", "ensures", "fields",
                   "variants", "type", "invariants", "value", "supertraits", "covers", "imports", "exports", "reexports", "safety", "deprecated",
                   "wit", "capabilities", "dependencies", "allow", "deny", "unsafe"]
        },
        "added": { "$ref": "#/$defs/strings" },
        "removed": { "$ref": "#/$defs/strings" },
        "before": { "type": ["string", "null"] },
        "after": { "type": ["string", "null"] }
      }
    },
    "change": {
      "type": "object",
      "required": ["kind", "path", "symbol", "before_path", "aspects", "public_api", "size_delta"],
      "additionalProperties": false,
      "properties": {
        "kind": {
          "enum": ["Added", "Removed", "Renamed", "Moved", "FunctionChanged", "TypeChanged", "ItemChanged",
                   "WitChanged", "FootprintChanged", "DependencyChanged", "PolicyChanged", "UnsafeChanged"]
        },
        "path": { "type": "string" },
        "symbol": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "type": "null" }] },
        "before_path": { "type": ["string", "null"] },
        "aspects": { "type": "array", "items": { "$ref": "#/$defs/aspect" } },
        "public_api": { "enum": ["breaking", "compatible", "none"] },
        "size_delta": { "type": ["integer", "null"] }
      }
    },
    "unsafe_entry_change": {
      "type": "object",
      "required": ["change", "kind", "item", "ordinal", "symbol", "location", "safety_before", "safety_after"],
      "additionalProperties": false,
      "properties": {
        "change": { "enum": ["added", "removed", "safety_changed"] },
        "kind": { "enum": ["unsafe_block", "unsafe_fn", "unsafe_trait", "unsafe_impl", "unsafe_module", "unsafe_extern_wasm", "unsafe_extern_c"] },
        "item": { "type": "string" },
        "ordinal": { "type": "integer", "minimum": 1 },
        "symbol": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "type": "null" }] },
        "location": { "$ref": "#/$defs/location" },
        "safety_before": { "type": ["string", "null"] },
        "safety_after": { "type": ["string", "null"] }
      }
    },
    "dependency_unsafe": {
      "type": "object", "required": ["package", "before", "after", "artifact", "entries"], "additionalProperties": false,
      "properties": {
        "package": { "type": "string" },
        "before": { "oneOf": [{ "$ref": "#/$defs/package_node" }, { "type": "null" }] },
        "after": { "$ref": "#/$defs/package_node" },
        "artifact": { "type": "boolean" },
        "entries": { "type": "array", "items": { "$ref": "#/$defs/unsafe_entry_change" } }
      }
    },
    "diff": {
      "type": "object", "required": ["changes", "dependency_unsafe", "summary"],
      "properties": {
        "changes": { "type": "array", "items": { "$ref": "#/$defs/change" } },
        "dependency_unsafe": { "type": "array", "items": { "$ref": "#/$defs/dependency_unsafe" } },
        "summary": {
          "type": "object", "required": ["changes", "breaking", "size"], "additionalProperties": false,
          "properties": {
            "changes": { "type": "integer", "minimum": 0 },
            "breaking": { "type": "integer", "minimum": 0 },
            "size": {
              "oneOf": [
                { "type": "null" },
                { "type": "object", "required": ["before", "after", "delta"], "additionalProperties": false,
                  "properties": { "before": { "type": "integer", "minimum": 0 }, "after": { "type": "integer", "minimum": 0 }, "delta": { "type": "integer" } } }
              ]
            }
          }
        }
      }
    },
    "impact": {
      "type": "object",
      "required": ["operation", "target", "path", "call_sites", "tests", "exported_interfaces", "component_worlds",
                   "breaking", "size_delta", "complete", "new_errors", "changes"],
      "additionalProperties": false,
      "properties": {
        "operation": { "type": "string", "pattern": "^[a-z][a-z_]*$" },
        "target": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "$ref": "#/$defs/module_key" }] },
        "path": { "type": "string" },
        "call_sites": { "type": "array", "items": { "$ref": "#/$defs/location" } },
        "tests": { "type": "array", "items": { "$ref": "#/$defs/named_symbol" } },
        "exported_interfaces": { "type": "array", "items": { "$ref": "#/$defs/named_symbol" } },
        "component_worlds": { "type": "array", "items": { "$ref": "#/$defs/named_symbol" } },
        "breaking": { "type": "boolean" },
        "size_delta": { "type": ["integer", "null"] },
        "complete": { "type": "boolean" },
        "new_errors": { "type": "array", "items": { "$ref": "#/$defs/diagnostic" } },
        "changes": { "type": "array", "items": { "$ref": "#/$defs/change" } }
      }
    },
    "operation_request": {
      "type": "object", "required": ["operation", "target", "arguments"], "additionalProperties": false,
      "properties": {
        "operation": { "type": "string" },
        "target": { "oneOf": [{ "type": "string" }, { "$ref": "#/$defs/node_ref" }] },
        "arguments": { "type": "object", "additionalProperties": { "type": ["string", "integer", "boolean"] } }
      }
    },
    "symbol_record": {
      "type": "object",
      "required": ["symbol", "path", "kind", "package", "visibility", "public", "location", "declaration", "signature", "docs", "effects", "members"],
      "additionalProperties": false,
      "properties": {
        "symbol": { "$ref": "#/$defs/symbol_id" },
        "path": { "type": "string" },
        "kind": { "$ref": "#/$defs/symbol_kind" },
        "package": { "$ref": "#/$defs/package_node" },
        "visibility": { "enum": ["package", "pub", "export"] },
        "public": { "type": "boolean" },
        "location": { "$ref": "#/$defs/opt_location" },
        "declaration": { "oneOf": [{ "$ref": "#/$defs/node_ref" }, { "type": "null" }] },
        "signature": { "type": "string" },
        "docs": { "type": ["string", "null"] },
        "effects": { "type": ["string", "null"] },
        "members": { "type": "array", "items": { "$ref": "#/$defs/symbol_id" } }
      }
    },
    "index_header": {
      "type": "object", "required": ["format", "version", "packages"], "additionalProperties": false,
      "properties": {
        "format": { "const": "sai-index" },
        "version": { "const": 1 },
        "packages": { "type": "array", "items": { "$ref": "#/$defs/index_package" } }
      }
    },
    "index_package": {
      "type": "object", "required": ["file", "blake3"], "additionalProperties": false,
      "properties": {
        "file": {
          "type": "string",
          "pattern": "^(?!wasi\\.)(?!sai\\.(?:core|alloc)\\.)[a-z](?:[a-z0-9]|-(?=[a-z0-9])){0,63}\\.[a-z](?:[a-z0-9]|-(?=[a-z0-9])){0,63}\\.(?:(?:[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|184467440737095[0-4][0-9]{4}|1844674407370955[0][0-9]{3}|18446744073709551[0-5][0-9]{2}|184467440737095516[0][0-9]|1844674407370955161[0-4]|18446744073709551615)|0\\.(?:[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|184467440737095[0-4][0-9]{4}|1844674407370955[0][0-9]{3}|18446744073709551[0-5][0-9]{2}|184467440737095516[0][0-9]|1844674407370955161[0-4]|18446744073709551615)|0\\.0\\.(?:0|[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|184467440737095[0-4][0-9]{4}|1844674407370955[0][0-9]{3}|18446744073709551[0-5][0-9]{2}|184467440737095516[0][0-9]|1844674407370955161[0-4]|18446744073709551615))\\.jsonl(?![\\s\\S])"
        },
        "blake3": { "type": "string", "pattern": "^[0-9a-f]{64}$" }
      }
    },
    "index_entry": {
      "type": "object", "required": ["hex", "def_path_hash", "path", "kind", "fingerprint"], "additionalProperties": false,
      "properties": {
        "hex": { "type": "string", "pattern": "^[0-9A-F]{6}$" },
        "def_path_hash": { "type": "string", "pattern": "^[0-9a-f]{32}$" },
        "path": { "type": "string", "minLength": 1 },
        "kind": { "$ref": "#/$defs/symbol_kind" },
        "fingerprint": { "type": "string", "pattern": "^[0-9a-f]{32}$" }
      }
    }
  }
}
```

The symbol record's `effects` member is the effect set of the function's entry in `EffectInterface` ([08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.9.3) as one string in canonical order, such as `"{ alloc, net.http }"`, so that it is printed exactly as in the other renderings of this chapter.

---

## 12.12 Decisions introduced by this chapter

These decisions refine D-TOOL-01, D-TOOL-02, and D-TOOL-03. They do not change them. The decisions N-H-28 and N-H-41, which this chapter applies, are introduced by [13-cli.md](13-cli.md).

| ID | Rule | Rationale |
|---|---|---|
| N-H-63 | A symbol is an entity with a canonical path of spec/04 §4.9.1 that is not a package or a module: items, members, trait and interface impls and their items, generic parameters, tests, properties, and WIT items. Modules are identified by their canonical path; local bindings, closures, `use` declarations, invariants, and world items are not symbols. Every symbol has one of the kinds of the table of §12.2.1 (§12.2.1). | Tools need a stable ID exactly for the entities that can be renamed or moved independently. A module is a file (D-SYN-08), so its path is its identity; the other excluded entities are parts of a symbol and are compared as its aspects. |
| N-H-64 | The `PackageId` in the URI authority is serialized as `<namespace>%3A<package>`. In step 3 of spec/04 §4.9.2, the text of each path component is percent-encoded: every UTF-8 byte outside `A`–`Z`, `a`–`z`, `0`–`9`, and ``- . _ ~ ! $ & ' ( ) * + , ; = : @`` is written `%XX` with uppercase hexadecimal digits. Parsing validates an RFC 3986 `reg-name`, decodes and validates the package identity, normalizes percent escapes to uppercase, checks nonempty path segments, and accepts exactly six uppercase identity hex digits (§12.2.2). | A raw `:` in an authority introduces a port delimiter and makes forms such as `symbol://acme:user/...` invalid or ambiguous. `%3A` is valid within an RFC 3986 `reg-name`; path component encoding keeps `/` unambiguous. |
| N-H-65 | The hexadecimal part is the 24 most significant bits of SipHash-1-3 (zero key) of `sai-symbol` `0x00` `<ns>:<pkg>` `0x00` `<canonical path>` `0x00` `k` (little-endian `u32`), for the smallest probe `k` that collides with no symbol allocated before it in canonical-path byte order and no hexadecimal part reserved by the symbol index. Dependencies are always allocated without an index (§12.2.3). | D-TOOL-02 requires generated IDs but does not say how. A deterministic seed gives the same IDs on every machine and for every user of a dependency, so IDs in shared logs and agent transcripts stay meaningful without a shared index. |
| N-H-66 | A package identity that matches several package nodes of the resolved graph selects, in this order: the root package or a workspace member; the node that the root package requires directly; the node with the highest selected version. Results name the chosen node with its version (§12.2.5). | Two compatibility lines of one package can coexist (package/02 §2.1). A fixed preference that favors the user's own code makes references unambiguous and the choice visible. |
| N-H-67 | Reconciliation of the symbol index matches current symbols to entries by `DefPathHash` (by canonical path if the compatibility line changed), then applies recorded changes of operations, rekeys members with their parent, and finally rematches symbols that have a unique partner with the same kind and fingerprint. Fingerprints hash the declaration's tokens without trivia, `NEWLINE`, docs, and the declaration's own name (§12.2.4). | IDs must survive renames and moves made by operations (P3), and should survive simple renames and moves made in a text editor. Requiring a unique partner avoids guessing between equal declarations. |
| N-H-68 | The hexadecimal part of a node ID is the pre-order index of the element among all nodes and tokens of the file's red tree, trivia excluded, written in uppercase hexadecimal with at least four digits (§12.3.2). | The index is computed by one traversal without a table, is the same for equal trees, and is short for the nodes that tools usually name. A node ID is meant to be valid for one snapshot only (§12.3.3). |
| N-H-69 | A graph query follows the edges of the selected kinds forward, in reverse, or both; starts at the given nodes or, without starts, at the roots of §12.4.3 rule 3; and prints one depth-first tree per start, with an expanded set per tree, the marks `seen` and `dep`, and children sorted forward before reverse, then by edge kind, printed form, and node key (§12.4.3). | `sai graph` output must be deterministic and finite on cyclic graphs, and must show every edge of the workspace once per tree. Fixed sort keys make the text and JSON outputs comparable across runs. |
| N-H-70 | Every operation of §12.6 obeys the common rules of §12.6.2: typed parameter values; nodes inside the target; edits only in workspace packages; no planning with syntax errors; edits made on the CST and formatted per format unit; the result is analyzed and must introduce no error (except the expected use-site errors of `change_type`), and refactorings preserve meaning; the symbol index is updated by the same command; application is atomic and deterministic. | Agents and editors must be able to apply an operation without reviewing a text patch (P3). These rules make each operation either a complete, checked, formatted change or a failure with a code. |
| N-H-71 | In `type` and `expr` operation values, a path may start with a package path as in a `use` declaration, followed by `.` and an item name (`acme:user/model.Email`). It names the item regardless of scope, and is written at each insertion point by §12.6.2 rule 5 (§12.6.2 rule 1). | A client that does not know the scopes of all insertion points must still be able to name a type or function unambiguously. The operation, which knows the scopes, chooses the spelling. |
| N-H-72 | Every diff change has a `public_api` value: `none` if no public entity (spec/04 §4.6.7, including `@doc(hidden)` items, spec/15 §15.13 rule 4) is changed; otherwise `breaking` if one of the twelve rules of §12.8.3 applies, else `compatible`. Bodies are not compared (N-H-73), so a body change can be breaking only through the inferred signature effect set of a public function or through the package footprint (§12.8.3). | `sai diff --public`, impact analysis, and `--allow-breaking` need one classification. The rules follow what can break a dependent package's compilation or its capability policy, and nothing else. |
| N-H-73 | The semantic diff does not compare function bodies, comments, formatting, or declaration order. A body change appears only through the properties derived from it: inferred effects, size, and the package footprint (§12.8.1 rule 2). | The diff describes the interface of the code, which is what reviewers and dependents rely on. Text diffs already show bodies; repeating them would hide the semantic changes. |
| N-H-74 | Diff entities are matched by same canonical path and kind, then by same kind and hexadecimal part of the symbol ID, then by unique kind-and-fingerprint partner, with members following their parent after each match by path change (§12.8.1 rule 3). | Exact paths are the common case. Symbol IDs recognize renames made by operations even when the body changed too, and fingerprints recognize editor renames, in the same order as reconciliation (N-H-67). |
| N-H-75 | Impact analysis counts distinct call sites in workspace files, tests and properties, exported interfaces, and component worlds, from the before version on the graphs of §12.4.2, and the size change only on request. Dependencies' interfaces, worlds, and tests are never counted. The impact is incomplete when the before version has errors (§12.7.2). | The counts must be exact and cheap enough to compute before every operation. Building artifacts is expensive, so it is optional; dependencies cannot be affected by an edit of the workspace. |
| N-H-76 | The symbol index is `.sai/index/index.json` with one JSON Lines file `<ns>.<pkg>.<line>.jsonl` per workspace package node. Its unique, UTF-8-byte-sorted header records each basename and the lowercase BLAKE3 digest of that package file's exact bytes. LSP and read-only consumers read under the shared lock from the same byte snapshots that are hashed; CLI index writers hold the exclusive lock for the full command. Writers atomically replace changed files, publish the matching header last, and remove obsolete files only after that header is durable. Invalid or mismatched committed files discard the whole index with `SYMBOL_INDEX_DISCARDED` only after journal recovery; unlisted files are ignored. Builds never read it (§12.9). | The index affects only symbol IDs in JSON output, so a corrupt index is replaced, not repaired, once no journaled transaction can recover it. Exact content digests detect interruption among files in the previously committed set; an unlisted file is not part of that set. |
| N-H-77 | At most one unresolved index-changing LSP edit may exist per workspace, including edits returned directly when the client lacks `workspace/applyEdit`. Journal it durably before sending or returning it; settle it after the exact planned source state and corresponding index update are committed. An unchanged snapshot does not cancel a returned edit, so its journal remains pending and blocks other index-writing operations. A timeout never cancels it. An in-memory pending edit may accelerate exact same-session matches but is never sufficient for recovery (§12.5.3, §12.9.3, §12.10.5–§12.10.6). | The server never writes files (§12.10.6), so a client-applied edit may outlive a server crash. Durable before/after states preserve source and symbol identity; the server cannot infer rejection from an unchanged snapshot, and must not overwrite another edit when observed content is unexpected. Automatic expiry could let a delayed client apply an edit after its ID mapping had been discarded. |
| N-H-78 | The language server uses the `utf-8` position encoding if the client offers it, and `utf-16` otherwise. It never chooses `utf-32` (§12.10.2). | UTF-8 offsets are the compiler's native representation (11 §11.5.3), so they avoid conversions and their errors. `utf-16` is the LSP default that every client supports. |
| N-H-79 | Semantic tooling analyzes a workspace snapshot as `sai check --tests` does, so tests and test-only code are in every graph and are updated by every operation. Capability facts (`Req`, `requires` edges, `FootprintChanged`) are those of the build without tests (§12.1.2 rule 2). | An operation that ignored tests would leave them broken, and impact analysis must count them. Capabilities describe the artifact, which never contains tests (spec/14). |
| N-H-80 | LSP names a file of a dependency by the `file` URI of its copy in the package cache. The server never edits such a file and ignores `didChange` for it in analysis (§12.10.3 rule 5). | Go-to-definition into dependencies must open a real file. The package cache is content-addressed and must stay unchanged (package/04). |
| N-H-87 | The semantic diff compares the unsafe inventories (`sai inspect unsafe`) of every dependency node whose lock entry changed version, source or checksum, or that is new, paired by identity and compatibility line, then by unique identity. Entries are matched by `(kind, item, ordinal)`; changes are `added`, `removed`, and `safety_changed` for `unsafe_fn` and `unsafe_trait` entries whose `# Safety` texts differ after trailing blanks of each line are removed. Artifact-form nodes get a record without entries. The records are the `dependency_unsafe` member of the diff, ordered by package and version (§12.8.7). | spec/12 §12.11.2 asks that new unsafe code of a dependency be visible in review (P1, P3). Keys from canonical paths survive edits and fresh symbol ID allocation, and depend only on the two lock entries and inventories, so the output is deterministic (P8). The diff reuses the inventory of `sai inspect unsafe` instead of a new analysis or command (P9), and checksums skip unchanged packages. |
| N-H-88 | An index package file with no symbol entries is zero bytes; a nonempty file has one canonical JSON object and one LF per entry, with no blank records. | A final LF is required to detect truncated records, but a package can have no symbols. A blank JSON line would not represent an entry and must not be accepted as an empty package. |
| N-H-89 | A tuple-field component writes its index as unpadded decimal, and a test/property component writes its decoded name as a simple double-quoted string with only `\\` and `\"` escapes; all other string code points are preserved without NFC normalization (spec/04 §4.9.1). | Multiple source integer bases/separators or string literal forms can denote one entity. Fixed path spellings keep canonical paths and symbol-index byte order unique while preserving the test name's code points. |

---

## 12.13 Diagnostics

This chapter owns the codes below that have no owner noted. They describe requests, operations, and the symbol index, not the program, so they are reported by the commands of [13-cli.md](13-cli.md) §13.6.16 to §13.6.18 and in the `data` of protocol errors (§12.5.2), and are never published as LSP diagnostics. All of them except `SYMBOL_INDEX_DISCARDED` make the request fail. The other rows are codes of other documents that this chapter reports or reacts to.

| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_SYMBOL_ID` | error | A string that starts with `symbol://` is not a valid symbol ID, or no symbol of the chosen package node has its hexadecimal part (§12.2.5). |
| `SYMBOL_INDEX_RECOVERY_REQUIRED` | error | An index operation is pending or its journal is unreadable, incompatible, or ambiguous; symbol ID allocation and indexed edits are blocked until recovery succeeds (§12.9.3). |
| `UNKNOWN_SYMBOL_PATH` | error | A canonical path does not parse or names no symbol or module, or a node key names no graph node (§12.2.5, §12.4.3). |
| `UNKNOWN_NODE_ID` | error | A node ID is malformed, names a file outside the workspace snapshot, or names no element of the given group (§12.3.3). |
| `STALE_SNAPSHOT` | error | A request names a document version that is not the current one; JSON-RPC code `-32801` (§12.3.3, §12.5.2). |
| `FIX_NOT_FOUND` | error | `sai/applyFix`: the snapshot has no diagnostic with the given code at the given node, or the diagnostic has no fix with the given index (§12.5.3). |
| `UNKNOWN_OPERATION` | error | The operation name is not one of §12.6 (§12.6.1). |
| `INVALID_OPERATION_ARGUMENT` | error | A parameter is missing, unknown, or not a valid value of its type, or a `node` parameter is not inside the target's declaration (§12.6.1, §12.6.2 rules 1 and 2, and the operations). |
| `INVALID_OPERATION_TARGET` | error | The target is not of a kind the operation accepts, is a module where a symbol is required, or is declared in a dependency; or a client cannot create files (§12.2.5 rule 4, §12.6.2 rule 3, §12.10.6). |
| `FILE_HAS_SYNTAX_ERRORS` | error | A source file of a workspace package has lexer or parser errors, so no operation can be planned (§12.6.2 rule 4). |
| `OPERATION_INTRODUCES_ERRORS` | error | The planned edits would introduce errors; `data.errors` lists them (§12.6.2 rule 6). |
| `OPERATION_NAME_CONFLICT` | error | A name that the operation would introduce is already declared or bound where it would be visible, or would shadow a name that a reference must keep (§12.6.3, §12.6.4, §12.6.5, §12.6.8, §12.6.10, §12.6.12). |
| `FUNCTION_STILL_REFERENCED` | error | `delete_function`: something outside the target's declaration, other than `use` items, still refers to it (§12.6.6). |
| `PARAMETER_STILL_USED` | error | `remove_parameter`: the body of the target, or of an impl of the trait method, still uses the parameter (§12.6.9). |
| `ARGUMENT_NOT_REMOVABLE` | error | `remove_parameter`: the argument at a call site is not a removable expression, so removing it could remove effects or a panic (§12.6.9). |
| `EXTRACT_NOT_POSSIBLE` | error | `extract_function`: the selected nodes cannot form a function body that preserves meaning (§12.6.10). |
| `INLINE_NOT_POSSIBLE` | error | `inline_function`: the function cannot be inlined while preserving meaning (§12.6.11). |
| `TRAIT_ALREADY_IMPLEMENTED` | error | `implement_trait`: the type already has an impl of the trait with the same arguments, written or derived (§12.6.13). |
| `SYMBOL_INDEX_DISCARDED` | info | The symbol index in `.sai/index/` is unreadable, has another format or version, or has an invalid file, and is discarded; `primary` is `null` (§12.9.3 rule 4). |
| `OPERATION_BREAKS_PUBLIC_API` | error | defined in [13](13-cli.md) §13.6.18 (N-H-41); also reported by `sai/applyOperation` without `allow_breaking` (§12.5.3) |
| `OUTPUT_WRITE_FAILED` | error | defined in [13](13-cli.md) §13.3.4; an edited file or an index file cannot be written; the plan is rolled back (§12.6.2 rule 8) |
| `INVALID_OPTION_VALUE` | error | defined in [13](13-cli.md) §13.4.1; a target or profile in the LSP configuration is invalid; reported with `window/showMessage` (§12.10.3 rule 3) |
| `TARGET_NOT_DECLARED` | error | defined in [package/01](../package/01-package-contract.md) §1.8; reported as in [13](13-cli.md) §13.4.2, and handled like `INVALID_OPTION_VALUE` in the LSP configuration (§12.10.3 rule 3) |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6. Answered with JSON-RPC code `-32603` (§12.5.2). |
| `UNUSED_IMPORT` | warning (lint) | defined in [spec/04](../spec/04-modules-and-names.md) §4.3.6; its fixes implement `source.organizeImports` (§12.10.5) |
| `UNUSED_VARIABLE` | warning (lint) | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.18.1; a warning of the checked copy does not make an operation fail (§12.6.2 rule 6, §12.6.14) |
