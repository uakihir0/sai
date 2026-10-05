# 04. Distribution and Security

**Status:** Normative
**Stage:** 3 (artifacts and hybrid packages: Stage 4)
**Related:** [01-package-contract.md](01-package-contract.md), [02-resolver.md](02-resolver.md), [03-lock-file.md](03-lock-file.md), [decisions.md](../design/decisions.md) (D-PKG-01 to D-PKG-05, D-CAP-01, D-TOOL-03, D-TOOL-05), [draft-reconciliation.md](../design/draft-reconciliation.md) (O-04), [spec/08](../spec/08-effects-and-capabilities.md), [spec/13](../spec/13-components-and-wit.md), [impl/07](../impl/07-component-lowering.md), [impl/13](../impl/13-cli.md), [targets-and-profiles.md](../targets/targets-and-profiles.md)

This chapter defines how package content is obtained and protected: the accepted Git URLs and their canonical form, the fetch procedure and the relation between release tags and the lock, the global package cache and its integrity, the selection and verification of artifacts for artifact and hybrid packages, the capability review and its approval, the handling of checksum mismatches and withdrawn versions, reproducible and offline builds, and the supply-chain threats that the design addresses. It states that SAI has no build scripts. Registries and signing are described as informative future work (O-04). Version selection is defined in [02-resolver.md](02-resolver.md), the lock file and the `sai1:` checksums in [03-lock-file.md](03-lock-file.md), capability semantics in [spec/08](../spec/08-effects-and-capabilities.md), and command-line spelling in [impl/13](../impl/13-cli.md).

---

## 4.1 Overview

### 4.1.1 Distribution model

1. In v0.1, packages are distributed through **Git repositories** and used locally through **path sources** (D-PKG-01). There is no registry and no central index (O-04). MVS needs only the versions that are actually requested, so no index is needed (D-PKG-02).
2. A published package version is a **release tag** in a repository ([02 §2.6.1](02-resolver.md)). Its content is the tree of the tagged commit in the package directory.
3. The standard packages other than `sai:core` and `sai:alloc` are ordinary Git packages. The toolchain ships them in a pre-populated cache (D-PKG-04, §4.4.3).
4. All Git operations are performed in-process by the toolchain's Git implementation (`gix` in the reference implementation). The system `git` program and Git hooks are never invoked (D-TOOL-05).

### 4.1.2 Trust model

The user trusts the toolchain and the content that they reviewed and locked. Under that assumption, SAI guarantees:

1. **No code execution.** No code from a dependency runs during resolution, fetching, or building (§4.10).
2. **Pinned content.** A build uses exactly the content recorded in `sai.lock`, or fails ([03 §3.7.3](03-lock-file.md)).
3. **Visible capabilities.** The capability footprint of new or changed content is reviewed before the lock records it (§4.7), and every build rejects reachable uses of capabilities outside the policies ([spec/08 §8.12.6](../spec/08-effects-and-capabilities.md)).
4. **Portable materialization.** Locked content is materialized identically on every supported host ([03 §3.4.3](03-lock-file.md), [03 §3.4.4](03-lock-file.md)).
5. **Secure transport.** Content is fetched only over authenticated, encrypted transports (§4.2).

The following are **not** protected against, and the documentation of the toolchain MUST say so:

- a dependency that misuses capabilities it was allowed and granted (for example, a package allowed `wasi:http` that sends data it receives to a third party);
- the first fetch of a version: the first lock trusts whatever the repository serves at that moment (trust on first use);
- a compromised toolchain, host, or global cache;
- the unavailability of an upstream repository, which is mitigated by the cache (§4.4) and by mirrors named in overrides (§4.11).

---

## 4.2 Git URLs

### 4.2.1 Accepted URLs

A Git URL appears in `from git` and `use git` in `sai.package` ([01 §1.10.3](01-package-contract.md), [01 §1.16](01-package-contract.md)), as the argument of `sai add`, and in `source git` in `sai.lock`. Every Git URL MUST satisfy these rules (N-G-47). A violation in `sai.package` or on the command line is reported at the URL; a violation in `sai.lock` is `LOCK_CORRUPT` ([03 §3.3.5](03-lock-file.md)).

1. The scheme is `https` or `ssh`, compared case-insensitively.
2. `http` and `git` are `INSECURE_GIT_URL`: neither authenticates the server nor protects the content in transit. The help suggests the `https` form.
3. The scp-like form `user@host:path` (no scheme) is `UNSUPPORTED_GIT_URL`, with a fix to `ssh://user@host/path`: the scp form is ambiguous with local paths and has no port.
4. `file` URLs and plain local paths are `UNSUPPORTED_GIT_URL`. The help suggests `from path`, or `overrides { <id> use path "<dir>" }` for a local checkout.
5. Every other scheme (`git+ssh`, `ftp`, ...) is `UNSUPPORTED_GIT_URL`.
6. An `https` URL with any user information (`user@`, `user:password@`, `token@`) is `CREDENTIALS_IN_URL`: the URL is written to `sai.package` and `sai.lock` and would leak the credential. An `ssh` URL may carry a user name (`ssh://git@github.com/...`) but not a password (`CREDENTIALS_IN_URL`).
7. The host is an ASCII DNS name (letters, digits, `-`, and `.`), an IPv4 address, or an IPv6 address in brackets. A non-ASCII host is `UNSUPPORTED_GIT_URL`. An internationalized host name MUST be written in its ASCII form (`xn--...`), which makes look-alike host names visible.
8. A port, if present, is a decimal number from 1 to 65535.
9. The path is not empty and consists of segments separated by `/`. A segment consists of ASCII letters, digits, `-`, `.`, `_`, and `~`. Empty segments (except a single trailing `/`), `.` and `..` segments, and percent-encoding are `UNSUPPORTED_GIT_URL`.
10. A query (`?`) or a fragment (`#`) is `UNSUPPORTED_GIT_URL`.
11. The URL is at most 2048 bytes long.

### 4.2.2 Canonical form

The **canonical form** of an accepted URL is obtained by:

1. writing the scheme and the host in lowercase;
2. removing the port if it is the default port of the scheme (443 for `https`, 22 for `ssh`);
3. removing a trailing `/`.

Nothing else changes. In particular, the case of the path, the user name of an `ssh` URL, and a `.git` suffix are preserved: `https://github.com/acme/json` and `https://github.com/acme/json.git` are different sources. Hosts differ in whether they treat such URLs as equal, and the toolchain does not guess (P9).

The canonical form is used wherever URLs are compared or recorded: in source selection ([02 §2.8](02-resolver.md)), in `sai.lock`, as the cache key (§4.4.1), and in the requirement written by `sai add`. A non-canonical URL in `sai.package` is accepted and canonicalized when it is read.

### 4.2.3 Authentication and transport

1. Credentials are never stored in `sai.package`, `sai.lock`, the cache, or any diagnostic.
2. **ssh:** authentication uses the user's SSH agent and default key files. The server's host key MUST be verified against the user's known-hosts file. An unknown or changed host key is `FETCH_FAILED`; the help explains how to verify the key. Host keys are never accepted automatically.
3. **https:** TLS certificates are verified against the host's trust store. Verification cannot be disabled. On an authentication challenge, credentials are requested from the Git credential helpers configured for the user.
4. The standard proxy environment variables (`HTTPS_PROXY`, `NO_PROXY`, and their lowercase forms) are honored for `https`.
5. No other Git configuration is read: no `url.<base>.insteadOf`, no `http.sslVerify`, no hooks, no templates, no `core.*` settings. A configuration that silently redirects a source would make the build depend on state outside the package (P1, P8). Mirrors are declared explicitly with `overrides { <id> use git "<mirror>" }`.
6. These settings affect only authentication and transport, never which content is used ([impl/13 §13.1.1](../impl/13-cli.md) rule 2).

---

## 4.3 Fetching

### 4.3.1 Protocol

1. Content is fetched with the Git smart protocol over `https` or `ssh`, using protocol version 2 when the server supports it.
2. A fetch requests as little as possible: the single ref of one release tag, or one commit by its id, with a shallow depth of 1. No branch and no other ref is fetched.
3. Listing tags (`sai update`, `sai add`; [02 §2.6.1](02-resolver.md) rule 3) reads the ref advertisement, restricted to the prefix `refs/tags/v` or `refs/tags/<dir>/v`. It fetches no objects.
4. The transport details (protocol version, shallow depth, partial-clone filters, pack negotiation) are deliberately free (N-G-48): they cannot change the result, because every object is checked against its Git object id and the content against the lock's checksums.

### 4.3.2 Procedure

`Fetch(url, d, v)` obtains the content of version `v` of the package in directory `d` of the repository at `url`. Let `u` be the canonical URL and `E` the lock entry for the version with the same source, if any.

1. **Cache.** If `E` exists and the cache holds verified content with `E`'s source checksum (§4.4.1), that content is used. No network access is made.
2. **Tag.** Otherwise the release tag of `v` ([02 §2.6.1](02-resolver.md)) is fetched from `u`. If the repository has no such tag, see §4.3.3 rule 3 when `E` exists; otherwise the error is `VERSION_NOT_FOUND` ([02 §2.6.2](02-resolver.md)).
3. **Peel.** An annotated tag is followed to its target, repeatedly, until a non-tag object is reached. That object MUST be a commit; otherwise the error is `INVALID_TAG`, naming the tag and the kind of object. Signatures on tag objects are not verified (§4.12).
4. **Lock check.** If `E` exists, the commit MUST equal `E`'s commit (§4.3.3).
5. **Tree.** The tree of the commit at `d` is read. A missing directory or a directory without `sai.package` is `MISSING_PACKAGE_FILE` ([02 §2.6.2](02-resolver.md)).
6. **Limits.** The size limits of §4.4.4 are enforced while the tree is walked.
7. **Checksums.** The source checksum, the WIT checksum, and the artifact checksums are computed from the Git objects ([03 §3.4](03-lock-file.md), [03 §3.5](03-lock-file.md), [03 §3.6](03-lock-file.md)). Path validation and collision checks happen here (`INVALID_PACKAGE_FILE_PATH`, `PATH_COLLISION`).
8. **Verify.** If `E` exists, the checksums MUST equal `E`'s (`CHECKSUM_MISMATCH`, [03 §3.7.3](03-lock-file.md)). Otherwise they become the values of the new entry.
9. **Materialize.** The selected files are written to a new directory below `tmp/` in the cache, made read-only, and renamed to their final place (§4.4.1). The tag record `(u, tag) -> (commit, checksum)` is added to the cache.
10. **Contract.** The contract is read from the cache and checked as in [02 §2.6.2](02-resolver.md) steps 4 and 5.

Fetches from different repositories may run in parallel; the result never depends on their order ([02 §2.14.2](02-resolver.md)). A path package inside a Git package ([02 §2.7.2](02-resolver.md)) is taken from the same commit and is cached under its own checksum.

### 4.3.3 Release tags and the lock

A release tag is the way to find the commit of a version when the version first enters the lock. From then on, the lock is authoritative: the content of a locked version is its locked commit with its locked checksums (D-PKG-03, N-G-49). Publishers MUST NOT move or delete a release tag; a broken release is fixed by publishing a new version.

1. Whenever the toolchain fetches or lists the release tag of a locked version, the tag MUST peel to the locked commit. Otherwise the error is `TAG_MOVED`, naming the package, the tag, the locked commit, and the commit the tag points to now. Nothing is cached and the lock is unchanged. Tags are compared by the commit they peel to: re-creating an annotated tag on the same commit is not a move.
2. A moved tag means that a published version was rewritten, by its maintainer or by an attacker. The help says so and offers two ways forward: keep the reviewed content by fetching from a mirror that still has the original tag (`overrides { <id> use git "<mirror>" }`, a source change that needs approval, §4.7.2), or accept the new content with `sai update <id>` (rule 4).
3. If the release tag of a locked version no longer exists, the toolchain fetches the locked commit by its id. If the server provides it and the checksums match, the content is used and the warning `TAG_MISSING` is reported. Otherwise the error is `VERSION_NOT_FOUND`, with a note that the locked commit is no longer available from the source. Deleting a tag therefore does not break builds whose content is still available (§4.6).
4. When `sai update` probes a node ([02 §2.10.5](02-resolver.md)) whose selected version stays the same and whose release tag has moved, the replacement of the content is a review item that needs approval (§4.7.2). With approval, the content of the tag's current commit is fetched, verified as a new entry, and recorded with its new commit, checksums, and footprint. Without approval, the command fails with `TAG_MOVED` and writes nothing.
5. A build from the cache does not contact the repository and therefore cannot observe a moved tag. This is deliberate: the build uses exactly the content that was reviewed and locked.

### 4.3.4 Disabled Git features

1. **No checkout.** Content is read from Git objects and materialized directly; no working tree is created.
2. **Hooks** are never run (D-PKG-02).
3. **Submodules** are never fetched. Gitlinks are skipped with `SUBMODULE_IGNORED` ([03 §3.4.2](03-lock-file.md)).
4. **Git LFS** objects are never fetched; pointer files are content like any other file ([03 §3.4.2](03-lock-file.md) rule 8). A package that needs large binary files commits them, within the limits of §4.4.4.
5. **Attributes and filters** (line-ending conversion, smudge and clean filters, `export-ignore`, `export-subst`) are never applied.

---

## 4.4 The global package cache

### 4.4.1 Location and layout

There is one global package cache per user, shared by all projects and all toolchain versions (N-G-50).

1. Its location is the value of the environment variable `SAI_CACHE_DIR` if it is set and is an absolute path. Otherwise it is `$XDG_CACHE_HOME/sai` (if `XDG_CACHE_HOME` is set and absolute) or `$HOME/.cache/sai` on Linux and other Unix hosts, `$HOME/Library/Caches/sai` on macOS, and `%LOCALAPPDATA%\sai\cache` on Windows. A relative `SAI_CACHE_DIR` is a usage error ([impl/13 §13.3.5](../impl/13-cli.md)); an empty one counts as unset. Apart from the authentication and transport settings of §4.2.3, which never affect which content is used, these are the only environment variables that the package system reads ([impl/13 §13.1.1](../impl/13-cli.md) rule 2).
2. The cache has this layout:

```text
<cache>/
    v1/                     layout version
        git/<repo-key>/     Git objects and fetched refs of one repository
        src/<digest>/       verified, read-only content of one package directory
        tags/               tag records
        tmp/                fetches in progress
        lock                cache lock file
```

3. `<repo-key>` is the SHA-256 of the canonical URL (§4.2.2), as 64 lowercase hexadecimal digits. `<digest>` is the 64 hexadecimal digits of a source checksum, without the `sai1:` prefix. `src/<digest>/` contains exactly the selected files of [03 §3.4.2](03-lock-file.md), with their paths.
4. A **tag record** maps `(canonical URL, tag name)` to the commit the tag peeled to when it was fetched, and `(canonical URL, commit, dir)` to the source checksum of that directory. Tag records let a later resolution find a locked or previously fetched version without network access ([03 §3.8.2](03-lock-file.md)).
5. The meaning of `src/` and of the tag records is normative. The internal format of `git/`, `tags/`, and `tmp/` is deliberately private to the toolchain, under the layout version `v1`; a toolchain that changes it uses a new layout version directory.

### 4.4.2 Integrity

1. An entry of `src/` is created only by §4.3.2 step 9, after its checksums matched the lock or became the values of a new lock entry. It is read-only and is never modified in place.
2. An entry is identified by its checksum. Two packages, versions, or repositories with identical content share one entry.
3. `sai fetch` ([impl/13 §13.6.13](../impl/13-cli.md)) re-hashes the cached content of every locked entry. An entry whose tree hash differs from its name is removed and fetched again, with the warning `CACHE_CORRUPT`. Under `--offline` it is removed and the command fails with `OFFLINE_FETCH_REQUIRED`.
4. Other commands use cached entries without re-hashing them ([03 §3.7.3](03-lock-file.md) rule 2).
5. Deleting the cache, or any directory in it, is always safe when no toolchain process is running: the next command fetches again. In v0.1 the toolchain never removes `src/` entries on its own; there is no automatic garbage collection.

### 4.4.3 Bundled standard packages

1. The toolchain ships the content of the standard packages of D-PKG-04 (`sai:collections`, `sai:text`, `sai:fmt`, `sai:io`, `sai:fs`, `sai:net`, `sai:http`, `sai:async`, `sai:process`) for a set of versions, together with a **bundled index**: for each shipped version, the canonical URL, the tag, the commit, and the source checksum.
2. The bundled index acts as a set of tag records. Its content is installed into `src/` on first use, after its checksum has been verified like any fetched content.
3. Bundled content is used only when the source of the node is the canonical URL in the bundled index. A fork or a mirror of a standard package is an ordinary Git source.
4. The bundled set never changes which version is selected ([02 §2.5.4](02-resolver.md) rule 4). A version that is not bundled is fetched from its repository.
5. A bundled index entry whose commit differs from the lock is treated like a moved tag (§4.3.3).

### 4.4.4 Limits

Fetched content is subject to fixed limits (N-G-51). They are not configurable, so that a package accepted on one host is accepted on every host (P8).

| Limit | Value |
|---|---|
| selected files in one package directory | 100,000 |
| size of one file | 256 MiB |
| total size of one package directory | 1 GiB |
| data transferred by one fetch | 4 GiB |
| path length, component length | 4096 bytes, 255 bytes ([03 §3.4.3](03-lock-file.md)) |
| release tags considered in one listing | 100,000 |

Exceeding a limit is the error `PACKAGE_TOO_LARGE`, naming the package, the limit, and the value reached. The fetch is aborted, and nothing is written to `src/`.

### 4.4.5 Concurrency

1. Fetches write only below `tmp/`, in directories with unique names, and may run concurrently in several processes.
2. Moving a verified directory from `tmp/` into `src/` and updating tag records require the exclusive lock on `<cache>/v1/lock`, which is held only for that step. If the target directory already exists, another process has installed the same content; the new copy is discarded.
3. Readers take no lock: entries are immutable and appear atomically.
4. A temporary directory left by an interrupted fetch is removed by the next writer once it is more than 24 hours old.

---

## 4.5 Artifact and hybrid packages (Stage 4)

An **artifact package** ships prebuilt Wasm components; a **hybrid package** ships both source and components ([01 §1.1.2](01-package-contract.md), [01 §1.7](01-package-contract.md)). Both declare `wit` and `export world` (N-G-19). Before Stage 4, declaring `artifact` is `FEATURE_NOT_IMPLEMENTED`.

### 4.5.1 Form selection

For every build, with target `t` and profile `p`, each active node `n` ([02 §2.5.3](02-resolver.md)) is used in exactly one **form**:

| Package kind | Form for `(t, p)` |
|---|---|
| source | source |
| artifact | the artifact for `t`; an artifact package can list only component targets ([01 §1.7](01-package-contract.md) item 2, `ARTIFACT_ON_CORE_TARGET`), so a build for a core or native target that activates it fails with `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` |
| hybrid | 1. source if `p` is `tiny`, if `t` is a core target, if `n` has no artifact for `t`, or if `n` is the root's allocator package (item 6); 2. otherwise the artifact if the root has `overrides { <id> use artifact }`; 3. otherwise source (D-PKG-05) |

1. The choice depends only on the package kind, the target, the profile, the root's overrides, and the root's `allocator` declaration. It is deterministic and is the same on every host (P8).
2. The form is not part of resolution and is not recorded in the lock ([03 §3.3.3](03-lock-file.md) rule 3). Changing the profile or an override never changes `sai.lock`.
3. The chosen form of every node is shown by `sai size` (the `forms` list of its result, [impl/13](../impl/13-cli.md) §13.6.14) and by `sai inspect deps` ([impl/13](../impl/13-cli.md) §13.6.15) (D-PKG-05).
4. An artifact package used under the `tiny` profile is used in artifact form, since it has no source. Its imports are subject to the profile's capability restrictions like any other code (D-TGT-02, [spec/08 §8.12.6](../spec/08-effects-and-capabilities.md)).
5. How an artifact is composed with the root's component is owned by [impl/07](../impl/07-component-lowering.md) §7.11.9. How a hybrid in source form provides its interfaces within the build (§4.5.3 item 4) is owned by impl/07 §7.11.10.
6. The package named by the root's `allocator` declaration ([01 §1.15](01-package-contract.md)) is always used in source form, in every target and profile, because its functions are linked into the module ([impl/10 §10.2.7](../impl/10-runtime.md) item 3, N-F-30). A hybrid allocator package therefore uses its source, and an `overrides { <id> use artifact }` of the root does not apply to it. An artifact package cannot be an allocator package: naming one in `allocator` is `ALLOCATOR_INTERFACE_MISMATCH` (defined in [impl/10](../impl/10-runtime.md) §10.2.7).

### 4.5.2 Artifact verification

An artifact is verified when an entry with that artifact is created or changed (for the root's targets), and whenever a build uses the artifact form and the artifact has not been verified for the entry's checksum before:

1. **Checksum.** The bytes of the artifact file MUST match the entry's `artifact <t>` checksum ([03 §3.6](03-lock-file.md); `CHECKSUM_MISMATCH` with kind `artifact <t>`).
2. **Validity.** The file MUST be a valid Wasm component in the binary format, not a core module, and its core modules MUST use the memory width of `t` (32-bit for `wasm32-*`, 64-bit for `wasm64-*`). Otherwise the error is `INVALID_ARTIFACT`, naming the reason. A Git LFS pointer file is invalid in this sense; the help mentions LFS ([03 §3.6](03-lock-file.md) rule 4).
3. **Conformance to the world.** Let `W` be the package's `export world`, resolved in its `wit` directory ([01 §1.9](01-package-contract.md)). The component MUST export every export of `W`, with types equal to those of `W`, and every import of the component MUST be an import of `W`, with equal types. Types are compared after resolving the WIT packages of the build ([spec/13 §13.7.1](../spec/13-components-and-wit.md)), so WASI imports must use the WASI version of `t`. A violation is `ARTIFACT_WIT_MISMATCH`, naming the item and the difference. Exports of the component that are not in `W` are allowed; they are hidden by composition and are not visible to dependents.
4. **Capabilities.** The capabilities corresponding to the component's imports are recorded as the entry's `imports <t>` field ([03 §3.6](03-lock-file.md) rule 5). They are checked at build time by spec/08 like the needs of any code; an artifact's imports are always reachable (D-CAP-01). There is no separate artifact capability code: an import that the policy does not allow is `CAPABILITY_NOT_ALLOWED` or `CAPABILITY_DENIED`, and an import the target cannot provide is `TARGET_CAPABILITY_UNAVAILABLE` ([spec/08 §8.12.6](../spec/08-effects-and-capabilities.md)).

Verification results are cached per artifact checksum, so an artifact is verified once per content. SAI verifies the interface of an artifact, not its behaviour, and not that it was built from the package's source (§4.11).

### 4.5.3 Hybrid consistency

D-PKG-05 lets the form of a hybrid package change with the target, the profile, and the root's overrides. A dependent must therefore compile identically against both forms (N-G-52):

1. Dependents of an artifact or hybrid package see the package only through the SAI view of its WIT ([spec/13 §13.7](../spec/13-components-and-wit.md)), in both forms. The SAI modules of a hybrid package's source are not visible to its dependents.
2. The source of a hybrid package MUST implement its `export world`, and the WIT generated from that world ([spec/13 §13.11](../spec/13-components-and-wit.md)) MUST equal the world of the same name in its `wit` directory: the same interfaces, items, and types. A difference is `HYBRID_WIT_MISMATCH`, reported when the hybrid package is checked as the root and whenever a build uses its source form. The help suggests regenerating the WIT with `sai bindgen wit`.
3. Together with §4.5.2 rule 3, this guarantees that switching forms never changes whether a dependent type-checks. The behaviour of the two forms is the publisher's responsibility.
4. **Source form provides within the build.** A hybrid dependency in source form is compiled with its `export world` applied, and it provides the interfaces that this world exports within the build ([spec/13 §13.5.2](../spec/13-components-and-wit.md), N-D-104, N-G-60). By item 1, its dependents call these interfaces as imported interfaces of its WIT in both forms. In source form each such call is linked to the dependency's impl through Canonical ABI glue inside the root's module ([impl/07](../impl/07-component-lowering.md) §7.11.10), just as composition links it in artifact form (impl/07 §7.11.9). The call therefore keeps the semantics and the effect of a component call in both forms. A provided interface is not an import of the root's component and need not be imported by the root's export world. It is available on every target, including core targets and `tiny`. In both forms it is a required capability of the build ([spec/08 §8.12.4](../spec/08-effects-and-capabilities.md)). The interfaces that the dependency's own reachable code imports become imports of the root's component (`WORLD_IMPORT_UNDECLARED`, spec/13 §13.5.2), and on a core target they are `TARGET_CAPABILITY_UNAVAILABLE` (spec/13 §13.6.2 item 4). Each node of the hybrid, one per compatibility line, provides the interfaces of its own version and is checked against its own WIT by item 2. The functions of the root's allocator package (§4.5.1 item 6) are not interfaces; they are linked as functions ([impl/10 §10.2.7](../impl/10-runtime.md)).
5. **Provided interfaces stay inside the component.** An interface that a dependency provides, within the build or by composition, MUST NOT cross the boundary of the root's component ([spec/13 §13.5.2](../spec/13-components-and-wit.md), N-D-105, N-G-61). Each of the following is the error `PROVIDED_INTERFACE_AT_BOUNDARY`, reported once per interface and case in every build whose artifact is a component, together with `WORLD_IMPORT_UNDECLARED` ([impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.9):
   1. the root's export world exports the interface, or a type of the interface occurs in a boundary type of an export of the root's export world;
   2. a type of the interface occurs in a boundary type of an interface that remains an import of the component;
   3. the interface is provided within the build, and a package used in artifact form imports it. The help suggests `overrides { <id> use artifact }` for the providing hybrid when that override is possible (§4.5.1), so that one composed instance serves every importer.

---

## 4.6 Integrity failures and withdrawn versions

### 4.6.1 Integrity failures

Content that does not match the lock is rejected (D-PKG-03, draft §73). The lock is never updated to match fetched content ([03 §3.7.3](03-lock-file.md)). The possible observations are:

| Observation | Code | Effect |
|---|---|---|
| a release tag of a locked version peels to another commit | `TAG_MOVED` | error; nothing cached, lock unchanged (§4.3.3) |
| the content of a locked commit does not match a checksum | `CHECKSUM_MISMATCH` | error; content discarded ([03 §3.7.3](03-lock-file.md)) |
| a cached entry does not match its checksum | `CACHE_CORRUPT` | warning; entry removed and fetched again (§4.4.2) |
| the release tag of a locked version is gone, the commit is available | `TAG_MISSING` | warning; the locked content is used (§4.3.3) |
| the release tag is gone and the commit is unavailable | `VERSION_NOT_FOUND` | error ([02 §2.6.2](02-resolver.md)) |
| the repository cannot be reached | `FETCH_FAILED` | error, unless the content is cached |
| an artifact is not a component or does not conform to its world | `INVALID_ARTIFACT`, `ARTIFACT_WIT_MISMATCH` | error (§4.5.2) |

Every one of these diagnostics names the package, the version, the canonical URL, and, where it applies, the tag, the commits, and the digests involved. Each is a resolution or fetch error of that package in the sense of the cascade rule C6 of [impl/11](../impl/11-diagnostics.md): diagnostics about uses of the package are suppressed.

### 4.6.2 No yanking

v0.1 has no mechanism to withdraw ("yank") a published version (N-G-53).

1. There is no registry that could carry yank metadata (O-04). A Git-based signal (a special ref or file) would make resolution depend on mutable remote state, which MVS deliberately avoids (D-PKG-02, P8).
2. MVS never selects a version above the highest minimum that some requirement names ([02 §2.4](02-resolver.md)). A bad version therefore enters a graph only through a requirement that names it or a higher one, never by accident of publication.
3. A broken or vulnerable release is fixed by publishing a new version. Dependents move to it with `sai update`, and libraries raise their minimum requirement so that their dependents cannot select the broken version.
4. Publishers MUST NOT delete or move release tags to withdraw a version (§4.3.3). Deleting a tag breaks every resolution that has not locked the version, and moving it is reported as `TAG_MOVED` to everyone who has.
5. Security advisories are future work (§4.12).

---

## 4.7 Review and approval

### 4.7.1 The review

A **review** describes one package version as a dependency (draft §75). `sai add`, `sai remove`, `sai update`, and `sai lock` show a review for every node that the resolution adds, or whose version, source, or content it changes; other commands show it only for nodes with an item that needs approval ([impl/13 §13.6.8](../impl/13-cli.md)). A review consists of these headings, in this order (N-G-54):

| Heading | JSON key | Value |
|---|---|---|
| `Package` | `package` | `<id>@<version>`; for a changed node `<id>@<old> -> <new>` |
| `Source` | `source` | `git "<canonical url>"`, followed by ` dir "<d>"` if present, or `path "<p>"` |
| `Form` | `form` | the package kind: `source`, `artifact`, or `hybrid` ([01 §1.1.2](01-package-contract.md)) |
| `Capabilities` | `capabilities` | the footprint: the source footprint united with the artifact imports for the root's targets, sorted by bytes ([spec/08 §8.12.8](../spec/08-effects-and-capabilities.md)) |
| `Supported` | `supported` | the root's targets that the package lists, sorted by bytes |
| `Not supported` | `not_supported` | the root's targets that the package does not list, sorted by bytes |
| `Estimated contribution` | `estimated_contribution_bytes` | the size of the largest artifact used in artifact form for one of the root's targets with the `release` profile (§4.5.1); for packages used only in source form there is no estimate (JSON `null`, human `unknown`), because their contribution depends on reachability and is reported by `sai size` |
| `Needs approval` | `needs_approval` | the codes of the items of §4.7.2, sorted by bytes |

1. In human format, a list is printed one value per line, and an empty list as the single value `none` ([impl/13 §13.6.8](../impl/13-cli.md)). Sizes are printed in the largest of `B`, `KiB`, `MiB` in which the value is at least 1, rounded up to an integer (`612 KiB`).
2. For a changed node, a capability that escalates ([02 §2.12.3](02-resolver.md)) is followed by ` (new)`, and a capability that disappeared is listed with ` (removed)`. In JSON, the additional keys `capabilities_added` and `capabilities_removed` carry the same lists.
3. For a changed node, the review states whether the WIT checksum changed ([03 §3.5](03-lock-file.md)): in human format as the line `interface unchanged` or `interface changed` below `Form`, in JSON as the boolean `interface_changed` (absent for new nodes and for packages without `wit`).
4. Review items that do not need approval are reported as diagnostics attached to the review:
   - `FOOTPRINT_EXCEEDS_POLICY` (info): the footprint contains capabilities that the package's own policy, or the root's package-level policy, does not allow ([02 §2.12.1](02-resolver.md) rule 3; resolution has no profile, [targets](../targets/targets-and-profiles.md) §5.5 item 6). The build rejects them only if they are reachable.
   - `FOOTPRINT_NOT_GRANTED` (info): the footprint of the package, or of a package reachable from it, is not covered by the `grant` of an edge to it ([01 §1.10.8](01-package-contract.md)).
   - `CONFUSABLE_PACKAGE_NAME` (warning): §4.11.1.
   - `DUPLICATE_PACKAGE_LINE` (lint, [02 §2.9](02-resolver.md)): the node is a second line of a package in the graph.
5. The example of draft §75, corrected to this format, is in §4.7.3.

### 4.7.2 Approval

Some changes to the lock mean that code which nobody has reviewed for this package may now run with capabilities that nobody has accepted. They are **approval items** (N-G-55). Each has a code, which is listed under `Needs approval` in the review of the node and is the error reported when approval is refused:

| Item | Code | Condition |
|---|---|---|
| capability escalation | `CAPABILITY_ESCALATION` | the node escalates as defined in [02 §2.12.3](02-resolver.md) rules 1–2: a capability of the new footprint is not covered by the old footprint, or the node is new and its footprint is not empty |
| source change | `SOURCE_CHANGED` | the node is in both the old and the new lock (same identity and line) with a Git source in both, and the canonical URL (§4.2.2) or the `dir` differs |
| content replacement | `TAG_MOVED` | `sai update` keeps the version of a node whose release tag has moved and would record the tag's new commit (§4.3.3 rule 4) |

1. **Source changes.** The review of a source change shows the old and the new source and states whether the tree checksum is unchanged ([03 §3.4](03-lock-file.md)). An unchanged checksum means the new source serves exactly the reviewed content, as a faithful mirror does; it still needs approval, because the new source decides the content of every later version of the node. A change from a Git source to a path source needs no approval: path packages are the user's own working copies and are not reviewed (N-G-44, [03 §3.3.3](03-lock-file.md)). A change from a path source to a Git source is treated as a new node for rule 2 of 02 §2.12.3, because the path entry has no recorded footprint.
2. **When approval is asked.** Approval items are computed only when a command would write a lock that differs from the existing one (02 §2.12.3). A build in Mode A, and every command under `--locked` or `--frozen`, never needs approval, because it writes nothing. When there is no previous lock, there are no approval items (02 §2.12.3 rule 6); the summary of all footprints is printed instead.
3. **How approval is given.** Approval is given by the user, never by a package: with `--approve`, or by answering the prompt of an interactive command ([impl/13 §13.4.6](../impl/13-cli.md)). One answer approves every item of the command; there is no partial approval, because a partial lock would be a different resolution than the one reviewed.
4. **Refusal.** A non-interactive command without `--approve`, or an interactive command whose user refuses, reports every item as an error with its code, writes neither `sai.package` nor `sai.lock`, and fails ([02 §2.12.3](02-resolver.md) rule 4). The help of `SOURCE_CHANGED` names both sources; the help of `TAG_MOVED` repeats the advice of §4.3.3 rule 2.
5. **Record.** The written lock is the record of approval: every capability in the footprint of an entry, every source, and every commit has been approved for that node (02 §2.12.3 rule 5). A reviewer of a change to `sai.lock` in version control sees the same information in the diff of the entries' `source`, `commit`, and `capabilities` fields ([03 §3.3.3](03-lock-file.md)).
6. Removing nodes, removing capabilities, lowering versions, and changes of the WIT checksum alone never need approval. A changed interface is shown in the review (§4.7.1 rule 3) and checked by the build.

### 4.7.3 The draft §75 example, corrected

Draft §75 shows the review printed by `sai add https://github.com/acme/sqlite`. In this format, for a root package whose `targets` are `wasm32-core`, `wasm32-wasi`, `wasm64-core`, and `wasm64-wasi`, and which already has a `sai.lock` that does not contain `acme:sqlite`, the review is:

```text
Package
  acme:sqlite@3.1.0

Source
  git "https://github.com/acme/sqlite"

Form
  artifact

Capabilities
  alloc
  wasi:filesystem

Supported
  wasm32-wasi
  wasm64-wasi

Not supported
  wasm32-core
  wasm64-core

Estimated contribution
  612 KiB

Needs approval
  CAPABILITY_ESCALATION
```

It is followed by the prompt `approve these changes? [y/N] ` of [impl/13 §13.4.6](../impl/13-cli.md). The same review in JSON:

```json
{"type":"result","kind":"review","data":{"package":"acme:sqlite@3.1.0","source":"git \"https://github.com/acme/sqlite\"","form":"artifact","capabilities":["alloc","wasi:filesystem"],"supported":["wasm32-wasi","wasm64-wasi"],"not_supported":["wasm32-core","wasm64-core"],"estimated_contribution_bytes":626688,"needs_approval":["CAPABILITY_ESCALATION"]}}
```

The corrections against the draft:

| Draft | This format | Reason |
|---|---|---|
| `Package` | `Package` | unchanged |
| (absent) | `Source` | an identity is not a location (D-PKG-01); the reviewer must see where the code comes from, and a source change is an approval item (§4.7.2) |
| `Artifact` / `Wasm Component` | `Form` / `artifact` | an artifact is always a Wasm component ([01 §1.7](01-package-contract.md)); the heading must also distinguish `source` and `hybrid` packages |
| `Capabilities` | `Capabilities` | the footprint of [spec/08 §8.12.8](../spec/08-effects-and-capabilities.md); for an artifact, all of its imports |
| `Supported`, `Not supported` | the same headings | relative to the root's targets; the draft listed only one unsupported target |
| `Estimated contribution` | `Estimated contribution` | defined for packages used in artifact form; `unknown` otherwise (§4.7.1) |
| (absent) | `Needs approval` | approval items of §4.7.2, so that a non-interactive caller can decide from the record alone |

Notes on the example:

1. `acme:sqlite` is new and its footprint is not empty, so it escalates ([02 §2.12.3](02-resolver.md) rule 2). In a package without a previous `sai.lock`, `Needs approval` would be `none` (§4.7.2 rule 2).
2. `wasm32-core` and `wasm64-core` are not supported, and cannot be: an artifact cannot be used on a core target (`ARTIFACT_ON_CORE_TARGET`, [01 §1.7](01-package-contract.md)). Unless the requirement is restricted with `only { wasm32-wasi, wasm64-wasi }` ([01 §1.10.5](01-package-contract.md)), the root's builds for the core targets fail with `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` ([02 §2.5.3](02-resolver.md)).
3. `612 KiB` is 626,688 bytes: the size of the larger of the package's two artifact files, which are used in artifact form for `wasm32-wasi` and `wasm64-wasi` under the `release` profile (§4.5.1).

---

## 4.8 Reproducible builds

Draft §72 states the goal: the same package sources, `sai.package`, `sai.lock`, compiler version, target, and profile regenerate an equivalent artifact. Output determinism of the compiler is owned by [impl/13 §13.5.3](../impl/13-cli.md) (N-H-29). This section defines the package system's part: every input that comes from a dependency is fixed by the lock and is the same on every host.

1. **Contents.** For a lock entry with a Git source, every host obtains byte-identical files: the locked commit, the files selected by [03 §3.4.2](03-lock-file.md), and nothing else. Checkout is not influenced by Git configuration, attributes, line-ending conversion, filters, or LFS (§4.3.4). File modes and timestamps are not part of package content and are never read by the toolchain.
2. **What reaches the compiler** (N-G-56). The package system passes to the compiler, for each active node, only its identity, its version, its verified files, and its chosen form (§4.5.1). The source URL, the commit, the cache location, and the order in which contents were fetched are not passed. Consequently, a mirror that serves identical content (equal tree checksum) produces identical output, and moving the cache changes nothing.
3. **Path packages** are not pinned by the lock (N-G-44). A path package in the same repository as the root is versioned with it and is reproducible to the same degree. A path to a directory outside the repository, including a path override ([01 §1.16](01-package-contract.md)), makes the build depend on that directory. Release builds SHOULD NOT use path packages or path overrides outside the root's repository; `sai inspect` shows the source of every node ([impl/13](../impl/13-cli.md)).
4. **The toolchain version** is not recorded in the lock ([03 §3.3](03-lock-file.md)). The `compiler` field of `sai.package` ([01 §1.5](01-package-contract.md)) states which versions are accepted, not which one is used. Projects that need byte-identical outputs SHOULD pin the toolchain version in their build environment, for example in CI configuration. The bundled standard packages (§4.4.3) do not make the graph depend on the toolchain: they are used only when they match the lock, and are fetched otherwise.
5. **Resolution without a lock** depends on the release tags at the time of the fetch ([02 §2.14.1](02-resolver.md)). Applications SHOULD commit `sai.lock` and build with `--locked` in CI ([03 §3.8.1](03-lock-file.md)). Libraries SHOULD commit their lock as well; it is never read when the library is a dependency ([02 §2.10.1](02-resolver.md)), and it makes the library's own tests reproducible.
6. **Artifacts.** SAI verifies the interface of an artifact, not its provenance (§4.5.2). Publishers of hybrid packages SHOULD build each artifact with the SAI toolchain from the package's own source at the tagged commit, with the `release` profile, and SHOULD state the toolchain version in the package's documentation, so that anyone can rebuild it and compare the bytes (impl/13 §13.5.3 makes the output byte-identical for equal inputs). Checking such claims automatically is future work (§4.12).
7. **No scripts.** No code of any package runs during resolution or building (§4.10, draft §72), so no host tool, environment, or time can enter the output through a package.

---

## 4.9 Offline operation

The flags are defined in [03 §3.8](03-lock-file.md): `--offline` forbids network access, `--locked` forbids writing the lock, and `--frozen` is both. This section defines what network access is and what is available without it.

1. **Network access of the toolchain** (N-G-57). The Git operations of §4.3 (listing tags, fetching commits) are the only network access the toolchain makes on its own. It never checks for updates, sends telemetry, or downloads toolchain components. A program that the toolchain runs (`sai run`, `sai test`) has only the network authority that [impl/13 §13.6.5](../impl/13-cli.md) and [impl/13 §13.6.6](../impl/13-cli.md) give it; that is the program's access, not the toolchain's.
2. **Meaning of offline.** Under `--offline` the toolchain opens no network connection of any kind: no DNS lookup, no TCP or UDP connection, and no connection to a proxy. A Git URL with a host name is not resolved.
3. **What is available offline:** the cache's `src/` entries, the tag records, and the bundled standard packages with their bundled index (§4.4.1, §4.4.3).
4. **What works offline:**
   - building, checking, and testing with a lock whose active nodes ([02 §2.5.3](02-resolver.md)) are all cached;
   - `sai lock` and resolution in Mode B, if the release tag of every named version is known from a tag record or the bundled index and its content is cached ([03 §3.8.2](03-lock-file.md) rule 3);
   - `sai remove`, with the same condition for the resolution that follows;
   - `sai update <id>@<w>`, if everything needed is cached ([02 §2.10.5](02-resolver.md) rule 7);
   - `sai fetch --offline`, which verifies the cache (§4.4.2 rule 3).
5. **What fails offline** with `OFFLINE_FETCH_REQUIRED`: listing tags (`sai add`, and `sai update` without an exact version), and every content or tag that is not cached. The diagnostic names each missing node with its canonical URL and, if known, its commit. Offline resolution never selects a different version because of what is cached (P8): it produces the online result or fails.
6. **Preparing for offline work.** `sai fetch` ([impl/13 §13.6.13](../impl/13-cli.md)) downloads the content of every Git entry of the lock, for every target, including test-only entries. After it succeeds, every command that does not need tag listing works with `--frozen`. Air-gapped environments can copy a cache directory, or point `SAI_CACHE_DIR` at a prepared one; cached entries are content-addressed and are used only when their checksums are those of the lock.

---

## 4.10 No build scripts

SAI has no build scripts. **No code from a package runs while that package is resolved, fetched, or compiled** (N-G-58). This holds for the root, for dependencies, and for the toolchain's bundled packages, and it extends the rule of D-PKG-02 and draft §72 from resolution to the whole build.

1. **`sai.package` is declarative.** No field names a program, a command, or a script to run ([01](01-package-contract.md)). Unknown fields are errors, not extension points.
2. **No macros and no compiler plugins.** SAI has no macros, and its attribute set is closed (D-SYN-09). A package cannot extend the compiler.
3. **No general compile-time execution.** There are no `const fn`s (D-EXPR-07), and constant expressions have no effects ([spec/08](../spec/08-effects-and-capabilities.md) N-D-8). Compile-time evaluation is limited to the constant expressions of the language and cannot call package functions, access the host, or loop without bound.
4. **No hooks.** There are no install, fetch, or post-checkout hooks. Git hooks, submodules, LFS, filters, and attributes are disabled (§4.3.4; D-TOOL-05: the system `git` is never invoked).
5. **No package-run code generation.** WIT bindings are produced by the compiler from the package's WIT files ([spec/13 §13.7.3](../spec/13-components-and-wit.md), [spec/13 §13.11](../spec/13-components-and-wit.md)); WIT files are data. A package whose source is generated by an external tool commits the generated files, which are then hashed and reviewed like any other file.
6. **Artifacts are not executed by the toolchain.** They are parsed, validated (§4.5.2), and composed ([impl/07](../impl/07-component-lowering.md)). Their code runs only as part of a program, with the capabilities checked by spec/08.
7. **Native code** is supplied as a Wasm component (§4.5), never as a host library built or loaded by the toolchain.
8. **Dependency tests are not run.** Building commands never run a dependency's tests. `sai test` runs the tests of the root or of selected workspace members ([impl/13](../impl/13-cli.md)).

Consequences:

- Resolving and building an untrusted package can consume resources, within the limits of §4.4.4, and produce outputs. It cannot read or write files outside the cache and `out/`, open connections, or run processes on the host.
- Builds need no host tools: no shell, no C compiler, and no Git installation (D-TOOL-05). They are the same on every host (P8).
- The remaining risk of a dependency is its behaviour when the program runs. That risk is bounded by the capabilities of the program, which are reviewed when the dependency is added (§4.7) and checked at every build ([spec/08 §8.12.6](../spec/08-effects-and-capabilities.md)).

---

## 4.11 Supply-chain threats

This section lists the threats that the package system considers, how each is mitigated, and what risk remains. It is the reference for the trust model of §4.1.2.

| Threat | Mitigation | Remaining risk |
|---|---|---|
| **Typosquatting**: a package named like a popular one | every requirement names its source URL explicitly; there is no default source and no name lookup (N-G-9, D-PKG-01); the review shows the source (§4.7.1); confusable identities are reported (§4.11.1) | a user who copies a wrong URL and ignores the warning |
| **Dependency confusion**: a public package shadows a private one of the same name | identities are not resolved through any search path; one source per node (N-G-9, [01 §1.10.4](01-package-contract.md)); divergent sources follow fixed rules ([02 §2.8.3](02-resolver.md)); a declared identity that differs from the requested one is `PACKAGE_IDENTITY_MISMATCH` ([02 §2.6.2](02-resolver.md)) | none from naming alone |
| **Tag rewriting**: a maintainer or attacker moves a release tag | locked versions are pinned by commit and checksum; a moved tag is `TAG_MOVED` (§4.3.3); fetching a locked commit always checks its tag | the first fetch of a version trusts the tag (trust on first use, §4.1.2) |
| **Repository takeover**: an attacker gains push access | existing locked versions are unaffected; new versions enter only through `sai add`, `sai update`, or a changed requirement, with review; new capabilities need approval (§4.7.2) | malicious code within capabilities already approved for the package |
| **Repository deletion** | cached content stays usable; a deleted tag of a locked version is `TAG_MISSING` if the commit is still served (§4.3.3); a mirror can be named in `overrides` | fresh machines cannot fetch content that no reachable source serves |
| **Tampered lock file**: a change to `sai.lock` points an entry to other content | the diff shows `source`, `commit`, checksums, and `capabilities` ([03 §3.3.3](03-lock-file.md)); when the commit is fetched, an existing release tag must peel to it (`TAG_MOVED`, §4.3.2) | a change accepted without review: content already in the reviewer's cache is not re-fetched, and an entry for a version without a release tag is used with only `TAG_MISSING` (§4.3.3 rule 3) |
| **Malicious artifact** | checksum, validity, and world conformance are verified; its imports are its capabilities and are always reachable (§4.5.2); hybrid packages default to source form (§4.5.1) | behaviour within the imported capabilities; that the artifact was not built from the source (§4.8 rule 6) |
| **Malicious source code** | capability checks at every build ([spec/08 §8.12.6](../spec/08-effects-and-capabilities.md)); `grant` limits per edge ([01 §1.10.8](01-package-contract.md)); the review shows the footprint | misuse of allowed capabilities (§4.1.2) |
| **Build-time attacks**: scripts, macros, plugins, hooks | none exist (§4.10) | bugs in the toolchain |
| **Trojan Source**: invisible or bidirectional characters | file paths exclude control and bidirectional control characters ([03 §3.4.3](03-lock-file.md)); source text excludes them, including inside comments and strings ([spec/02](../spec/02-lexical-structure.md), `FORBIDDEN_CHARACTER`); identities are lowercase ASCII ([01 §1.4](01-package-contract.md)) | homoglyphs in identifiers and string contents allowed by spec/02 |
| **Path attacks**: `..`, absolute paths, symbolic links, case collisions, device names | path validation and file selection ([03 §3.4.2](03-lock-file.md), [03 §3.4.3](03-lock-file.md), [03 §3.4.4](03-lock-file.md)); materialization only below the cache (§4.3.2) | none |
| **Resource exhaustion**: huge repositories, many tags, packing bombs | the limits of §4.4.4 (`PACKAGE_TOO_LARGE`) | consumption up to the limits |
| **Network attacker** | HTTPS and SSH only, verified host keys, TLS that cannot be disabled (§4.2); checksums verify locked content independently of the transport | the first fetch over a compromised trusted host |
| **Credential leakage** | credentials are never part of a URL (`CREDENTIALS_IN_URL`, §4.2.1); locks and diagnostics contain only canonical URLs; credentials come from the SSH agent or credential helpers (§4.2.3) | compromise of the credential store itself |
| **Cache tampering** by a local user | entries are read-only and content-addressed; `sai fetch` re-hashes them (§4.4.2) | a compromised host or cache is outside the trust model (§4.1.2) |

**Mirrors.** A root can replace the source of any node with `overrides { <id> use git "<url>" }` ([01 §1.16](01-package-contract.md)). This is the mechanism for internal mirrors, for forks that carry a fix, and for a source that survived a deletion. The switch is a source change and needs approval (§4.7.2); the review states whether the mirror serves identical content.

### 4.11.1 Confusable package names

When a review contains a node that is new in the lock, its identity `a` is compared with every other identity `b` of the new graph, of the toolchain's bundled packages (§4.4.3), and with the root's own identity (N-G-59). The pair is **confusable** if `a ≠ b` and one of the following holds:

1. `skel(a) = skel(b)`, where `skel` of an identity is computed by deleting every `-`, then replacing every `0` by `o` and every `1` by `l`, then replacing every occurrence of `rn` by `m` and of `vv` by `w`, scanning left to right without overlap. For example, `acme:json-parser`, `acme:jsonparser`, and `acme:js0nparser` have equal skeletons, and so do `acrne:json` and `acme:json`.
2. The Levenshtein distance between `a` and `b`, as byte strings including the namespace and the `:`, is 1, and both names (the part after `:`) are at least 4 bytes long. For example, `sai:colections` and `sai:collections`, or `acne:json` and `acme:json`.

For each confusable pair, the warning `CONFUSABLE_PACKAGE_NAME` is attached to the review of `a`. It names both identities and their sources, and states which rule matched. Pairs whose identities are both in the old lock are not reported again. The warning never blocks the command and is not an approval item: an intended dependency with a similar name is common (`acme:image` and `acme:images`), and the source in the review decides.

`sai add` performs the same comparison for the identity it is about to add ([02 §2.11](02-resolver.md) rule 5). Identities are compared only with the graph and the bundled set; there is no list of popular packages, because there is no registry (O-04).

---

## 4.12 Future work: registries and signing (informative)

This section is informative. Registries and package signing are future work (O-04; draft §73 anticipates package signing). Nothing in v0.1 depends on them, and their absence is a deliberate decision, not a gap in this specification: Git sources with locked checksums cover the needs of v0.1, and every extension below can be added without changing the meaning of existing `sai.package` and `sai.lock` files.

- **Registries.** A future source kind such as `from registry "<name>"` would map identities to sources and carry metadata that Git cannot: yanking (§4.6.2), deprecation notices, and advisories. MVS would not change ([02 §2.4](02-resolver.md)): a registry would only answer the questions that release tags answer today. A new source kind needs a new lock format version ([03 §3.10](03-lock-file.md)).
- **Signatures.** A signature would cover the identity, the version, and the `sai1:` source checksum of a release ([03 §3.4](03-lock-file.md)), not the Git commit, so that it stays valid across mirrors and is independent of Git's hash function. Keys would be bound to namespaces. Verifying a signature on the first fetch would remove the trust-on-first-use limitation of §4.1.2. Signatures on Git tag objects are not verified in v0.1 (§4.3.2 step 3).
- **Checksum transparency.** A public, append-only log of observed `(canonical URL, tag, commit, checksum)` records would let a first fetch detect a tag that was moved before the user ever locked it, and would make targeted attacks visible.
- **Advisories.** A database of known-vulnerable versions would be checked against the lock by a dedicated command, and reported as warnings during resolution.
- **Artifact provenance.** An attestation would link an artifact checksum to the source checksum, the toolchain version, and the profile it was built with (§4.8 rule 6), so that the toolchain could rebuild it and compare.

---

## 4.13 Decisions introduced by this chapter

Each entry is provisional until it is merged into [decisions.md](../design/decisions.md).

| ID | Rule | Rationale |
|---|---|---|
| N-G-47 | Git URLs are `https` or `ssh` only; `http` and `git` are `INSECURE_GIT_URL`; scp-like, `file`, local paths, other schemes, non-ASCII hosts, percent-encoding, queries, and fragments are `UNSUPPORTED_GIT_URL`; credentials in URLs are `CREDENTIALS_IN_URL`; the canonical form lowercases scheme and host, drops a default port and a trailing `/`, and changes nothing else (§4.2). | P5: only authenticated transports; P8: one spelling per source in locks and cache keys; P9: no guessing about host-specific URL equivalence. |
| N-G-48 | Git transport details (protocol version, depth, filters, negotiation) are deliberately left to the implementation; only the Git configuration of §4.2.3 is honored (§4.3.1). | The result is fixed by object ids and lock checksums, so the freedom cannot affect output (P8); ignoring `insteadOf` and similar settings keeps sources explicit (P1). |
| N-G-49 | The lock is authoritative after the first fetch: a release tag that no longer peels to the locked commit is `TAG_MOVED`; a deleted tag with an available commit is `TAG_MISSING`; replacing locked content under the same version needs approval in `sai update` (§4.3.3). | P5, P8: published versions are immutable; rewriting is visible and never silently accepted. |
| N-G-50 | One global, content-addressed package cache per user, located by `SAI_CACHE_DIR` or the host's cache directory, with a versioned layout; `src/` and tag records are normative, other internal formats are deliberately private (§4.4.1). | P8: content-addressing makes sharing safe; a private internal format lets implementations evolve without breaking users. |
| N-G-51 | Fixed, non-configurable size limits for fetched content: 100,000 files, 256 MiB per file, 1 GiB per package, 4 GiB per fetch, 4096/255-byte paths, 100,000 tags (`PACKAGE_TOO_LARGE`, §4.4.4). | P8: a package accepted on one host is accepted on all; protection against resource exhaustion (P5). |
| N-G-52 | Dependents see artifact and hybrid packages only through the SAI view of their WIT, in both forms; the WIT generated from a hybrid package's source MUST equal its declared world (`HYBRID_WIT_MISMATCH`, §4.5.3). | P4, P8: switching forms under D-PKG-05 never changes whether a dependent type-checks. |
| N-G-53 | There is no yanking in v0.1; broken releases are superseded by new versions; tags MUST NOT be deleted or moved (§4.6.2). | P8: resolution never depends on mutable remote metadata; MVS never picks a version nobody named. |
| N-G-54 | The review has the headings Package, Source, Form, Capabilities, Supported, Not supported, Estimated contribution, Needs approval, in this order, with fixed JSON keys (§4.7.1). | P1, P5: costs and authority of a dependency are visible before it is accepted (draft §75); a fixed schema serves agents and CI (P8). |
| N-G-55 | Approval items are `CAPABILITY_ESCALATION`, `SOURCE_CHANGED` (a different canonical URL or `dir` for a Git node in both locks), and `TAG_MOVED` content replacement; approval is all-or-nothing per command and recorded by the written lock (§4.7.2). | P5: new authority and new provenance need a human decision; P8: a partially approved lock would be a resolution nobody reviewed. |
| N-G-56 | The package system passes to the compiler only each node's identity, version, verified files, and form; URLs, commits, and the cache location are not compiler inputs (§4.8). | P8: mirrors with identical content and moved caches give identical outputs. |
| N-G-57 | Git fetching is the toolchain's only network access; there are no update checks or telemetry; `--offline` means no DNS lookup and no connection of any kind (§4.9). | P5: the toolchain itself uses no ambient authority beyond what the user asked for; P8: offline results equal online results or fail. |
| N-G-58 | No code from a package runs while it is resolved, fetched, or compiled: no build scripts, macros, plugins, hooks, package-run code generation, or execution of artifacts; building commands never run dependency tests (§4.10). | P5, P8: building untrusted code is safe and host-independent; extends D-PKG-02 and draft §72 from resolution to the whole build. |
| N-G-59 | A node new in a review is checked for confusable identities against the graph, the bundled packages, and the root by a fixed skeleton rule and a Levenshtein-1 rule; matches are the warning `CONFUSABLE_PACKAGE_NAME` and never block (§4.11.1). | P5: typosquatting is made visible; P8: a fixed, deterministic rule instead of a heuristic list; P9: no registry is needed. |
| N-G-60 | A hybrid dependency in source form provides the interfaces of its `export world` within the build, through Canonical ABI glue inside the root's module, on every target and profile; provided interfaces are not imports of the root's component but remain required capabilities, and each compatibility line provides its own version (§4.5.3 item 4). | D-PKG-05, P8: both forms of a hybrid give a working build with the same call semantics and effects, also under `tiny` and on core targets where composition is unavailable; P5: the form never hides authority. |
| N-G-61 | An interface provided by a dependency never crosses the boundary of the root's component: it is not exported, its types are in no boundary type of an export or remaining import, and an interface provided within the build is not imported by an artifact (`PROVIDED_INTERFACE_AT_BOUNDARY`, §4.5.3 item 5). | P4: the Component Model requires the named types of a component's imports and exports to be imported or exported; P8: one provider instance per interface, so the result does not depend on the form. |

---

## 4.14 Diagnostics

| Code | Severity | Description |
|---|---|---|
| `INSECURE_GIT_URL` | error | a Git URL uses `http` or `git` (§4.2.1) |
| `UNSUPPORTED_GIT_URL` | error | a Git URL uses the scp-like form, `file`, a local path, another scheme, a non-ASCII host, an invalid path, a query, or a fragment (§4.2.1) |
| `CREDENTIALS_IN_URL` | error | a Git URL contains credentials (§4.2.1) |
| `INVALID_TAG` | error | a release tag does not peel to a commit (§4.3.2) |
| `TAG_MOVED` | error | a release tag of a locked version peels to another commit than the lock records, or its content replacement was not approved (§4.3.3, §4.7.2) |
| `TAG_MISSING` | warning | the release tag of a locked version no longer exists; the locked commit was used (§4.3.3) |
| `CACHE_CORRUPT` | warning | a cached entry does not match its checksum and was removed (§4.4.2) |
| `PACKAGE_TOO_LARGE` | error | fetched content exceeds a limit of §4.4.4 |
| `INVALID_ARTIFACT` | error | an artifact is not a valid Wasm component, or uses the wrong memory width (§4.5.2) |
| `ARTIFACT_WIT_MISMATCH` | error | an artifact does not conform to the package's `export world` (§4.5.2) |
| `HYBRID_WIT_MISMATCH` | error | the WIT generated from a hybrid package's source differs from its declared world (§4.5.3) |
| `PROVIDED_INTERFACE_AT_BOUNDARY` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.2; the cases of §4.5.3 item 5, each reported once per interface and case in every build whose artifact is a component |
| `SOURCE_CHANGED` | error | a lock change moves a node to another source and was not approved (§4.7.2) |
| `FOOTPRINT_EXCEEDS_POLICY` | info | a reviewed footprint contains capabilities that the package's or the root's policy does not allow (§4.7.1) |
| `FOOTPRINT_NOT_GRANTED` | info | a reviewed footprint is not covered by the `grant` of an edge (§4.7.1) |
| `CONFUSABLE_PACKAGE_NAME` | warning | a new identity is confusable with another identity of the graph, the bundled packages, or the root (§4.11.1) |
| `VERSION_NOT_FOUND` | error | defined in [02](02-resolver.md) §2.11; the release tag is gone and the locked commit is unavailable (§4.3.3) |
| `FETCH_FAILED` | error | defined in [02](02-resolver.md) §2.6.2; the repository cannot be reached, or a host key is unknown or changed (§4.2.3) |
| `MISSING_PACKAGE_FILE` | error | defined in [02](02-resolver.md) §2.6.2; see §4.3.2 |
| `PACKAGE_IDENTITY_MISMATCH` | error | defined in [02](02-resolver.md) §2.6.2; see §4.11 |
| `CAPABILITY_ESCALATION` | error | defined in [02](02-resolver.md) §2.12.3; an approval item (§4.7.2) |
| `CHECKSUM_MISMATCH` | error | defined in [03](03-lock-file.md) §3.7.3; see §4.3.2, §4.5.2, §4.6.1 |
| `OFFLINE_FETCH_REQUIRED` | error | defined in [03](03-lock-file.md) §3.8.2; see §4.4.2, §4.9 |
| `INVALID_PACKAGE_FILE_PATH` | error | defined in [03](03-lock-file.md) §3.4.3; see §4.3.2 |
| `PATH_COLLISION` | error | defined in [03](03-lock-file.md) §3.4.4; see §4.3.2 |
| `SUBMODULE_IGNORED` | warning | defined in [03](03-lock-file.md) §3.4.2; see §4.3.4 |
| `LOCK_CORRUPT` | error | defined in [03](03-lock-file.md) §3.3.5; an invalid URL in `sai.lock` (§4.2.1) |
| `ARTIFACT_ON_CORE_TARGET` | error | defined in [01-package-contract.md](01-package-contract.md) §1.7 item 2, for every target that is not a component target ([targets](../targets/targets-and-profiles.md) §1.4 item 6); an artifact package can list only component targets (§4.5.1, §4.7.3) |
| `ALLOCATOR_INTERFACE_MISMATCH` | error | defined in [impl/10](../impl/10-runtime.md) §10.2.7; among other cases, an artifact package named by the root's `allocator` (§4.5.1 item 6) |
| `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` | error | defined in [01-package-contract.md](01-package-contract.md) §1.8; a build for a core or native target that activates an artifact package (§4.5.1, §4.7.3) |
| `DUPLICATE_PACKAGE_LINE` | warning (lint) | defined in [02-resolver.md](02-resolver.md) §2.9; shown in the review (§4.7.1) |
| `CAPABILITY_NOT_ALLOWED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; also for artifact imports (§4.5.2) |
| `CAPABILITY_DENIED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; also for artifact imports (§4.5.2) |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; also for artifact imports (§4.5.2) |
| `FORBIDDEN_CHARACTER` | error | defined in [spec/02](../spec/02-lexical-structure.md) §2.1; see §4.11 |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; `artifact` before Stage 4 (§4.5) |
