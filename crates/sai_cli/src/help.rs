use std::fmt::Write as _;

/// One command row in the order shown by `sai --help`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    /// Display-only command spelling; dispatch parses command and subcommand
    /// as separate argument tokens.
    pub display_name: &'static str,
    pub summary: &'static str,
}

/// The command list from `docs/impl/13-cli.md` §13.1.2.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        display_name: "new",
        summary: "create a package",
    },
    CommandSpec {
        display_name: "check",
        summary: "analyze without code generation",
    },
    CommandSpec {
        display_name: "build",
        summary: "build the target artifact",
    },
    CommandSpec {
        display_name: "component build",
        summary: "build a component",
    },
    CommandSpec {
        display_name: "run",
        summary: "build and run a command component",
    },
    CommandSpec {
        display_name: "test",
        summary: "build and run tests",
    },
    CommandSpec {
        display_name: "fmt",
        summary: "format source files",
    },
    CommandSpec {
        display_name: "add",
        summary: "add a dependency",
    },
    CommandSpec {
        display_name: "remove",
        summary: "remove a dependency",
    },
    CommandSpec {
        display_name: "update",
        summary: "move dependencies to newer versions",
    },
    CommandSpec {
        display_name: "lock",
        summary: "resolve and write sai.lock without building",
    },
    CommandSpec {
        display_name: "fetch",
        summary: "download every active locked dependency",
    },
    CommandSpec {
        display_name: "size",
        summary: "explain the size of the artifact",
    },
    CommandSpec {
        display_name: "inspect",
        summary: "print analysis results",
    },
    CommandSpec {
        display_name: "graph",
        summary: "query the semantic graphs",
    },
    CommandSpec {
        display_name: "diff",
        summary: "semantic diff between two revisions",
    },
    CommandSpec {
        display_name: "edit",
        summary: "apply a structured semantic operation",
    },
    CommandSpec {
        display_name: "bindgen wit",
        summary: "generate WIT or show WIT from the SAI side",
    },
    CommandSpec {
        display_name: "explain",
        summary: "explain a diagnostic code",
    },
    CommandSpec {
        display_name: "lsp",
        summary: "language server",
    },
];

const LONGEST_COMMAND_NAME: usize = 15;
const WASI_VERSION: &str = "0.2.3";
// The v0.1 reference implementation's targets, filtered into canonical order.
const TARGETS: &str = "wasm32-core wasm64-core wasm32-wasi";

const COMMAND_SYNOPSES: &[(&str, &str)] = &[
    (
        "check",
        "check [--target <name>] [--profile <name>] [--tests] [--fix] [BUILD-OPTIONS]",
    ),
    (
        "build",
        "build [--target <name>] [--profile <name>] [BUILD-OPTIONS]",
    ),
    (
        "component build",
        "component build [--target <name>] [--profile <name>] [BUILD-OPTIONS]",
    ),
    (
        "run",
        "run [--target <name>] [--profile <name>] [--env <name>[=<value>]]... [--dir <host>[::<guest>]]... [--allow-network] [BUILD-OPTIONS] [-- <argument>...]",
    ),
    (
        "test",
        "test [--target <name>] [--profile <name>] [--package <ns>:<pkg>]... [--workspace] [--exact] [--list] [--no-run] [--seed <n>] [BUILD-OPTIONS] [<filter>...] [-- <filter>...]",
    ),
    ("fmt", "fmt [--check] [<path>...]"),
    (
        "add",
        "add <url> [--dir <subdir>] [--version <requirement>] [--test] [--only <target>]... [--grant <capability>]... [--locked] [--offline] [--frozen] [--approve]",
    ),
    (
        "remove",
        "remove <ns>:<pkg> [--locked] [--offline] [--frozen] [--approve]",
    ),
    (
        "update",
        "update [<ns>:<pkg>[@<version>]]... [--dry-run] [--locked] [--offline] [--frozen] [--approve]",
    ),
    ("lock", "lock [--locked] [--offline] [--frozen] [--approve]"),
    (
        "fetch",
        "fetch [--locked | --frozen] [--offline] [--approve]",
    ),
    (
        "size",
        "size [--target <name>] [--profile <name>] [--why <name>] [BUILD-OPTIONS]",
    ),
    (
        "inspect",
        "inspect <subcommand> [ARGUMENTS] [BUILD-OPTIONS]",
    ),
    (
        "graph",
        "graph <graph> [<start>...] [--in | --both] [--depth <n>] [--edge <kind>]... [--deps]",
    ),
    ("diff", "diff [<from> [<to>]] [--size] [--public]"),
    ("edit", "edit <operation> [OPTIONS]"),
    ("bindgen wit", "bindgen wit [OPTIONS]"),
    ("explain", "explain <CODE> | --list"),
    ("lsp", "lsp [OPTIONS]"),
];

const BUILD_OPTIONS: &str = concat!(
    "  --target <name>          Select one target.\n",
    "  --profile <name>         Select `dev`, `release`, or `tiny`.\n",
    "  --allow <CODE>           Set a lint level; repeatable.\n",
    "  --warn <CODE>            Set a lint level; repeatable.\n",
    "  --deny <CODE>            Set a lint level; repeatable.\n",
    "  --deny-warnings          Treat remaining warnings as errors.\n",
    "  --jobs <n>               Set the maximum worker count.\n",
);

const RESOLUTION_OPTIONS: &str = concat!(
    "  --locked                 Do not write `sai.lock`.\n",
    "  --offline                Do not access the network.\n",
    "  --frozen                 Combine `--locked` and `--offline`.\n",
    "  --approve                Approve reviewed dependency changes.\n",
);

const WASM_FEATURE_OPTION: &str =
    "  --wasm-feature <name>    Enable `tail-call` or `simd128`; repeatable.\n";

const LSP_OPTIONS: &str = concat!(
    "  --stdio                  Use standard input and output for LSP.\n",
    "  --target <name>          Set the initial target.\n",
    "  --profile <name>         Set the initial profile.\n",
    "  --locked                 Do not write `sai.lock`.\n",
    "  --offline                Do not access the network.\n",
    "  --frozen                 Combine `--locked` and `--offline`.\n",
);

const GLOBAL_OPTIONS: &str = concat!(
    "-C, --directory <dir>  Package discovery start directory.\n",
    "-q, --quiet            Suppress progress and summary lines.\n",
    "-v, --verbose          Show progress for every package.\n",
    "-h, --help             Print this help.\n",
    "--format <format>      Output format: `human` or `json`.\n",
    "--color <color>        Color policy: `auto`, `always`, or `never`.\n",
    "--max-errors <n>       Maximum diagnostics; default 50.\n",
    "--version              Print version.\n",
);

const LSP_GLOBAL_OPTIONS: &str = concat!(
    "-q, --quiet            Suppress progress and summary lines.\n",
    "-v, --verbose          Show progress for every package.\n",
    "-h, --help             Print this help.\n",
    "--format <format>      Accepted for compatibility; has no effect.\n",
    "--color <color>        Color policy: `auto`, `always`, or `never`.\n",
    "--max-errors <n>       Maximum diagnostics; default 50.\n",
    "--version              Print version.\n",
);

/// Renders top-level help in the command order specified by the CLI contract.
pub fn render_top_level_help() -> String {
    let mut output = String::from(
        "Usage: sai [GLOBAL-OPTIONS] <command> [<subcommand>] [OPTIONS] [ARGUMENTS] [-- PASSTHROUGH]\n\nCommands:\n",
    );
    for command in COMMANDS {
        writeln!(
            output,
            "  {:<width$} {}",
            command.display_name,
            command.summary,
            width = LONGEST_COMMAND_NAME
        )
        .expect("writing to a String cannot fail");
    }
    output
}

/// Renders command-specific help for `sai new`.
pub fn render_new_help() -> String {
    concat!(
        "Usage: sai new <path> --name <ns>:<pkg> [--target <name>]...\n\n",
        "Create a library package in a new directory.\n\n",
        "Options:\n",
        "      --name <ns>:<pkg>  Required package identity. Each segment is 1–64 bytes, starts with a lowercase ASCII letter, and contains lowercase ASCII letters, digits, and single interior hyphens. The `wasi` namespace and `sai:core`/`sai:alloc` identities are reserved.\n",
        "      --target <name>    Target to declare; repeatable and order-preserving. Defaults to `wasm32-wasi`.\n",
        "      --format <format>  Output format: `human` or `json`.\n",
        "      --color <color>    Color policy: `auto`, `always`, or `never`.\n",
        "  -C, --directory <dir>  Package discovery start directory (unused by `new`).\n",
        "  -q, --quiet            Suppress progress lines.\n",
        "  -v, --verbose          Show progress for every package.\n",
        "      --max-errors <n>   Maximum diagnostics; default 50.\n",
        "  -h, --help             Print help.\n",
        "      --version          Print version.\n",
    )
    .to_owned()
}

/// Renders synopsis-oriented help for a documented command.
pub fn render_command_help(name: &str) -> Option<String> {
    if name == "new" {
        return Some(render_new_help());
    }
    let synopsis = COMMAND_SYNOPSES
        .iter()
        .find_map(|(command, synopsis)| (*command == name).then_some(*synopsis))?;
    let command = COMMANDS
        .iter()
        .find(|command| command.display_name == name)?;
    let mut output = format!("Usage: sai {synopsis}\n\n{}", command.summary);
    let options = command_options(name);
    if !options.is_empty() {
        output.push_str("\n\nOptions:\n");
        output.push_str(&options);
    }
    output.push_str("\nGlobal options:\n");
    output.push_str(if name == "lsp" {
        LSP_GLOBAL_OPTIONS
    } else {
        GLOBAL_OPTIONS
    });
    Some(output)
}

fn command_options(name: &str) -> String {
    let mut options = String::new();
    if name == "lsp" {
        options.push_str(LSP_OPTIONS);
        return options;
    }
    options.push_str(match name {
        "check" => concat!(
            "  --tests                  Include tests and test-only code.\n",
            "  --fix                    Apply machine-applicable fixes.\n",
        ),
        "run" => concat!(
            "  --env <name>[=<value>]   Set or pass through an environment variable; repeatable.\n",
            "  --dir <host>[::<guest>]  Preopen a host directory; repeatable.\n",
            "  --allow-network          Allow outgoing network requests.\n",
        ),
        "test" => concat!(
            "  --package <ns>:<pkg>     Select a workspace member; repeatable.\n",
            "  --workspace              Select every workspace member.\n",
            "  --exact                  Match the complete test path.\n",
            "  --list                   List selected tests without generating code.\n",
            "  --no-run                 Build the harness without running it.\n",
            "  --seed <n>               Set the default property-test seed.\n",
        ),
        "fmt" => concat!(
            "  --check                  Report files that need formatting; write nothing.\n",
            "  --stdin-kind <kind>      Format stdin as `source` or `package`.\n",
        ),
        "add" => concat!(
            "  --path <dir>             Add a local package dependency.\n",
            "  --dir <subdir>           Select a package directory in the repository.\n",
            "  --version <requirement>  Set the version requirement.\n",
            "  --test                   Add a test-only dependency.\n",
            "  --only <target>          Restrict the dependency to a target; repeatable.\n",
            "  --grant <capability>     Grant a capability; repeatable.\n",
        ),
        "update" => "  --dry-run                Show the resolution without writing it.\n",
        "size" => "  --why <name>             Explain why an item is in the artifact.\n",
        "inspect" => concat!(
            "  unsafe | mir | effects | capabilities | layout | wit | deps\n",
            "                           Select the report to inspect.\n",
            "  --package <ns>:<pkg>     Select packages with `unsafe`; repeatable.\n",
            "  --phase <phase>          Select a MIR phase with `mir`.\n",
            "  --why <effect>           Show an effect chain for `effects`.\n",
            "  --all                    Show every type layout with `layout`.\n",
            "  --tests                  Include test dependency edges with `deps`.\n",
        ),
        "graph" => concat!(
            "  --in                     Follow edges against their direction.\n",
            "  --both                   Follow edges in both directions.\n",
            "  --depth <n>              Limit traversal depth to 1–64.\n",
            "  --edge <kind>            Keep one edge kind; repeatable.\n",
            "  --deps                   Expand dependency nodes.\n",
        ),
        "diff" => concat!(
            "  --size                   Include the estimated size change.\n",
            "  --public                 Compare public API only.\n",
        ),
        "edit" => concat!(
            "  --request <file>         Read a structured operation from JSON.\n",
            "  --dry-run                Show impact and edits without writing.\n",
            "  --allow-breaking         Apply a breaking public API change.\n",
            "  --size                   Estimate the artifact size change.\n",
        ),
        "bindgen wit" => concat!(
            "  -o <path>                Write generated WIT to a path (`-` for stdout).\n",
            "  --show <name>            Show the SAI view of a WIT package or interface.\n",
        ),
        "explain" => "  --list                   List diagnostic codes.\n",
        "lsp" => "  --stdio                  Use standard input and output for LSP.\n",
        _ => "",
    });

    if is_building_command(name) {
        options.push_str(BUILD_OPTIONS);
    }
    if is_resolving_command(name) {
        options.push_str(RESOLUTION_OPTIONS);
    }
    if matches!(name, "build" | "component build" | "run" | "test" | "size") {
        options.push_str(WASM_FEATURE_OPTION);
    }
    options
}

fn is_building_command(name: &str) -> bool {
    matches!(
        name,
        "check"
            | "build"
            | "component build"
            | "run"
            | "test"
            | "size"
            | "inspect"
            | "graph"
            | "diff"
            | "edit"
            | "bindgen wit"
    )
}

fn is_resolving_command(name: &str) -> bool {
    is_building_command(name) || matches!(name, "add" | "remove" | "update" | "lock" | "fetch")
}

/// Renders `sai --version`, optionally including the detailed version lines.
pub fn render_version(verbose: bool) -> String {
    render_version_for(env!("CARGO_PKG_VERSION"), verbose)
}

fn render_version_for(version: &str, verbose: bool) -> String {
    let mut output = format!("sai {version}\n");
    if verbose {
        writeln!(output, "language {version}").expect("writing to a String cannot fail");
        writeln!(output, "wasi {WASI_VERSION}").expect("writing to a String cannot fail");
        writeln!(output, "targets {TARGETS}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        COMMANDS, render_command_help, render_new_help, render_top_level_help, render_version_for,
    };

    #[test]
    fn lists_every_command_in_the_documented_order_with_its_summary() {
        let help = render_top_level_help();
        let command_lines = help
            .lines()
            .skip_while(|line| *line != "Commands:")
            .skip(1)
            .collect::<Vec<_>>();

        assert_eq!(command_lines.len(), COMMANDS.len());
        for (line, command) in command_lines.into_iter().zip(COMMANDS) {
            assert!(line.starts_with(&format!("  {:<15} ", command.display_name)));
            assert!(line.ends_with(command.summary));
        }
    }

    #[test]
    fn renders_the_short_version_as_one_line() {
        assert_eq!(render_version_for("0.1.0", false), "sai 0.1.0\n");
    }

    #[test]
    fn documents_new_package_options_and_identity_rules() {
        let help = render_new_help();

        assert!(help.starts_with("Usage: sai new <path> --name <ns>:<pkg>"));
        assert!(help.contains("--target <name>"));
        assert!(help.contains("1–64 bytes"));
        assert!(help.contains("namespace and `sai:core`/`sai:alloc` identities are reserved"));
        assert!(help.contains("Defaults to `wasm32-wasi`"));
        assert!(help.contains("Package discovery start directory (unused by `new`)."));
    }

    #[test]
    fn command_help_includes_command_and_shared_options() {
        let check = render_command_help("check").unwrap();
        assert!(check.contains("--tests"));
        assert!(check.contains("--fix"));
        assert!(check.contains("--target"));
        assert!(check.contains("--jobs"));
        assert!(check.contains("--format"));

        let bindgen = render_command_help("bindgen wit").unwrap();
        assert!(bindgen.contains("-o <path>"));
        assert!(bindgen.contains("--show <name>"));
        assert!(!bindgen.contains("--check"));

        let lsp = render_command_help("lsp").unwrap();
        assert!(lsp.contains("--stdio"));
        assert!(!lsp.contains("--directory"));
    }

    #[test]
    fn renders_verbose_version_metadata_in_the_documented_order() {
        assert_eq!(
            render_version_for("0.1.0", true),
            concat!(
                "sai 0.1.0\n",
                "language 0.1.0\n",
                "wasi 0.2.3\n",
                "targets wasm32-core wasm64-core wasm32-wasi\n",
            )
        );
    }
}
