# 13. The `sai` Command Line

**Status:** Normative for the reference implementation (see README §2). Command names, option names, exit codes (§13.3.4), and the JSON result records (§13.6) are also normative for every conforming tool that provides the command (N-H-17).
**Stage:** per command, see §13.1.2. The command-line framework (§13.2–§13.5) is Stage 0.
**Related:** [11-diagnostics.md](11-diagnostics.md), [12-semantic-tooling.md](12-semantic-tooling.md), [14-formatter.md](14-formatter.md), [09-optimization-and-size.md](09-optimization-and-size.md), [16-interfaces-and-wit.md](16-interfaces-and-wit.md), [spec/13-components-and-wit.md](../spec/13-components-and-wit.md), [spec/14-testing.md](../spec/14-testing.md), [spec/15-attributes-and-lints.md](../spec/15-attributes-and-lints.md), [package/01-package-contract.md](../package/01-package-contract.md), [package/02-resolver.md](../package/02-resolver.md), [package/03-lock-file.md](../package/03-lock-file.md), [package/04-distribution-and-security.md](../package/04-distribution-and-security.md), [targets/targets-and-profiles.md](../targets/targets-and-profiles.md), [design/decisions.md](../design/decisions.md) (D-TOOL-01, D-TOOL-03, D-TOOL-05, D-PKG-02, D-TGT-01, D-TGT-02, C-06)

This chapter defines `sai`, the single command of the SAI toolchain (draft §77): how it finds the package it works on, its global options and output conventions, its exit codes, the options shared by building commands, the layout of the files it writes, and every command with its synopsis, options, behavior, output, and exit codes. It defines the command-line surface only. The meaning of what a command computes is owned by other documents and linked from each command: test semantics by spec/14, WIT generation by spec/13, size attribution by impl/09, resolution and locking by package/02–04, lint levels by spec/15, and the semantic operations by impl/12.

---

## 13.1 Overview

### 13.1.1 Principles

1. **One tool.** All functions of the toolchain are subcommands of `sai` (draft §77). There are no separate formatter, package manager, or build tool binaries.
2. **No configuration besides `sai.package`.** The CLI reads no configuration file. Everything that affects a result is in the source, `sai.package`, `sai.lock`, the compiler version, and the command line (draft §72). Environment variables affect only presentation (§13.7), the location of the global package cache ([package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.4.1), and the transport and authentication settings that [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.2.3 lets Git use: the proxy variables `HTTPS_PROXY` and `NO_PROXY` (and their lowercase forms), the SSH agent (`SSH_AUTH_SOCK`), and the Git credential helpers configured for the user. These settings never affect build output or which content is used (N-H-18).
3. **Determinism.** For the same inputs, a command produces byte-identical output files and byte-identical standard output (P8). The only exceptions are the `tool.version` field of the JSON header, timing values that a command explicitly documents (only `sai test --format json`), and the output of the program under `sai run`.
4. **Machine-readable output for every command.** Every command accepts `--format json` and then writes the NDJSON stream of [11-diagnostics.md](11-diagnostics.md) §11.9 (D-TOOL-01).
5. **Explicit choices.** The target and the profile are selected only by `--target` and `--profile` (C-06). There are no environment variables, configuration files, or aliases that select them.

### 13.1.2 Command summary

| Command | Purpose | Stage | Needs a package | Resolves dependencies | Writes |
|---|---|---|---|---|---|
| `sai new` | create a package | 0 | no | no | a new directory |
| `sai check` | analyze without code generation | 0 | yes | yes | `.sai/` |
| `sai build` | build the target artifact | 1 | yes | yes | `out/`, `.sai/` |
| `sai component build` | build a component | 4 | yes | yes | `out/`, `.sai/` |
| `sai run` | build and run a command component | 4 | yes | yes | `out/`, `.sai/` |
| `sai test` | build and run tests | 3 | yes | yes | `.sai/` |
| `sai fmt` | format source files | 0 | no (§13.6.7) | no | source files |
| `sai add` | add a dependency | 3 | yes | yes | `sai.package`, `sai.lock` |
| `sai remove` | remove a dependency | 3 | yes | yes | `sai.package`, `sai.lock` |
| `sai update` | move dependencies to newer versions | 3 | yes | yes | `sai.lock` |
| `sai lock` | resolve and write `sai.lock` without building | 3 | yes | yes | `sai.lock` |
| `sai fetch` | download every active locked dependency | 3 | yes | yes | the global cache |
| `sai size` | explain the size of the artifact | 2 | yes | yes | `out/`, `.sai/` |
| `sai inspect` | print analysis results | per subcommand | yes | yes | `.sai/` |
| `sai graph` | query the semantic graphs | 6 | yes | yes | `.sai/` |
| `sai diff` | semantic diff between two revisions | 6 | yes | yes | `.sai/` |
| `sai edit` | apply a structured semantic operation | 6 | yes | yes | source files, `.sai/` |
| `sai bindgen wit` | generate WIT, or show WIT from the SAI side | 4 | yes | yes | `out/` |
| `sai explain` | explain a diagnostic code | 0 | no | no | nothing |
| `sai lsp` | language server | 0 (base), 6 (extensions) | no | yes | `.sai/` |

The commands of draft §77 and D-TOOL-03 are all present. `sai lock` is required by [package/02-resolver.md](../package/02-resolver.md) §2.10.4. `sai fetch` and `sai edit` are added by this chapter (N-H-19). A toolchain that does not implement a command's stage MUST reject the command with `FEATURE_NOT_IMPLEMENTED` ([spec/01-introduction.md](../spec/01-introduction.md) §1.4.5), with `primary: null`, `stage` set to the stage of the table, and `feature` set as follows: `semantic-protocol` (the Stage 6 feature name of spec/01 §1.4.5) for `sai graph`, `sai diff`, `sai edit`, and the `sai/*` extensions of `sai lsp`; the command path, such as `"sai run"` or `"sai component build"`, for the commands of Stages 0–4. The command exits with 1.

---

## 13.2 Package discovery

### 13.2.1 The root package

A command that needs a package finds its **root package** ([package/01-package-contract.md](../package/01-package-contract.md) §1.1.1) as follows:

1. The **start directory** is the argument of `--directory` (§13.3.1), resolved against the current directory, or the current directory itself.
2. Starting at the start directory, the CLI looks for a regular file named exactly `sai.package`. If none exists, it moves to the parent directory and repeats, until it reaches the file-system root.
3. The first directory that contains `sai.package` is the **package directory** of the root package.
4. If no directory contains one, the command fails with `NO_PACKAGE_FOUND` and exit code 2. The message names the start directory, and the help suggests `sai new`.

The search stops at the first `sai.package`. It does not look further for an enclosing package. Symbolic links are followed when the start directory is resolved, and the search then walks up the resolved path. A `sai.package` that is a directory or a broken link is ignored by the search.

### 13.2.2 Workspace and workspace members

The **workspace root** is the package directory of the root package. The **workspace members** are the root package plus every package in the resolved graph that is required `from path` ([package/01-package-contract.md](../package/01-package-contract.md) §1.10.3) and whose package directory lies inside the workspace root (N-H-20).

Workspace members are treated like the root package in these respects:

- Their lints are reported ([11-diagnostics.md](11-diagnostics.md) §11.3.3, the dependency cap does not apply to them), with the levels of their own attributes and of the root's `lints` block. A member's own `lints` block is root-only and is ignored ([package/01-package-contract.md](../package/01-package-contract.md) §1.13).
- `sai fmt` without file arguments formats them (§13.6.7).
- `sai check`, `sai test`, `sai graph`, `sai diff`, `sai edit`, and the LSP server analyze their source and index their symbols.

Workspace members are dependencies in every other respect. In particular, their root-only declarations are ignored, and only the root package's `out/` and `.sai/` directories are used. This includes `requires test` ([package/01-package-contract.md](../package/01-package-contract.md) §1.10.6): a member that is not the root has no test edges, so its tests cannot use its own test dependencies. When `sai test` or `sai check --tests` analyzes the tests of such a member (§13.6.6), each of its test-only uses is `TEST_DEPENDENCY_UNAVAILABLE` (defined in [spec/14-testing.md](../spec/14-testing.md) §14.2.5 item 5), and its other tests are still analyzed. Running the command in the member's directory (`-C <member directory>`) makes the member the root. There is no separate workspace manifest: a workspace is a package that happens to depend on packages in its own directory tree (P7).

### 13.2.3 Paths printed by the CLI

Paths in human output are relative to the current directory if the file lies inside the workspace root, and otherwise use the forms of [11-diagnostics.md](11-diagnostics.md) §11.5.4. Paths in JSON output always use the forms of §11.5.4, except `sai new`'s `directory` field, which echoes its explicit path argument as specified in §13.6.1. Separators are `/` on every host.

---

## 13.3 Command-line syntax, output, and exit codes

### 13.3.1 Syntax and global options

```text
sai [GLOBAL-OPTIONS] <command> [<subcommand>] [OPTIONS] [ARGUMENTS] [-- PASSTHROUGH]
```

Parsing rules (N-H-21):

1. Arguments are processed left to right. The first argument that does not start with `-` is the command. For `component`, `bindgen`, and `inspect`, the next such argument is the subcommand.
2. Long options have the form `--name` or `--name <value>`. The form `--name=<value>` is also accepted and means the same thing. Option names are lowercase ASCII words separated by `-`.
3. Short options exist only as aliases of these four options: `-h` (`--help`), `-q` (`--quiet`), `-v` (`--verbose`), and `-C` (`--directory`). Short options cannot be combined (`-qv` is `UNKNOWN_OPTION`).
4. Global options may appear before or after the command. Command options must appear after the command.
   When an option spelling is defined both globally and for the selected command, the command-specific meaning takes precedence after that command. In v0.1, `add --version <requirement>` selects a package version; `--version` before the command prints toolchain version information.
5. An argument `--` ends option processing. Everything after it is passed to the program (`sai run`), to the test filter (`sai test`), or is `UNEXPECTED_ARGUMENT` for other commands.
6. A boolean option given twice is accepted. A value option given twice is `CONFLICTING_OPTIONS`, except where a command says the option is repeatable.
7. Option values that name enumerations (`--format`, `--color`, `--target`, `--profile`, and so on) are matched exactly and are case-sensitive.

Global options:

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--format <f>` | `human`, `json` | `human` | output form (§13.3.3, D-TOOL-01) |
| `--color <c>` | `auto`, `always`, `never` | `auto` | color in human output (§13.3.2) |
| `-C`, `--directory <dir>` | a path | the current directory | start directory of the package search (§13.2.1). Relative paths in other arguments are still resolved against the process's current directory. |
| `-q`, `--quiet` | — | off | suppress the progress lines (§13.3.6) and the summary line ([11-diagnostics.md](11-diagnostics.md) §11.10.8). Diagnostics are still printed. |
| `-v`, `--verbose` | — | off | print progress lines for every package, not only for the root (§13.3.6) |
| `--max-errors <n>` | an integer `>= 0` | `50` | see §13.4.5. Accepted by every command. |
| `-h`, `--help` | — | — | print help for the command and exit with 0 (§13.3.7) |
| `--version` | — | — | print the version and exit with 0 (§13.3.7), except for the command-specific `sai add --version <requirement>` form |

`--quiet` and `--verbose` together are `CONFLICTING_OPTIONS`.

### 13.3.2 Streams and color

- **Standard output** carries the command's result: the formatted text of `sai fmt -`, the table of `sai size`, the Markdown of `sai explain`, the reports of `inspect`, `graph`, and `diff`, and, in human format, the program's own output under `sai run`. With `--format json` it carries the NDJSON stream and nothing else.
- **Standard error** carries diagnostics, progress lines, and the summary line in human format. With `--format json` nothing is written to standard error ([11-diagnostics.md](11-diagnostics.md) §11.9.1). Under `sai run`, the program's output is then captured into the result record (§13.6.5).

Color is used in human output when the first applicable rule applies (N-H-22):

1. `--color always`: yes. `--color never`: no.
2. The environment variable `CLICOLOR_FORCE` is set to a non-empty value other than `0`: yes.
3. The environment variable `NO_COLOR` is set to a non-empty value: no.
4. Otherwise (`auto`), color is used for a stream if that stream is a terminal and `TERM` is not `dumb`. Standard output and standard error are decided separately.

JSON output is never colored. The color codes are those of [11-diagnostics.md](11-diagnostics.md) §11.10.7.

### 13.3.3 JSON output

With `--format json`, every command writes exactly one NDJSON stream as defined in [11-diagnostics.md](11-diagnostics.md) §11.9: a header, diagnostic records, result records, and a summary. Result records have this form:

```json
{"type":"result","kind":"<kind>","data":{...}}
```

The `kind` values and the members of `data`, in their serialization order, are defined with each command in §13.6. All rules of [11](11-diagnostics.md) §11.9.2 (deterministic serialization) and §11.9.3 (versioning) apply to result records too. A command that produces no result writes no result record. A command that fails before it produces a result writes the header, the diagnostics, and the summary.

In the examples and record descriptions of this chapter, `…` stands for a value that is omitted for brevity, such as a SHA-256 digest, and `<…>` names the content of a value. Records that contain them are illustrations, not valid records; every other example record is valid against the schema of §13.8.

Help and version output are not affected by `--format json` (§13.3.7).

### 13.3.4 Exit codes

Every command exits with exactly one of these codes (N-H-23):

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | at least one diagnostic with severity `error` was emitted ([11-diagnostics.md](11-diagnostics.md) §11.3.1) |
| 2 | usage error: the command line is invalid, or no package was found (§13.3.5). The command did nothing. |
| 3 | the command ran correctly and reports a negative result: a test failed (`sai test`), or the program failed or trapped (`sai run`) |
| 4 | the toolchain could not write its own output (standard output, standard error, or a file it had to write) |
| 101 | `INTERNAL_COMPILER_ERROR` (defined in [spec/01-introduction.md](../spec/01-introduction.md) §1.10, reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6) |
| 130 | the command was interrupted (SIGINT on Unix-like hosts, Ctrl-C on Windows) |

If several apply, the first one in this order is used: 130, 101, 4, 2, 1, 3, 0. For example, `sai test` with a compile error in a test exits with 1 even if other tests failed, and an interrupted command exits with 130 even if errors were reported before the interrupt. Warnings and info diagnostics never affect the exit code, unless `--deny-warnings` turns warnings into errors (§13.4.4).

Exit code 4 is reported with `OUTPUT_WRITE_FAILED` when a file cannot be written and standard error is still writable. When standard output fails, the toolchain writes one line `sai: cannot write to standard output: <reason>` to standard error. If standard error fails too, it exits with 4 without a message.

On interrupt, the command stops at the next safe point, leaves no partially written file (§13.5.4), prints the diagnostics found so far, and does not print a summary. With `--format json` it writes the summary record with `exit_code` 130 if standard output is still writable.

### 13.3.5 Usage errors

A **usage error** is an error found in the command line before any work is done. It is reported as a diagnostic with `primary: null` and the `data` object `{ "argument_index": <n>, "argument": "<text>" }`, where `argument_index` is the 1-based position of the offending argument (0 if the problem is a missing argument), and `argument` is its text (empty if missing). When a usage error occurs, the command does nothing else and exits with 2. All usage errors of one command line are reported, not only the first.

The usage error codes are exactly these:

| Code | Condition |
|---|---|
| `UNKNOWN_COMMAND` | the command or subcommand does not exist. Candidates per [11-diagnostics.md](11-diagnostics.md) §11.4.5. |
| `UNKNOWN_OPTION` | the option does not exist for this command. Candidates are drawn from the command's options. |
| `MISSING_ARGUMENT` | a value option has no value, or a required positional argument is missing |
| `INVALID_OPTION_VALUE` | a value is not one of the allowed values, or is not of the required form. The help lists the allowed values. Also a relative `SAI_CACHE_DIR` (§13.7). |
| `UNEXPECTED_ARGUMENT` | a positional argument that the command does not take |
| `CONFLICTING_OPTIONS` | two options that cannot be combined, or a value option given twice |
| `NO_PACKAGE_FOUND` | no `sai.package` was found (§13.2.1) |
| `UNKNOWN_LINT`, `NOT_A_LINT`, `DEPRECATED_DIAGNOSTIC_CODE` | a lint flag names an unknown code, a hard code, or a deprecated code (§13.4.4, [11-diagnostics.md](11-diagnostics.md) §11.12.3). `DEPRECATED_DIAGNOSTIC_CODE` is a warning and does not stop the command. |

An unknown `--target` or `--profile` name is `INVALID_OPTION_VALUE`. A known target that the root package does not list is `TARGET_NOT_DECLARED` ([package/01-package-contract.md](../package/01-package-contract.md) §1.8). That is an error about the package, not about the command line, and exits with 1.

Example:

```text
$ sai build --tagret wasm32-wasi
error[UNKNOWN_OPTION]: unknown option `--tagret` for `sai build`
  = help: a similar option exists: `--target`

build failed: 1 error
```

### 13.3.6 Progress lines

In human format, a command that does work on packages writes **progress lines** to standard error, unless `--quiet` is given. Each line has the form `<verb> <subject>`, with lowercase verbs:

| Verb | Subject | Written when |
|---|---|---|
| `fetching` | `<ns>:<pkg>@<version>` | a package is downloaded into the global cache |
| `resolving` | `dependencies` | resolution runs because `sai.lock` is missing or out of date |
| `checking` | `<ns>:<pkg>@<version>` | a package's source is analyzed |
| `building` | `<ns>:<pkg>@<version> (<target>, <profile>)` | code is generated for the root package |
| `wrote` | `<path> (<n> bytes)` | an output file was written |
| `running` | `<path>` | `sai run` starts the program |
| `formatted` | `<path>` | `sai fmt` changed a file |

Without `--verbose`, `fetching` and `checking` lines are written only for workspace members. With `--verbose`, they are written for every package. Progress lines are written in a deterministic order: packages in the order of [11-diagnostics.md](11-diagnostics.md) §11.11.1 rule 2, then output files in byte order of their path. An implementation that works in parallel buffers the lines to keep this order. Progress lines contain no timings, no counters, and no terminal control sequences other than color. They are never rewritten in place (P8).

With `--format json`, there are no progress lines. A command reports the files it wrote in its result record.

### 13.3.7 Help and version

- `sai --help` and `sai help` print the list of commands with a one-line description each, in the order of §13.1.2, to standard output, and exit with 0. `sai` without arguments prints the same text to standard error and exits with 2, because no command was given.
- `sai <command> --help` and `sai help <command>` print the command's synopsis, a one-line description, and its options in the order of this chapter's option tables. The text is informative and may change between versions. It is not affected by `--format`.
- `sai --version` prints one line to standard output: `sai <version>`, where `<version>` is the toolchain version. In v0.1 the toolchain version is also the language version that `compiler` requirements are checked against ([package/01-package-contract.md](../package/01-package-contract.md) §1.5) (N-H-24). With `--verbose`, three more lines follow, in this order: `language <version>`, `wasi <version>` (the newest WASI 0.2 release the toolchain supports), and `targets <names>`, with the target names separated by single spaces in the order of [targets/targets-and-profiles.md](../targets/targets-and-profiles.md).

```text
$ sai --version --verbose
sai 0.1.0
language 0.1.0
wasi 0.2.3
targets wasm32-core wasm64-core wasm32-wasi
```

---

## 13.4 Options shared by building commands

The **building commands** are `check`, `build`, `component build`, `run`, `test`, `size`, `inspect`, `graph`, `diff`, `edit`, `bindgen wit`, and `lsp`. They accept the options of this section unless a command says otherwise.

### 13.4.1 `--target <name>`

- Selects the target ([targets/targets-and-profiles.md](../targets/targets-and-profiles.md), D-TGT-01). The value MUST be a target name known to the toolchain (`INVALID_OPTION_VALUE`), and the root package MUST list it in `targets` (`TARGET_NOT_DECLARED`, [package/01-package-contract.md](../package/01-package-contract.md) §1.8).
- **Default**: the first target listed in the root package's `targets` declaration (N-H-25). With `targets { wasm32-core, wasm32-wasi }`, `sai build` builds for `wasm32-core`.
- Exactly one target per invocation. Building for several targets takes several invocations. Repeating `--target` is `CONFLICTING_OPTIONS`.
- The target affects analysis as well as code generation, because dependencies can be target-specific and capabilities are checked per target ([package/01-package-contract.md](../package/01-package-contract.md) §1.10.5, §1.8 rule 5). `sai check` therefore checks one target too.

### 13.4.2 `--profile <name>`

- Selects the profile: `dev`, `release`, or `tiny` (D-TGT-02). Any other value is `INVALID_OPTION_VALUE`. User-defined profiles do not exist in v0.1.
- `--profile` is the only way to select a profile (C-06). There is no `--release` flag.
- **Default**: `dev`, for every command (N-H-25).
- The profile affects analysis: its default denials (D-TGT-02, [targets](../targets/targets-and-profiles.md) §5.4) can reject a build for capability reasons, and `budget` limits are checked after code generation ([package/01-package-contract.md](../package/01-package-contract.md) §1.14). A profile `allow` is not a restriction: it only lifts the profile's own default denials, within the package-level `allow` ([targets](../targets/targets-and-profiles.md) §5.5, N-G-15). Profiles never change the meaning of an accepted program (C-03).

### 13.4.3 `--locked`, `--offline`, and `--frozen`

The semantics of resolution, locking, and fetching are owned by [package/02-resolver.md](../package/02-resolver.md), [package/03-lock-file.md](../package/03-lock-file.md), and [package/04-distribution-and-security.md](../package/04-distribution-and-security.md). This chapter defines the flags and relies on the following behavior of those documents:

- **Default.** A building command reads `sai.lock`. If the lock file is missing, or does not match `sai.package` (a requirement was added, removed, or changed), the command resolves the graph and writes a new `sai.lock` before it continues, and writes the progress line `resolving dependencies`. Resolution by MVS uses only the versions named by requirements, so it never lists tags (D-PKG-02). Only `sai update` and `sai add` look for newer releases.
- **`--locked`.** The command MUST NOT write `sai.lock`. If the lock file is missing, the command fails with `LOCK_MISSING`, and if it would change, with `LOCK_OUT_OF_DATE` ([package/03-lock-file.md](../package/03-lock-file.md) §3.8.1), before any other analysis. CI configurations use `--locked` to make sure the committed lock file is the one that is used.
- **`--offline`.** The command MUST NOT access the network. Every package it needs must already be in the global cache (D-PKG-04: the standard packages are pre-populated). A package that would have to be fetched is `OFFLINE_FETCH_REQUIRED` (defined in [package/03-lock-file.md](../package/03-lock-file.md) §3.8.2). `sai fetch` (§13.6.13) fills the cache ahead of time.
- **`--frozen`** means `--locked` and `--offline` together (package/03 §3.8). Giving `--frozen` with either of them is accepted and changes nothing.
- Checksums are always verified, and a mismatch always rejects the build (D-PKG-03). No flag disables this.

Every building command, `sai add`, `sai remove`, `sai update`, `sai lock`, and `sai fetch` accept all three flags. The commands that exist to change the lock fail under `--locked` or `--frozen` with `LOCK_OUT_OF_DATE` as soon as a change would be written (package/03 §3.8.1 rule 4). So `sai lock --locked` checks that the committed lock is up to date and writes nothing. `--offline` does not prevent writing the lock (package/03 §3.8.2 rule 5).

### 13.4.4 Lint flags

| Option | Meaning |
|---|---|
| `--allow <CODE>` | set the level of the lint `CODE` to `allow` |
| `--warn <CODE>` | set the level of the lint `CODE` to `warn` |
| `--deny <CODE>` | set the level of the lint `CODE` to `deny` |
| `--deny-warnings` | after level resolution and the dependency cap, turn every remaining warning, lint or hard warning, into an error; infos are not changed ([11-diagnostics.md](11-diagnostics.md) §11.3.3 rule 2, [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.16.7) |

1. `--allow`, `--warn`, and `--deny` are repeatable. Each occurrence takes exactly one name. A name is anything that a level attribute accepts: a single lint code. There are no lint groups ([15-attributes-and-lints.md](../spec/15-attributes-and-lints.md) §15.15.5, N-A-47). Comma-separated lists are not accepted (P9).
2. A name that is not a lint is `UNKNOWN_LINT` or `NOT_A_LINT`, and a deprecated name is `DEPRECATED_DIAGNOSTIC_CODE` ([11-diagnostics.md](11-diagnostics.md) §11.12.3). The first two are usage errors (§13.3.5).
3. Naming the same code in two different level flags is `CONFLICTING_OPTIONS` (N-H-26). The command line has no internal precedence, so the order of the flags never matters. Naming the same code twice with the same level is accepted.
4. The flags form the level source `cli` ([11-diagnostics.md](11-diagnostics.md) §11.3.4). How that source ranks against attributes, the `lints` block, and `policy` is defined by [15-attributes-and-lints.md](../spec/15-attributes-and-lints.md). `--deny-warnings` is not a level source: it is applied after all levels are resolved, so no attribute can override it.
5. Lint flags apply to the root package and the workspace members only. Lints in other dependencies are dropped whatever their level ([11-diagnostics.md](11-diagnostics.md) §11.3.3 rule 1).

### 13.4.5 `--max-errors <n>`, `--jobs <n>`

- `--max-errors <n>` limits the number of error diagnostics that are printed ([11-diagnostics.md](11-diagnostics.md) §11.11.3). The default is 50, and 0 means no limit. The limit never changes the exit code, the summary counts, or the work done. It is a global option (§13.3.1).
- `--jobs <n>` sets the maximum number of worker threads. `n` MUST be at least 1. The default is the number of logical processors available to the process. The value never changes any output (P8).

`--fix` is accepted only by `sai check` (§13.6.2).

### 13.4.6 Approval: `--approve`

A resolution can produce changes that need **approval**: a capability escalation (`CAPABILITY_ESCALATION`), a source change (`SOURCE_CHANGED`), or, for `sai update`, the replacement of the content of a locked version whose release tag has moved (`TAG_MOVED`) ([package/02-resolver.md](../package/02-resolver.md) §2.12.3, [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.3.3 rule 4, §4.7.2). Item 4 of [package/02](../package/02-resolver.md) §2.12.3 lets an interactive command ask, and leaves the non-interactive form to this chapter (N-H-30):

1. With `--approve`, every such change is approved. The review is still printed, and the command continues.
2. Without `--approve`, the command is **interactive** if `--format` is `human` and both standard input and standard error are terminals. An interactive command prints the review, then writes the prompt `approve these changes? [y/N] ` to standard error and reads one line from standard input. The answers `y` and `yes`, in any letter case and without surrounding spaces, approve. Any other answer, including an empty line or end of input, refuses.
3. A non-interactive command without `--approve`, and an interactive one whose user refuses, reports each change as an error with its code, writes neither `sai.lock` nor `sai.package`, and exits with 1. The help of each error says to run the command again with `--approve` after reading the review.
4. The new lock file is the record of the approval (package/02 §2.12.3 rule 5). A resolution in Mode A never needs approval, and so `--locked` and `--frozen` builds never ask.
5. `--approve` is accepted by every command that may resolve: the building commands, `sai add`, `sai remove`, `sai update`, `sai lock`, and `sai fetch`. With `--locked` or `--frozen` it has no effect.

Agents and CI use `--format json`, which is never interactive, together with `--approve` after they have checked the `review` records (§13.6.8).


### 13.4.7 `--wasm-feature <name>`

Enables an opt-in Wasm feature of [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) §6.2.1 for this build (N-H-38).

1. The values are exactly `tail-call` and `simd128`. Any other value, including a feature that impl/06 marks "Used" or "Never", is `INVALID_OPTION_VALUE`, and the help lists the two accepted names.
2. The option is repeatable, and each occurrence names one feature. Naming a feature twice is accepted. The default is that no opt-in feature is enabled, for every target and every profile. No profile, target, `sai.package` declaration, or environment variable enables one.
3. The option is accepted by `sai build`, `sai component build`, `sai run`, `sai test`, and `sai size`, the commands that generate code. Other commands reject it with `UNKNOWN_OPTION`.
4. An enabled feature never changes the meaning of a program (impl/06 §6.2.2). It changes only the instructions that impl/06 lists and the `target_features` section. The set of enabled features is therefore one more input of the build for §13.5.3: the output is byte-identical for the same feature set, and the output path does not change (`out/<target>/<profile>/`).
5. The JSON `build` record (§13.6.3) lists the enabled features in `wasm_features`, sorted by name.

---

## 13.5 Files written by the toolchain

### 13.5.1 Build outputs: `out/`

Build outputs go to `out/<target>/<profile>/` below the package directory of the root package (D-TOOL-03). The file names are fixed (N-H-27). `<name>` is the name part of the root package's identity: `image` for `acme:image`.

| File | Written by | Content |
|---|---|---|
| `out/<target>/<profile>/<name>.wasm` | `sai build`, `sai run`, and `sai size` for a Wasm target, and `sai component build` for `wasm32-wasi` / `wasm64-wasi` | the core module (core targets) or the component (WASI targets) |
| `out/<target>/<profile>/<name>.component.wasm` | `sai component build` for `wasm32-core` / `wasm64-core` | the component that wraps the core module (D-TGT-01, [spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.6.2) |
| `out/<target>/wit/<name>.wit` | `sai bindgen wit` | the generated WIT package ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.11) |
| `out/<target>/<profile>/<name>`, and `out/<target>/<profile>/<name>.exe` on `x86_64-windows` | `sai build` for a native target (Stage 7) | the native executable ([targets](../targets/targets-and-profiles.md) §2.8.1, §2.8.5) |

- For a WASI target, `sai build` and `sai component build` produce the same component and write the same file.
- `sai component build` for a core target also writes `<name>.wasm`, the core module that the component wraps.
- The generated WIT does not depend on the profile, so it is written below `out/<target>/wit/`. It depends on the target, because WASI references carry the target's WASI version ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.11.4).
- Test harness modules are not build outputs. They are kept in `.sai/cache/`.
- The toolchain writes only the files listed here. It never deletes files in `out/`, and it leaves files it did not write untouched. A failed build writes nothing and leaves the previous output in place ([11-diagnostics.md](11-diagnostics.md) §11.3.1). This includes a build whose only errors are `SIZE_BUDGET_EXCEEDED` ([package/01-package-contract.md](../package/01-package-contract.md) §1.14.3 item 4) or warnings that `--deny-warnings` turned into errors (§13.4.4).

### 13.5.2 Tooling state: `.sai/`

Tooling state goes to `.sai/` below the package directory of the root package, separate from the build outputs (draft §65, D-TOOL-03):

| Path | Content | Format |
|---|---|---|
| `.sai/index/` | the persistent symbol index (D-TOOL-02) | [12-semantic-tooling.md](12-semantic-tooling.md) §12.9 |
| `.sai/cache/` | incremental-compilation results, size attribution tables, test harness modules, and other derived data | private to the toolchain version |
| `.sai/lock` | an empty file used for the workspace lock (§13.5.4) | — |

- Nothing in `.sai/` is needed for build correctness (draft §64). Deleting `.sai/` never changes the result of a command. It only costs time, and it loses the symbol identities that survive renames ([12-semantic-tooling.md](12-semantic-tooling.md) §12.2.4).
- The content of `.sai/cache/` is valid only for the toolchain version that wrote it. A toolchain that finds a cache written by another version, or a cache it cannot read, discards it without a diagnostic.
- `.sai/` and `out/` are excluded from package checksums ([package/01-package-contract.md](../package/01-package-contract.md) §1.6.1 rule 6).
- `sai new` writes a `.gitignore` that lists both directories (§13.6.1). `.sai/index/` is local tooling state and is not meant to be committed (N-H-28).

### 13.5.3 Reproducibility

A build is determined by the source of all packages in the graph, `sai.package`, `sai.lock`, the toolchain version, the target, and the profile (draft §72). For the reference implementation, the files of §13.5.1 MUST be byte-identical whenever these inputs are identical (N-H-29). In particular, they MUST NOT depend on:

- absolute paths: the name section and every other embedded path use the canonical paths of [spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9 and the source references of [11-diagnostics.md](11-diagnostics.md) §11.5.4;
- the time, the date, the host, the user, or environment variables;
- `--jobs`, the order of work, or the content of `.sai/` and of the global cache, beyond the verified package contents;
- whether the build was incremental.

### 13.5.4 Atomic writes and concurrent commands

- Every file the toolchain writes (outputs, `sai.package`, `sai.lock`, source files changed by `fmt`, `--fix`, `add`, `remove`, `update`, and `edit`) is written to a temporary file in the same directory and then renamed over the target. A reader never sees a partially written file.
- A command that writes to `out/`, `.sai/`, `sai.package`, `sai.lock`, or source files holds an exclusive lock on `.sai/lock` for its whole duration. Commands that only read hold a shared lock. A command that has to wait writes the progress line `waiting for .sai/lock` once and then blocks. The LSP server holds a shared lock and takes the exclusive lock only while it writes.
- `sai new` is the exception: it does not discover or modify an existing package, so it cannot acquire that package's `.sai/lock`. It claims a new destination with atomic directory creation, or an existing empty destination by atomically creating `src`; a concurrent invocation that loses this claim fails with `DESTINATION_NOT_EMPTY`. It writes each file to a same-directory temporary file, publishes it with a no-replace rename, and writes `sai.package` last as the package-discovery marker. On failure or interruption, it removes files whose identities still match the files it published. It does not synchronize with commands operating on an enclosing package; the new package is not part of that package's graph unless explicitly required.
- A command that changes `sai.package` edits the CST it parsed. The declarations it changes are written in the canonical layout of [package/01-package-contract.md](../package/01-package-contract.md) §1.3.1 rule 6, and the rest of the file, including comments, is kept byte for byte.
- Locking of the global package cache is owned by [package/04-distribution-and-security.md](../package/04-distribution-and-security.md).

---

## 13.6 Commands

Each command is described by a synopsis, its options (in addition to the global options of §13.3.1 and, for building commands, the options of §13.4), its behavior, its human output, its JSON result records, and its exit codes. Exit codes not listed for a command (2 for usage errors, 4, 101, 130) apply to every command as defined in §13.3.4.

JSON result records use the conventions of [11-diagnostics.md](11-diagnostics.md) §11.9.2. In addition (N-H-31):

- A **package reference** is the string `<ns>:<pkg>@<version>`, for example `"acme:image@0.1.0"`.
- A **file record** describes a file the command wrote: `{"path":"out/wasm32-wasi/dev/image.wasm","bytes":20480,"sha256":"<64 lowercase hex digits>"}`. `path` uses the forms of [11-diagnostics.md](11-diagnostics.md) §11.5.4.
- Arrays are in the order stated by each command. Where no order is stated, they are sorted by the byte order of their first member.

The JSON Schema of every result kind is given in §13.8.

### 13.6.1 `sai new`

```text
sai new <path> --name <ns>:<pkg> [--target <name>]...
```

Creates a new library package in the directory `<path>`. Stage 0. The command does not need, and does not look for, an enclosing package.

| Option | Meaning |
|---|---|
| `--name <ns>:<pkg>` | the package identity. Required (`MISSING_ARGUMENT`). It MUST satisfy [package/01-package-contract.md](../package/01-package-contract.md) §1.4 rules 1–4; otherwise it is `INVALID_OPTION_VALUE`, and the help names the violated rule. |
| `--target <name>` | a target to declare in `targets`. Repeatable; the order is kept. Default: `wasm32-wasi` alone. An unknown name is `INVALID_OPTION_VALUE`; a repeated name is `CONFLICTING_OPTIONS`. |

Here `--target` declares targets. It does not select one, because `sai new` is not a building command.

Behavior:

1. `<path>` MUST NOT exist, or MUST be an empty directory. Otherwise the command fails with `DESTINATION_NOT_EMPTY` (exit code 1) and writes nothing.
2. The command creates `<path>` and `<path>/src/` and writes exactly these three files, with LF line endings and one final newline. `<ns>:<pkg>` is the given identity, `<L>` is the toolchain's language version reduced to `MAJOR.MINOR` (`0.1` for 0.1.0), and `<targets>` are the declared targets separated by `, `.

   `sai.package`:

   ```sai-package
   package <ns>:<pkg> @ 0.1.0
   compiler ^<L>
   source "src"

   targets { <targets> }
   ```

   If `targets { <targets> }` does not fit in 100 columns, it is written one target per line, as [package/01-package-contract.md](../package/01-package-contract.md) §1.3.1 rule 6 requires.

   `.gitignore`:

   ```text
   /out/
   /.sai/
   ```

   `src/lib.sai`:

   ```sai
   pub fn add(a: I32, b: I32) -> I32 {
       a + b
   }

   test "add adds two numbers" {
       assert add(2, 3) == 5
   }
   ```

3. The new package is a library: its root module has no entry point. Adding `pub fn main(env: owned process.Env) -> process.ExitCode` to `src/lib.sai` makes its WASI build a command component ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.5.3). There is one template, and there are no template options (N-H-32).
4. No `sai.lock` is written: the package has no requirements, and the first building command writes the lock (§13.4.3).
5. If `<path>` lies inside the package directory of another package, the new package is not added to it. It becomes a workspace member only when that package requires it `from path` (§13.2.2).

Human output: three progress lines `wrote <path> (<n> bytes)`, in the order `sai.package`, `.gitignore`, `src/lib.sai`, each path relative to the current directory.

JSON result: one record of kind `new`.

```json
{"type":"result","kind":"new","data":{"package":"acme:image@0.1.0","directory":"image","files":[{"path":"sai.package","bytes":79,"sha256":"…"},{"path":".gitignore","bytes":13,"sha256":"…"},{"path":"src/lib.sai","bytes":107,"sha256":"…"}]}}
```

`directory` is `<path>` as given, with `/` separators and without a trailing `/`; an absolute path supplied by the caller is echoed here. Each `files[].path` is relative to the new package directory, with `/` separators and no `.` or `..` segments. The files appear in the order of item 2. Human progress paths are relative to the current directory.

Exit codes: 0 on success; 1 for `DESTINATION_NOT_EMPTY`.

### 13.6.2 `sai check`

```text
sai check [--target <name>] [--profile <name>] [--tests] [--fix] [BUILD-OPTIONS]
```

Runs every analysis of a build except code generation, for the root package and every active dependency (draft §78). Stage 0; analyses of later stages are run as far as the toolchain implements them (§13.1.2). `BUILD-OPTIONS` are the options of §13.4.

| Option | Meaning |
|---|---|
| `--tests` | check the package as `sai test` sees it: test edges are active ([package/02-resolver.md](../package/02-resolver.md) §2.5.3), and test declarations and test-only code are analyzed ([spec/14-testing.md](../spec/14-testing.md)). Without `--tests`, the package is checked as `sai build` sees it. |
| `--fix` | apply machine-applicable fixes, as defined in [11-diagnostics.md](11-diagnostics.md) §11.7.5 |

Behavior:

1. The command resolves dependencies (§13.4.3) and then runs, for the selected target and profile: parsing, `sai.package` validation, name resolution, type checking, ownership and borrow checking, effect inference and checking, the capability checks of [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.6, contract checking, the component checks that do not need an artifact, and every lint.
2. It does not generate code. Therefore it does not report diagnostics that need generated code: size budgets (`SIZE_BUDGET_EXCEEDED`, [package/01-package-contract.md](../package/01-package-contract.md) §1.14), `NO_COMPONENT_EXPORTS` ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.5.3), and other diagnostics that their owning chapter assigns to artifact production. Every other error that `sai build` with the same options would report, `sai check` reports too (N-H-33).
3. With `--fix`, fixes are applied only to files of the root package and the workspace members (source files and `sai.package`). A fix with an edit in any other file is not applied. The command writes the files changed by the fixes and prints the diagnostics of the final round. `--fix` does not format the changed files.
4. The command writes `.sai/index/` and `.sai/cache/`, and `sai.lock` as §13.4.3 allows. It writes nothing to `out/`.

Human output: the progress lines `resolving` and `checking` (§13.3.6), the diagnostics, the summary line ([11-diagnostics.md](11-diagnostics.md) §11.10.8), and, with `--fix`, one line `fixed <n> problems in <m> files` before the summary line if `n > 0`.

JSON result: one record of kind `check`, after the diagnostics.

```json
{"type":"result","kind":"check","data":{"target":"wasm32-wasi","profile":"dev","tests":false,"packages":4,"fixes_applied":2,"changed_files":["src/user.sai"]}}
```

`packages` is the number of packages analyzed (the root and its active dependencies). `fixes_applied` is the number of applied fixes, and `changed_files` lists the changed files in byte order. Without `--fix`, `fixes_applied` is 0 and `changed_files` is empty.

Exit codes: 0 if no error was reported; 1 otherwise.

**Current implementation slice.** `sai check` parses visible root-package `.sai` files, validates the module tree, and performs target-filtered Phase 1 declaration collection for the selected target. Under `--locked` and `--frozen`, it also reads the active graph's source-form dependencies and includes their visible `.sai` files in the same VFS input and workspace snapshot. Each dependency receives its own module-tree validation and Phase 1 declaration pass with dependency role; it does not inherit root-package core-export checks or root `--tests` analysis. Active source package scopes are finalized dependency-first, and each consumer resolves module aliases, item imports and `pub use` chains through the exact active direct source edge. Foreign items keep their original canonical `Res`/`DefId`; ordinary imports and re-exports apply package visibility checks. The root and active source dependencies share bounded workspace budgets for source bytes, source files, tree entries, and inspected path bytes. Artifact-form dependencies and WIT package scopes remain gated. The target defaults to the first manifest target; `--target` selects a known target and reports `TARGET_NOT_DECLARED` when the package omits it. Phase 1 reports declaration, test/property-name, and interface/module-file conflict diagnostics, with earlier declarations linked as related locations. It validates every surviving `@export_name` against its Stage 1 target rules without enabling other Stage 1 attributes. For ordinary `wasm32-core` and `wasm64-core` checks, it collects root-package `export fn` names and reports duplicate or reserved `memory` names after target filtering. Component builds and `--tests` skip package export-table uniqueness because those outputs do not use the root package's core export namespace.

The driver reports local current-module and unknown-module `use` targets, then binds successfully resolved local module aliases and aliases to interfaces declared in the target's parent module. It builds direct member tables for local interfaces and resolves interface item imports alongside local and foreign module item imports and local/foreign `pub use` re-export chains, preserving each definition's canonical `Res`/`DefId`. Foreign module aliases use the selected dependency's canonical module definition. Ordinary foreign imports require externally visible items; `pub use` requires public visibility and adds only a successful re-export to the consumer's member table. A foreign interface target is recognized separately and retains its Stage 4 gate, with private foreign interfaces diagnosed as `PRIVATE_ITEM`; foreign interface member imports remain deferred. Local interface members remain outside the containing module's `members`; a successful public interface-member `pub use` enters the re-exporting module's `members`, while private interface members cannot be publicly re-exported. Effective member visibility is inherited from the interface. Re-export cycles are reported once, and module aliases and item imports are checked in source order so the later conflicting binding receives `DUPLICATE_BINDING`. Same-entity imports produce `REDUNDANT_IMPORT`, and unresolved items include up to three nearby names from the target's resolved member namespace. Explicit aliases retain their spelling; otherwise the final path segment or package name supplies the binding, with hyphens changed to underscores. For parseable use paths, binding-name keyword/reserved-name checks run before target resolution, including for unknown and foreign targets; `pub use` without an item list is diagnosed before malformed-path resolution is deferred.

The package-scope bridge is memoized by the driver's `package_scopes(PackageKey)` query. Its identity includes package topology, exact module source inputs, target facts, build context, and the package keys and fingerprints of direct foreign scopes. Workspace-bound diagnostics are rebound to each request's snapshot. Dependency source changes and transitive scope changes therefore invalidate consumers without incorporating unrelated graph branches.

For every unlocked invocation with a syntax-valid root manifest, `sai check` also runs root package semantic validation against the current toolchain version and reports its errors and warnings. Locked and frozen invocations use the lock preflight's existing validation path instead, avoiding duplicate diagnostics. Dependency manifest validation remains part of the locked and frozen path; the other dependency-complete analysis requirements remain gated where unsupported.

Phase 1 declarations and successfully inserted local aliases or item imports are checked against the shadowable-name table in spec/16 §16.4 for `SHADOWS_PRELUDE`; reserved names receive `RESERVED_TYPE_NAME` instead. Same-entity local item imports from the exact `sai:core` or `sai:alloc` package produce `REDUNDANT_IMPORT` after canonical definition-path comparison. The module-target alias grammar is lowercase, so only `panic` and `unreachable` can collide in that form. The complete canonical prelude table remains unavailable. Stage 4 worlds, foreign interface members, artifacts and WIT remain gated by `FEATURE_NOT_IMPLEMENTED`.

When an inactive or otherwise deferred foreign/interface candidate precedes a known local re-export candidate with the same name, this temporary bridge uses the successful local result. General function-body type checking, lints, and fixes remain unavailable and are represented by `FEATURE_NOT_IMPLEMENTED`, with the narrow function-body slices below as the exception.

The successful `check` paths are deliberately narrow. A complete single root package may contain a zero-byte root `lib.sai` with no declarations or `use` imports after Phase 1; one or more public, attribute-free root `const` declarations in a source file with no module header, `use` imports, tests, or properties, where every top-level source item is one of those constants; or one public, attribute-free function in a source file with no module header, `use` imports, tests, or properties. Each constant's name must be written without a raw-identifier prefix and use ASCII UPPER_SNAKE_CASE, and its direct type must be `Bool`, `Char`, `Byte`, a fixed-width integer (`I8` through `I64`, or `U8` through `U64`), `ISize`, `USize`, `F32`, `F64`, the canonical prelude type `Str`, or the unit type `()`. The `Str` case is accepted only for the exact bare source spelling `Str` when early resolution defers that path and the canonical prelude table identifies `sai:core/str::Str`; this narrow fallback is needed because this unlocked success slice has no package-member scopes. It accepts only a direct HIR string literal; a byte-string literal supplied for `Str` reports `TYPE_MISMATCH`, while a byte-string constant with type `Slice<Byte>` remains gated. Unit is accepted only when HIR represents both the type and initializer as empty tuples (`Tuple([])`). A direct scalar literal with unit type reports `TYPE_MISMATCH`; blocks, aliases, and other unit-valued expressions remain gated. A nonempty tuple value supplied for a scalar or unit type also remains gated because this slice does not infer tuple field types without an expected tuple type. Nonempty flat tuples are accepted only when their declared type and initializer have the same arity and every field has a supported primitive type or canonical `Str` and a direct scalar literal, or the unit type and literal `()`. A direct scalar literal supplied for a tuple type reports `TYPE_MISMATCH`; arity and field type mismatches report `TYPE_MISMATCH`, while out-of-range numeric fields retain their literal-range diagnostic. Nonempty nested tuples, arrays, aliases, blocks, and other field expressions remain gated. The transparent built-in aliases `Int` and `UInt` are also covered because they resolve to `I64` and `U64`; user-defined aliases remain unsupported. Each initializer must be a supported direct literal, the exact unit literal, or a supported flat tuple literal, and every source declaration must match one active Phase 1 const owner with a supported literal check. The literal checker uses resolved primitive identity for primitive types, accepts matching boolean, character, and byte literals, checks integer values against the primitive's mathematical bounds including a directly negated literal, and rounds decimal floats directly to their declared width using IEEE 754 roundTiesToEven. For `ISize` and `USize`, those bounds use the selected target's pointer width (32 or 64 bits); `Int` and `UInt` remain resolved `I64` and `U64`. A float that rounds to infinity reports `LITERAL_OUT_OF_RANGE`; a nonzero float that rounds to zero is accepted. Character values are decoded Unicode scalar values; byte values are decoded bytes in `0..=255`, with the lexer enforcing the character and byte escape rules in spec/02 §2.6.3. A type mismatch or out-of-range value reports its specific diagnostic and does not emit a result. Duplicate declarations report their normal duplicate-binding diagnostic and do not emit a result. Expressions other than the supported direct literals, unit literal, and flat tuple literals; private or attributed constants, raw identifiers, non-ASCII or non-UPPER_SNAKE_CASE names, references, non-`Str` types, and every other source shape retain the `FEATURE_NOT_IMPLEMENTED` gate. Custom lint levels and `--deny-warnings` also retain the gate because the general lint checker is not implemented. All success paths require one complete root source module, no active dependency graph or WIT inputs, and no manifest semantics beyond compiler/source/targets and descriptive metadata. The package validator checks the compiler requirement and every declared target before success. The command is limited to the default `dev` profile and `wasm32-core` target, without `--locked`, `--frozen`, `--tests`, or `--fix`. A successful check emits the specified result record. Additional modules, dependencies, WIT/artifact configuration, other targets/profiles, and incomplete inputs retain the gate. This does not establish Stage 0 conformance or enable general checking of non-empty programs.

For `--locked` and `--frozen`, `sai check` holds the shared workspace lock while it reads the root manifest and lockfile. Before returning `LOCK_MISSING`, it reads and parses the bounded root manifest to preserve a package identity from an available header; manifest diagnostics are suppressed so missing-lock precedence is preserved, and malformed manifests without a valid header keep `root` and `package` null. A present lock receives syntax, local-validity, and complete-graph validation before the CLI compares the root contract. Dependency-bearing format-1 locks return `LOCK_OUT_OF_DATE` with guidance to run `sai lock`; format 1 does not contain the source and grant metadata needed for locked dependency validation. For format 2, the CLI plans every entry's source against the complete lock graph and current root overrides before target/test activation pruning. It resolves path pins from their locked requirers, checks current contracts for every path-pinned package including inactive packages, and fetches Git path pins by their exact locked commit through the verified cache. Active Git packages are loaded by locked commit and their included contract edges are compared with the lock; active local path packages are read from their current directories. `--offline` and `--frozen` prohibit network access and require each needed Git source to be in the verified cache. After source checks, the CLI activates the selected target and `--tests` graph, reports active dependency compiler incompatibility as `COMPILER_VERSION_MISMATCH` at the dependency manifest, derives source/artifact metadata for active dependencies, and rejects artifact-only allocators with `ALLOCATOR_INTERFACE_MISMATCH` and `use artifact` overrides on non-hybrid packages with `INVALID_OVERRIDE`. It then adapts the fully validated graph to the driver's `PackageGraph`. For every active source-form dependency, the CLI safely snapshots the manifest-declared source tree through its retained package-directory capability, rejects source-root symlinks, does not follow inner symlinks, and retains only visible `.sai` source bytes. It builds each dependency's module tree and target-filtered Phase 1 declarations in one workspace snapshot with the root package, and continues to report dependency source syntax errors when the root source is missing or the root package is artifact-only. When scopes can be collected, it resolves each active source package after its dependencies so root imports see finalized member and re-export tables. A shared budget bounds the complete check workspace to 256 MiB of source bytes, 100,000 `.sai` files, 250,000 inspected tree entries, and 64 MiB of inspected path bytes. Root enumeration stops when its remaining budget is exhausted; dependency readers receive each remaining budget before scanning and report work performed even when a snapshot fails, so discarded reads cannot be charged again to the root or a later dependency. VFS inputs share the snapshot's source-byte allocations instead of copying dependency payloads. Under `--locked` and `--frozen`, the root's declared WIT directory and active artifact-selected dependencies' WIT directories are captured into the same VFS snapshot; this includes source-selected hybrids that declare artifact components. Loader content failures are emitted once for the package that owns the failing `Dir` unit, with dependency paths under `dep:<identity>@<version>/...`; parser/model locations are included when the pinned parser exposes them, while `Resolve::push` validation failures remain locationless. If the root manifest itself is syntactically invalid, the CLI still validates a present lock locally, but cannot compare the root contract. WIT package scopes, later source/type analyses, and successful `check` result records remain incomplete and are reported through `FEATURE_NOT_IMPLEMENTED`.

For each inspected dependency, syntax, package-header identity/version, and lock-edge contract checks precede compiler compatibility diagnostics. The toolchain version is checked only for active dependencies. An inactive path pin is still checked against the lock contract, but an incompatible compiler requirement on that inactive package does not block `check`.

The function-body success paths accept only safe, synchronous, non-generic public functions with no `where`, `borrows`, `effects`, `requires`, or `ensures` clauses and a statement-free block. One form has exactly one named immutable parameter and a plain path tail that resolves to that parameter binding. The other has no parameters, an explicit supported primitive result type, and a direct boolean, character, byte, integer, or floating-point literal tail. Both forms compare against resolved type identities; the literal form reuses const-literal mismatch and range diagnostics, including target-width `ISize`/`USize` bounds. The transparent prelude aliases `Int` and `UInt` are supported as `I64` and `U64`; user-defined aliases are gated. Omitted and unit returns, `Str`, byte-string literals, attributes, operators, statements, and other body forms remain gated. When the expected result is unit in the one-parameter form, its non-unit tail is discarded as specified in spec/06 §6.11.1. A type mismatch or out-of-range literal reports its diagnostic at the tail and suppresses the result. Separately, the check walks active Phase 1 owner descriptors in every complete source package and reports `NON_ASCII_PUBLIC_NAME` for `pub`/`export` items, `pub` fields, variants and fields of `pub` enums, items and members of public traits, inherent `pub` methods, interfaces and worlds with every locally declared WIT name (including function parameters), and active public re-export names. Trait-impl members are checked at the trait declaration to avoid a duplicate report. Module and file names continue to be checked by module-tree validation. This pass is independent of the function body shape and attributes, covers additional source modules and active dependencies, and does not report items removed by the selected target. An unsupported input may therefore report both `NON_ASCII_PUBLIC_NAME` and `FEATURE_NOT_IMPLEMENTED`. Non-ASCII parameter names outside interfaces and worlds remain gated because the `NON_ASCII_IDENTIFIER` lint pass is unavailable. Names that could trigger `NAMING_CONVENTION`, mutable parameters that could trigger `UNNECESSARY_VAR`, unnecessary raw identifiers, user-defined type aliases, and type paths written with unnecessary raw-identifier prefixes remain gated. The checker matches a parameter path by resolved binding ID and does not provide general body type checking.

CLI regression tests also verify active Git contract mismatches, offline cache misses that identify the locked URL and commit, and corrupt-cache removal with both `CACHE_CORRUPT` and `OFFLINE_FETCH_REQUIRED` diagnostics.

Locked dependency-source regressions verify dependency syntax and Phase 1 diagnostics with `dep:<identity>@<version>/...` paths, including when the root source directory is missing or the root package is artifact-only.

### 13.6.3 `sai build`

```text
sai build [--target <name>] [--profile <name>] [BUILD-OPTIONS]
```

Builds the artifact of the root package for one target and one profile. Stage 1 (core targets); Stage 4 for WASI targets; Stage 7 for native targets.

Behavior:

1. The command runs the analyses of `sai check` (without `--tests`) and then generates code for the root package and every active dependency.
2. The artifact is the one of [spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.6.2: on `wasm32-core` and `wasm64-core`, a core module whose exports are the `export fn` items of the root package (D-MOD-02); on `wasm32-wasi` and `wasm64-wasi`, a component, which is a command or a reactor (spec/13 §13.5.3); on a native target (Stage 7), a native executable ([targets](../targets/targets-and-profiles.md) §2.8.5). On a core target with `export world`, the build reports the info diagnostic `EXPORT_WORLD_NOT_APPLIED` (defined in [spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.6.2, N-D-32; see also [package/01-package-contract.md](../package/01-package-contract.md) §1.9 rule 5).
3. The root package is always compiled from its source. Its `artifact` declaration describes the package to dependents only ([package/01-package-contract.md](../package/01-package-contract.md) §1.7). A root package without `source` cannot be built: `sai build`, `sai component build`, `sai run`, `sai test`, and `sai size` report `ROOT_PACKAGE_HAS_NO_SOURCE`, while `sai check` validates the package and verifies its artifacts ([package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.5) (N-H-34). For dependencies, the form is chosen by D-PKG-05.
4. After code generation, the `budget` limits of the selected profile are checked against the artifact bytes in memory ([package/01-package-contract.md](../package/01-package-contract.md) §1.14.3, [09-optimization-and-size.md](09-optimization-and-size.md) §9.10.6). An exceeded budget is the error `SIZE_BUDGET_EXCEEDED`.
5. If no error was reported, the output file of §13.5.1 is written atomically (§13.5.4). If an error was reported, nothing is written to `out/`. "Error" means a diagnostic whose severity is `error` after level resolution, the dependency cap and `--deny-warnings` ([11-diagnostics.md](11-diagnostics.md) §11.3.3): a warning fails the build only under `--deny-warnings` (§13.4.4), and an info never does. In particular `LARGE_STACK_FRAME` ([06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) §6.7.2) is an info, so `--deny-warnings` cannot make acceptance depend on optimization ([09-optimization-and-size.md](09-optimization-and-size.md) §9.2.2 item 3).

Human output: the progress lines `resolving`, `checking`, `building`, and `wrote`, the diagnostics, and the summary line if a diagnostic was printed.

```text
$ sai build --target wasm32-wasi --profile release
checking acme:image@0.1.0
building acme:image@0.1.0 (wasm32-wasi, release)
wrote out/wasm32-wasi/release/image.wasm (48213 bytes)
```

JSON result: one record of kind `build`, written only when the artifact was written.

```json
{"type":"result","kind":"build","data":{"target":"wasm32-wasi","profile":"release","artifact":"component","component_kind":"reactor","world":"acme:image.service","wasm_features":[],"outputs":[{"path":"out/wasm32-wasi/release/image.wasm","bytes":48213,"sha256":"…"}]}}
```

- `artifact` is `"core_module"` or `"component"`.
- `component_kind` is `"command"` or `"reactor"` for a component, and `null` for a core module.
- `world` is the canonical path of the world the component implements ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9), and `null` for a core module and for a command without `export world`.
- `wasm_features` lists the opt-in features enabled with `--wasm-feature` (§13.4.7), sorted by name.
- `outputs` lists the written files in byte order of their paths.

Exit codes: 0 if the artifact was written; 1 otherwise.

### 13.6.4 `sai component build`

```text
sai component build [--target <name>] [--profile <name>] [BUILD-OPTIONS]
```

Builds a component for any target (D-TGT-01). Stage 4.

- For `wasm32-wasi` and `wasm64-wasi`, the command is identical to `sai build`: it produces the same component, writes the same file, and writes the same `build` record.
- For `wasm32-core` and `wasm64-core`, the command builds the core module and wraps it as a component with the exports of the root package's `export world` and no imports ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.6.2, [impl/07-component-lowering.md](07-component-lowering.md)). Reachable world imports are `TARGET_CAPABILITY_UNAVAILABLE` ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12.7). A component without exports is `NO_COMPONENT_EXPORTS`. The command writes both files of §13.5.1, `<name>.wasm` and `<name>.component.wasm`, and the `build` record lists both, with `artifact` set to `"component"`.

Exit codes: as for `sai build`.

### 13.6.5 `sai run`

```text
sai run [--target <name>] [--profile <name>] [--env <name>[=<value>]]... [--dir <host>[::<guest>]]... [--allow-network] [BUILD-OPTIONS] [-- <argument>...]
```

Builds the root package as `sai build` does and runs it as a command component. Stage 4.

| Option | Meaning |
|---|---|
| `--env <name>=<value>` | set the environment variable `<name>` of the program to `<value>`. Repeatable. |
| `--env <name>` | pass the host's value of `<name>`, if it is set. Repeatable. |
| `--dir <host>[::<guest>]` | make the host directory `<host>` available to the program as the preopened directory `<guest>` (default: `<host>` as written). Repeatable. |
| `--allow-network` | let the program open network connections and make outgoing HTTP requests |

Behavior:

1. The selected target MUST be a WASI target, `wasm32-wasi` or `wasm64-wasi`. On any other target (a core target or a native target), the command fails with `TARGET_NOT_RUNNABLE` before building ([targets](../targets/targets-and-profiles.md) §2.3, §4.3 item 5).
2. The command builds the component and writes it like `sai build`. If the component is a reactor, the command fails with `MISSING_ENTRY_POINT`, whose help shows the entry point of [spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.5.3.
3. The reference implementation runs the component in an embedded Component Model runtime and calls its `wasi:cli/run` export (N-H-35). The host implements the WASI interfaces of the target's WASI version, with only the authority the command line grants (P5):
   - **Arguments**: the first argument is `<name>`, the name part of the root package's identity, followed by the arguments after `--`, unchanged.
   - **Environment**: only the variables given with `--env`, in command-line order. The host's environment is not passed by default.
   - **Standard streams**: inherited from `sai run` (in human format; see item 5 for JSON).
   - **File system**: only the `--dir` directories, with read and write access. Without `--dir`, there are no preopened directories.
   - **Network**: without `--allow-network`, every attempt to open a socket or send an HTTP request fails with the WASI error code for denied access (`access-denied`). The program can handle that error; it does not trap.
   - **Clocks and randomness**: provided from the host.
4. The program's outcome is `success` if `run` returns `ok`, `failure` if it returns `err` (that is, `main` returned a non-zero `ExitCode`, spec/13 §13.5.3), and `trapped` if the component trapped. A trap is reported in human format as one line on standard error, `sai: program trapped: <description>`, and not as a diagnostic. The exit code is 0 for `success` and 3 otherwise (§13.3.4). The program's own exit status is not passed through, because WASI `run` returns only `ok` or `err`.
5. With `--format json`, the program's standard output and standard error are captured and reported in the result record, so that the NDJSON stream stays valid. Standard input is inherited in both formats.

Human output: the progress lines of `sai build`, then `running <path>`, then the program's output.

JSON result: one record of kind `run` after the `build` record.

```json
{"type":"result","kind":"run","data":{"path":"out/wasm32-wasi/dev/hello.wasm","arguments":["hello","--count","3"],"outcome":"failure","trap":null,"stdout":"counted 3\n","stderr":"","stdout_bytes":10,"stderr_bytes":0}}
```

`stdout` and `stderr` are the captured bytes decoded as UTF-8, with every invalid sequence replaced by U+FFFD. `stdout_bytes` and `stderr_bytes` are the numbers of bytes before decoding. `trap` is the trap description for `trapped`, and `null` otherwise.

Exit codes: 0 for `success`; 1 if building failed, for `TARGET_NOT_RUNNABLE`, and for `MISSING_ENTRY_POINT`; 3 for `failure` and `trapped`.

### 13.6.6 `sai test`

```text
sai test [--target <name>] [--profile <name>] [--package <ns>:<pkg>]... [--workspace] [--exact] [--list] [--no-run] [--seed <n>] [BUILD-OPTIONS] [<filter>...] [-- <filter>...]
```

Builds and runs the tests and properties of the selected packages. Stage 3. The meaning of tests and properties (discovery, isolation, assertions, generators, seeds, counterexamples; there is no shrinking, N-I-46) is owned by [spec/14-testing.md](../spec/14-testing.md). This section defines selection, execution order, and output.

| Option | Meaning |
|---|---|
| `--package <ns>:<pkg>` | select the tests of this workspace member. Repeatable. A package that is not a workspace member (§13.2.2) is `INVALID_OPTION_VALUE`. |
| `--workspace` | select the tests of every workspace member. Conflicts with `--package` (`CONFLICTING_OPTIONS`). |
| `<filter>` | select only the tests whose test path contains the filter (see below). Repeatable. Arguments after `--` are filters too, so that a filter can start with `-`. |
| `--exact` | a filter selects only the test whose test path equals it |
| `--list` | list the selected tests and exit, without generating code |
| `--no-run` | build the test harness, but do not run it |
| `--seed <n>` | the seed of every property that has no seed of its own ([spec/14-testing.md](../spec/14-testing.md)). `n` is a decimal integer in `0..=2^64-1`. |

Without `--package` and `--workspace`, only the root package is selected.

Selection:

1. The **test path** of a test or property is its canonical path ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9): `acme:image.test["add adds two numbers"]` for a test in the root module, `acme:image/codec.property["roundtrip"]` for a property in the module `codec`.
2. A test of a selected package is selected if no filter is given, or if at least one filter is a substring of its test path (byte-wise comparison), or, with `--exact`, equal to it.
3. Tests that are not selected are counted as `filtered_out`. Selecting no test is not an error.

Execution:

1. The command checks the packages as `sai check --tests` does, generates the test harness of [spec/14-testing.md](../spec/14-testing.md) for the selected target and profile (§13.6.6.2), and stores it in `.sai/cache/` (§13.5.1). It writes nothing to `out/`.
2. The harness runs in the embedded runtime of §13.6.5, as §13.6.6.3 describes. It has only the authority that spec/14 gives to tests. The command line adds none: `sai test` has no `--env`, `--dir`, or `--allow-network` (N-H-36).
3. The **test order** is: package rank ([11-diagnostics.md](11-diagnostics.md) §11.11.1 rule 2), then file path in byte order, then the start offset of the declaration. Up to `--jobs` tests run at the same time. Each test's result is written as soon as that test and every test before it in the test order have finished (N-H-36). The output therefore does not depend on `--jobs` or on timing, except for the `duration_us` values in JSON.
4. A test **fails** if it panics, traps, or (for a property) finds a counterexample, as defined by spec/14. Under the `tiny` profile a panic is a trap (D-TGT-02), so its failure has no message.

Human output: progress lines and diagnostics on standard error; the test report on standard output:

```text
$ sai test
checking acme:image@0.1.0
building acme:image@0.1.0 (wasm32-wasi, dev)
running 3 tests
test acme:image.test["add adds two numbers"] ... ok
test acme:image/decode.test["truncated header"] ... FAILED
property acme:image/codec.property["roundtrip"] ... ok

failures:

acme:image/decode.test["truncated header"]
  panicked at src/decode.sai:42:5: assertion failed: header.len() == 0

test result: FAILED. 2 passed, 1 failed, 0 filtered out
```

- Each result line is `<test|property> <test path> ... <ok|FAILED>`.
- The `failures:` block lists the failed tests in test order. Each failure has the test path, then indented lines: the failure message with its location, and, for a property, `counterexample: <value>` and `seed: <n>`, so that `--seed <n>` reproduces it.
- The last line is `test result: <ok|FAILED>. <p> passed, <f> failed, <s> filtered out`. Human output has no timings.
- With `--list`, standard output has one test path per line, in test order, and nothing else.

JSON results: with `--list`, one record of kind `test.list`. Otherwise one record of kind `test` per test that ran, in test order, followed by one record of kind `test.summary`.

```json
{"type":"result","kind":"test","data":{"path":"acme:image/decode.test[\"truncated header\"]","package":"acme:image@0.1.0","declaration":"test","location":{"file":{"path":"src/decode.sai","package":"acme:image@0.1.0","snapshot":null},"start":{"byte":812,"line":38,"col":1,"utf16_col":1},"end":{"byte":1050,"line":44,"col":2,"utf16_col":2}},"outcome":"failed","duration_us":1532,"failure":{"kind":"panic","message":"assertion failed: header.len() == 0","location":{"file":{"path":"src/decode.sai","package":"acme:image@0.1.0","snapshot":null},"start":{"byte":990,"line":42,"col":5,"utf16_col":5},"end":{"byte":990,"line":42,"col":5,"utf16_col":5}}},"property":null}}
{"type":"result","kind":"test.summary","data":{"target":"wasm32-wasi","profile":"dev","ran":true,"selected":3,"passed":2,"failed":1,"filtered_out":0}}
```

- `declaration` is `"test"` or `"property"`. `location` is the declaration's location in the form of [11-diagnostics.md](11-diagnostics.md) §11.9.5.
- `outcome` is `"passed"` or `"failed"`. `failure` is `null` for a passed test. `failure.kind` is `"panic"`, `"trap"`, or `"counterexample"`. `failure.message` is the panic message, the trap description, or the property's failure message, and `null` for a trap under `tiny`. `failure.location` is the panic location as a zero-length location (§13.6.6.3 item 6), or `null`.
- `property` is `null` for a test. For a property it is `{"seed":<n>,"cases":<n>,"counterexample":<string or null>}`: the seed used, the number of generated cases that ran, and the counterexample text of the failing case, exactly as generated and without shrinking ([spec/14-testing.md](../spec/14-testing.md) §14.5.6, N-I-46), or `null` when it is absent (spec/14 §14.5.6 item 4).
- `duration_us` is the wall-clock time of the test in microseconds. It is the only nondeterministic value in the stream (§13.1.1).
- With `--no-run`, no `test` record is written, and `test.summary` has `ran: false` and zero counts except `selected` and `filtered_out`.
- `test.list` data: `{"tests":[{"path":…,"package":…,"declaration":…,"location":…}]}` in test order.

Exit codes: 0 if every test that ran passed; 1 if an error diagnostic was reported (for example, a test does not compile); 3 if no error was reported and at least one test failed.

#### 13.6.6.1 Collection and selection

1. **Options.** In a `sai test` session, the driver sets the input `test_options(())` in phase 3, together with `build_config` ([01](01-architecture.md) §1.7.2): the selected packages (the root package, the members named by `--package`, or every member with `--workspace`), the filters in command-line order, `--exact`, and the value of `--seed`. In every other session it is empty. The selected packages are the `tests` list of the build keys ([08](08-effect-and-capability-analysis.md) §8.7.1).
2. **Collection.** The query `test_selection(())` of `sai_resolve` ([01](01-architecture.md) §1.3.1) lists every `test` and `property` of the selected packages that survives `@target` for the selected target. It reads them from the item trees, as the name check of [03](03-name-resolution-and-type-checking.md) §3.3.2 does, together with the module tree, `package_graph` and `test_options`. It reads no body, no type and no result of the check.

```rust
pub struct TestSelection {
    pub tests: Vec<CollectedTest>,     // in test order (Execution item 3)
    pub selected: u32,
    pub filtered_out: u32,
}

pub struct CollectedTest {
    pub owner: OwnerId,
    pub kind: TestKind,                // Test | Property
    pub path: Arc<str>,                // the test path text, spec/14 §14.5.2 item 2
    pub index: Option<u32>,            // `k`: the position among the selected tests; None if filtered out
}
```

3. **Selection.** The filters of Selection items 2 and 3 are matched against `path`. The selected tests are numbered `k = 0, 1, …` in test order, and `index` holds `k`.
4. **Readers.** `build_roots(K)` of the `TestHarness` key lists `Test(t)` exactly for the selected tests ([08](08-effect-and-capability-analysis.md) §8.7.2), and the harness entries of [04](04-mir.md) §4.13.10 are numbered by `k`. The `WithTests` check analyzes every test of the selected packages, selected or not, and does not read `test_selection`. The diagnostics of `sai test` therefore do not depend on the filters (P8).
5. **`--list`** runs the check of Execution item 1 and generates no code. If the check reports no error, the command prints the selected tests in test order. Otherwise it prints no test, and its exit code is 1. The list is therefore exactly the set of tests that `sai test` runs with the same options (N-H-86).

#### 13.6.6.2 The test harness

1. **Build.** When the `WithTests` check reports no error ([spec/14](../spec/14-testing.md) §14.7.1 item 2), the driver builds the harness for the `TestHarness` key ([08](08-effect-and-capability-analysis.md) §8.7.1) with phases 11 and 12 ([01](01-architecture.md) §1.2.1). The harness is one core module of the target's pointer width on every Wasm target. Phase 13 never runs for it, because a harness has no WIT world and no import of the program (spec/14 §14.7.1 item 1, §14.3.1 item 3), so a component would add only glue (N-H-82). `sai test` therefore also runs on `wasm64-wasi`, whose component output is optional ([targets](../targets/targets-and-profiles.md) §2.7). On a native target, the command stops before analysis with the `FEATURE_NOT_IMPLEMENTED` of [targets](../targets/targets-and-profiles.md) §1.4 item 4.
2. **Imports.** The harness imports functions only from the module `sai-harness`, and each one only if the harness calls it. `ptr` is `i32` on wasm32 and `i64` on wasm64.

| Field | Core type | Caller | Meaning |
|---|---|---|---|
| `panic` | `(func (param ptr ptr))` | `deliver` for the harness channel ([10](10-runtime.md) §10.5.4) | the address and the length of a panic report |
| `generated` | `(func)` | a case entry, after the last parameter value | the values of the current case are complete |
| `counterexample` | `(func (param ptr ptr i32))` | a counterexample entry | the address and the length of the counterexample text, then 1 if it was truncated and 0 otherwise |

   Any other import would need a capability other than `alloc`, which a test cannot have (spec/14 §14.3.1 item 3). The runner treats one as an internal compiler error ([11](11-diagnostics.md) §11.4.6).
3. **Exports.** The exports are listed in the byte order of their names ([06](06-wasm-lowering-and-layout.md) §6.10.5). `<k>` is the index of a selected test (§13.6.6.1), in decimal without leading zeros.

| Export | Core type | Function |
|---|---|---|
| `case.<k>` | `(func (param i32) (result i32))` | the case entry of property `k` ([04](04-mir.md) §4.13.10): runs case `i` and returns 0 if the result is `false`, otherwise 1 |
| `counterexample.<k>` | `(func (param i32))` | the counterexample entry of property `k`: generates the values of case `i` again and delivers their text |
| `memory` | memory | the module's memory, if it has one ([06](06-wasm-lowering-and-layout.md) §6.3.7) |
| `test.<k>` | `(func)` | the function of `Item(t)` for the selected `test` `t` with index `k`, which has no parameter and no result |

4. **Manifest.** With the module, the compiler returns the manifest of [04](04-mir.md) §4.13.10. For each index `k`, it gives the owner and the kind of the test. For a property, it also gives the base seed `S` (spec/14 §14.5.2) and the number of cases `N` (§14.5.1). The runner reads `S` and `N` only from the manifest, so they are computed once, by the compiler (N-H-83).
5. **Panic mode.** The harness channel is the reporting channel of every harness. The panic mode is therefore `Report` under `dev` and `release`, and `TrapOnly` under `tiny` ([10](10-runtime.md) §10.5.3, spec/14 §14.4.2). The `@panic_hook` of the root package is never imported.

#### 13.6.6.3 Running tests and capturing failures

1. **Engine.** The runner executes the harness with the core-module engine of the embedded runtime of §13.6.5 (N-H-35). The engine is configured identically for every test (N-H-85):
   - the Wasm features of the target and of `--wasm-feature` (§13.4.7);
   - NaN canonicalization, so that a test whose result depends on NaN bits has the same outcome on every host;
   - no fuel, no epoch interruption, and no memory limit below the maximum of the target's memory type (spec/14 §14.4.3 item 3);
   - an engine call stack of 8 MiB on every host.

   No WASI interface, clock or source of randomness is provided, because the harness imports none of them (§13.6.6.2 item 2).
2. **Instances.** Each selected test runs in its own store and instance, created from the compiled module and the host functions of §13.6.6.2 item 2 (spec/14 §14.7.3). Instantiation runs no code ([10](10-runtime.md) §10.7.1). The runner never calls an instance again after a trap ([10](10-runtime.md) §10.7.4), and it drops the store when the test ends. Up to `--jobs` instances run at the same time, each on its own thread.
3. **Host functions.**
   - `panic(p, n)` copies the `n` bytes at `p` from the memory and returns. The instance traps right afterwards ([10](10-runtime.md) §10.5.4). Only the first call in an instance counts; `__sai_panicking` prevents a second one ([10](10-runtime.md) §10.5.8).
   - `generated()` marks the current case.
   - `counterexample(p, n, t)` copies the text and the flag.

   A range that is not inside the memory, which only undefined behavior in unsafe code can cause, makes the call count as not made. Copied bytes are decoded as UTF-8, with every invalid sequence replaced by U+FFFD.
4. **Tests.** The runner calls `test.<k>` once. The test passes if the call returns.
5. **Properties.** The runner calls `case.<k>(i)` for `i = 0 … N − 1` in the same instance, and stops at the first failing case (spec/14 §14.5.5). Case `i` fails if the call returns 0 or traps. The number of cases that ran is `i + 1` after a failure at case `i`, and `N` when the property passes.

   After a failure at case `i`, if `generated` was called in case `i`, the runner creates one more instance and calls `counterexample.<k>(i)` (spec/14 §14.5.6 item 2). The counterexample is the text of its `counterexample` call, followed by `…` (U+2026) if the flag is 1 (spec/14 §14.5.6 item 3). The counterexample is absent in two cases (spec/14 §14.5.6 item 4):
   - `generated` was not called in the failing case;
   - the second instance panicked or trapped before it called `counterexample`.

   A panic of the second instance is never reported.
6. **Failures.** A failed test has exactly one failure, of one of the kinds of spec/14 §14.4.1 item 3:
   - **`panic`** if `panic` was called. The report, without its final line feed, has one of two forms ([10](10-runtime.md) §10.5.5): `panic: <message>`, or `panic at <file>:<line>:<column>: <message>`.
     - `<file>` is the longest file string of the build ([10](10-runtime.md) §10.5.2) that both begins the text after `panic at ` and is followed by `:` in it.
     - `<line>` and `<column>` are the decimal numbers that follow.
     - `<message>` is the rest of the text after `: `.

     The location is the zero-length location at that line and UTF-8 column of that file, in the form of [11](11-diagnostics.md) §11.9.5. It is `null` for the first form. A report in any other form is an internal compiler error.
   - **`trap`** if the instance trapped and `panic` was not called. The reason comes from the engine's trap code, as in the table below. The message is `trap: <reason>` when the panic mode is `Report`, and `null` when it is `TrapOnly`.
   - **`counterexample`** if `case.<k>` returned 0. The message is `property returned false`, and the location is `null`.

| Engine trap code (`wasmtime::Trap`) | Reason |
|---|---|
| `UnreachableCodeReached` | `unreachable` |
| `MemoryOutOfBounds` | `memory out of bounds` |
| `TableOutOfBounds` | `table out of bounds` |
| `BadSignature` | `indirect call type mismatch` |
| `IntegerDivisionByZero` | `integer divide by zero` |
| `IntegerOverflow` | `integer overflow` |
| `BadConversionToInteger` | `invalid conversion to integer` |
| `StackOverflow` | `call stack exhausted` |
| any other code, `IndirectCallToNull` among them | `other` |

7. **Duration.** `duration_us` runs from the creation of the test's first instance to the end of its last call. It includes the counterexample instance.

### 13.6.7 `sai fmt`

```text
sai fmt [--check] [<path>...]
sai fmt [--check] [--stdin-kind source|package] -
```

Formats SAI source files and `sai.package` files into the canonical form of [14-formatter.md](14-formatter.md) (D-TOOL-04). Stage 0. The command has no formatting options (D-TOOL-04).

| Option | Meaning |
|---|---|
| `--check` | do not rewrite source files; report every file that is not in canonical form |
| `-` | read one file from standard input and write the formatted text to standard output |
| `--stdin-kind <k>` | with `-`: `source` (default) formats a SAI source file, `package` a `sai.package` file. Without `-` it is `CONFLICTING_OPTIONS`. |

File selection:

1. **No path.** The command needs a package (§13.2.1). It formats, for the root package and every workspace member: `sai.package` and every file below the source directory whose name ends in `.sai`. Workspace members are found from the `from path` requirements of the `sai.package` files, transitively, without resolving or fetching anything (§13.2.2).
2. **Paths.** No package is needed. A path that names a file is formatted: as a package contract if its name is `sai.package`, and as a SAI source file otherwise. An explicit symbolic link to a regular file is resolved to its target. A symbolic link to a directory is not traversed. A path that names a directory is searched recursively for files whose name ends in `.sai` or is `sai.package`. The search does not follow symbolic links, skips directories whose name starts with `.`, and skips a directory named `out` that is next to a `sai.package` file. A path that does not exist is `PATH_NOT_FOUND`.
3. **`-`.** Standard input is formatted. `-` cannot be combined with paths (`CONFLICTING_OPTIONS`). Diagnostics name the file `<stdin>` ([11-diagnostics.md](11-diagnostics.md) §11.5.4).
4. The selected files are processed in byte order of their paths. A file named twice is processed once.

Behavior:

1. A file with a lexical or syntax error is **refused**: its diagnostics are reported, and the file is left unchanged ([14-formatter.md](14-formatter.md) §14.2). The other files are still formatted.
2. Without `--check`, every file whose canonical form differs from its content is rewritten atomically (§13.5.4), and its path is reported with the progress line `formatted <path>`. Files already in canonical form are not written, so their modification time does not change.
3. With `--check`, no source file is rewritten. The command may create the internal `.sai/lock` bookkeeping file needed to coordinate shared reads. Every file whose canonical form differs from its content is reported with the error `UNFORMATTED_FILE` ([14-formatter.md](14-formatter.md) §14.11), whose primary location is the first line that differs.
4. With `-` and without `--check`, the canonical form is written to standard output, even if it equals the input (in JSON format, into the result record). A refused input writes nothing to standard output.

JSON result: one record of kind `fmt`.

```json
{"type":"result","kind":"fmt","data":{"check":false,"files":12,"changed":["src/user.sai"],"refused":[],"output":null}}
```

`files` is the number of selected files. `changed` lists the files that were rewritten (or, with `--check`, would be), and `refused` the refused files, both in byte order. For `-`, the path is `<stdin>`. With `-` and `--format json`, standard output carries the NDJSON stream, so the formatted text is carried in `output` instead. `output` is `null` for a refused input, with `--check`, and whenever `-` is not used.

Exit codes: 0 if no file was refused, no path was missing, and, with `--check`, every file was canonical; 1 otherwise.

### 13.6.8 Dependency commands: common rules

`sai add`, `sai remove`, `sai update`, `sai lock`, and `sai fetch` are Stage 3. Their semantics are owned by [package/02-resolver.md](../package/02-resolver.md) (resolution, `sai add` package/02 §2.11, `sai update` package/02 §2.10.5, `sai remove` package/02 §2.10.6, `sai lock` package/02 §2.10.4), [package/03-lock-file.md](../package/03-lock-file.md) (the lock file), and [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) (fetching, the review of package/04 §4.7). This chapter defines their command lines and output. Common rules:

1. They operate on the root package. A workspace member's requirements are changed by running the command with `-C <member directory>`.
2. They accept `--locked`, `--offline`, `--frozen`, and `--approve` (§13.4.3, §13.4.6).
3. Edits of `sai.package` follow §13.5.4: only the changed requirement is rewritten, in canonical layout. A new requirement is appended as the last entry of the `requires` block, or of the `requires test` block with `--test`. If the block does not exist, it is appended at the end of the file, after one blank line (N-H-37).
4. If the command fails, neither `sai.package` nor `sai.lock` is changed. The two files are written only after the resolution and the review have succeeded, `sai.package` first.

**Lock changes.** Every command that resolves reports how the lock file changed, as a `lock` object in its result record:

```json
{"written":true,"added":["acme:sqlite@3.1.0"],"removed":[],"changed":[{"package":"acme:base64","from":"0.4.2","to":"0.4.5"}]}
```

`written` says whether `sai.lock` was written. `added` and `removed` list package references, and `changed` lists nodes whose selected version changed, each sorted by identity bytes, then version precedence. In human format the same information is printed as lines `added <ref>`, `removed <ref>`, and `updated <ns>:<pkg> <from> -> <to>`, in this order.

**Review.** For every node that the resolution adds, or whose version, source, or content it changes, the command shows the review of [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.7.1 (draft §75). A content change of an already-locked version is the replacement of a moved release tag by `sai update`: the version stays the same, the tag's new commit would be recorded, and the review lists the approval item `TAG_MOVED` (package/04 §4.3.3 rule 4, §4.7.2). In human format each review is a block of headings and indented values. `sai add`, `sai remove`, `sai update`, and `sai lock` write it to standard output, because it is their result. Every other command writes it to standard error, and only for nodes with an item that needs approval (§13.4.6).

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

Unused source clauses
  none
```

This is the example of package/04 §4.7.3. The headings, their order, and their values are defined by package/04 §4.7.1. An empty list is printed as the single value `none`. In JSON, each review is one record of kind `review`, written before the command's own record:

```json
{"type":"result","kind":"review","data":{"package":"acme:sqlite@3.1.0","source":"git \"https://github.com/acme/sqlite\"","form":"artifact","capabilities":["alloc","wasi:filesystem"],"supported":["wasm32-wasi","wasm64-wasi"],"not_supported":["wasm32-core","wasm64-core"],"estimated_contribution_bytes":626688,"needs_approval":["CAPABILITY_ESCALATION"],"unused_sources":[]}}
```

`form` is `"source"`, `"artifact"`, or `"hybrid"` (package/02 §2.11 rule 7). `estimated_contribution_bytes` is `null` when package/04 gives no estimate. `needs_approval` lists the codes of the review items that need approval, in byte order. Each `unused_sources` object identifies the requiring package, whether it is the root, the version it named, and the source clause, in the order defined by package/04 §4.7.1.

### 13.6.9 `sai add`

```text
sai add <url> [--dir <subdir>] [--version <requirement>] [--test] [--only <target>]... [--grant <capability>]... [--locked] [--offline] [--frozen] [--approve]
sai add <ns>:<pkg> [--version <requirement>] [--test] [--only <target>]... [--grant <capability>]... [--locked] [--offline] [--frozen] [--approve]
sai add --path <dir> [--test] [--only <target>]... [--grant <capability>]... [--approve]
```

Adds a requirement to the root package and resolves. Exactly one of `<url>`, `<ns>:<pkg>`, and `--path` MUST be given (`MISSING_ARGUMENT`, `CONFLICTING_OPTIONS`).

| Option | Meaning |
|---|---|
| `<url>` | a Git URL in one of the forms of [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.2. The requirement gets `from git "<canonical URL>"`. |
| `<ns>:<pkg>` | the identity of a standard package that ships with the toolchain (D-PKG-04). The requirement gets the package's canonical URL ([package/01-package-contract.md](../package/01-package-contract.md) §1.10.3 rule 1). Any other identity is `INVALID_OPTION_VALUE`, with the help that a dependency is named by its repository URL, because there is no registry (D-PKG-01). |
| `--path <dir>` | a local package directory. The requirement gets `from path "<dir>"`, with `<dir>` relative to the root package directory and `/` as the separator. |
| `--dir <subdir>` | with `<url>`: the package directory inside the repository (`dir "<subdir>"`) |
| `--version <requirement>` | the version requirement, in the syntax of [package/01-package-contract.md](../package/01-package-contract.md) §1.10.2, for example `^1.4` or `">=1.2, <1.8"`. Not allowed with `--path`. |
| `--test` | add to `requires test` instead of `requires` |
| `--only <target>` | add `only { … }` with the given targets, in command-line order. Repeatable. |
| `--grant <capability>` | add `grant { … }` with the given capabilities, in command-line order. Repeatable. |

Behavior (the semantics are package/02 §2.11):

1. Without `--version`, the command lists the release tags of the source and selects the highest release that is not a pre-release (package/02 §2.11 rule 2), or, with `--path`, reads the declared version. The requirement is `^<version>` with the full version, for example `^1.6.2`. A source without release tags is `NO_RELEASE_TAGS`. With `--version`, the release tag of the requirement's minimum MUST exist (`VERSION_NOT_FOUND`).
2. The identity is read from the fetched `sai.package`, never from the URL (P6). If the root already requires that identity in `requires` or `requires test`, the command fails with `ALREADY_REQUIRED`. If it is the root's own identity, it fails with `PACKAGE_REQUIRES_ITSELF` (package/02 §2.11 rule 4). A confusable name is the warning `CONFUSABLE_PACKAGE_NAME` (package/02 §2.11 rule 5).
3. The command validates the new entry as package/01 validates any requirement, resolves with the current lock as floors, prints the reviews (§13.6.8), asks for approval where §13.4.6 requires it, and then writes `sai.package` and `sai.lock`. A new node with a non-empty capability footprint escalates when a previous lock exists (package/02 §2.12.3 rules 2 and 6), so in an established package `sai add` of a package with capabilities needs approval.
4. `sai add` does not change source files. The help line after success shows the `use` declaration that binds the package (D-MOD-01), for example `use acme:json`.

Human output: progress lines, the reviews, the lock changes, and the line `added acme:json @ ^1.6.2 to requires`.

JSON result: the `review` records, then one record of kind `add`:

```json
{"type":"result","kind":"add","data":{"package":"acme:json","requirement":"^1.6.2","section":"requires","source":"git \"https://github.com/acme/json\"","lock":{"written":true,"added":["acme:json@1.6.2"],"removed":[],"changed":[]}}}
```

Exit codes: 0 on success; 1 if the requirement was not added.

### 13.6.10 `sai remove`

```text
sai remove <ns>:<pkg> [--locked] [--offline] [--frozen] [--approve]
```

Removes the requirement on `<ns>:<pkg>` from the root's `requires` or `requires test` and resolves ([package/02-resolver.md](../package/02-resolver.md) §2.10.6). If the root has no such requirement, the command fails with `NOT_REQUIRED`. The entry is deleted together with its continuation lines and the comments attached to it ([14-formatter.md](14-formatter.md) §14.6). If the block becomes empty, the whole block is deleted. Nodes that are no longer reachable disappear from the lock. The command does not change source files: a remaining `use` of the package is reported by the next check.

JSON result: one record of kind `remove`:

```json
{"type":"result","kind":"remove","data":{"package":"acme:json","section":"requires","lock":{"written":true,"added":[],"removed":["acme:json@1.6.2"],"changed":[]}}}
```

Exit codes: 0 on success; 1 otherwise.

### 13.6.11 `sai update`

```text
sai update [<ns>:<pkg>[@<version>]]... [--dry-run] [--locked] [--offline] [--frozen] [--approve]
```

Raises the selected versions within their compatibility lines ([package/02-resolver.md](../package/02-resolver.md) §2.10.5, N-G-30). Without arguments, every node is updated. With `<ns>:<pkg>`, only that package is probed; with `<ns>:<pkg>@<version>`, its node is set to exactly that version. A repeated package is `CONFLICTING_OPTIONS`.

| Option | Meaning |
|---|---|
| `--dry-run` | compute and print the result, but write nothing |

- `sai update` never edits `sai.package`, never moves a node to another line, and never updates pinned nodes (package/02 §2.10.5 rule 4). Pinned nodes are reported as skipped.
- Errors are those of package/02, among them `UPDATE_CHANGES_LINE`, `UPDATE_VERSION_NOT_SELECTED`, `VERSION_NOT_FOUND`, and, under `--offline`, `OFFLINE_FETCH_REQUIRED` (package/02 §2.10.5 rule 7).

Human output: the reviews, the lock changes, and one line `skipped <ns>:<pkg> (pinned by <path or override>)` per pinned node. With `--dry-run`, the lock changes are preceded by the line `dry run: sai.lock not written`. Without changes, the only line is `sai.lock is up to date`.

JSON result: the `review` records, then one record of kind `update`:

```json
{"type":"result","kind":"update","data":{"dry_run":false,"lock":{"written":true,"added":[],"removed":[],"changed":[{"package":"acme:json","from":"1.6.2","to":"1.7.0"}]},"skipped":[{"package":"acme:local","reason":"path"}]}}
```

`skipped[].reason` is `"path"` for a `from path` pin and `"override"` for a pin by a root override. With `--dry-run`, `lock.written` is `false`.

Exit codes: 0 on success, including when nothing changed; 1 otherwise.

### 13.6.12 `sai lock`

```text
sai lock [--locked] [--offline] [--frozen] [--approve]
```

Resolves and writes `sai.lock` without building ([package/02-resolver.md](../package/02-resolver.md) §2.10.4 rules 2–3). The lock is resolved from the current lock file if it is usable, and from scratch (Mode B without floors) when `sai.lock` does not exist. A lock file with merge conflict markers is resolved as [package/03-lock-file.md](../package/03-lock-file.md) §3.9 defines. Deleting `sai.lock` and running `sai lock` gives the minimal resolution.

JSON result: the `review` records, then one record of kind `lock`: `{"lock":{…}}` with the lock changes of §13.6.8.

Exit codes: 0 on success; 1 otherwise.

### 13.6.13 `sai fetch`

```text
sai fetch [--locked | --frozen] [--offline] [--approve]
```

Downloads into the global package cache the content of every entry of `sai.lock` that has a Git source, for every target and including test-only entries, so that later commands can run with `--offline` (§13.4.3). If the lock file is missing or out of date, the command first resolves and writes it, unless `--locked` forbids it. Contents that are already in the cache are verified against their checksums and not downloaded again. With `--offline`, the command only verifies the cache and fails with `OFFLINE_FETCH_REQUIRED` for each missing entry. The cache layout and its locking are owned by [package/04-distribution-and-security.md](../package/04-distribution-and-security.md).

Human output: one progress line `fetching <ref>` per downloaded entry, in identity order, and then `<n> packages fetched, <m> already cached`.

JSON result: one record of kind `fetch`:

```json
{"type":"result","kind":"fetch","data":{"fetched":["acme:json@1.7.0"],"cached":["acme:base64@0.4.5","sai:collections@1.2.0"],"lock":{"written":false,"added":[],"removed":[],"changed":[]}}}
```

Exit codes: 0 if every entry is in the cache afterwards; 1 otherwise.

### 13.6.14 `sai size`

```text
sai size [--target <name>] [--profile <name>] [--why <name>] [BUILD-OPTIONS]
```

Builds the artifact as `sai build` does, writes it under the same condition (§13.6.3 item 5), and reports where its bytes come from (draft §48, §49). Stage 2. The attribution itself (the categories, which bytes count for which item, and the retention chains) is owned by [09-optimization-and-size.md](09-optimization-and-size.md) §9.10. This section defines the command line and the presentation.

| Option | Meaning |
|---|---|
| `--why <name>` | explain why an item is in the artifact. `<name>` is a category name of impl/09 (such as `allocator`), a package identity, or a canonical path of a function ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9). A name that is not in the artifact is reported with the info diagnostic `NOT_PRESENT_IN_BUILD`, and the command exits with 0. |

Human output on standard output. Without `--why`, the attribution tree of impl/09:

```text
SAI code                  3.2 KiB

Dependencies
  acme:hash               8.1 KiB

Runtime features
  allocator               2.2 KiB

Unicode                   0.0 KiB
WASI                      0.0 KiB

Other
  module structure        0.4 KiB
  component wrapper       0.7 KiB

─────────────────────────────────
Total                    14.6 KiB
```

- Each line is a name, indented by 2 spaces per tree level, and a size right-aligned to a common column. The column is the larger of 33 and the length of the longest name plus its indentation plus 12. A group heading without its own size is followed by its children, and groups are separated by one blank line, as impl/09 orders them.
- Sizes are printed in KiB with exactly one decimal: `bytes × 10 / 1024`, rounded to the nearest integer with ties rounded up, then printed with a decimal point before the last digit.
- The six top-level groups are always printed, in the order of [09-optimization-and-size.md](09-optimization-and-size.md) §9.10.1, also with 0 bytes. A child of `Runtime features` or `Other` is printed only if it has at least one byte (impl/09 §9.10.1 item 4): the example is a `tiny` build, which has no custom sections ([06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) §6.11), so `metadata` is absent.
- The separator line consists of `─` (U+2500) repeated to the width of the size column. The last line is the total: the sum of the six top-level groups, which is the size of the artifact, including its module structure, metadata and component wrapper (impl/09 §9.10.1 item 1).
- If the build uses a hybrid package, one blank line and then one line `form <package reference> <form>` per hybrid package follow the total, in the order of `forms` below (D-PKG-05, [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.5.1 item 3).

With `--why`, the retention chain of impl/09, from the item to a root, one link per line, each nested by 2 spaces and prefixed with `└ ` (draft §49):

```text
allocator
└ String.concat
  └ build_message
    └ create_user
```

JSON result: one `build` record (§13.6.3) if the artifact was written, then one record of kind `size` or `size.why`:

```json
{"type":"result","kind":"size","data":{"target":"wasm32-wasi","profile":"tiny","path":"out/wasm32-wasi/tiny/app.wasm","total_bytes":14930,"groups":[{"name":"SAI code","bytes":3277,"children":[]},{"name":"Dependencies","bytes":8294,"children":[{"name":"acme:hash","bytes":8294,"children":[]}]},{"name":"Runtime features","bytes":2253,"children":[{"name":"allocator","bytes":2253,"children":[]}]},{"name":"Unicode","bytes":0,"children":[]},{"name":"WASI","bytes":0,"children":[]},{"name":"Other","bytes":1106,"children":[{"name":"module structure","bytes":412,"children":[]},{"name":"component wrapper","bytes":694,"children":[]}]}],"forms":[{"package":"acme:hash@1.0.3","kind":"source","form":"source"}]}}
{"type":"result","kind":"size.why","data":{"name":"allocator","chain":["allocator","sai:alloc/string.String.concat","acme:app.build_message","acme:app.create_user"]}}
```

`groups` is the attribution tree of impl/09 in its order: always the six top-level groups, each with its non-empty children (impl/09 §9.10.1). Each node has `name`, `bytes` (exact), and `children`. `total_bytes` is the sum of the `bytes` of the six groups, which is the size of the artifact. `path` is the path of the written artifact, and `null` when nothing was written (below). `forms` has one entry for every active node of the build other than the root, sorted by identity bytes, then by version precedence: `package` is its package reference, `kind` is `source`, `artifact`, or `hybrid` ([package/01-package-contract.md](../package/01-package-contract.md) §1.1.2), and `form` is the form chosen for this target and profile, `source` or `artifact` ([package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.5.1). `chain` lists the links from the item to the root. Function links are canonical paths. The human output shortens them to the last component of the path that is unique within the chain.

When the only errors of the build are `SIZE_BUDGET_EXCEEDED`, the report is still printed, the tree or the chain, computed from the artifact built in memory ([09-optimization-and-size.md](09-optimization-and-size.md) §9.10.6). Nothing is written to `out/` ([package/01-package-contract.md](../package/01-package-contract.md) §1.14.3 item 4), so there is no `build` record and `path` is `null`. With any other error, no report is printed.

Exit codes: as for `sai build`. A build that exceeds a budget therefore exits with 1, after its report.

### 13.6.15 `sai inspect`

```text
sai inspect <subcommand> [ARGUMENTS] [BUILD-OPTIONS]
```

Prints the results of an analysis (D-TOOL-03). Each subcommand runs the analyses it needs, for the selected target and profile, and nothing more. The output goes to standard output. Subcommands that need generated code (`mir`, `layout`) generate it, but write nothing to `out/`.

| Subcommand | Stage | Prints |
|---|---|---|
| `unsafe` | 2 | every unsafe construct in the active graph |
| `mir <path>` | 1 | the MIR of a function |
| `effects [<path>]` | 3 | effect sets of functions |
| `capabilities` | 3 | required capabilities with their roots and chains |
| `layout <type>` | 1 | the memory layout of a type |
| `wit` | 4 | the generated WIT package |
| `deps` | 3 | the resolved dependency graph and the chosen forms |

#### `sai inspect unsafe`

```text
sai inspect unsafe [--package <ns>:<pkg>]...
```

Prints the unsafe inventory of [spec/12-unsafe-and-ffi.md](../spec/12-unsafe-and-ffi.md) §12.11.1 for the selected target: every entry of every package of the resolved graph, including `sai:core` and `sai:alloc`. The set of entries, their fields, and their order are defined there (N-I-35), and computed by [08-effect-and-capability-analysis.md](08-effect-and-capability-analysis.md) §8.10.4. The inventory is computed without test analysis: an entry inside a test or property, or inside an item that `@target` removes for the selected target, lists no operations (§8.10.4 items 7 and 8, N-D-97); `sai test` checks and reports the operations of test code. With `--package`, only the named packages are listed; a name that is not in the graph is `INVALID_OPTION_VALUE`.

The entry kinds are spelled `unsafe_block`, `unsafe_fn`, `unsafe_trait`, `unsafe_impl`, `unsafe_module`, `unsafe_extern_wasm`, and `unsafe_extern_c`. The operations of spec/12 §12.3.1 are spelled by their row: `deref` (1), `call_unsafe_fn` (2), `call_unsafe_fn_ptr` (3), `call_extern_c` (4), and `unsafe_impl` (5).

Human output: one heading per package, `<package>@<version>` with the prefix `dep:` of [11-diagnostics.md](11-diagnostics.md) §11.5.4 for dependencies, followed by ` (non-portable)` if spec/12 §12.9.2 item 3 applies. Then one line per entry with its location, its kind, the canonical path of the enclosing item, and the number of unsafe operations directly inside it, followed by one indented line per operation with its location, its row name, and for calls the canonical path of the callee. `@target` predicates are printed after the canonical path as `@target(…)`. The columns are separated by at least 4 spaces and aligned within each package. The `# Safety` sections and the symbol and node IDs are only in the JSON record. The last line counts the entries and the packages.

The example names two packages with `--package`, so the entries of `sai:core` and `sai:alloc` are not listed. Entries are ordered by the bytes of `item` within each package (§8.10.4 item 9).

```text
$ sai inspect unsafe --package acme:image --package acme:simd
acme:image@0.1.0
  src/buffer.sai:40:9    unsafe_block     acme:image/buffer.Buffer.as_bytes    1 operation
    src/buffer.sai:41:13     call_unsafe_fn    sai:core/slice.Slice.from_raw_parts
  src/decoder.sai:1:1    unsafe_module    acme:image/decoder                   3 operations
    src/decoder.sai:14:13    deref
    src/decoder.sai:15:13    deref
    src/decoder.sai:22:9     call_unsafe_fn    sai:core/ptr.PtrMut.write
dep:acme:simd@1.0.1
  src/lib.sai:12:1       unsafe_fn        acme:simd.load_unaligned             1 operation
    src/lib.sai:14:5         deref

3 unsafe entries in 2 packages
```

The JSON record of kind `inspect.unsafe` has `{"packages":[{"package":…,"non_portable":false,"counts":{"unsafe_block":1,…},"entries":[{"kind":"unsafe_block","item":<canonical path>,"symbol":<symbol ID or null>,"node":<node ID>,"location":…,"target":[<predicate text>],"safety":<string or null>,"operations":[{"row":"deref","location":…,"callee":<canonical path or null>}]}]}]}`. `counts` has one member per entry kind, including kinds with the count 0. `symbol` is the symbol ID of the enclosing item ([12-semantic-tooling.md](12-semantic-tooling.md) §12.2), or `null` when `item` is a module or the syntactic path of a removed item ([08](08-effect-and-capability-analysis.md) §8.10.4 item 2), and `node` is the node ID of the construct (12 §12.3); both refer to the snapshot that the command read. `safety` is the text of the `# Safety` section of an `unsafe_fn` or `unsafe_trait` entry as [08](08-effect-and-capability-analysis.md) §8.10.4 item 5 extracts it: the doc text of the section's lines joined with LF, without blank lines at its start and end, and the empty string for a section without text. It is `null` for other entries and when the section is absent.

#### `sai inspect mir`

```text
sai inspect mir <path> [--phase <phase>]
```

Prints the MIR of the function `<path>` (a canonical path of a function of the root package or a workspace member) in the text format of [04-mir.md](04-mir.md). `--phase` takes the phase names of [04](04-mir.md) §4.14.1: `built`, `drop-elaborated`, `analyzed`, `optimized`, `mono` and `final`. The default `final` is the MIR that is passed to the backend. The first four print the generic bodies of the function's owner once. `mono` and `final` print one body per instance of the function that the build contains, in the order of the instantiation key of [04](04-mir.md) §4.13.1, which is the position of the instance in `mono_instances`. An unknown path is `UNKNOWN_SYMBOL_PATH`. The JSON record `inspect.mir` has `{"function":…,"phase":…,"bodies":[{"instance":…,"text":…}]}`.

The MIR text is meant for compiler developers. Its format can change with every toolchain version, and it is not covered by the stability rules of §13.3.3.

#### `sai inspect effects`

```text
sai inspect effects [<path>] [--why <effect>]
```

Prints the effect set of every function of the root package and the workspace members, or of the function `<path>` alone, which may belong to any package of the graph ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md); [08](08-effect-and-capability-analysis.md) §8.9.1, N-D-96). The listing omits tests and properties, which are analyzed only by `sai test`, and functions whose effect set is incomplete because of an error; a `<path>` that has no effect set is the info diagnostic `NOT_PRESENT_IN_BUILD` with a note that says why. Functions are listed in byte order of their canonical paths. Each line shows the canonical path, whether the set is `declared` (the function has an `effects` clause) or `inferred`, and the set in the canonical order of spec/08 §8.2.3:

```text
$ sai inspect effects
acme:user/service.create_user    declared    effects { net.http, db.write }
acme:user/service.validate       inferred    effects {}
```

With `<path>` and `--why <effect>`, the command prints the why chain of [spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.13 for that effect, in the rendering of §8.13.3. If the effect is not in the function's set, the command reports the info diagnostic `NOT_PRESENT_IN_BUILD`. `--why` without `<path>` is `MISSING_ARGUMENT`. An unknown path is `UNKNOWN_SYMBOL_PATH`.

JSON: `inspect.effects` with `{"functions":[{"path":…,"symbol":…,"declared":true,"effects":["net.http","db.write"],"location":…}]}`, or, with `--why`, `inspect.effects.why` with `{"function":…,"effect":…,"chain":<steps>}`, where `<steps>` has the form of the `steps` definition of [11-diagnostics.md](11-diagnostics.md) §11.9.5.

#### `sai inspect capabilities`

```text
sai inspect capabilities
```

Lists every capability that the build for the selected target and profile requires, and for each capability every root that reaches it, each with its shortest chain ([spec/08-effects-and-capabilities.md](../spec/08-effects-and-capabilities.md) §8.12, §8.13.2). Capabilities appear in byte order of their names, and roots in byte order of their canonical paths. The status of each capability is checked against the root's effective policy and the target: `allowed`, `not allowed`, `denied`, or `unavailable` (the codes of spec/08 §8.12.6 are reported as diagnostics too).

```text
$ sai inspect capabilities --target wasm32-wasi
alloc        allowed
  root main    src/main.sai:3:1
    = why: main                      src/main.sai:3:1
           -> sync_users             src/main.sai:12:5
           -> sai:alloc/vec.Vec.push src/main.sai:13:9   declares effects { alloc }
           => alloc requires alloc

wasi:http    allowed
  root main    src/main.sai:3:1
    = why: main                      src/main.sai:3:1
           -> sync_users             src/main.sai:12:5
           -> users.fetch_all        src/main.sai:14:9   declares effects { net.http }
           => net.http requires wasi:http
```

JSON: `inspect.capabilities` with `{"target":…,"profile":…,"capabilities":[{"capability":"wasi:http","status":"allowed","roots":[{"root":…,"location":…,"chain":<steps>}]}]}`. `status` is `allowed`, `not_allowed`, `denied`, or `unavailable`.

#### `sai inspect layout`

```text
sai inspect layout <type>... | --all
```

Prints the layout table of [06-wasm-lowering-and-layout.md](06-wasm-lowering-and-layout.md) §6.4.16 for the selected target. Each `<type>` is a type written in SAI syntax and resolved in the scope of the root module, for example `Option<I32>` or `user.User`. It MUST be fully instantiated. An argument that does not parse, does not resolve, or has unbound generic parameters is `INVALID_OPTION_VALUE`, and the help gives the reason. `--all` prints every type laid out by the build that is defined in the root package or a workspace member, in byte order of the printed types. The types are printed with `CanonType` ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9.1).

```text
$ sai inspect layout 'Option<I32>' 'Option<Bool>' --target wasm32-core
Option<I32>
  size 8, align 4
  encoding: tagged, tag U8 at 0, payload at 4
  leaves: i32 i32

Option<Bool>
  size 1, align 1
  encoding: niche in Some at 0, None = 2
  leaves: i32
```

Each type is a block: the type; `size <n>, align <n>`; for a struct or tuple, one line `field <name> at <offset>, size <n>` per field in memory order; for an enum, one `encoding:` line; and the `leaves:` line with the Wasm value types of the leaves, separated by spaces. Blocks are separated by one blank line.

JSON: `inspect.layout` with `{"target":…,"types":[{"type":…,"size":…,"align":…,"fields":[{"name":…,"offset":…,"size":…}],"encoding":<encoding or null>,"leaves":["i32","i32"]}]}`. `encoding` is `null` for a type that is not an enum, and otherwise one of:

| `kind` | Members | impl/06 case |
|---|---|---|
| `"tagged"` | `tag_type`, `tag_offset`, `payload_offset`, `tags` (array of `{variant, value}`) | tagged enum |
| `"niche"` | `dataful_variant`, `niche_offset`, `niche_type`, `values` (array of `{variant, value}`) | niche-encoded enum |
| `"single"` | `variant` | only one variant is inhabited (Case B) |
| `"empty"` | none | no variant is inhabited (Case A) |

#### `sai inspect wit`

```text
sai inspect wit
```

Prints the WIT package generated from the root package for the selected target to standard output ([spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.11). The text is byte-identical to the file that `sai bindgen wit` writes (§13.6.19): both print `generated_wit_text` of the root ([16](16-interfaces-and-wit.md) §16.10.1 and §16.10.2 item 2). JSON: `inspect.wit` with `{"target":…,"package":<WIT package name>,"text":…}`.

#### `sai inspect deps`

```text
sai inspect deps [--tests]
```

Prints the resolved graph that is active for the selected target ([package/02-resolver.md](../package/02-resolver.md) §2.5.3), with test edges if `--tests` is given. Each node is written as `<identity>#<line>` (package/02 §2.2.3), followed by the selected version, the chosen form (`source` or `artifact`, D-PKG-05), and the source. Children appear in the order of the requirer's edges (by identity bytes). A node that was already printed is printed again only as its name followed by ` (*)`. Test edges are marked `[test]`.

```text
$ sai inspect deps --target wasm32-wasi
acme:server@1.0.0
├─ acme:image#1 1.2.0 artifact git "https://github.com/acme/image"
│  └─ acme:base64#0.4 0.4.5 source git "https://github.com/acme/base64"
└─ acme:json#1 1.7.0 source git "https://github.com/acme/json"
   └─ acme:base64#0.4 (*)
```

JSON: `inspect.deps` with `{"target":…,"root":<package reference>,"nodes":[{"node":"acme:json#1","version":"1.7.0","form":"source","source":…,"footprint":[…],"edges":[{"node":…,"requirement":…,"test":false}]}]}`. The root's edges are the edges of the first node. The other nodes are sorted by identity bytes, then by line. `footprint` is the capability footprint recorded in the lock (package/02 §2.12.1).

Exit codes for every `sai inspect` subcommand: 0 if no error was reported; 1 otherwise. The report is printed whenever the analysis it needs has completed, even if errors were reported.

### 13.6.16 `sai graph`

```text
sai graph <graph> [<start>...] [--in | --both] [--depth <n>] [--edge <kind>]... [--deps]
```

Queries one of the semantic graphs of [12-semantic-tooling.md](12-semantic-tooling.md) §12.4 (draft §62). Stage 6.

| Argument or option | Meaning |
|---|---|
| `<graph>` | `symbols`, `references`, `calls`, `effects`, `capabilities`, or `tests` (12 §12.4.2). Any other value is `INVALID_OPTION_VALUE`. |
| `<start>` | a start node: a canonical path ([spec/04-modules-and-names.md](../spec/04-modules-and-names.md) §4.9) or a symbol ID (D-TOOL-02), resolved by 12 §12.2.5, or a node key of 12 §12.4.1 that starts with `module:`, `package:`, `effect:`, or `capability:`, resolved by 12 §12.4.3 rule 2. Repeatable. Without a start node, the query starts at every root of the graph that belongs to the root package or a workspace member (12 §12.4.3). |
| `--in` | follow edges against their direction (callers instead of callees). |
| `--both` | follow edges in both directions. `--in` with `--both` is `CONFLICTING_OPTIONS`. |
| `--depth <n>` | the maximum number of edges from a start node, 1 to 64. The default is 1 with a start node and 64 without one. `0` and values above 64 are `INVALID_OPTION_VALUE`. |
| `--edge <kind>` | keep only edges of this kind (12 §12.4.2). Repeatable. A kind that the graph does not have is `INVALID_OPTION_VALUE`. |
| `--deps` | also expand nodes that belong to dependencies. By default they are printed but not expanded (12 §12.4.3 rule 4). |

The query is evaluated by 12 §12.4.3. Human output is one tree per start node, in the order of the arguments. Each line after the start line is `<label> <node>`, drawn with the characters of `sai inspect deps` (§13.6.15). `<label>` is the edge kind for an edge followed forward, and `<-` followed by the edge kind for an edge followed in reverse (12 §12.4.3 rule 5). `<node>` is the printed form of the node (12 §12.4.1): the canonical path of a symbol or module, `<ns>:<pkg>@<version>` for a package node, the spelling of an effect atom, or a capability name. Children are sorted by 12 §12.4.3 rule 4: forward before reverse, then by edge kind, printed form, and node key, each in byte order. A node that was already expanded in the same tree is printed with ` (*)` and not expanded again. A dependency node that is not expanded because `--deps` is absent is printed with ` (dep)`.

```text
$ sai graph calls acme:user/service.create_user --depth 2
acme:user/service.create_user
├─ calls acme:user/db.insert
│  └─ calls sai:alloc/string.String.concat (dep)
└─ calls acme:user/model.User.new
```

JSON: one record of kind `graph`, whose `data` is the query result of 12 §12.4.4. Exit codes: 0 if the query was evaluated; 1 if a start node did not resolve (`UNKNOWN_SYMBOL_PATH` or `UNKNOWN_SYMBOL_ID`) or analysis reported an error that prevents building the graph.

### 13.6.17 `sai diff`

```text
sai diff [<from> [<to>]] [--size] [--public]
```

Computes the semantic diff of [12-semantic-tooling.md](12-semantic-tooling.md) §12.8 between two versions of the workspace (draft §67). Stage 6.

1. A **version** is either a Git revision of the repository that contains the workspace root, or the working tree. The default for `<from>` is `HEAD`, and the default for `<to>` is the working tree (N-H-42). So `sai diff` compares the last commit with the files on disk, `sai diff main` compares `main` with the files on disk, and `sai diff v1.2.0 v1.3.0` compares two revisions.
2. Revisions are resolved with `gix` (D-TOOL-05) and accept the revision syntax of `gix`: commit IDs, branch and tag names, and suffixes such as `~1`. The system Git is never run. If the workspace root is not inside a Git repository and a revision is needed, the command fails with `NOT_A_GIT_REPOSITORY`. A revision that does not resolve to a commit is `UNKNOWN_REVISION`.
3. For each version, the workspace at the root package's path inside that version is analyzed as `sai check` would analyze it, for the selected target and profile, with that version's `sai.package` and `sai.lock`. A version without `sai.lock` is resolved in memory without writing anything. Dependencies that are not in the global cache are fetched, unless `--offline` is given (`OFFLINE_FETCH_REQUIRED`). If either version has an error that prevents analysis, the command reports it, with the version named in a note, and exits with 1.
4. `--size` also builds both versions and reports the size changes of 12 §12.8.4. Without it, no code is generated and no size is shown.
5. `--public` restricts the output to changes of the public API (12 §12.8.3).
6. Nothing is written to the workspace. `out/` is not touched. Analysis results of the other version are kept only in `.sai/cache/`.
7. The diff also reviews the unsafe code of dependencies: for every dependency whose lock entry changed version, source or checksum between the versions, or that is new, the entries of `sai inspect unsafe` (§13.6.15) for the selected target are compared (12 §12.8.7, N-H-87). There is no option for it. With `--public` the review is omitted.

Human output: the text rendering of 12 §12.8.6, followed by the `DependencyUnsafe` blocks of 12 §12.8.7. If the versions have no semantic difference and no dependency has an unsafe change, the command prints `no semantic changes`.

```text
$ sai diff --size
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

JSON: one record of kind `diff`, whose `data` is the diff document of 12 §12.8.5, including its `dependency_unsafe` member, extended by `"from"` and `"to"`: the commit ID in 40 lowercase hexadecimal digits, or `null` for the working tree. Exit codes: 0 if the diff was computed, whether or not it found changes; 1 otherwise. A tool that wants to fail on breaking changes reads the `breaking` count of the record.

### 13.6.18 `sai edit`

```text
sai edit <operation> <target> [<parameter>=<value>]... [--dry-run] [--allow-breaking] [--size]
sai edit --request <file> [--dry-run] [--allow-breaking] [--size]
```

Applies one semantic operation of [12-semantic-tooling.md](12-semantic-tooling.md) §12.6 to the workspace (draft §63). Stage 6.

| Argument or option | Meaning |
|---|---|
| `<operation>` | an operation name of 12 §12.6, such as `rename_symbol`. An unknown name is `UNKNOWN_OPERATION`. |
| `<target>` | the operation's target: a canonical path or a symbol ID (12 §12.2.5), or, for an operation whose targets include modules, a module key `module:<canonical path>` (12 §12.4.1, §12.6.1) |
| `<parameter>=<value>` | one operation parameter. The parameter names and value types are those of the operation's table in 12 §12.6. The value is everything after the first `=`, parsed as the parameter's type (12 §12.6.2). A missing required parameter, an unknown parameter, a parameter given twice, or a value that does not parse is `INVALID_OPERATION_ARGUMENT`. |
| `--request <file>` | read the operation request as JSON (12 §12.6.1) from `<file>`, or from standard input if `<file>` is `-`. It cannot be combined with `<operation>` (`CONFLICTING_OPTIONS`). |
| `--dry-run` | compute the impact analysis and the edits, print them, and write nothing |
| `--allow-breaking` | apply the operation even if the impact analysis reports a breaking change of the public API |
| `--size` | also estimate the size change of the artifact (12 §12.7.2 rule 5) |

Behavior:

1. The command analyzes the workspace like `sai check` for the selected target and profile, plans the operation (12 §12.6.1 steps 1–4), and computes its impact (12 §12.7).
2. If a precondition fails, the operation's error is reported (12 §12.6), nothing is written, and the command exits with 1.
3. If the impact analysis reports `breaking: true` and neither `--allow-breaking` nor `--dry-run` is given, the command reports `OPERATION_BREAKS_PUBLIC_API`, writes nothing, and exits with 1 (N-H-41). The help names `--allow-breaking`.
4. Otherwise, unless `--dry-run` is given, the edits are written atomically per file (§13.5.4), and the symbol index is updated so that every symbol keeps its symbol ID (12 §12.2.4). Only files of the root package and the workspace members are ever edited (12 §12.6.2 rule 3).
5. After writing, the command checks the workspace again. A diagnostic that did not exist before the edit is reported normally. The operation's guarantees (12 §12.6.2) make such a diagnostic a toolchain defect only for errors, and not for the errors listed in the impact's `new_errors` (12 §12.7.3): `change_type` produces them at use sites by design (12 §12.6.7), and they are reported here as ordinary diagnostics. A new warning, such as a new unused import, can be legitimate.

Human output: the impact report of 12 §12.7.4, then one line `edited <path>` per changed file (or `would edit <path>` with `--dry-run`), in byte order of the paths.

```text
$ sai edit add_parameter acme:user/service.create_user name=email type=Email 'argument=Email.empty()' --dry-run --size
add_parameter acme:user/service.create_user

affected
  14 call sites
  3 tests
  2 exported interfaces
  1 component world

breaking public API: yes
estimated Wasm size: +420 bytes

would edit src/api.sai
would edit src/service.sai
```

The size line is printed only if `--size` is given. With `--size`, the root package is built before and after the edit, with the files after the edit kept in memory for a dry run.

JSON: one record of kind `edit` with `{"operation":…,"target":<symbol ID, or module key for a module target>,"dry_run":true,"impact":<impact record of 12 §12.7.3>,"edits":[<TextEdit of 11 §11.7.1>],"changed_files":[…]}`. `edits` uses snapshot `null` and is sorted per [11-diagnostics.md](11-diagnostics.md) §11.7.4 rule 2. Exit codes: 0 if the operation was planned (and, without `--dry-run`, applied) and no new error was reported; 1 otherwise. A `change_type` whose impact has `new_errors` therefore exits with 1 after writing its edits, because rule 5 reports those errors. With `--dry-run`, `new_errors` is part of the impact record only, and is not reported as diagnostics.

### 13.6.19 `sai bindgen wit`

```text
sai bindgen wit [-o <path>]
sai bindgen wit --show <wit package or interface>
```

Generates WIT from the root package's interfaces and worlds, or shows the SAI view of an external WIT package (D-WIT-02). Stage 4.

1. **Generation.** The command analyzes the root package for the selected target and writes the WIT package of [spec/13-components-and-wit.md](../spec/13-components-and-wit.md) §13.11 to `out/<target>/wit/<name>.wit` (§13.5.1), or to `<path>` with `-o`. `-o -` writes the text to standard output. The text depends on the target only through WASI versions (spec/13 §13.11.4). It does not depend on the profile, and `--profile` is accepted and ignored. A package without interfaces and worlds produces a file with the `package` line only. The text is `generated_wit_text` of the root ([16](16-interfaces-and-wit.md) §16.10.1), and it is written only when the root has no errors and the text is complete ([16](16-interfaces-and-wit.md) §16.10.2 item 1).
2. **`--show`.** The argument is a WIT package name `ns:name` or an interface `ns:name/iface`, visible to the root package's build (spec/13 §13.7.1). The command prints the SAI view of spec/13 §13.7.3 as SAI source in canonical format ([14-formatter.md](14-formatter.md), [16](16-interfaces-and-wit.md) §16.4.9): one `interface` declaration per WIT interface, in WIT order, each preceded by the line comment `// <ns>:<name>/<iface>@<version>` and separated by one blank line. A name that is not visible is `UNKNOWN_WIT_ITEM` (spec/13). `--show` with `-o` is `CONFLICTING_OPTIONS`.

```text
$ sai bindgen wit --target wasm32-wasi
wrote out/wasm32-wasi/wit/image.wit (612 bytes)
```

JSON: `bindgen.wit` with `{"target":…,"package":<WIT package name>,"output":<file record or null>,"text":<string or null>}`. `output` is set when a file was written. `text` is set for `-o -` and for `--show`. Exit codes: 0 on success; 1 otherwise.

### 13.6.20 `sai explain`

```text
sai explain <CODE>
sai explain --list
```

Prints the explanation of a diagnostic code or a lint, as defined by [11-diagnostics.md](11-diagnostics.md) §11.13. Stage 0. The command needs no package, accepts none of the options of §13.4, and reads no file. The human output is the Markdown text of 11 §11.13.2. JSON: kind `explain` (11 §11.13.2), or, with `--list`, one record of kind `explain.list` with `{"codes":[{"code":…,"class":"hard","summary":…}]}` in byte order. Exit codes: 0 if the code is registered; 1 for `UNKNOWN_DIAGNOSTIC_CODE`. `sai explain` without an argument and without `--list` is `MISSING_ARGUMENT`.

**Current implementation slice.** `sai explain --list` is implemented for all 493 registered codes in human and JSON output. An unknown code reports `UNKNOWN_DIAGNOSTIC_CODE` with ranked candidates. A known code still reports `FEATURE_NOT_IMPLEMENTED` until its complete metadata and Markdown explanation record are available.

### 13.6.21 `sai lsp`

```text
sai lsp [--stdio] [--target <name>] [--profile <name>]
```

Runs the language server of [12-semantic-tooling.md](12-semantic-tooling.md) §12.10. The base protocol is Stage 0; the SAI extensions (`sai/*` methods, 12 §12.5) are Stage 6.

1. The server speaks the Language Server Protocol over standard input and standard output, with the `Content-Length` framing of LSP. `--stdio` selects this transport. It is the default and the only transport in v0.1, and the option exists because editors pass it. Sockets and pipes are not supported.
2. `sai lsp` is the only exception to §13.1.1 rule 4 and §13.3.2 (N-H-39): standard input and standard output belong to the protocol. `--format` is accepted and has no effect. Usage errors detected before the connection starts are written to standard error in human form, and the command exits with 2. Once the connection is running, every diagnostic and log message goes through the protocol (`textDocument/publishDiagnostics`, `window/logMessage`), and nothing is written to standard error.
3. The package is not discovered from the current directory. The server discovers one root package per workspace folder that the client announces, using §13.2.1 with the folder as the start directory (12 §12.10.3). `--directory` is therefore `UNKNOWN_OPTION` for `sai lsp`.
4. `--target` and `--profile` give the initial target and profile. Without them, each root package uses the defaults of §13.4.1 and §13.4.2. The client can change them at run time (12 §12.10.3). `--locked`, `--offline`, and `--frozen` are accepted and apply to every resolution the server performs. The server never prompts for approval: a resolution that needs approval is reported as a diagnostic on `sai.package`, as for a non-interactive command (§13.4.6 rule 3).
5. The server writes `.sai/index/` and `.sai/cache/`, and it writes `sai.lock` when a resolution changes it, unless `--locked` is given. It writes source files only when the client applies a workspace edit itself, or through `workspace/applyEdit` requests that the client accepts (12 §12.10.6).
6. Exit codes follow LSP: 0 if the client sent `shutdown` before `exit`, and 1 if it sent `exit` without `shutdown` or closed standard input without `exit`.

---

## 13.7 Environment variables

The CLI reads only the environment variables in this table (§13.1.1 rule 2, N-H-18). None of them selects a target, a profile, an option, or any input that affects a result.

| Variable | Effect |
|---|---|
| `NO_COLOR` | disables color when set to a non-empty value (§13.3.2) |
| `CLICOLOR_FORCE` | forces color when set to a non-empty value other than `0` (§13.3.2) |
| `TERM` | the value `dumb` disables automatic color (§13.3.2) |
| `SAI_CACHE_DIR` | the directory of the global package cache. Its location, layout, and locking are owned by [package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.4.1 (N-G-50, N-H-40). |
| `HOME`, `XDG_CACHE_HOME`, `LOCALAPPDATA` | used only to compute the default of `SAI_CACHE_DIR` |
| `HTTPS_PROXY`, `https_proxy`, `NO_PROXY`, `no_proxy` | proxies for `https` fetches ([package/04-distribution-and-security.md](../package/04-distribution-and-security.md) §4.2.3) |
| `SSH_AUTH_SOCK` | the SSH agent for `ssh` fetches (package/04 §4.2.3) |

The default of `SAI_CACHE_DIR`, when it is not set, is the one of package/04 §4.4.1 rule 1:

1. on Linux and other Unix systems except macOS: `$XDG_CACHE_HOME/sai` if `XDG_CACHE_HOME` is set to an absolute path, and `$HOME/.cache/sai` otherwise;
2. on macOS: `$HOME/Library/Caches/sai`;
3. on Windows: `%LOCALAPPDATA%\sai\cache`.

An empty `SAI_CACHE_DIR` counts as unset. A `SAI_CACHE_DIR` that is set to a path that is not absolute is a usage error (package/04 §4.4.1 rule 1): it is reported as `INVALID_OPTION_VALUE` (§13.3.5) with `argument_index` 0 and `argument` `SAI_CACHE_DIR=<value>`, and the help says that the value must be an absolute path. If the default cannot be computed (no `HOME` or `LOCALAPPDATA`), a command that needs the cache fails with `CACHE_UNAVAILABLE` and exit code 1, and the help suggests setting `SAI_CACHE_DIR`.

The Git credential helpers that package/04 §4.2.3 consults read their own configuration. That configuration affects only authentication, never which content is used.

The program run by `sai run`, and the tests run by `sai test`, see none of these variables. Their environment is exactly the one of §13.6.5 and spec/14.

---

## 13.8 JSON Schema of the result records

Every result record that a command of this chapter writes MUST validate against the schema below (JSON Schema 2020-12). It refines the `result` definition of [11-diagnostics.md](11-diagnostics.md) §11.9.5, whose definitions it references by the `$id` `urn:sai:schema:diagnostics:1`. The definitions `graph_result`, `diff`, and `impact` are those of the semantic-tooling schema of [12-semantic-tooling.md](12-semantic-tooling.md) §12.11. As in 11, the property order of each definition is the serialization order. The format of `sai inspect mir` text is excluded from the stability rules (§13.6.15), but its record is not.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "urn:sai:schema:cli-results:1",
  "title": "SAI CLI result record, version 1",
  "type": "object",
  "required": ["type", "kind", "data"],
  "properties": { "type": { "const": "result" }, "kind": { "type": "string" }, "data": { "type": "object" } },
  "allOf": [
    { "if": { "properties": { "kind": { "const": "new" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/new" } } } },
    { "if": { "properties": { "kind": { "const": "check" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/check" } } } },
    { "if": { "properties": { "kind": { "const": "build" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/build" } } } },
    { "if": { "properties": { "kind": { "const": "run" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/run" } } } },
    { "if": { "properties": { "kind": { "const": "test" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/test" } } } },
    { "if": { "properties": { "kind": { "const": "test.summary" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/test_summary" } } } },
    { "if": { "properties": { "kind": { "const": "test.list" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/test_list" } } } },
    { "if": { "properties": { "kind": { "const": "fmt" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/fmt" } } } },
    { "if": { "properties": { "kind": { "const": "review" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/review" } } } },
    { "if": { "properties": { "kind": { "const": "add" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/add" } } } },
    { "if": { "properties": { "kind": { "const": "remove" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/remove" } } } },
    { "if": { "properties": { "kind": { "const": "update" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/update" } } } },
    { "if": { "properties": { "kind": { "const": "lock" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/lock_result" } } } },
    { "if": { "properties": { "kind": { "const": "fetch" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/fetch" } } } },
    { "if": { "properties": { "kind": { "const": "size" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/size" } } } },
    { "if": { "properties": { "kind": { "const": "size.why" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/size_why" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.unsafe" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_unsafe" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.mir" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_mir" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.effects" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_effects" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.effects.why" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_effects_why" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.capabilities" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_capabilities" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.layout" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_layout" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.wit" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_wit" } } } },
    { "if": { "properties": { "kind": { "const": "inspect.deps" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/inspect_deps" } } } },
    { "if": { "properties": { "kind": { "const": "graph" } } }, "then": { "properties": { "data": { "$ref": "urn:sai:schema:semantic:1#/$defs/graph_result" } } } },
    { "if": { "properties": { "kind": { "const": "diff" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/diff" } } } },
    { "if": { "properties": { "kind": { "const": "edit" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/edit" } } } },
    { "if": { "properties": { "kind": { "const": "bindgen.wit" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/bindgen_wit" } } } },
    { "if": { "properties": { "kind": { "const": "explain" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/explain" } } } },
    { "if": { "properties": { "kind": { "const": "explain.list" } } }, "then": { "properties": { "data": { "$ref": "#/$defs/explain_list" } } } }
  ],
  "$defs": {
    "uint": { "type": "integer", "minimum": 0 },
    "location": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/location" },
    "opt_location": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/opt_location" },
    "steps": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/steps" },
    "symbol_id": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/symbol_id" },
    "identity": { "type": "string", "pattern": "^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*$" },
    "package_ref": { "type": "string", "pattern": "^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*@[0-9]+\\.[0-9]+\\.[0-9]+([-+].*)?$" },
    "target": { "enum": ["wasm32-core", "wasm64-core", "wasm32-wasi", "wasm64-wasi"] },
    "profile": { "enum": ["dev", "release", "tiny"] },
    "strings": { "type": "array", "items": { "type": "string" } },
    "unused_source": {
      "type": "object", "required": ["requirer", "requirer_is_root", "named_version", "source"], "additionalProperties": false,
      "properties": { "requirer": { "$ref": "#/$defs/package_ref" }, "requirer_is_root": { "type": "boolean" }, "named_version": { "type": "string" }, "source": { "type": "string" } }
    },
    "file_record": {
      "type": "object", "required": ["path", "bytes", "sha256"], "additionalProperties": false,
      "properties": { "path": { "type": "string" }, "bytes": { "$ref": "#/$defs/uint" }, "sha256": { "type": "string", "pattern": "^[0-9a-f]{64}$" } }
    },
    "lock_changes": {
      "type": "object", "required": ["written", "added", "removed", "changed"], "additionalProperties": false,
      "properties": {
        "written": { "type": "boolean" },
        "added": { "type": "array", "items": { "$ref": "#/$defs/package_ref" } },
        "removed": { "type": "array", "items": { "$ref": "#/$defs/package_ref" } },
        "changed": { "type": "array", "items": { "type": "object", "required": ["package", "from", "to"], "additionalProperties": false,
          "properties": { "package": { "$ref": "#/$defs/identity" }, "from": { "type": "string" }, "to": { "type": "string" } } } }
      }
    },
    "new": {
      "type": "object", "required": ["package", "directory", "files"], "additionalProperties": false,
      "properties": { "package": { "$ref": "#/$defs/package_ref" }, "directory": { "type": "string" }, "files": { "type": "array", "items": { "$ref": "#/$defs/file_record" } } }
    },
    "check": {
      "type": "object", "required": ["target", "profile", "tests", "packages", "fixes_applied", "changed_files"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "profile": { "$ref": "#/$defs/profile" }, "tests": { "type": "boolean" }, "packages": { "$ref": "#/$defs/uint" }, "fixes_applied": { "$ref": "#/$defs/uint" }, "changed_files": { "$ref": "#/$defs/strings" } }
    },
    "build": {
      "type": "object", "required": ["target", "profile", "artifact", "component_kind", "world", "wasm_features", "outputs"], "additionalProperties": false,
      "properties": {
        "target": { "$ref": "#/$defs/target" }, "profile": { "$ref": "#/$defs/profile" },
        "artifact": { "enum": ["core_module", "component"] },
        "component_kind": { "enum": ["command", "reactor", null] },
        "world": { "type": ["string", "null"] },
        "wasm_features": { "type": "array", "items": { "enum": ["simd128", "tail-call"] }, "uniqueItems": true },
        "outputs": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/file_record" } }
      }
    },
    "run": {
      "type": "object", "required": ["path", "arguments", "outcome", "trap", "stdout", "stderr", "stdout_bytes", "stderr_bytes"], "additionalProperties": false,
      "properties": {
        "path": { "type": "string" }, "arguments": { "$ref": "#/$defs/strings" },
        "outcome": { "enum": ["success", "failure", "trapped"] }, "trap": { "type": ["string", "null"] },
        "stdout": { "type": "string" }, "stderr": { "type": "string" }, "stdout_bytes": { "$ref": "#/$defs/uint" }, "stderr_bytes": { "$ref": "#/$defs/uint" }
      }
    },
    "test_entry": {
      "type": "object", "required": ["path", "package", "declaration", "location"],
      "properties": { "path": { "type": "string" }, "package": { "$ref": "#/$defs/package_ref" }, "declaration": { "enum": ["test", "property"] }, "location": { "$ref": "#/$defs/location" } }
    },
    "test": {
      "allOf": [{ "$ref": "#/$defs/test_entry" }],
      "required": ["outcome", "duration_us", "failure", "property"],
      "properties": {
        "outcome": { "enum": ["passed", "failed"] },
        "duration_us": { "$ref": "#/$defs/uint" },
        "failure": { "oneOf": [{ "type": "null" }, { "type": "object", "required": ["kind", "message", "location"], "additionalProperties": false,
          "properties": { "kind": { "enum": ["panic", "trap", "counterexample"] }, "message": { "type": ["string", "null"] }, "location": { "$ref": "#/$defs/opt_location" } } }] },
        "property": { "oneOf": [{ "type": "null" }, { "type": "object", "required": ["seed", "cases", "counterexample"], "additionalProperties": false,
          "properties": { "seed": { "$ref": "#/$defs/uint" }, "cases": { "$ref": "#/$defs/uint" }, "counterexample": { "type": ["string", "null"] } } }] }
      }
    },
    "test_summary": {
      "type": "object", "required": ["target", "profile", "ran", "selected", "passed", "failed", "filtered_out"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "profile": { "$ref": "#/$defs/profile" }, "ran": { "type": "boolean" }, "selected": { "$ref": "#/$defs/uint" }, "passed": { "$ref": "#/$defs/uint" }, "failed": { "$ref": "#/$defs/uint" }, "filtered_out": { "$ref": "#/$defs/uint" } }
    },
    "test_list": { "type": "object", "required": ["tests"], "properties": { "tests": { "type": "array", "items": { "$ref": "#/$defs/test_entry" } } } },
    "fmt": {
      "type": "object", "required": ["check", "files", "changed", "refused", "output"], "additionalProperties": false,
      "properties": { "check": { "type": "boolean" }, "files": { "$ref": "#/$defs/uint" }, "changed": { "$ref": "#/$defs/strings" }, "refused": { "$ref": "#/$defs/strings" }, "output": { "type": ["string", "null"] } }
    },
    "review": {
      "type": "object", "required": ["package", "source", "form", "capabilities", "supported", "not_supported", "estimated_contribution_bytes", "needs_approval", "unused_sources"], "additionalProperties": false,
      "properties": {
        "package": { "$ref": "#/$defs/package_ref" }, "source": { "type": "string" }, "form": { "enum": ["source", "artifact", "hybrid"] },
        "capabilities": { "$ref": "#/$defs/strings" }, "supported": { "type": "array", "items": { "$ref": "#/$defs/target" } }, "not_supported": { "type": "array", "items": { "$ref": "#/$defs/target" } },
        "estimated_contribution_bytes": { "oneOf": [{ "$ref": "#/$defs/uint" }, { "type": "null" }] },
        "needs_approval": { "type": "array", "items": { "enum": ["CAPABILITY_ESCALATION", "SOURCE_CHANGED"] } },
        "unused_sources": { "type": "array", "items": { "$ref": "#/$defs/unused_source" } }
      }
    },
    "section": { "enum": ["requires", "requires test"] },
    "add": {
      "type": "object", "required": ["package", "requirement", "section", "source", "lock"], "additionalProperties": false,
      "properties": { "package": { "$ref": "#/$defs/identity" }, "requirement": { "type": "string" }, "section": { "$ref": "#/$defs/section" }, "source": { "type": "string" }, "lock": { "$ref": "#/$defs/lock_changes" } }
    },
    "remove": {
      "type": "object", "required": ["package", "section", "lock"], "additionalProperties": false,
      "properties": { "package": { "$ref": "#/$defs/identity" }, "section": { "$ref": "#/$defs/section" }, "lock": { "$ref": "#/$defs/lock_changes" } }
    },
    "update": {
      "type": "object", "required": ["dry_run", "lock", "skipped"], "additionalProperties": false,
      "properties": { "dry_run": { "type": "boolean" }, "lock": { "$ref": "#/$defs/lock_changes" },
        "skipped": { "type": "array", "items": { "type": "object", "required": ["package", "reason"], "additionalProperties": false,
          "properties": { "package": { "$ref": "#/$defs/identity" }, "reason": { "enum": ["path", "override"] } } } } }
    },
    "lock_result": { "type": "object", "required": ["lock"], "additionalProperties": false, "properties": { "lock": { "$ref": "#/$defs/lock_changes" } } },
    "fetch": {
      "type": "object", "required": ["fetched", "cached", "lock"], "additionalProperties": false,
      "properties": { "fetched": { "type": "array", "items": { "$ref": "#/$defs/package_ref" } }, "cached": { "type": "array", "items": { "$ref": "#/$defs/package_ref" } }, "lock": { "$ref": "#/$defs/lock_changes" } }
    },
    "size_group": {
      "type": "object", "required": ["name", "bytes", "children"], "additionalProperties": false,
      "properties": { "name": { "type": "string" }, "bytes": { "$ref": "#/$defs/uint" }, "children": { "type": "array", "items": { "$ref": "#/$defs/size_group" } } }
    },
    "size": {
      "type": "object", "required": ["target", "profile", "path", "total_bytes", "groups", "forms"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "profile": { "$ref": "#/$defs/profile" }, "path": { "type": ["string", "null"] }, "total_bytes": { "$ref": "#/$defs/uint" },
        "groups": { "type": "array", "minItems": 6, "maxItems": 6, "items": { "$ref": "#/$defs/size_group" } },
        "forms": { "type": "array", "items": { "type": "object", "required": ["package", "kind", "form"], "additionalProperties": false,
          "properties": { "package": { "$ref": "#/$defs/package_ref" }, "kind": { "enum": ["source", "artifact", "hybrid"] }, "form": { "enum": ["source", "artifact"] } } } } }
    },
    "size_why": { "type": "object", "required": ["name", "chain"], "additionalProperties": false, "properties": { "name": { "type": "string" }, "chain": { "type": "array", "minItems": 1, "items": { "type": "string" } } } },
    "inspect_unsafe": {
      "type": "object", "required": ["packages"], "additionalProperties": false,
      "properties": { "packages": { "type": "array", "items": { "type": "object", "required": ["package", "non_portable", "counts", "entries"], "additionalProperties": false,
        "properties": { "package": { "$ref": "#/$defs/package_ref" }, "non_portable": { "type": "boolean" },
          "counts": { "type": "object", "required": ["unsafe_block", "unsafe_fn", "unsafe_trait", "unsafe_impl", "unsafe_module", "unsafe_extern_wasm", "unsafe_extern_c"], "additionalProperties": { "$ref": "#/$defs/uint" } },
          "entries": { "type": "array", "items": { "type": "object", "required": ["kind", "item", "symbol", "node", "location", "target", "safety", "operations"], "additionalProperties": false,
            "properties": { "kind": { "enum": ["unsafe_block", "unsafe_fn", "unsafe_trait", "unsafe_impl", "unsafe_module", "unsafe_extern_wasm", "unsafe_extern_c"] },
              "item": { "type": "string" }, "symbol": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "type": "null" }] }, "node": { "type": "string" },
              "location": { "$ref": "#/$defs/location" }, "target": { "$ref": "#/$defs/strings" }, "safety": { "type": ["string", "null"] },
              "operations": { "type": "array", "items": { "type": "object", "required": ["row", "location", "callee"], "additionalProperties": false,
                "properties": { "row": { "enum": ["deref", "call_unsafe_fn", "call_unsafe_fn_ptr", "call_extern_c", "unsafe_impl"] }, "location": { "$ref": "#/$defs/location" }, "callee": { "type": ["string", "null"] } } } } } } } } } } }
    },
    "inspect_mir": {
      "type": "object", "required": ["function", "phase", "bodies"], "additionalProperties": false,
      "properties": { "function": { "type": "string" }, "phase": { "type": "string" },
        "bodies": { "type": "array", "items": { "type": "object", "required": ["instance", "text"], "additionalProperties": false, "properties": { "instance": { "type": "string" }, "text": { "type": "string" } } } } }
    },
    "inspect_effects": {
      "type": "object", "required": ["functions"], "additionalProperties": false,
      "properties": { "functions": { "type": "array", "items": { "type": "object", "required": ["path", "symbol", "declared", "effects", "location"], "additionalProperties": false,
        "properties": { "path": { "type": "string" }, "symbol": { "$ref": "#/$defs/symbol_id" }, "declared": { "type": "boolean" }, "effects": { "$ref": "#/$defs/strings" }, "location": { "$ref": "#/$defs/location" } } } } }
    },
    "inspect_effects_why": {
      "type": "object", "required": ["function", "effect", "chain"], "additionalProperties": false,
      "properties": { "function": { "type": "string" }, "effect": { "type": "string" }, "chain": { "$ref": "#/$defs/steps" } }
    },
    "inspect_capabilities": {
      "type": "object", "required": ["target", "profile", "capabilities"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "profile": { "$ref": "#/$defs/profile" },
        "capabilities": { "type": "array", "items": { "type": "object", "required": ["capability", "status", "roots"], "additionalProperties": false,
          "properties": { "capability": { "type": "string" }, "status": { "enum": ["allowed", "not_allowed", "denied", "unavailable"] },
            "roots": { "type": "array", "items": { "type": "object", "required": ["root", "location", "chain"], "additionalProperties": false,
              "properties": { "root": { "type": "string" }, "location": { "$ref": "#/$defs/location" }, "chain": { "$ref": "#/$defs/steps" } } } } } } } }
    },
    "variant_value": { "type": "object", "required": ["variant", "value"], "additionalProperties": false, "properties": { "variant": { "type": "string" }, "value": { "type": "integer" } } },
    "encoding": {
      "oneOf": [
        { "type": "null" },
        { "type": "object", "required": ["kind", "tag_type", "tag_offset", "payload_offset", "tags"], "additionalProperties": false,
          "properties": { "kind": { "const": "tagged" }, "tag_type": { "enum": ["U8", "U16", "U32", "I8", "I16", "I32"] }, "tag_offset": { "$ref": "#/$defs/uint" }, "payload_offset": { "$ref": "#/$defs/uint" }, "tags": { "type": "array", "items": { "$ref": "#/$defs/variant_value" } } } },
        { "type": "object", "required": ["kind", "dataful_variant", "niche_offset", "niche_type", "values"], "additionalProperties": false,
          "properties": { "kind": { "const": "niche" }, "dataful_variant": { "type": "string" }, "niche_offset": { "$ref": "#/$defs/uint" }, "niche_type": { "enum": ["U8", "U16", "U32", "U64"] }, "values": { "type": "array", "items": { "$ref": "#/$defs/variant_value" } } } },
        { "type": "object", "required": ["kind", "variant"], "additionalProperties": false, "properties": { "kind": { "const": "single" }, "variant": { "type": "string" } } },
        { "type": "object", "required": ["kind"], "additionalProperties": false, "properties": { "kind": { "const": "empty" } } }
      ]
    },
    "inspect_layout": {
      "type": "object", "required": ["target", "types"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" },
        "types": { "type": "array", "items": { "type": "object", "required": ["type", "size", "align", "fields", "encoding", "leaves"], "additionalProperties": false,
          "properties": { "type": { "type": "string" }, "size": { "$ref": "#/$defs/uint" }, "align": { "type": "integer", "minimum": 1 },
            "fields": { "type": "array", "items": { "type": "object", "required": ["name", "offset", "size"], "additionalProperties": false,
              "properties": { "name": { "type": "string" }, "offset": { "$ref": "#/$defs/uint" }, "size": { "$ref": "#/$defs/uint" } } } },
            "encoding": { "$ref": "#/$defs/encoding" }, "leaves": { "type": "array", "items": { "enum": ["i32", "i64", "f32", "f64"] } } } } } }
    },
    "inspect_wit": {
      "type": "object", "required": ["target", "package", "text"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "package": { "type": "string" }, "text": { "type": "string" } }
    },
    "inspect_deps": {
      "type": "object", "required": ["target", "root", "nodes"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "root": { "$ref": "#/$defs/package_ref" },
        "nodes": { "type": "array", "minItems": 1, "items": { "type": "object", "required": ["node", "version", "form", "source", "footprint", "edges"], "additionalProperties": false,
          "properties": { "node": { "type": "string" }, "version": { "type": "string" }, "form": { "enum": ["root", "source", "artifact"] }, "source": { "type": ["string", "null"] }, "footprint": { "$ref": "#/$defs/strings" },
            "edges": { "type": "array", "items": { "type": "object", "required": ["node", "requirement", "test"], "additionalProperties": false,
              "properties": { "node": { "type": "string" }, "requirement": { "type": "string" }, "test": { "type": "boolean" } } } } } } } }
    },
    "diff": {
      "allOf": [{ "$ref": "urn:sai:schema:semantic:1#/$defs/diff" }],
      "required": ["from", "to"],
      "properties": { "from": { "type": ["string", "null"], "pattern": "^[0-9a-f]{40}$" }, "to": { "type": ["string", "null"], "pattern": "^[0-9a-f]{40}$" } }
    },
    "edit": {
      "type": "object", "required": ["operation", "target", "dry_run", "impact", "edits", "changed_files"], "additionalProperties": false,
      "properties": { "operation": { "type": "string" }, "target": { "oneOf": [{ "$ref": "#/$defs/symbol_id" }, { "type": "string", "pattern": "^module:" }] }, "dry_run": { "type": "boolean" },
        "impact": { "$ref": "urn:sai:schema:semantic:1#/$defs/impact" },
        "edits": { "type": "array", "items": { "$ref": "urn:sai:schema:diagnostics:1#/$defs/edit" } }, "changed_files": { "$ref": "#/$defs/strings" } }
    },
    "bindgen_wit": {
      "type": "object", "required": ["target", "package", "output", "text"], "additionalProperties": false,
      "properties": { "target": { "$ref": "#/$defs/target" }, "package": { "type": "string" }, "output": { "oneOf": [{ "$ref": "#/$defs/file_record" }, { "type": "null" }] }, "text": { "type": ["string", "null"] } }
    },
    "explain": {
      "type": "object", "required": ["code", "class", "severity", "default_level", "stage", "owner", "since", "status", "superseded_by", "summary", "markdown"], "additionalProperties": false,
      "properties": { "code": { "type": "string" }, "class": { "enum": ["hard", "lint"] }, "severity": { "enum": ["error", "warning", "info", null] },
        "default_level": { "enum": ["allow", "info", "warn", "deny", null] }, "stage": { "type": "integer", "minimum": 0, "maximum": 7 }, "owner": { "type": "string" },
        "since": { "type": "string" }, "status": { "enum": ["active", "deprecated", "removed"] }, "superseded_by": { "type": ["string", "null"] }, "summary": { "type": "string" }, "markdown": { "type": "string" } }
    },
    "explain_list": {
      "type": "object", "required": ["codes"], "additionalProperties": false,
      "properties": { "codes": { "type": "array", "items": { "type": "object", "required": ["code", "class", "summary"], "additionalProperties": false,
        "properties": { "code": { "type": "string" }, "class": { "enum": ["hard", "lint"] }, "summary": { "type": "string" } } } } }
    }
  }
}
```

Notes:

1. In `inspect.deps`, the first node is the root package with `form` `"root"` and `source` `null`. Every other node has `form` `"source"` or `"artifact"`, the form chosen for the build (D-PKG-05), and `source` in the spelling of `sai.package` (`git "<url>"` or `path "<dir>"`).
2. The `tag_type` and `niche_type` values are the integer types of the tag or niche field as impl/06 §6.4 chooses them.
3. A consumer MUST ignore members it does not know, because new optional members and enum values can be added within version 1 ([11-diagnostics.md](11-diagnostics.md) §11.9.3). The `additionalProperties: false` constraints describe the members defined by this version of the chapter, and the conformance suite checks them against the toolchain of the same version.

---

## 13.9 Decisions introduced by this chapter

These rules fill gaps that [design/decisions.md](../design/decisions.md) leaves open. They use provisional IDs until they are merged into the decision record. N-H-1 to N-H-16 are introduced by [11-diagnostics.md](11-diagnostics.md).

| ID | Rule | Rationale |
|---|---|---|
| N-H-17 | Command names, option names, exit codes, and JSON result records are normative for every conforming tool that provides the command. Help text is informative. | Agents and CI scripts are written against the command line (P3, P4). Help text is for people and may improve freely. |
| N-H-18 | The CLI reads no configuration file. Environment variables affect only presentation, the location of the global cache, and transport (§13.7). | Every input that affects a result is in the package, the lock, the toolchain version, or the command line (draft §72, P8). |
| N-H-19 | `sai fetch` and `sai edit` are added to the commands of draft §77 and D-TOOL-03. | Offline builds need a way to fill the cache (package/03 §3.8.2). The semantic operations of draft §63 need a command-line entry point for agents without an LSP client. |
| N-H-20 | Workspace members are the root plus the `from path` dependencies inside the root's directory. There is no workspace manifest. | A workspace then needs no new concept, and every member is still an ordinary package (P7). |
| N-H-21 | Long options with `--name value` or `--name=value`, four short aliases, no combined short options, global options anywhere, and `--` for pass-through. | One predictable syntax (P9). Combined short options would add a second way to spell the same thing. |
| N-H-22 | Color is decided by `--color`, then `CLICOLOR_FORCE`, then `NO_COLOR`, then terminal detection per stream. JSON is never colored. | Follows established conventions, and the explicit option always wins (C-06). |
| N-H-23 | Exit codes 0, 1, 2, 3, 4, 101, and 130 with a fixed precedence. 3 is a negative result of a correct run. | Scripts must distinguish "the code is wrong" (1), "the command line is wrong" (2), and "the program or a test failed" (3) without parsing output. |
| N-H-24 | In v0.1 the toolchain version is the language version checked by `compiler` requirements. | One version number avoids a second compatibility matrix during v0.x. |
| N-H-25 | The default target is the first entry of `targets`. The default profile is `dev` for every command. | The package states its primary target first. Defaults never depend on the command, so `check` and `build` see the same program (P8). |
| N-H-26 | Naming one code in two different CLI level flags is an error. The order of flags has no meaning. | A command line whose result depends on flag order is easy to misread (P9). |
| N-H-27 | Output file names are fixed: `<name>.wasm`, `<name>.component.wasm`, and `wit/<name>.wit`. There is no option to rename outputs inside `out/`. | Tools find artifacts without reading configuration (P3). |
| N-H-28 | `.sai/index/` is local state and is listed in `.gitignore` by `sai new`. | Symbol IDs are tooling metadata, not build inputs (draft §64). Committing them would create merge conflicts with no benefit to the build. |
| N-H-29 | The files in `out/` are byte-identical for identical inputs, independent of paths, time, host, `--jobs`, caches, and incrementality. | Reproducible builds (draft §72, P8). |
| N-H-30 | Approval: `--approve` approves. Otherwise a human-format command with terminals on standard input and standard error prompts, and every other case fails with the approval code. | [package/02](../package/02-resolver.md) §2.12.3 allows prompting. A non-interactive run must never wait for input or approve silently (P5). |
| N-H-31 | Result records use package references `ns:pkg@version`, file records with SHA-256, and arrays in a stated or byte order. | Deterministic, machine-checkable output (P3, P8). |
| N-H-32 | `sai new` has one template, a library with one function and one test, and writes no lock. | Templates are a second configuration surface. A library is the common case, and a lock is created by the first command that resolves. |
| N-H-33 | `sai check` reports every error that `sai build` would report except those that need generated code. | Agents use `check` as the fast inner loop, so it must not miss errors that a build would find later (P3). |
| N-H-34 | The root package is always compiled from source. A root without `source` cannot be built and is only validated by `check`. | The root's `artifact` declaration describes the package to its dependents. Building it from its own published artifact would be circular. |
| N-H-35 | `sai run` uses an embedded runtime and grants only the authority given by `--env`, `--dir`, and `--allow-network`. | Least authority by default (P5). The run is the same on every host. |
| N-H-36 | `sai test` streams results in test order, independent of `--jobs`, and the command line adds no authority to tests. | Deterministic output (P8). Test authority is a language rule (spec/14), not a command-line choice. |
| N-H-37 | `sai add` appends a new requirement as the last entry of its block. If the block is missing, it is appended at the end of the file. | Appending gives the smallest diff, and the order of requirements has no meaning. |
| N-H-38 | Opt-in Wasm features are enabled only with the repeatable `--wasm-feature` option, whose values are `tail-call` and `simd128`. | impl/06 §6.2.2 delegates the syntax. Enabling a feature changes no meaning, so a per-build flag is enough, and keeping it out of `sai.package` and profiles keeps the default output the same for every user. |
| N-H-39 | `sai lsp` uses standard input and output for the protocol. `--format` has no effect, and nothing is written to standard error once the connection runs. | Editors start servers on stdio. Mixing NDJSON or human text into the stream would break the framing. |
| N-H-40 | The global cache is located by `SAI_CACHE_DIR`, which defaults to the platform cache directory, exactly as package/04 §4.4.1 (N-G-50) defines. | The cache is machine state, not package state. One variable keeps the location explicit, and platform defaults follow host conventions. |
| N-H-41 | `sai edit` refuses an operation that breaks the public API unless `--allow-breaking` is given. A dry run never refuses. | Breaking changes must be explicit (P1, P5). Agents see the impact first and decide. |
| N-H-42 | `sai diff` compares `HEAD` with the working tree by default, and takes up to two Git revisions resolved by `gix`. | It matches the common review question "what did I change?". Git is the only source of revisions (D-TOOL-05, D-PKG-01). |
| N-H-82 | The test harness is one core module of the target's pointer width on every Wasm target, and it is never wrapped as a component. It imports only `panic`, `generated` and `counterexample` from the module `sai-harness`. It exports `test.<k>` for each test, `case.<k>` and `counterexample.<k>` for each property, and `memory` (§13.6.6.2). | Tests are hermetic, so the harness has no WASI or interface import, and a component would add only glue (P7). The same core form runs on every Wasm target, including `wasm64-wasi`, whose component output is optional (targets N-F-65). |
| N-H-83 | The runner drives the cases of a property from the host, with one call of `case.<k>(i)` per case in one instance. The base seed and the number of cases come from the compiler's manifest, and the runner never computes them again (§13.6.6.2, §13.6.6.3). | The runner knows the failing case without reading the memory of a trapped instance (P8). One computation of `S` and `N` cannot disagree with another (P9). |
| N-H-84 | A failure is a `panic` exactly when the harness channel delivered a report. The report is parsed against the file strings of the build, and its location is zero-length. Any other trap is classified by the engine's trap code into the reasons of spec/14 §14.4.1 item 4. The `generated` import decides whether a counterexample can exist (§13.6.6.3). | spec/14 §14.4.2 requires the runner to read the report. Matching the known file strings keeps the parse unambiguous for every path (P8). A report holds a position only, so a zero-length location claims no extent that it does not know (P1). |
| N-H-85 | Every test runs in a fresh store with the same engine configuration on every host: the target's features, NaN canonicalization, no fuel, no epoch interruption, no memory limit below the target's, and an 8 MiB engine stack (§13.6.6.3). | spec/14 §14.4.3 item 3 forbids limits of the runner, and §14.7.3 requires isolation. Fixed settings make the outcome independent of the host (P8). |
| N-H-86 | The selection options of `sai test` are the input `test_options`, and the selection is the query `test_selection`, which reads only item trees. `--list` runs the check and lists the selection only if the check reports no error (§13.6.6.1). | The check never depends on the filters (P8), and the list names exactly the tests that would run (P9). |

---

## 13.10 Diagnostics

Codes owned by this chapter, and codes of other documents that this chapter reports. Usage errors (§13.3.5) exit with 2.

| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_COMMAND` | error | The command or subcommand does not exist (usage error, §13.3.5). |
| `UNKNOWN_OPTION` | error | The option does not exist for this command (usage error). |
| `MISSING_ARGUMENT` | error | A value option has no value, or a required argument is missing (usage error). |
| `INVALID_OPTION_VALUE` | error | A value is not one of the allowed values or not of the required form (usage error). |
| `UNEXPECTED_ARGUMENT` | error | A positional argument that the command does not take (usage error). |
| `CONFLICTING_OPTIONS` | error | Two options that cannot be combined, a value option given twice, or one lint code in two level flags (usage error). |
| `NO_PACKAGE_FOUND` | error | No `sai.package` was found from the start directory upward (§13.2.1, usage error). |
| `OUTPUT_WRITE_FAILED` | error | A file that the toolchain had to write could not be written. Exit code 4 (§13.3.4). |
| `CACHE_UNAVAILABLE` | error | The location of the global cache cannot be determined (§13.7). |
| `DESTINATION_NOT_EMPTY` | error | `sai new` was given a directory that exists and is not empty (§13.6.1). |
| `ROOT_PACKAGE_HAS_NO_SOURCE` | error | A command that compiles the root package was run on a root without `source` (§13.6.3). |
| `TARGET_NOT_RUNNABLE` | error | `sai run` was used with a target that is not a WASI target: a core target or a native target (§13.6.5). |
| `MISSING_ENTRY_POINT` | error | `sai run` was used on a reactor component; the package has no `main` (§13.6.5). |
| `PATH_NOT_FOUND` | error | A path given to `sai fmt` does not exist (§13.6.7). |
| `WORKSPACE_LOCK_FAILED` | error | `sai fmt` could not identify or acquire a selected file's package lock (§13.5.4, §13.6.7). |
| `NOT_PRESENT_IN_BUILD` | info | The item named by `sai size --why` or `sai inspect effects --why` is not in the build or the effect set (§13.6.14, §13.6.15). |
| `NOT_A_GIT_REPOSITORY` | error | `sai diff` needs a revision, but the workspace root is not in a Git repository (§13.6.17). |
| `UNKNOWN_REVISION` | error | A revision given to `sai diff` does not resolve to a commit (§13.6.17). |
| `OPERATION_BREAKS_PUBLIC_API` | error | `sai edit` would break the public API and `--allow-breaking` was not given (§13.6.18). |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [spec/01](../spec/01-introduction.md) §1.4.5; reported for commands of an unimplemented stage (§13.1.2). |
| `INTERNAL_COMPILER_ERROR` | error | defined in [spec/01](../spec/01-introduction.md) §1.10; reported as in [11-diagnostics.md](11-diagnostics.md) §11.4.6. Exit code 101. |
| `UNKNOWN_DIAGNOSTIC_CODE` | error | defined in [11-diagnostics.md](11-diagnostics.md) §11.13. |
| `DEPRECATED_DIAGNOSTIC_CODE` | warning | defined in [11-diagnostics.md](11-diagnostics.md) §11.12.3. |
| `UNKNOWN_LINT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.15.4; the registry lookup is in [11-diagnostics.md](11-diagnostics.md) §11.12.3. A usage error when it comes from a lint flag. |
| `NOT_A_LINT` | error | defined in [spec/15](../spec/15-attributes-and-lints.md) §15.15.4; the registry lookup is in [11-diagnostics.md](11-diagnostics.md) §11.12.3. A usage error when it comes from a lint flag. |
| `TARGET_NOT_DECLARED` | error | defined in [package/01](../package/01-package-contract.md) §1.8. |
| `EXPORT_WORLD_NOT_APPLIED` | info | defined in [spec/13](../spec/13-components-and-wit.md) §13.6.2 (N-D-32). |
| `SIZE_BUDGET_EXCEEDED` | error | defined in [package/01](../package/01-package-contract.md) §1.14.3; checked by `sai build`, `sai component build`, `sai run`, and `sai size` after code generation (§13.6.3 item 4). The build fails and writes nothing; `sai size` still prints its report (§13.6.14). |
| `NO_COMPONENT_EXPORTS` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.5.3. |
| `UNKNOWN_WIT_ITEM` | error | defined in [spec/13](../spec/13-components-and-wit.md) §13.7.2. |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | defined in [spec/08](../spec/08-effects-and-capabilities.md) §8.12.6. |
| `LOCK_MISSING` | error | defined in [package/03](../package/03-lock-file.md) §3.7.1. |
| `LOCK_OUT_OF_DATE` | error | defined in [package/03](../package/03-lock-file.md) §3.8.1. |
| `OFFLINE_FETCH_REQUIRED` | error | defined in [package/03](../package/03-lock-file.md) §3.8.2. |
| `CAPABILITY_ESCALATION` | error | defined in [package/02](../package/02-resolver.md) §2.12.3; reported when approval is missing (§13.4.6). |
| `SOURCE_CHANGED` | error | defined in [package/04](../package/04-distribution-and-security.md) §4.7.2; approval per [package/02](../package/02-resolver.md) §2.12.3; reported when approval is missing (§13.4.6). |
| `TAG_MOVED` | error | defined in [package/04](../package/04-distribution-and-security.md) §4.3.3; an approval item of `sai update` (§4.7.2 there), reported when approval is missing (§13.4.6, §13.6.8). |
| `TEST_DEPENDENCY_UNAVAILABLE` | error | defined in [spec/14](../spec/14-testing.md) §14.2.5 item 5; a test-only use in a workspace member that is not the root (§13.2.2). |
| `ALREADY_REQUIRED`, `NO_RELEASE_TAGS`, `VERSION_NOT_FOUND` | error | defined in [package/02](../package/02-resolver.md) §2.11 (`sai add`). |
| `PACKAGE_REQUIRES_ITSELF` | error | defined in [package/01](../package/01-package-contract.md) §1.10; reported by `sai add` (package/02 §2.11). |
| `CONFUSABLE_PACKAGE_NAME` | warning | defined in [package/04](../package/04-distribution-and-security.md) §4.11.1; reported by `sai add` (package/02 §2.11). |
| `NOT_REQUIRED` | error | defined in [package/02](../package/02-resolver.md) §2.10.6 (`sai remove`). |
| `UPDATE_CHANGES_LINE`, `UPDATE_VERSION_NOT_SELECTED` | error | defined in [package/02](../package/02-resolver.md) §2.10.5 (`sai update`). |
| `UNFORMATTED_FILE` | error | defined in [14-formatter.md](14-formatter.md) §14.11 (`sai fmt --check`). |
| `UNKNOWN_SYMBOL_PATH`, `UNKNOWN_SYMBOL_ID` | error | defined in [12-semantic-tooling.md](12-semantic-tooling.md) §12.2.5. |
| `UNKNOWN_OPERATION`, `INVALID_OPERATION_ARGUMENT`, `INVALID_OPERATION_TARGET`, `FILE_HAS_SYNTAX_ERRORS`, `OPERATION_INTRODUCES_ERRORS`, and the precondition codes of the operations | error | defined in [12-semantic-tooling.md](12-semantic-tooling.md) §12.6; reported by `sai edit` (§13.6.18). |
| `SYMBOL_INDEX_DISCARDED` | info | defined in [12-semantic-tooling.md](12-semantic-tooling.md) §12.9.3; reported by every command that analyzes the workspace. |
