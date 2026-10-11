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
    use super::{COMMANDS, render_top_level_help, render_version_for};

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
