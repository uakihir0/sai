# Targets and Profiles

**Status:** Normative

**Related:** [decisions.md](../design/decisions.md) (D-TGT-01, D-TGT-02, D-CAP-01, D-CAP-02, D-WASM-01, D-WASM-02, D-WASM-05, D-EFF-03, D-ERR-01, D-THREAD-01, D-ASYNC-01, D-UNSAFE-01, D-PKG-05) · [spec/01](../spec/01-introduction.md) §1.4, §1.5 · [spec/08](../spec/08-effects-and-capabilities.md) §8.12 · [spec/11](../spec/11-async-and-concurrency.md) §11.10 · [spec/12](../spec/12-unsafe-and-ffi.md) §12.7, §12.9 · [spec/13](../spec/13-components-and-wit.md) §13.5, §13.6 · [spec/15](../spec/15-attributes-and-lints.md) §15.11 · [spec/16](../spec/16-core-library.md) §16.12.4, §16.16.3 · [package/01](../package/01-package-contract.md) §1.7, §1.8, §1.10.8, §1.11, §1.14 · [package/04](../package/04-distribution-and-security.md) §4.5.1 · [impl/06](../impl/06-wasm-lowering-and-layout.md) · [impl/07](../impl/07-component-lowering.md) · [impl/08](../impl/08-effect-and-capability-analysis.md) · [impl/09](../impl/09-optimization-and-size.md) · [impl/10](../impl/10-runtime.md) · [impl/13](../impl/13-cli.md)

This document defines the targets of SAI and their properties (address width, memory, Wasm features, WASI version, the native targets of Stage 7), the authoritative table of the capabilities that each target provides, the entry points of every kind of artifact, and the three build profiles `dev`, `release` and `tiny`, including the effective capability policy of a build. It is the "targets doc" that [spec/08](../spec/08-effects-and-capabilities.md), [spec/13](../spec/13-components-and-wit.md), [package/01](../package/01-package-contract.md) and the implementation chapters refer to. It does not define types (spec/05), the effect system and the capability checks themselves (spec/08), panics (spec/10), async (spec/11), the foreign function interfaces (spec/12), components and worlds (spec/13), the core library (spec/16), the syntax of `sai.package` (package/01), or the command line (impl/13). Where this document states a consequence of those chapters, it cites them; the rules it adds are listed in §6.

---

## 1. Scope and conformance

### 1.1 What this document fixes

For every target, this document fixes:

1. the **kind** of the target (core, WASI, or native) and the kind of artifact each building command produces for it (§2.1, §2.3);
2. the **address width**: the pointer size `PS`, the width of `USize` and `ISize`, and the Wasm memory type (§2.1);
3. the **Wasm feature set** that the output may use and that a host must support (§2.4);
4. the **WASI version** of WASI targets (§2.5);
5. the **bounds** within which a compiler chooses the shadow stack size and the initial and maximum linear memory ([spec/01](../spec/01-introduction.md) §1.5.2 item 3; §2.6);
6. the **target capabilities** that a build for the target provides ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.7; §3);
7. the **entry points** of the artifact (§4).

For every profile, it fixes the code generation choices of D-TGT-02 and where each is implemented, the capabilities the profile denies by default, and the effective policy that the capability check of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6 uses for the root package (§5).

### 1.2 Conformance

1. A conforming compiler MUST support `wasm32-core` and `wasm32-wasi`, each in the profiles `dev`, `release` and `tiny` ([spec/01](../spec/01-introduction.md) §1.4.2 item 5, N-J-1).
2. `wasm64-core` and `wasm64-wasi` are OPTIONAL. A compiler that offers one of them MUST conform on it in all three profiles, with the single exception of §2.7: component output for `wasm64-wasi` is OPTIONAL even for a compiler that offers `wasm64-wasi`.
3. The native targets of §2.8 belong to Stage 7 (feature name `native-target`, [spec/01](../spec/01-introduction.md) §1.4.5). They are OPTIONAL for v0.1 conformance.
4. Every profile is available on every target. No target restricts the profiles, and no profile restricts the targets.
5. A conformance claim names the targets it covers ([spec/01](../spec/01-introduction.md) §1.4.3), in the canonical order of §2.2.

### 1.3 Terminology

- A **target** is the platform a build produces code for. Every building command builds for exactly one target ([impl/13](../impl/13-cli.md) §13.4.1).
- A **core target** is `wasm32-core` or `wasm64-core`. A **WASI target** is `wasm32-wasi` or `wasm64-wasi`; [package/01](../package/01-package-contract.md) also calls these **component targets**. The **Wasm targets** are the core targets and the WASI targets. A **native target** is one of the targets of §2.8.
- The **artifact** of a build is the file that the build produces: a **core module**, a **component**, or, on a native target, a **native executable**. Which one a command produces is fixed by §2.3. Some capabilities depend on the artifact, not only on the target (§3.2).
- A **wasm64 target** is `wasm64-core` or `wasm64-wasi`; a **wasm32 target** is `wasm32-core` or `wasm32-wasi`.
- A **profile** is `dev`, `release` or `tiny` (D-TGT-02). Each build has exactly one profile, selected by `--profile` ([impl/13](../impl/13-cli.md) §13.4.2).
- The **package-level policy** of a package `Q` is the pair `(allow(Q), deny(Q))` of its `sai.package` ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.5). The **effective policy** of a build is the policy against which the root package is checked in that build (§5.5).
- A capability `c` is **in the WASI namespace** when it is a WIT package or interface capability whose namespace is `wasi` (`wasi:http`, `wasi:filesystem/types`).

### 1.4 Target names

1. The target names are exactly the names of the first column of the tables of §2.1 and §2.8.1. They are case-sensitive and contain `-`, so they are written as `WORD`s in `sai.package` ([package/01](../package/01-package-contract.md) §1.8) and as `STRING`s in `@target` ([spec/15](../spec/15-attributes-and-lints.md) §15.11.1).
2. A target name is valid wherever a target is named: in `targets`, in `only { … }` of a requirement, in an `artifact` entry ([package/01](../package/01-package-contract.md) §1.7, §1.8, §1.10.5), in `@target`, and as the value of `--target`. Any other name is `UNKNOWN_TARGET` (defined in [package/01](../package/01-package-contract.md)) or, on the command line, `INVALID_OPTION_VALUE` (defined in [impl/13](../impl/13-cli.md)).
3. **Every target name of this document is known to every toolchain**, including the native names of a toolchain that does not implement Stage 7 (N-F-62). Naming a target is not a use of it: a package may list a native target in `targets`, use it in `only { … }` and test it in `@target`, and the package still builds for its other targets with any toolchain.
4. **Selecting** a target that the toolchain does not implement is a use. A building command whose selected target (explicit or by default, §2.2) is a native target, on a toolchain without Stage 7, fails before analysis with `FEATURE_NOT_IMPLEMENTED` (defined in [spec/01](../spec/01-introduction.md) §1.4.5) with the data `stage = 7` and `feature = "native-target"`, reported at the target's entry in `targets`. A toolchain that does not offer an OPTIONAL Wasm target (§1.2 item 2) reports the same code with `stage = 2` and `feature` the target name for `wasm64-core`, and `stage = 4` and the target name for `wasm64-wasi`, the stages in which [impl/15](../impl/15-conformance-and-testing.md) places them. These numbers only fill the diagnostic's data; they do not make the targets required (item 2 of §1.2).
5. The diagnostics of [package/01](../package/01-package-contract.md) §1.8 apply to every target name alike: `TARGET_NOT_DECLARED` when the selected target is not in the root's `targets`, and `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` when an active dependency does not list it.
6. A native target is not a component target. An `artifact` entry for a native target is therefore rejected by [package/01](../package/01-package-contract.md) §1.7 item 2, which requires a component target; the code is `ARTIFACT_ON_CORE_TARGET` (defined in package/01), whose meaning this document extends to every target that is not a component target (§6, N-F-62).

Rationale for item 3: a package that supports a native target in a later toolchain must stay usable as a Wasm dependency in earlier toolchains (P5, P8). Rejecting the name would split the ecosystem by toolchain version for no safety benefit.

---

## 2. Targets

### 2.1 The Wasm targets

| Target | Kind | Artifact of `sai build` | `PS` | `USize`, `ISize` | Memory type | WASI | Status |
|---|---|---|---|---|---|---|---|
| `wasm32-core` | core | core module | 4 | 4 bytes | 32-bit memory | none | required |
| `wasm64-core` | core | core module | 8 | 8 bytes | 64-bit memory (memory64) | none | OPTIONAL |
| `wasm32-wasi` | WASI | component | 4 | 4 bytes | 32-bit memory | 0.2.3 | required |
| `wasm64-wasi` | WASI | component | 8 | 8 bytes | 64-bit memory (memory64) | 0.2.3 | OPTIONAL, experimental (§2.7) |

1. **Core targets** produce a freestanding core Wasm module without WASI (D-TGT-01). The module imports only the functions of `extern "wasm"` blocks and the `@panic_hook` ([spec/12](../spec/12-unsafe-and-ffi.md) §12.7, [spec/10](../spec/10-errors-and-panics.md) §10.6), and exports only what §4.5 lists.
2. **WASI targets** produce a Component Model component for WASI 0.2 (D-TGT-01). WASI preview 1 is out of scope: no WASI target produces a preview 1 module, and the toolchain uses no preview 1 adapter ([impl/07](../impl/07-component-lowering.md) §7.11.3).
3. `PS` is the size in bytes of an address, of `ptr` in [impl/06](../impl/06-wasm-lowering-and-layout.md) and of `USize` and `ISize` ([spec/05](../spec/05-types.md)). It fixes `USIZE_MAX`, `USIZE_BITS` and the other target-dependent items of [spec/16](../spec/16-core-library.md) §16.8.1. On every target `USize` has at least 32 and at most 64 bits.
4. The page size is 65536 bytes on every Wasm target. Custom page sizes are never used (§2.4).
5. Every Wasm target uses at most one linear memory, index 0, which is defined by the module, never imported and never shared ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.7).
6. The Wasm targets are little-endian, as Wasm is.
7. The properties of this table do not depend on the profile, the package or the command (N-F-63).

### 2.2 Canonical order, defaults and selection

1. The **canonical order** of the targets is `wasm32-core`, `wasm64-core`, `wasm32-wasi`, `wasm64-wasi`, followed by the native targets in the order of §2.8.1. Every list of targets that a tool prints is in this order: `sai --version --verbose` ([impl/13](../impl/13-cli.md) §13.3.7), conformance claims, and the target lists of `sai add` reports. A toolchain lists only the targets it implements.
2. The **default target** of a building command is the first entry of the root package's `targets` declaration, in the order in which it is written, not in canonical order ([impl/13](../impl/13-cli.md) §13.4.1, N-H-25). There is no host-dependent default and no environment variable that selects a target (C-06).
3. A command builds for one target. Building for several targets takes several invocations ([impl/13](../impl/13-cli.md) §13.4.1).
4. Analysis depends on the target: `@target` items ([spec/15](../spec/15-attributes-and-lints.md) §15.11), `only { … }` dependencies ([package/01](../package/01-package-contract.md) §1.10.5), `PS`, and the target check of capabilities (§3). `sai check` therefore checks one target as well, and a program can be conforming for one target and not for another ([spec/01](../spec/01-introduction.md) §1.4.1 rule 3).

### 2.3 Artifacts per command

| Command | `wasm32-core`, `wasm64-core` | `wasm32-wasi` | `wasm64-wasi` |
|---|---|---|---|
| `sai build`, `sai size` | core module `<name>.wasm` | component `<name>.wasm` | core module validated, then `FEATURE_NOT_IMPLEMENTED` (§2.7) |
| `sai component build` | core module `<name>.wasm` and the component `<name>.component.wasm` that wraps it (§4.6) | component `<name>.wasm` | as for `sai build` |
| `sai run` | `TARGET_NOT_RUNNABLE` ([impl/13](../impl/13-cli.md) §13.6.5) | component `<name>.wasm`, run as a command | as for `sai build` |
| `sai test` | no artifact in `out/`; the test harness, a core module of the target's pointer width, is stored in `.sai/cache/` and run by the test runner ([impl/13](../impl/13-cli.md) §13.6.6.2, N-H-82) | same | same |
| `sai check` | no artifact; analysis for the artifact that `sai build` would produce | same | same |

1. The files are written to `out/<target>/<profile>/` with the names of [impl/13](../impl/13-cli.md) §13.5.1 (N-H-27). `<name>` is the name part of the root package's identity.
2. `sai check` and the language server evaluate every artifact-dependent rule (the capability `host`, §3.2; `@target(host)`, [spec/15](../spec/15-attributes-and-lints.md) §15.11.2; whether `export fn` items are roots, [spec/08](../spec/08-effects-and-capabilities.md) §8.12.3) for the artifact of `sai build` on the same target.
3. The kind of a component (command or reactor) is decided by [spec/13](../spec/13-components-and-wit.md) §13.5.3 and §4 below. It never depends on the profile.
4. On a native target, `sai build` produces a native executable (§2.8.5).

### 2.4 Wasm features

1. **Baseline.** The baseline of every Wasm target is WebAssembly 2.0 (D-WASM-01): mutable globals, sign-extension operators, non-trapping float-to-int conversions, multi-value, bulk memory operations and reference types. The wasm64 targets additionally require memory64 for memory 0 (D-WASM-01). The table index type stays 32-bit on every target.
2. **Opt-in features.** `tail-call` and `simd128` are off by default on every target and in every profile. They are enabled only per build, with `--wasm-feature` ([impl/13](../impl/13-cli.md) §13.4.7, N-H-38). No target, profile or `sai.package` declaration enables them. Enabling one never changes the meaning of a program ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.2.2).
3. **Never used.** Threads (atomics and shared memory), exception handling in any version, GC and typed function references, multi-memory, relaxed SIMD, extended constant expressions, custom page sizes and the start section are never used by any v0.1 target. No SAI construct requires them ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.2.1).
4. **Profile independence.** The feature set of a build is a function of the target and the enabled opt-in features only. It never depends on the profile (N-F-1 of [impl/06](../impl/06-wasm-lowering-and-layout.md)).
5. **Host requirement.** A host that runs an artifact for a Wasm target MUST support the baseline of item 1 (with memory64 on wasm64 targets) and every opt-in feature enabled for that build. A host for a WASI target MUST in addition implement the Component Model and the WASI interfaces that the component imports, at a version that the Component Model accepts for the imported version (§2.5). The exact feature list of each build is recorded in its `target_features` custom section, except in `tiny` ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.11.3).
6. The `wasm32` and `wasm64` variants of a target kind accept the same programs, except where a rule depends on `PS`, on `memory64` or on the target name: `@target`, `USize` overflow, the layout limits of [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.4.1, and the C type aliases ([spec/16](../spec/16-core-library.md) §16.12.4).

### 2.5 WASI version

1. Each WASI target fixes one WASI version per toolchain release. In v0.1 both WASI targets fix **WASI 0.2.3** (N-F-63). `sai --version --verbose` prints it as `wasi 0.2.3` ([impl/13](../impl/13-cli.md) §13.3.7).
2. The WASI version determines: the version of every `wasi:` package that the build reads ([spec/13](../spec/13-components-and-wit.md) §13.7.1); the version in every WASI import of the component (`wasi:io/streams@0.2.3`); the version of `wasi:cli/run` exported by a command (§4.3); and the WASI references in the generated WIT ([spec/13](../spec/13-components-and-wit.md) §13.11.4). The WASI packages are `wasi:cli`, `wasi:clocks`, `wasi:filesystem`, `wasi:http`, `wasi:io`, `wasi:random` and `wasi:sockets`.
3. The WASI WIT that the toolchain ships is the **stable subset** of the release: every item that the WASI WIT marks `@unstable` is absent. In WASI 0.2.3 this removes, for example, `wasi:cli/exit.exit-with-code` and `wasi:clocks/timezone`. This is why a WASI 0.2 command can report only success or failure ([spec/16](../spec/16-core-library.md) §16.16.3 item 3). An item marked `@since` a version at most 0.2.3 is present.
4. The interfaces of the stable subset, which are the WIT interface capabilities that exist for the WASI packages ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.1), are:

   | Package | Interfaces |
   |---|---|
   | `wasi:cli` | `environment`, `exit`, `run`, `stdin`, `stdout`, `stderr`, `terminal-input`, `terminal-output`, `terminal-stdin`, `terminal-stdout`, `terminal-stderr` |
   | `wasi:clocks` | `monotonic-clock`, `wall-clock` |
   | `wasi:filesystem` | `types`, `preopens` |
   | `wasi:http` | `types`, `incoming-handler`, `outgoing-handler` |
   | `wasi:io` | `error`, `poll`, `streams` |
   | `wasi:random` | `random`, `insecure`, `insecure-seed` |
   | `wasi:sockets` | `network`, `instance-network`, `udp`, `udp-create-socket`, `tcp`, `tcp-create-socket`, `ip-name-lookup` |

   Any other name in the WASI namespace is `UNKNOWN_CAPABILITY` (defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.1), for example `wasi:filesystem/read` (R-13) and `wasi:clocks/timezone`.
5. The worlds that the toolchain ships with these packages (`wasi:cli/command`, `wasi:cli/imports`, `wasi:http/proxy`, `wasi:http/imports` and the `imports` worlds of the other packages) can be named by `export world` ([package/01](../package/01-package-contract.md) §1.9) and by `include` ([spec/13](../spec/13-components-and-wit.md) §13.4).
6. A host that implements a later WASI 0.2 release accepts the components of this toolchain, because Component Model imports of `0.2.3` are satisfied by any compatible `0.2.x` with `x ≥ 3`. The toolchain never imports a version newer than the one of item 1.
7. Changing the WASI version of a target is a change of the toolchain, not of a program. It changes the generated WIT and the import names, and therefore the artifact, but never the meaning of an accepted program.
8. Core targets have no WASI version. A `wasi:` name in their builds is still a known capability name (it can be written in `allow` and `deny`), but no WASI capability is provided (§3.1).

### 2.6 Linear memory bounds

[spec/01](../spec/01-introduction.md) §1.5.2 item 3 makes the default shadow stack size and the initial and maximum linear memory implementation-defined within the bounds of this section (N-F-64). A compiler documents its choice per compiler version.

1. **Shadow stack.** When a module has a shadow stack, its size `S` MUST satisfy:
   - `S` is a multiple of 16, at least 1 MiB (1048576 bytes) and at most 64 MiB (67108864 bytes);
   - `S` is the same for every profile and every opt-in feature set of a given target and compiler version, because which programs overflow must not depend on the profile beyond the frame sizes that optimization changes (C-03; [impl/09](../impl/09-optimization-and-size.md) §9.2.2);
   - the stack lies below static data and above the null guard, as D-WASM-02 prescribes, so that an overflow reaches the guard check and traps.
   A module whose functions never need a shadow frame has no shadow stack, `S = 0` (D-WASM-02, N-F-3 of [impl/06](../impl/06-wasm-lowering-and-layout.md)). No package declaration, profile or command-line option changes `S` in v0.1.
2. **Initial memory.** When a module has a memory, its minimum size in pages MUST be at least `max(1, ceil(heap_base / 65536))`, where `heap_base` is the first address above the null guard, the shadow stack, static data and runtime state. It MUST be at most that value plus 256 pages (16 MiB). A host therefore never has to provide much more memory than a program's static layout needs before the first allocation.
3. **Maximum memory.** The memory type either has no maximum, or a maximum of at least 16384 pages (1 GiB) and at most the limit of the memory type (65536 pages for a 32-bit memory). A host MAY impose a smaller limit of its own; then `memory.grow` fails earlier, which the program observes as an out-of-memory trap ([impl/10](../impl/10-runtime.md) §10.2.6).
4. **Reference values.** The reference implementation uses `S = 1 MiB` in every profile (N-F-2), the minimum `max(1, ceil(__heap_base / 65536))` of item 2 exactly, and no maximum (N-F-4) ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.3, §6.3.6, §6.3.7).
5. **Consequences.** The call depth at which a program overflows the shadow stack and the allocation volume at which it runs out of memory are implementation-defined consequences of these choices ([spec/01](../spec/01-introduction.md) §1.5.2 item 3). Both end the instance with a trap ([spec/10](../spec/10-errors-and-panics.md)). Neither is a diagnostic.
6. The memory is exported under the name `memory` whenever it exists, on every Wasm target ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.7). Inside a component it is an internal export of the core module ([spec/13](../spec/13-components-and-wit.md) §13.5.2).

### 2.7 The experimental target `wasm64-wasi`

`wasm64-wasi` is experimental because it depends on memory64 support in the Component Model, which the pinned component tooling does not yet provide (D-TGT-01; [impl/07](../impl/07-component-lowering.md) §7.11.7, N-D-58). For a compiler that offers `wasm64-wasi` (N-F-65):

1. Analysis MUST be fully supported: `sai check`, the language server, `sai bindgen wit` and every diagnostic behave as on `wasm32-wasi`, with `PS = 8` and the capability `memory64`.
2. The core module MUST be produced and validated: lowering, layout, runtime and optimization are those of every wasm64 target ([impl/06](../impl/06-wasm-lowering-and-layout.md)).
3. Component output is OPTIONAL. A compiler that does not produce it stops every command that would write a component for `wasm64-wasi` (`sai build`, `sai component build`, `sai run`, `sai size`), after item 2, with `FEATURE_NOT_IMPLEMENTED` (defined in [spec/01](../spec/01-introduction.md) §1.4.5), data `stage = 4` and `feature = "wasm64-component"`, at the target's entry in `targets`. The build fails and writes nothing ([impl/13](../impl/13-cli.md) §13.5.1). The reference implementation does not produce it in v0.1.
4. A compiler that produces the component MUST use the Canonical ABI with 64-bit memory as the Component Model specifies it; every other rule of this document and of [spec/13](../spec/13-components-and-wit.md) applies unchanged.
5. Because this exception is part of the target's definition, a compiler that offers `wasm64-wasi` without component output still conforms on it (§1.2 item 2).

### 2.8 Native targets (Stage 7)

Native targets compile SAI to machine code for an operating system (draft §57). They are Stage 7 (feature `native-target`). The implementation chapters of v0.1 describe only the Wasm backends ([roadmap](../roadmap.md) §1.10); this section fixes what a program can observe on a native target, so that portable code can be written against it now.

#### 2.8.1 The native target table

| Target | Platform | C data model | `PS`, `USize` | `CChar` | `CLong`, `CULong` | Executable name |
|---|---|---|---|---|---|---|
| `x86_64-linux` | Linux on x86-64, System V ABI | LP64 | 8 | `I8` | `I64`, `U64` | `<name>` |
| `aarch64-linux` | Linux on AArch64, AAPCS64 | LP64 | 8 | `U8` | `I64`, `U64` | `<name>` |
| `aarch64-macos` | macOS on Apple silicon (arm64) | LP64 | 8 | `I8` | `I64`, `U64` | `<name>` |
| `x86_64-windows` | Windows on x86-64, Microsoft x64 ABI | LLP64 | 8 | `I8` | `I32`, `U32` | `<name>.exe` |

1. The aliases of [spec/16](../spec/16-core-library.md) §16.12.4 that the table does not list are the same on every native target: `CSChar`, `CUChar` are `I8`, `U8`; `CShort`, `CUShort` are `I16`, `U16`; `CInt`, `CUInt` are `I32`, `U32`; `CLongLong`, `CULongLong` are `I64`, `U64`; `CSize`, `CSSize` are `USize`, `ISize`; `CFloat`, `CDouble` are `F32`, `F64`. Only `CChar`, `CLong` and `CULong` differ between targets, as spec/16 states (N-F-66).
2. The C calling convention of `unsafe extern "C"` functions ([spec/12](../spec/12-unsafe-and-ffi.md) §12.9) is the platform's default C convention of the second column. `@repr(C)` records follow the platform's C struct layout ([spec/15](../spec/15-attributes-and-lints.md) §15.9).
3. All native targets are little-endian and have 64-bit addresses. A native target has no Wasm memory type, no Wasm feature set (§2.4) and no WASI version of its own (§2.8.4).

#### 2.8.2 Libraries

1. For an `unsafe extern "C"` block with `module "name"`, the library is located as the platform's system linker locates it for the option `-l<name>` in its default search directories: `lib<name>.so` or `lib<name>.a` on the Linux targets, `lib<name>.dylib`, `lib<name>.tbd` or `lib<name>.a` on `aarch64-macos`, and `<name>.lib` on `x86_64-windows` ([spec/12](../spec/12-unsafe-and-ffi.md) §12.9.1 item 2). The preference between the dynamic and the static form is the platform linker's default.
2. A library that cannot be found is the error `NATIVE_LIBRARY_NOT_FOUND` (N-F-67), reported at the `module` string of the first `unsafe extern "C"` block, in the diagnostic order of [spec/01](../spec/01-introduction.md) §1.6, that names it and has a reachable call. Its data are the library name, the target and the directories searched. A block none of whose functions is reachable never causes a library to be linked or searched for.
3. A block without `module` resolves its symbols against the **default libraries** of the target: the platform C library and its math library (`libc` and `libm` on Linux, `libSystem` on macOS, the Universal C Runtime on Windows), and nothing else.
4. The same library name in several blocks, in one or several packages, is linked once.
5. Linking is static at build time for SAI code and for the default libraries' import stubs. A dynamic library found by item 1 is loaded by the operating system when the executable starts; this is not dynamic linking in the sense of §5.7, because the set of libraries is fixed by the build.

#### 2.8.3 Native capabilities

A native target provides `alloc`, `native_ffi`, `process`, and every WASI package of §2.5 (§3.1). It provides neither `host` (there is no `extern "wasm"` on a native target), nor `memory64` (a Wasm feature), nor `threads` (D-THREAD-01), nor any non-WASI WIT capability, because a native executable is not composed with components. The exception is an interface that a hybrid dependency provides within the build (§3.1 item 3).

#### 2.8.4 WASI interfaces on native targets

The standard packages reach the operating system through WASI interfaces, as on WASI targets ([spec/16](../spec/16-core-library.md) §16.16). On a native target the toolchain links its own native implementation of the WASI 0.2.3 interfaces into the executable instead of importing them (N-F-67). The interfaces, their types, and the effects and capabilities of their use are those of §2.5, so the capability checks and the WIT that a program sees are the same as on `wasm32-wasi`. The native implementation grants the authority of the operating-system process: the preopened directories are the process's current directory under the name `.`, and network access is not restricted.

#### 2.8.5 Native artifacts and failures

1. `sai build` for a native target produces one executable, `out/<target>/<profile>/` followed by the executable name of §2.8.1. `sai component build` for a native target is `COMPONENT_TARGET_MISMATCH` (defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2), because a native executable cannot be a component.
2. A native build requires the entry point `main` of §4.2. A root package without it, built by `sai build` for a native target, is `NATIVE_ENTRY_POINT_REQUIRED` (N-F-67). `export fn` items are not exported from the executable; a root package that declares one receives `EXPORT_FN_NOT_APPLIED` (defined in [spec/13](../spec/13-components-and-wit.md) §13.6.4) once, and `export world` is not applied (`EXPORT_WORLD_NOT_APPLIED`, defined in spec/13 §13.6.2).
3. The `ExitCode` that `main` returns becomes the process exit status, all eight bits of it.
4. A trap on a native target terminates the process abnormally, as the C function `abort` does. A panic report, in the `Report` mode of §5.2, is written to the process's standard error before the trap. Stack overflow is detected by a guard region below the native stack and also terminates the process; the shadow stack of §2.6 does not exist on native targets.
5. Profiles apply to native targets with the same meaning as on Wasm targets (§5). The debug information of §5.2 becomes the platform's symbol table and debug information (DWARF on Linux and macOS, PDB on Windows): `dev` keeps symbols and line tables, `release` keeps function symbols only, and `tiny` strips both.

### 2.9 Example

A package that supports a freestanding core module and a WASI component:

```sai-package
package acme:words @ 1.0.0
compiler ^0.1
source "src"
targets { wasm32-core, wasm32-wasi, wasm64-core }
allow { alloc, memory64 }
```

`sai build` builds for `wasm32-core`, the first listed target. `sai build --target wasm64-core` builds the 64-bit core module; it requires `memory64`, which is why the root allows it (§3.1 item 4). Without that entry the wasm64 build is `CAPABILITY_NOT_ALLOWED`, while the wasm32 builds are unaffected. `sai build --target wasm64-wasi` is `TARGET_NOT_DECLARED`. Code that depends on the address width tests the target capability `memory64` ([spec/15](../spec/15-attributes-and-lints.md) §15.11):

```sai
@target(memory64)
pub fn address_bits() -> U32 {
    return 64
}

@target(none(memory64))
pub fn address_bits() -> U32 {
    return 32
}
```

---

## 3. Target capabilities

### 3.1 The capability table

This table is the authoritative statement of which capabilities each target provides ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.7, C-07) (N-F-68). A required capability `r` of a build ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.4) is **provided** when the cell for `r`'s row and the build's column is "yes". A WIT interface capability is in the row of its package.

| Capability | core target, core module | core target, `sai component build` | `wasm32-wasi` | `wasm64-wasi` | native target |
|---|---|---|---|---|---|
| `alloc` | yes | yes | yes | yes | yes |
| `wasi:cli`, `wasi:clocks`, `wasi:filesystem`, `wasi:http`, `wasi:io`, `wasi:random`, `wasi:sockets` | no | no | yes | yes | yes (§2.8.4) |
| a WIT package of another namespace, or one of its interfaces | no (item 3) | no (item 3) | yes | yes | no (item 3) |
| `host` | yes | no | no | no | no |
| `threads` | no | no | no | no | no |
| `native_ffi` | no | no | no | no | yes |
| `process` | no | no | no | no | yes |
| `memory64` | `wasm64-core` only | `wasm64-core` only | no | yes | no |

1. **Columns.** A core target has two columns, because the capability `host` depends on the artifact (§3.2). `sai build`, `sai size`, `sai test` and `sai check` on a core target use the first column; `sai component build` on a core target uses the second. Every command on a WASI target uses the column of that target.
2. **WASI.** A WASI target provides every interface of every WASI package of §2.5 at the target's WASI version. A core target provides none: a reachable WASI interface effect, or a built-in effect whose capability is a WASI package, is `TARGET_CAPABILITY_UNAVAILABLE` (defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6) on a core target, in both of its columns (draft §39; [spec/13](../spec/13-components-and-wit.md) §13.6.2 item 2).
3. **Other WIT packages.** A WIT capability of a package outside the WASI namespace (an interface that the component imports and that composition with another component satisfies) is provided by the WASI targets. It is not provided in the second column, because a component built for a core target has no imports at all (D-TGT-01, [spec/13](../spec/13-components-and-wit.md) §13.6.2 item 2), and not on a native target, which is never composed (§2.8.3). The exception, in every column, is an interface that a hybrid dependency in source form provides within the build, and a package all of whose interfaces it provides: the build links the calls to the dependency and imports nothing for them ([spec/13](../spec/13-components-and-wit.md) §13.5.2 and §13.6.2 item 4, [package/04](../package/04-distribution-and-security.md) §4.5.3 item 4).
4. **`memory64`** is provided exactly by the wasm64 targets, and every build for a wasm64 target requires it ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.4). It is not produced by any effect. A root package that builds for a wasm64 target therefore needs `memory64` in its `allow`; a dependency never requires it.
5. **`alloc`** is provided by every target. Whether a build may use it is decided by the policies alone; `tiny` does not deny it (§5.4).
6. The table does not depend on the profile. Profiles restrict what the root package may use through its effective policy (§5.5), never what the target provides.

### 3.2 Artifact-dependent capabilities

1. **`host`** is provided only when the artifact is a core module (N-F-68). A component imports only WIT interfaces, so a reachable call of an `extern "wasm"` function in a component build is `TARGET_CAPABILITY_UNAVAILABLE` ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.7, [spec/13](../spec/13-components-and-wit.md) §13.6.4). The predicate `@target(host)` follows the same rule ([spec/15](../spec/15-attributes-and-lints.md) §15.11.2), so that code that calls the host can be removed from component builds.
2. **Commands that produce no artifact** (`sai check`, the language server, `sai bindgen wit`) and test builds ([spec/14](../spec/14-testing.md) §14.7.1) use the column of the artifact that `sai build` produces for the same target: the first column on a core target.
3. The declaration of an `extern "wasm"` block, and of a `@panic_hook`, is never an error by itself in a component build. Only a reachable call requires `host` ([spec/10](../spec/10-errors-and-panics.md) §10.6.2 item 1).

### 3.3 The `wasi:io` implication

Every WASI 0.2 package other than `wasi:io` uses the resources of `wasi:io` in its interface: `wasi:cli/stdout` returns an `output-stream` of `wasi:io/streams`, `wasi:clocks/monotonic-clock` returns a `pollable` of `wasi:io/poll`, and the stream errors of `wasi:io/error` appear in every stream operation. Code that uses one of those packages therefore requires `wasi:io` interfaces as well, through interface effects of the library code that wraps them and through the types of the component's imports (`TypeImp`, [spec/08](../spec/08-effects-and-capabilities.md) §8.12.4). The functions of `wasi:io` only operate on resources that another WASI interface has created, so `wasi:io` grants no authority of its own. This section therefore defines (N-F-69):

1. A policy `(A, D)` **grants** a required capability `r` that is `wasi:io` or an interface of `wasi:io` when either
   - some `a ∈ A` covers `r` ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.1), or
   - some `a ∈ A` is in the WASI namespace and is neither `wasi:io` nor an interface of it,

   and no `d ∈ D` overlaps `r`. For every other `r`, "grants" is the relation of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.5 unchanged.
2. The relation of item 1 is used by every check that asks whether a policy grants a capability: checks 1 and 2 of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6, for every package of the build and for the root's effective policy (§5.5), and the review of capability footprints at resolution time ([package/02](../package/02-resolver.md) §2.12). It is also used by the `grant` check of a requirement edge ([package/01](../package/01-package-contract.md) §1.10.8 item 2), with the grant set `G` as the policy `(G, ∅)`: `grant { wasi:cli }` lets a dependency write to standard output. It is not used where a name must be *covered* by another name, as in `PROFILE_ALLOW_EXCEEDS_PACKAGE` (§5.5 item 1).
3. A `deny` entry still applies. `allow { wasi:cli }` with `deny { wasi:io/streams }` does not grant `wasi:io/streams`, so a program that writes to standard output through `sai:process` is rejected with `CAPABILITY_DENIED` (defined in spec/08).
4. The implication changes no required set: `Req`, the capability footprints recorded in `sai.lock`, and the imports of a component are computed as before ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.4, [spec/13](../spec/13-components-and-wit.md) §13.5.2). It changes only which policies grant them. In particular a component imports `wasi:io/poll` only when its reachable code or the types of its imports require it.
5. The implication is the one that [spec/11](../spec/11-async-and-concurrency.md) §11.10.2 item 5 relies on: every effect that produces a pollable is granted together with `wasi:io/poll`.
6. On a core target, `wasi:io` is not provided (§3.1), whatever the policy grants.

Example. With `allow { alloc, wasi:cli }`, a command that writes to standard output requires `wasi:cli/stdout` and `wasi:io/streams` (the stream type and its `blocking-write-and-flush`). Both are granted: the first is covered by `wasi:cli`, the second is implied by it.

### 3.4 The Component Model async ABI

No v0.1 target provides the Component Model async ABI (N-F-70). Consequently:

1. An exported interface whose functions include an `async fn` is `ASYNC_EXPORT_UNSUPPORTED` (defined in [spec/11](../spec/11-async-and-concurrency.md) §11.10.4) on every target, including in `sai component build` for a core target.
2. Every imported WIT function that a v0.1 target can call is synchronous. The stable WASI 0.2.3 subset (§2.5) has no `async` function and no `future<T>` or `stream<T>` type. A WIT item of another package whose signature is `async` or contains `future<T>` or `stream<T>` needs the async ABI. Before Stage 5 its use is `FEATURE_NOT_IMPLEMENTED` ([spec/13](../spec/13-components-and-wit.md) §13.2.2). From Stage 5 on, on a target without the ABI, its use is `WIT_UNSUPPORTED_FEATURE` (defined in [spec/13](../spec/13-components-and-wit.md) §13.9.2). As there, the diagnostic is reported where a SAI program uses the item, not when the WIT package is loaded.
3. Async code runs inside a synchronous export or `main` through an executor of `sai:async` ([spec/11](../spec/11-async-and-concurrency.md) §11.9), which waits on WASI targets through `wasi:io/poll` (§3.3) and, on core targets, completes only futures that wake themselves ([spec/11](../spec/11-async-and-concurrency.md) §11.9.2).
4. The feature name `wasi-async` of Stage 5 ([spec/01](../spec/01-introduction.md) §1.4.5) names the lowering to the async ABI. A later version of this document that adds a target with the ABI will state it in the table of §3.1; until then the feature is not available on any target, whatever stages a compiler implements.

### 3.5 Threads

No v0.1 target provides `threads` (D-THREAD-01). The effect `thread.spawn` and the capability `threads` exist as names only. A build that requires `threads` is `TARGET_CAPABILITY_UNAVAILABLE` on every target, and every instance runs on one thread ([spec/11](../spec/11-async-and-concurrency.md) §11.11). The Wasm threads proposal is never used (§2.4).

### 3.6 Applying the target check

1. Check 3 of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6 compares every `r ∈ Req` with the column of §3.1 for the build's target and artifact. Its result does not depend on any policy or on the profile.
2. The diagnostic `TARGET_CAPABILITY_UNAVAILABLE` names the target, the artifact kind when the target has two columns, and the capability, with the why chain of [spec/08](../spec/08-effects-and-capabilities.md) §8.13. Its help names the targets on which the capability is provided, in canonical order, restricted to those listed in the root's `targets`; when the capability is `host` and the build is a component build, the help instead suggests `@target(host)` on the code that calls the host.
3. Check 3 is reported independently of checks 1 and 2. A capability that is both unavailable and not granted receives both diagnostics. No `allow`, package-level or in a profile, makes a capability available (§5.6 item 5).

---

## 4. Entry points

### 4.1 Overview

An **entry point** is a place where execution of an artifact starts. SAI code runs only when the host calls an entry point; instantiation runs no code (§4.7). The entry points of each kind of artifact are:

| Artifact | Built by | Entry points | Role of `main` |
|---|---|---|---|
| core module | `sai build` for a core target | the root package's `export fn` items | not an entry point (§4.5) |
| command component | a component build whose root module defines `main` | the export `wasi:cli/run`, plus the exports of the export world | called by the `run` glue (§4.3) |
| reactor component | a component build without `main` | the exports of the export world | absent |
| component of a core target | `sai component build` for a core target | the exports of the export world | not permitted (§4.6) |
| native executable | `sai build` for a native target | the process entry | required (§4.8) |

A component is a command or a reactor as [spec/13](../spec/13-components-and-wit.md) §13.5.3 defines (N-D-31); this section fixes the form of `main` that the definition uses and what each target does with it.

### 4.2 The entry point `main`

1. **Candidate.** A function item named `main` declared at the top level of the root module of the root package is the **entry-point candidate** of the build. A function named `main` in another module of the root package, in the root module of a dependency, or as a method, is an ordinary function. An item removed by `@target` does not exist ([spec/15](../spec/15-attributes-and-lints.md) §15.11.3) and is never a candidate. Items named `main` that are not functions are not candidates.
2. **Form.** The candidate MUST have exactly the signature

   ```text
   pub fn main(env: owned process.Env) -> process.ExitCode
   ```

   where `process.Env` and `process.ExitCode` denote the designated items `sai:process.Env` and `sai:process.ExitCode` ([spec/16](../spec/16-core-library.md) §16.16.3), through any path or alias that resolves to them. In particular, the candidate MUST be `pub`, MUST have exactly one parameter, whose mode is `owned` and whose type is `Env`, and MUST return `ExitCode`. It MUST NOT be generic, `unsafe`, `export`, or a method. An `effects` clause, `requires` clauses and `ensures` clauses are permitted and have their usual meaning; the parameter's name is free (N-F-71).
3. **Violations.** A candidate declared `async` is `ASYNC_MAIN` (defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.6). Any other deviation from item 2 is `INVALID_ENTRY_POINT` (error), reported at the candidate's name, with the expected signature in its help. When the only deviation is a missing `pub`, the diagnostic carries a structured fix that inserts `pub`. A candidate with a violation is not an entry point, and the artifact is not built.
4. **Resolution.** When `sai:process` is not a dependency of the root package, the paths `process.Env` and `process.ExitCode` are ordinary unresolved names ([spec/16](../spec/16-core-library.md) §16.16.1, N-J-26). The compiler then reports the unresolved names and no `INVALID_ENTRY_POINT`.
5. **Independence of the target.** Items 1 to 4 are checked in every build of the root package and in `sai check`, on every target, including core targets where `main` is not applied (§4.5). Whether a root package is a valid program therefore does not depend on the target except through `@target`.
6. **Semantics.** A call of `main` is an ordinary call. `main` takes ownership of the environment value and drops it, or passes it on, like any other owned value. The `ExitCode` it returns is the result of the program. A panic in `main` traps the instance like every panic ([spec/10](../spec/10-errors-and-panics.md) §10.4.1); there is no unwinding to the entry point.

### 4.3 Commands on WASI targets

On `wasm32-wasi` and `wasm64-wasi` a component build whose root package has a valid entry point produces a command:

1. **World.** The export world `W'` is extended as if it contained `include wasi:cli/command@0.2.3` (§2.5; [spec/13](../spec/13-components-and-wit.md) §13.5.3). The component exports `wasi:cli/run@0.2.3` and may import every interface of `wasi:cli/imports@0.2.3`. Which of them it actually imports is the set `Imp` of [spec/13](../spec/13-components-and-wit.md) §13.5.2, computed from the reachable code of `main`, of the other roots and of their callees.
2. **Glue.** The `run` export is implemented by the glue of [impl/07](../impl/07-component-lowering.md) §7.8.4. It calls the package-private constructor `Env.for_entry` of `sai:process`, which performs no host call ([spec/16](../spec/16-core-library.md) §16.16.3), passes the result to `main`, and returns `ok` when the returned code is `0` and `err` otherwise. `main`, `Env.for_entry` and the glue are roots of reachability ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.3).
3. **The environment is a bundle of authority.** The `Env` value gives access to every capability value of the standard packages (`env.stdout()`, `FileSystem.from_env(env)`, `Network.from_env(env)`, `HttpClient.from_env(env)`; [spec/16](../spec/16-core-library.md) §16.16). Holding `Env` does not by itself permit any operation (N-F-73):
   - **statically**, every operation of these values has an effect whose capability the build must be granted by every policy of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6, including the root's effective policy for the selected profile (§5.5);
   - **in the artifact**, the component imports exactly the interfaces in `Imp`, so an operation that the code never reaches has no import through which it could be performed;
   - **at run time**, the host decides how much authority the imports carry. A host MAY supply an interface whose operations fail; the program then observes WASI error results, never a trap of SAI. `sai run` grants only what its command line names ([impl/13](../impl/13-cli.md) §13.6.5): `--env`, `--dir` and `--allow-network`.
4. **Exit codes.** WASI 0.2 `run` transmits only success or failure ([spec/13](../spec/13-components-and-wit.md) N-D-31). A command that returns `ExitCode.from_code(3)` therefore ends with the host's failure status, not with status `3`. `Env.exit` ([spec/16](../spec/16-core-library.md) §16.16.3) uses the same mapping on WASI 0.2.3: code `0` is success and every other code is failure (§2.5 item 3: `exit-with-code` is unstable and absent).
5. **Running.** `sai run` requires a WASI target and a command: on another target it is `TARGET_NOT_RUNNABLE`, and on a reactor `MISSING_ENTRY_POINT` (both defined in [impl/13](../impl/13-cli.md) §13.6.5).

### 4.4 Reactors on WASI targets

A component build on a WASI target whose root package has no entry-point candidate produces a reactor ([spec/13](../spec/13-components-and-wit.md) §13.5.3). Its entry points are the functions of its exported interfaces and world-level exported functions, through the export glue of [impl/07](../impl/07-component-lowering.md) §7.8. A reactor without exports is `NO_COMPONENT_EXPORTS`; an export world that itself exports `wasi:cli/run` while `main` exists is `WORLD_DUPLICATE_ITEM` (both defined in spec/13). A reactor has no initialization export (§4.7).

### 4.5 Core modules

`sai build` for `wasm32-core` or `wasm64-core` produces a core module (N-F-74):

1. **Exports.** The module exports exactly:
   - every `export fn` item of the root package, in any of its modules, under its export name ([spec/04](../spec/04-modules-and-names.md) §4.6.6, N-A-15; [spec/15](../spec/15-attributes-and-lints.md) §15.7), through the wrapper of [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7 when one is needed;
   - the memory, under the name `memory`, when the module has one ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.7, §6.10.5).

   It exports no `_start`, no `_initialize`, no `cabi_realloc`, no post-return function and no table or global. A dependency's `export fn` is an ordinary `pub` function (N-A-15).
2. **Imports.** The module imports exactly the reachable functions of `extern "wasm"` blocks ([spec/12](../spec/12-unsafe-and-ffi.md) §12.7), under their module and item names, and the `@panic_hook` function when the panic mode is `Report` or reachable code calls it ([spec/10](../spec/10-errors-and-panics.md) §10.6.2 item 2; [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.1). It imports nothing else: no memory, no table, no WASI function.
3. **The export world is not applied.** If the root declares `export world`, the build reports `EXPORT_WORLD_NOT_APPLIED` (defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2).
4. **`main` is not applied** (N-F-72). A valid entry point of the root package is neither an export nor a root of reachability of a core module: a core module has no command form, and its host calls its exports directly. The build reports the information diagnostic `ENTRY_POINT_NOT_APPLIED` once, at the name of `main`, whose help suggests building for a WASI target. Like `EXPORT_WORLD_NOT_APPLIED`, it is reported by every command that analyzes the root package for a core module (`sai check`, `sai build`, `sai size` for a core target), so that hybrid packages that also build commands for WASI targets are accepted without errors. It is not reported in test builds.
5. A core module with no exports is valid. It is an empty module apart from what item 2 imports; its host can instantiate it but never calls SAI code.

### 4.6 Components of core targets

`sai component build` for a core target produces a reactor component without imports ([spec/13](../spec/13-components-and-wit.md) §13.6.2 items 2 and 3, [impl/07](../impl/07-component-lowering.md) §7.11.5):

1. Its entry points are the exports of the export world, as for a reactor (§4.4).
2. A root package with an entry-point candidate is `COMPONENT_TARGET_MISMATCH` (defined in spec/13 §13.6.2), because a command needs `wasi:cli`.
3. `export fn` items are not exported. A root package that declares one receives `EXPORT_FN_NOT_APPLIED` (defined in spec/13 §13.6.4) once.
4. The capability column is the second column of §3.1: neither `host` nor any WIT interface is provided, except the interfaces that a hybrid dependency provides within the build (§3.1 item 3).

### 4.7 No start function and no initialization

No artifact of any target has a start function, a `_start` or `_initialize` export, or any other initialization step that runs SAI code ([impl/10](../impl/10-runtime.md) §10.7.1, N-F-37; D-CAP-02). Static data is placed by data segments and runtime state is valid at instantiation. Consequently a reactor or a core module can be called immediately after instantiation, and a command's only code before `main` is the `run` glue and `Env.for_entry`.

### 4.8 Native executables

On a native target (Stage 7):

1. The executable's process entry is provided by the toolchain. It initializes the native WASI implementation of §2.8.4, constructs the `Env` value with `Env.for_entry`, calls `main`, and terminates the process with the exit status `code()` of the returned `ExitCode`, as eight bits (§2.8.5 item 3). On native targets `Env.exit` also transmits the full code.
2. A root package without a valid entry point is `NATIVE_ENTRY_POINT_REQUIRED` (§2.8.5 item 2). `export fn` and `export world` are not applied, with the information diagnostics of §2.8.5 item 2.
3. The process's command-line arguments, environment variables and standard streams are those that `Env` exposes; the program name is the first argument.

### 4.9 Examples

A core module with one export:

```sai
export fn add(a: I32, b: I32) -> I32 {
    return a + b
}
```

For `wasm32-core` in profile `release`, the module exports `add` with type `(i32, i32) -> i32` and has no memory and no imports. No code needs linear memory, and because no `@panic_hook` is declared there is no reporting channel, so the overflow check of `+` traps without a report (§5.2). The package needs no capability.

A core module that calls its host:

```sai
extern "wasm" module "env" {
    fn now_ms() -> I64
}

export fn tick() -> I64
    effects { host }
{
    return now_ms()
}
```

The module imports `env.now_ms` and exports `tick`. The root package must allow `host`. `sai component build` for the same target rejects the call with `TARGET_CAPABILITY_UNAVAILABLE`, because a component does not provide `host` (§3.2).

A command:

```sai
use sai:core/fmt.{Writer}
use sai:process

pub fn main(env: owned process.Env) -> process.ExitCode
    effects { stdio.write }
{
    var out = env.stdout()
    out.write_str("hello\n")
    process.ExitCode.success()
}
```

With `targets { wasm32-wasi }`, a `requires` entry for `sai:process`, and `allow { wasi:cli }`, `sai build` produces a command component that exports `wasi:cli/run@0.2.3`. It imports `wasi:cli/stdout@0.2.3` and the `wasi:io` interfaces that writing to the stream needs: `wasi:io/streams@0.2.3`, and `wasi:io/error@0.2.3` through the types of `streams` ([spec/13](../spec/13-components-and-wit.md) §13.5.2 rule 3). The `wasi:io` interfaces are granted by the implication of §3.3. If the package also listed `wasm32-core`, and its dependencies supported that target, `sai build --target wasm32-core` would produce a core module without exports and report `ENTRY_POINT_NOT_APPLIED`. The capability check would pass there as well, because `main` is not a root of a core module (§4.5 item 4), so its `stdio.write` is not reachable.

An invalid entry point:

```sai
use sai:process

fn main(env: owned process.Env) -> process.ExitCode {
    let _ = env
    process.ExitCode.success()
}
```

This is `INVALID_ENTRY_POINT`, with a structured fix that inserts `pub`.

---

## 5. Profiles

### 5.1 Selection

1. There are exactly three profiles: `dev`, `release` and `tiny` (D-TGT-02). User-defined profiles do not exist in v0.1.
2. A command selects the profile only through `--profile` ([impl/13](../impl/13-cli.md) §13.4.2, C-06). The default is `dev` for every command. Another name is `INVALID_OPTION_VALUE` (defined in [impl/13](../impl/13-cli.md)). No environment variable, configuration file or package declaration selects a profile.
3. A build has exactly one profile, and every package of the build is compiled with it (N-F-75). There is no per-package profile: a dependency is optimized, checked and laid out exactly like the root. A prebuilt artifact dependency keeps the code generation of its own build, but it is used only where package/04 permits it (§5.7 item 3).
4. `profile <name> { … }` blocks are effective only in the root package ([package/01](../package/01-package-contract.md) §1.14.1 item 3, N-G-15). Their `allow` lifts default denials of §5.4 (§5.5), and their `budget` adds size limits (§5.8). A block for a profile other than the three is `UNKNOWN_PROFILE` (defined in [package/01](../package/01-package-contract.md)).
5. Every profile is available on every target (§1.2). Artifacts of different profiles are written to different directories, `out/<target>/<profile>/` ([impl/13](../impl/13-cli.md) §13.5.1).

### 5.2 What each profile sets

The following table fixes every property that depends on the profile, and the section that implements it. It refines the table of D-TGT-02; a row that D-TGT-02 states in words is given here in the form that an implementation follows.

| Property | `dev` | `release` | `tiny` | Implemented in |
|---|---|---|---|---|
| Optimization pipeline | the `dev` column | the `release` column | the `tiny` column | [impl/09](../impl/09-optimization-and-size.md) §9.3.1 |
| Inlining and optimizer constants | — | the `release` row | the `tiny` row | [impl/09](../impl/09-optimization-and-size.md) §9.3.4 |
| Identical code folding | no | no | yes | [impl/09](../impl/09-optimization-and-size.md) §9.5 |
| Dead-code elimination | item-level and final DCE on unoptimized bodies | after optimization | after optimization and ICF | [impl/09](../impl/09-optimization-and-size.md) §9.6 |
| Overflow, bounds and contract checks | always on | always on | always on | [spec/06](../spec/06-expressions-and-statements.md), [spec/09](../spec/09-contracts-and-refinements.md) |
| Panic mode | `Report` if the artifact has a reporting channel, otherwise `TrapOnly` | as `dev` | `TrapOnly` | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.8.12.1, [impl/10](../impl/10-runtime.md) §10.5.3 |
| Custom sections | `name` (module, function, local and global names), `producers`, `target_features`, `sai.meta`, DWARF | `name` (module and function names), `producers`, `target_features`, `sai.meta` | none except `component-type:sai` in component builds | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.11.1 (N-F-21) |
| DWARF | DWARF 5 line table and subprogram entries | none | none | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.12 (N-F-22) |
| Allocator, when the root declares no `allocator` | the default allocator | the default allocator | the tiny allocator | [impl/10](../impl/10-runtime.md) §10.2.1 |
| Capabilities denied by default | none | none | §5.4 | §5.4, §5.5 |
| Size budgets | the root's `profile dev` budget | the root's `profile release` budget | the root's `profile tiny` budget | [impl/09](../impl/09-optimization-and-size.md) §9.10.6 |

1. **Reporting channels.** The channel of `Report` mode is the root's `@panic_hook` in a core module, and standard error in a component that imports `wasi:cli/stderr` ([impl/10](../impl/10-runtime.md) §10.5.3). A native executable always has standard error as its channel (§2.8.5 item 4). An artifact without a channel uses `TrapOnly` in every profile.
2. **Debug information** on native targets follows §2.8.5 item 5.
3. **The allocator package** declared with `allocator <pkg>` ([package/01](../package/01-package-contract.md) §1.15) is used in every profile; the row above applies only without it.
4. D-TGT-02 separates DWARF from the name section. `release` has no DWARF and no local names, but keeps the metadata sections, which are not debug information ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.11.1, N-F-21).

### 5.3 What a profile never changes

Profiles never change the semantics of a program (C-03). For every target, the following are the same in all three profiles (N-F-80):

1. **Behavior.** The observable behavior of an accepted program: its results, its effects on the host, which checks fail, and where a panic or trap occurs ([impl/09](../impl/09-optimization-and-size.md) §9.2.1). Only the panic report differs: whether it is delivered, and the exact message text in `TrapOnly` mode, which is never delivered.
2. **Analysis.** Name resolution, type checking, borrow checking, effect inference, reachability and the required capabilities `Req` ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.3), and the set of Mono instances ([impl/09](../impl/09-optimization-and-size.md) §9.2.2 item 1).
3. **Machine model.** The target's feature set (§2.4, N-F-1), the shadow stack size `S` (§2.6), the layout of every type and the addresses of static data ([impl/06](../impl/06-wasm-lowering-and-layout.md)).
4. **Boundary.** The imports and the function exports of the artifact, computed before optimization ([impl/09](../impl/09-optimization-and-size.md) §9.8 item 1, N-F-20). The only exceptions are the core imports of the panic channel, which follow the panic mode ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.1, [impl/10](../impl/10-runtime.md) §10.5.3), and the `memory` export, which follows whether the module needs a memory ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.3.7). The imports of a component never depend on the profile.
5. **Acceptance.** A program is accepted in one profile exactly when it is accepted in another, except for the profile's capability denials (§5.4 to §5.6), its budgets (§5.8), and the closed list of optimization-dependent diagnostics ([impl/09](../impl/09-optimization-and-size.md) §9.2.2 item 3).

### 5.4 Default denials

Each profile `p` has a **denial predicate** `denied_p` on capability names (N-F-76):

1. For `dev` and `release`, `denied_p(c)` is false for every `c`.
2. For `tiny`, `denied_p(c)` is true exactly when
   - `c` is in the WASI namespace (§1.3): the name `wasi:<pkg>` of one of the seven WASI packages of §2.5, including `wasi:io`, or the name of one of their interfaces; or
   - `c` is `host` or `threads`.
3. `tiny` does not deny `alloc`, WIT capabilities outside the WASI namespace, `memory64`, `native_ffi` or `process`. Allocation is controlled by the tiny allocator, not by a denial (§5.2). A WIT interface of another component is satisfied by composition or within the build (§3.1 item 3), not by the host, so it is not the "WASI" of D-TGT-02. `memory64` is required by every wasm64 build, and denying it would make `tiny` unusable there. `native_ffi` and `process` are the native counterparts of authority that D-TGT-02 does not list.
4. Every denial class is closed under containment: when `denied_p(c)` holds for a WIT package, it holds for each of its interfaces, and the reverse. A denial therefore never splits a package capability.
5. The remaining restrictions of D-TGT-02, dynamic linking and reflection, are not capabilities (§5.7).

### 5.5 The effective policy

The **effective policy** of the root package for a build with profile `p` is `(A_p, D)` (N-F-77), where

```text
A_p = { a ∈ allow(root) | ¬denied_p(a) } ∪ { e ∈ L_p | denied_p(e) }
D   = deny(root)
```

and `L_p` is the set of names in the `allow` of the root's `profile p { … }` block, or the empty set when the block or its `allow` is absent.

1. **Package `allow` is the maximum.** Every name in `L_p` MUST be covered by an entry of `allow(root)` ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.1); otherwise it is `PROFILE_ALLOW_EXCEEDS_PACKAGE` (defined in [package/01](../package/01-package-contract.md) §1.14.2). This uses the relation "covers" of spec/08, not the `wasi:io` implication of §3.3. Consequently `A_p` grants nothing that `allow(root)` does not grant.
2. **Entrywise.** The subtraction removes whole entries. Because every denial class is closed under containment (§5.4 item 4), removing an entry removes exactly the denied capabilities it covers, and no entry is partly denied.
3. **Entries without effect.** A name in `L_p` that `denied_p` does not deny is already in the effective policy through `allow(root)`; listing it is permitted and changes nothing ([package/01](../package/01-package-contract.md) §1.14.2 item 5). For `dev` and `release`, `A_p = allow(root)`.
4. **The `wasi:io` implication** (§3.3) applies to `(A_p, D)`. A profile `allow` that lifts any WASI package other than `wasi:io` therefore also grants the `wasi:io` interfaces that its use needs, unless `D` denies them.
5. **Use.** The effective policy replaces the root's own policy in check 2 of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6, and in check 1 for the root package. The policies of dependencies are never affected by the profile ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.5).
6. **Resolution time.** The review of capability footprints ([package/02](../package/02-resolver.md) §2.12.1) compares footprints with the package-level policy of the root, not with an effective policy, because resolution has no profile.

### 5.6 Reporting a profile denial

A capability `r ∈ Req` that the root's effective policy does not grant is reported with the codes of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6. There is no separate code for profiles: a profile restricts the checked policy, and spec/08 already classifies every capability that the checked policy does not allow (N-F-78).

1. If some entry of `deny(root)` overlaps `r`, the diagnostic is `CAPABILITY_DENIED`.
2. Otherwise it is `CAPABILITY_NOT_ALLOWED`. It is in the **profile case** when the root's package-level policy `(allow(root), deny(root))` grants `r`, with the implication of §3.3, so that only the default denial of the profile rejects it. In the profile case the primary location, the message suffix "with profile `p`", the note "profile `p` denies `r` by default", the `profile` field of the data and the structured fix that lifts the denial in `profile p { allow { … } }` are those of [impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.6.
3. **Capabilities granted by implication.** When `r` is `wasi:io` or one of its interfaces and `allow(root)` grants it only through the implication of §3.3, no entry of `allow(root)` covers `r`. Adding `r` itself to the profile `allow` would then be `PROFILE_ALLOW_EXCEEDS_PACKAGE` (§5.5 item 1). The fix therefore adds the first entry of `allow(root)`, in source order, that implies `r`. Wherever impl/08 §8.7.6 uses "the first package-level `allow` entry that covers `r`", for the location and for the `policy` field, it uses that entry.
4. **Once per capability.** The root is checked once against its effective policy, which also stands for its own policy. Each `r` is therefore reported once for the root ([impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.6 item 2, N-D-83).
5. **Target check.** Check 3 is independent of the policies ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.6). A capability that the target does not provide is `TARGET_CAPABILITY_UNAVAILABLE` whether or not a policy also rejects it, and a profile `allow` never makes it available.
6. **Timing.** The checks run on the reachable set of [spec/08](../spec/08-effects-and-capabilities.md) §8.12.3, before optimization. Their result never depends on what optimization removes ([impl/09](../impl/09-optimization-and-size.md) §9.8 item 6).

### 5.7 Dynamic linking and reflection

D-TGT-02 lists dynamic linking and reflection among the default denials of `tiny`. They are not capabilities, cannot appear in `allow` or `deny`, and cannot be lifted ([package/01](../package/01-package-contract.md) §1.14.2 item 4). In v0.1 no construct of the language, the core library or the toolchain performs either of them, in any profile, so the denial has no observable consequence (N-F-79):

1. **Dynamic linking** means loading code that was not fixed when the artifact was built. Two things are not dynamic linking: composing the artifact of a dependency with the root's component at build time ([package/04](../package/04-distribution-and-security.md) §4.5.1), and linking the native libraries of §2.8.2, which the build fixes.
2. **Reflection** means inspecting types or items at run time by name. SAI has no run-time type information. `sai.meta` and the `name` section are metadata for tools, are never read by the program, and are absent from `tiny` builds (§5.2).
3. Under `tiny`, a hybrid dependency is used in source form, never as a prebuilt artifact (the hybrid row of [package/04](../package/04-distribution-and-security.md) §4.5.1, D-PKG-05). An artifact package, which has no source, is used in artifact form (package/04 §4.5.1 item 4). It is composed at build time, so this is not dynamic linking (item 1). All of its imports count as required capabilities ([spec/08](../spec/08-effects-and-capabilities.md) §8.12.4), and check 2 tests them against the root's effective policy for `tiny`.
4. A later version that adds a mechanism for either one will define it as a capability or as a profile property, and `tiny` will deny it by default.

### 5.8 Budgets

`profile p { budget { wasm <= L } }` in the root's `sai.package` limits the size of the artifact of every build with profile `p`, on every target that the root is built for ([package/01](../package/01-package-contract.md) §1.14.3). The measurement and the diagnostic `SIZE_BUDGET_EXCEEDED` (defined in package/01) are specified by [impl/09](../impl/09-optimization-and-size.md) §9.10.6. A budget does not depend on the target; a package that needs different limits for different targets cannot express them in v0.1. Budgets of dependencies are ignored like their other profile blocks.

### 5.9 Examples

A command that is small and still writes to standard output:

```sai-package
package acme:hello @ 1.0.0
compiler ^0.1
source "src"
targets { wasm32-wasi }
requires {
    sai:process @ ^0.1
        from git "https://github.com/sai-lang/process"
}
allow { alloc, wasi:cli }
profile tiny {
    allow { wasi:cli }
    budget { wasm <= 16 KiB }
}
```

With the `main` of §4.9, `sai build --profile tiny` is accepted. The effective policy for `tiny` is `A_tiny = { alloc } ∪ { wasi:cli }`: `alloc` is not denied, `wasi:cli` is denied by `tiny` but lifted by the profile `allow`. The `wasi:io` interfaces of standard output are granted by the implication of §3.3. The build uses `TrapOnly`, the tiny allocator if anything allocates, and no custom section except `component-type:sai`. The component's imports are the same as in `dev`.

Without the `profile tiny` block, the same build reports the profile case of §5.6 item 2:

```text
error[CAPABILITY_NOT_ALLOWED]: capability `wasi:cli` is required but not allowed by package `acme:hello` with profile `tiny`
```

It has the note "profile `tiny` denies `wasi:cli` by default" and a structured fix that adds `profile tiny { allow { wasi:cli } }` to `sai.package`. The same diagnostic is reported for each other capability of the build that the profile alone rejects, such as `wasi:io/streams`. For those `wasi:io` capabilities the fix also adds `wasi:cli`, which implies them (§5.6 item 3). `sai build` in `dev` or `release` is accepted either way. With `profile tiny { allow { wasi:http } }` instead, the result is `PROFILE_ALLOW_EXCEEDS_PACKAGE`, because the package-level `allow` does not cover `wasi:http`.

---

## 6. Decisions introduced by this document

The following rules are decided here, within the decisions of [decisions.md](../design/decisions.md) and by the principles P1 to P9. Decisions N-F-1 to N-F-61 are those of [impl/06](../impl/06-wasm-lowering-and-layout.md), [impl/09](../impl/09-optimization-and-size.md) and [impl/10](../impl/10-runtime.md), which this document cites but does not repeat.

| ID | Rule | Rationale |
|---|---|---|
| N-F-62 | Every target name of §2.1 and §2.8.1 is known to every toolchain. Listing a target, using it in `only` or testing it in `@target` is not a use of it. Selecting a target that the toolchain does not implement is `FEATURE_NOT_IMPLEMENTED`: `stage = 7`, `feature = "native-target"` for native targets; `stage = 2` and `stage = 4` with the target name for unoffered `wasm64-core` and `wasm64-wasi`. `ARTIFACT_ON_CORE_TARGET` applies to every target that is not a component target, native targets included (§1.4). | A package that adds a native target later stays usable as a Wasm dependency with earlier toolchains. Failing only on selection keeps errors where the user acts (P5, P8). |
| N-F-63 | The properties of a target (kind, address width, memory type, feature set, WASI version) do not depend on the profile, the package or the command. Both WASI targets fix WASI 0.2.3, restricted to its stable items. `@unstable` items are absent (§2.1, §2.5). | One fixed version per toolchain makes imports, WIT and capability names reproducible (P8). Unstable items could change without notice (P6). |
| N-F-64 | The shadow stack size `S` is 0 or a multiple of 16 between 1 MiB and 64 MiB, the same in every profile. The initial memory lies between `max(1, ceil(heap_base / 65536))` pages and that value plus 256 pages. The maximum is absent or at least 16384 pages and at most the memory type's limit (§2.6). | spec/01 makes these implementation-defined. Bounds keep programs portable between conforming compilers, and a profile-independent `S` keeps overflow behavior out of the profile (C-03). |
| N-F-65 | For `wasm64-wasi`, analysis and the core module are required of a compiler that offers the target. Component output is OPTIONAL; without it, every command that would write the component stops with `FEATURE_NOT_IMPLEMENTED`, `stage = 4`, `feature = "wasm64-component"` (§2.7). | The Component Model's memory64 support is not available in the pinned tooling (D-TGT-01, N-D-58). Requiring it would make the target impossible to offer. |
| N-F-66 | The native target table: four targets with their C data models. Only `CChar`, `CLong` and `CULong` vary; every other C alias is the same on every native target. `unsafe extern "C"` uses the platform's default C convention, and `@repr(C)` its struct layout (§2.8.1). | Portable FFI code needs fixed aliases. The variable ones are exactly those that differ between real C ABIs (spec/16). |
| N-F-67 | Native builds: libraries are located like `-l<name>` of the platform linker (`NATIVE_LIBRARY_NOT_FOUND` otherwise). Default libraries are the C and math libraries. The toolchain links a native implementation of WASI 0.2.3 with the authority of the process. A native executable requires `main` (`NATIVE_ENTRY_POINT_REQUIRED`), exits with the full 8-bit code, and ends by `abort` on a trap. Profiles map to symbols and DWARF or PDB (§2.8.2 to §2.8.5). | The same standard packages and capability checks work on Wasm and native targets (P4). An executable without an entry point cannot run. |
| N-F-68 | The capability table of §3.1 is authoritative. A core target has two columns, for core modules and for components; `host` is provided only to core modules. Commands without an artifact and test builds use the column of the artifact that `sai build` produces (§3.1, §3.2). | `host` is a property of the artifact, not of the target (spec/08, spec/13). Fixing the column for `sai check` makes analysis predictable (P8). |
| N-F-69 | The `wasi:io` implication. A policy grants `wasi:io` or one of its interfaces also when it allows any other WASI package or interface, unless a `deny` entry overlaps it. It applies to checks 1 and 2, to effective policies, to `grant` sets and to footprint review, and changes no required set (§3.3). | `wasi:io` has no authority of its own: its functions act only on resources that other WASI interfaces create. Without the rule every WASI user would have to list it (P1). spec/11 §11.10.2 relies on it. |
| N-F-70 | No v0.1 target provides the Component Model async ABI. Exported async interfaces are `ASYNC_EXPORT_UNSUPPORTED`. Imported WIT items that are `async` or use `future`/`stream` are `FEATURE_NOT_IMPLEMENTED` before Stage 5 and `WIT_UNSUPPORTED_FEATURE` at the use from Stage 5 on (§3.4). | The async ABI is provisional upstream (spec/11 §11.10.1). Synchronous exports with `block_on` cover the use cases (R-04). |
| N-F-71 | The entry-point candidate is a function item `main` at the top level of the root module of the root package. It MUST be `pub fn main(env: owned process.Env) -> process.ExitCode` with the designated items, not generic, not `unsafe`, not `export`; effects and contracts are permitted. `async` is `ASYNC_MAIN`; any other deviation is `INVALID_ENTRY_POINT`. The check is the same on every target (§4.2). | A malformed `main` would otherwise silently make a reactor. Checking it on every target keeps validity independent of the target (P8). |
| N-F-72 | On a core module, a valid `main` is neither an export nor a root of reachability. The info `ENTRY_POINT_NOT_APPLIED` is reported once at its name by every command that analyzes the root for a core module, except test builds (§4.5 item 4). | Hybrid packages must build on core targets without errors, as for `EXPORT_WORLD_NOT_APPLIED` (spec/13 N-D-32), and a core host has no command convention. |
| N-F-73 | The `Env` value of `main` gives access to every capability value. What a build may use is decided statically by the policies, what it imports is exactly `Imp`, and the authority at run time is the host's; denied access is a WASI error, not a trap (§4.3 item 3). | Makes D-CAP-02's "bundle determined by `allow`" precise without a second run-time permission system (P1, P5). |
| N-F-74 | A core module exports exactly the root's `export fn` items and `memory` when it exists, and imports exactly the reachable `extern "wasm"` functions and the `@panic_hook` when it is used. It has no start, `_initialize` or `cabi_realloc` (§4.5). | A core module's interface is what its source declares (P1, P7); nothing is added for an assumed host. |
| N-F-75 | A build has one profile, and every source package of the build is compiled with it (§5.1 item 3). | Per-package profiles would make the behavior of a dependency depend on configuration outside it, and would complicate the whole-program pipeline (P8). |
| N-F-76 | `denied_tiny(c)` holds exactly for capabilities in the WASI namespace (including `wasi:io`), `host` and `threads`. `dev` and `release` deny nothing. `alloc`, non-WASI WIT, `memory64`, `native_ffi` and `process` are not denied (§5.4). | Reads D-TGT-02's "WASI, host, threads" as capability classes that are closed under containment. Non-WASI WIT is composition, not host authority, and `memory64` is required by every wasm64 build. |
| N-F-77 | The effective policy is `A_p = { a ∈ allow(root) \| ¬denied_p(a) } ∪ { e ∈ L_p \| denied_p(e) }`, `D = deny(root)`. Profile `allow` names must be covered by the package `allow`. The `wasi:io` implication applies to it. Dependencies are unaffected (§5.5). | Makes package/01 §1.14.2 item 3 a formula. Entrywise subtraction is exact because the denial classes are closed under containment. |
| N-F-78 | A capability that the profile alone denies is `CAPABILITY_NOT_ALLOWED` in the profile case of impl/08 §8.7.6; there is no separate code. For a `wasi:io` capability that the package `allow` grants only by implication, the fix and the policy entry use the first package-level `allow` entry that implies it. The target check is reported independently (§5.6). | spec/08 §8.12.6 already classifies a capability that the checked (effective) policy does not allow, and stable codes must not overlap (D-TOOL-01). The implying entry is the smallest valid lift, because `r` itself would exceed the package `allow` (P2). |
| N-F-79 | Dynamic linking and reflection are not capabilities and cannot be lifted. No v0.1 construct performs either, so their denial in `tiny` has no observable consequence. Build-time composition of artifacts and native libraries fixed by the build are not dynamic linking (§5.7). | D-TGT-02 names them so that later mechanisms are denied by default in `tiny`. Defining them now prevents a later reinterpretation (P6). |
| N-F-80 | Profiles never change behavior, analysis, the feature set, `S`, layout, or the boundary. The only exceptions are the panic-channel imports and the `memory` export, which follow the panic mode and the memory, and acceptance differs only by profile denials, budgets and the closed list of impl/09 §9.2.2 (§5.3). | C-03, stated as a checkable list so that profile equivalence can be tested ([impl/09](../impl/09-optimization-and-size.md) §9.11.2). |

---

## 7. Diagnostics

### 7.1 Diagnostics defined by this document

| Code | Severity | Description |
|---|---|---|
| `INVALID_ENTRY_POINT` | error | the function `main` of the root module of the root package does not have the form `pub fn main(env: owned process.Env) -> process.ExitCode` (§4.2) |
| `ENTRY_POINT_NOT_APPLIED` | info | the root package defines `main`, but the build produces a core module, which has no entry point (§4.5 item 4) |
| `NATIVE_ENTRY_POINT_REQUIRED` | error | `sai build` for a native target, and the root package has no entry point (§2.8.5 item 2) |
| `NATIVE_LIBRARY_NOT_FOUND` | error | a library named by a reachable `unsafe extern "C"` block cannot be found for a native target (§2.8.2) |

`ENTRY_POINT_NOT_APPLIED` is a hard info, not a lint ([spec/15](../spec/15-attributes-and-lints.md) §15.17.1 reason 2: a fact about the build), like `EXPORT_WORLD_NOT_APPLIED`. The three errors are hard errors.

### 7.2 Diagnostics used by this document and defined elsewhere

| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_TARGET` | error | defined in [package/01](../package/01-package-contract.md) §1.8; a name that is not a target name of §1.4 |
| `TARGET_NOT_DECLARED` | error | defined in [package/01](../package/01-package-contract.md) §1.8; the selected target is not in the root's `targets` |
| `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` | error | defined in [package/01](../package/01-package-contract.md) §1.8; an active dependency does not list the selected target |
| `ARTIFACT_ON_CORE_TARGET` | error | defined in [package/01](../package/01-package-contract.md) §1.7; extended by N-F-62 to every target that is not a component target |
| `UNKNOWN_PROFILE` | error | defined in [package/01](../package/01-package-contract.md) §1.14.1; a profile block for a profile other than `dev`, `release`, `tiny` |
| `PROFILE_ALLOW_EXCEEDS_PACKAGE` | error | defined in [package/01](../package/01-package-contract.md) §1.14.2; used by §5.5 item 1 |
| `SIZE_BUDGET_EXCEEDED` | error | defined in [package/01](../package/01-package-contract.md) §1.14.3; measured by [impl/09](../impl/09-optimization-and-size.md) §9.10.6 |
| `CAPABILITY_NOT_GRANTED` | error | defined in [package/01](../package/01-package-contract.md) §1.10.8; the `grant` check, with the implication of §3.3 |
| `INVALID_OPTION_VALUE` | error | defined in [impl/13](../impl/13-cli.md) §13.3.5; an unknown `--target` or `--profile` value |
| `TARGET_NOT_RUNNABLE` | error | defined in [impl/13](../impl/13-cli.md) §13.6.5; `sai run` for a target that is not a WASI target |
| `MISSING_ENTRY_POINT` | error | defined in [impl/13](../impl/13-cli.md) §13.6.5; `sai run` for a reactor |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; used with the data of §1.4 item 4, §2.7 item 3 and §3.4 item 2 |
| `CAPABILITY_NOT_ALLOWED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; also for a capability that only the profile denies (the profile case, §5.6 item 2; [impl/08](../impl/08-effect-and-capability-analysis.md) §8.7.6) |
| `CAPABILITY_DENIED` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6 |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6; the target check against the table of §3.1 |
| `UNKNOWN_CAPABILITY` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.1; includes WASI names outside the stable 0.2.3 subset (§2.5) |
| `EXPORT_WORLD_NOT_APPLIED` | info | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2; core modules and native executables |
| `EXPORT_FN_NOT_APPLIED` | info | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.4; component builds and native executables |
| `COMPONENT_TARGET_MISMATCH` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2; also `sai component build` for a native target (§2.8.5 item 1) |
| `NO_COMPONENT_EXPORTS` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.3 |
| `WORLD_DUPLICATE_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.4.2; an export world that exports `wasi:cli/run` while `main` exists (§13.5.3) |
| `WIT_UNSUPPORTED_FEATURE` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.9.2; async WIT items from Stage 5 on (§3.4 item 2) |
| `ASYNC_MAIN` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.2.6 |
| `ASYNC_EXPORT_UNSUPPORTED` | error | defined in [spec/11](../spec/11-async-and-concurrency.md) §11.10.4; every v0.1 target (§3.4) |
