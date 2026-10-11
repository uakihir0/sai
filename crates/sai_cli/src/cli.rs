use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Component, Path, PathBuf};

use crate::args::{ColorChoice, OutputFormat, ParsedArgs, UsageError, UsageErrorKind, parse_args};
use crate::help::{render_command_help, render_top_level_help, render_version};
use crate::new_package::parse_new_command;
use sai_diagnostics::EXPLAIN_CODE_SUMMARIES;
use sai_driver::CancellationToken;
use sai_driver::new_package::{
    NewPackageError, NewPackageReceipt, create_new_package_with_cancellation,
};

struct CliDiagnostic {
    code: &'static str,
    message: String,
    argument_index: Option<usize>,
    argument: String,
    help: Option<String>,
    data: Option<String>,
}

#[derive(Clone, Copy)]
struct CliOutputOptions {
    format: OutputFormat,
    quiet: bool,
    color: ColorChoice,
    max_errors: usize,
}

/// Runs the implemented CLI commands and writes their output to the supplied streams.
///
/// The process entry point supplies locked standard streams. Keeping the writers
/// injectable makes output and broken-pipe behavior testable without subprocesses.
pub fn run(arguments: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write) -> i32 {
    run_with_cancellation(arguments, &CancellationToken::default(), stdout, stderr)
}

/// Runs the CLI with a cancellation token supplied by the driver.
pub fn run_with_cancellation(
    arguments: &[OsString],
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    if arguments.is_empty() && !cancellation.is_cancelled() {
        return match stderr.write_all(render_top_level_help().as_bytes()) {
            Ok(()) => 2,
            Err(_) => 4,
        };
    }
    let parsed = parse_args(arguments);
    if cancellation.is_cancelled() {
        return report_interrupted(&parsed, stdout, stderr);
    }
    if parsed.globals.version {
        return write_stdout_text(stdout, &render_version(parsed.globals.verbose), stderr);
    }
    if parsed.globals.help {
        let help = command_help(&parsed);
        return write_stdout_text(stdout, &help, stderr);
    }

    let Some(command) = parsed.command.as_ref() else {
        return report_usage_errors(&parsed, "sai", parsed.usage_errors.clone(), stdout, stderr);
    };
    let command_name = command.name.to_string_lossy().into_owned();
    match command_name.as_str() {
        "new" => run_new(&parsed, command, cancellation, stdout, stderr),
        "explain" => run_explain(&parsed, command, stdout, stderr),
        "help" => {
            let help = command_help(&parsed);
            write_stdout_text(stdout, &help, stderr)
        }
        _ => {
            let feature = unimplemented_command(command);
            let mut errors = parsed.usage_errors.clone();
            if feature.is_some() {
                errors.extend(validate_known_command_options(command));
            }
            if !errors.is_empty() {
                report_usage_errors(&parsed, &command_path(command), errors, stdout, stderr)
            } else if let Some((stage, feature)) = feature {
                report_feature_not_implemented(
                    CliOutputOptions {
                        format: parsed.globals.format,
                        quiet: parsed.globals.quiet,
                        color: parsed.globals.color,
                        max_errors: parsed.globals.max_errors,
                    },
                    &command_path(command),
                    stage,
                    &feature,
                    stdout,
                    stderr,
                )
            } else {
                let mut errors = parsed.usage_errors.clone();
                if let Some(error) = subcommand_usage_error(command) {
                    errors.push(error);
                } else {
                    errors.push(UsageError {
                        kind: UsageErrorKind::UnknownCommand,
                        argument_index: command.command_index + 1,
                        argument: command.name.clone(),
                    });
                }
                report_usage_errors(&parsed, &command_path(command), errors, stdout, stderr)
            }
        }
    }
}

fn run_explain(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let mut errors = parsed.usage_errors.clone();
    errors.extend(validate_known_command_options(command));
    if !errors.is_empty() {
        return report_usage_errors(parsed, "explain", errors, stdout, stderr);
    }

    let global_indices = command
        .global_argument_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let list = command
        .arguments
        .iter()
        .enumerate()
        .any(|(index, argument)| {
            !global_indices.contains(&index)
                && crate::args::split_os_option(argument).is_some_and(|(name, _)| name == "--list")
        });
    if list {
        return report_explain_list(parsed.globals.format, stdout, stderr);
    }

    let positionals = collect_command_positionals(command, &None);
    let name = positionals
        .first()
        .map(|(argument, _)| argument.to_string_lossy().into_owned())
        .unwrap_or_default();
    if sai_diagnostics::Code::from_name(&name).is_none() {
        return report_unknown_diagnostic_code(parsed, &name, stdout, stderr);
    }

    report_feature_not_implemented(
        CliOutputOptions {
            format: parsed.globals.format,
            quiet: parsed.globals.quiet,
            color: parsed.globals.color,
            max_errors: parsed.globals.max_errors,
        },
        "explain",
        0,
        "sai explain",
        stdout,
        stderr,
    )
}

fn report_explain_list(
    format: OutputFormat,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    match format {
        OutputFormat::Human => {
            let mut output = String::new();
            for entry in EXPLAIN_CODE_SUMMARIES {
                use std::fmt::Write as _;
                writeln!(
                    output,
                    "{} {} {}",
                    entry.code.as_str(),
                    entry.class.as_str(),
                    entry.summary
                )
                .expect("writing to a String cannot fail");
            }
            write_stdout_text(stdout, &output, stderr)
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_explain_list(stdout) {
                report_stdout_failure(stderr, error);
                4
            } else {
                0
            }
        }
    }
}

fn write_json_explain_list(writer: &mut impl Write) -> io::Result<()> {
    write_json_success_header(writer, "explain")?;
    writer.write_all(b"{\"type\":\"result\",\"kind\":\"explain.list\",\"data\":{\"codes\":[")?;
    for (index, entry) in EXPLAIN_CODE_SUMMARIES.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        write!(
            writer,
            "{{\"code\":\"{}\",\"class\":\"{}\",\"summary\":\"{}\"}}",
            entry.code.as_str(),
            entry.class.as_str(),
            escape_json(entry.summary)
        )?;
    }
    writeln!(writer, "]}}}}")?;
    writeln!(
        writer,
        "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,\"dependency\":0}},\"exit_code\":0}}"
    )
}

fn report_unknown_diagnostic_code(
    parsed: &ParsedArgs,
    name: &str,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let candidates = diagnostic_code_candidates(name);
    let safe_name = diagnostic_display_name(name);
    let diagnostic = CliDiagnostic {
        code: "UNKNOWN_DIAGNOSTIC_CODE",
        message: format!("unknown diagnostic code `{safe_name}`"),
        argument_index: None,
        argument: String::new(),
        help: candidates
            .first()
            .map(|candidate| format!("did you mean `{candidate}`?")),
        data: Some(format!(
            "{{\"name\":\"{}\",\"candidates\":[{}]}}",
            escape_json(name),
            candidates
                .iter()
                .map(|candidate| format!("\"{candidate}\""))
                .collect::<Vec<_>>()
                .join(",")
        )),
    };

    match parsed.globals.format {
        OutputFormat::Human => {
            if let Err(error) = write_human_diagnostics(
                stderr,
                "explain",
                std::slice::from_ref(&diagnostic),
                !parsed.globals.quiet,
                color_enabled(parsed.globals.color, io::stderr().is_terminal()),
                1,
                parsed.globals.max_errors,
            ) {
                report_stderr_failure(error)
            } else {
                1
            }
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_diagnostics(stdout, "explain", &[diagnostic], 1, 1) {
                report_stdout_failure(stderr, error);
                4
            } else {
                1
            }
        }
    }
}

fn diagnostic_display_name(name: &str) -> String {
    const MAX_CHARS: usize = 64;
    let mut display = String::new();
    let mut truncated = false;
    for character in name.chars() {
        let value = if character.is_control() {
            format!("\\u{{{:X}}}", character as u32)
        } else {
            character.to_string()
        };
        if display.chars().count() + value.chars().count() > MAX_CHARS {
            truncated = true;
            break;
        }
        display.push_str(&value);
    }
    if truncated {
        while display.chars().count() >= MAX_CHARS {
            display.pop();
        }
        display.push('…');
    }
    display
}

fn diagnostic_code_candidates(name: &str) -> Vec<&'static str> {
    let input = name.chars().collect::<Vec<_>>();
    let maximum_code_length = EXPLAIN_CODE_SUMMARIES
        .iter()
        .map(|entry| entry.code.as_str().chars().count())
        .max()
        .unwrap_or_default();
    if input.len() > 2 && input.len() > maximum_code_length.saturating_mul(3) / 2 {
        return Vec::new();
    }

    let maximum_distance = (input.len() / 3).max(1);
    let mut candidates = Vec::new();
    for entry in EXPLAIN_CODE_SUMMARIES {
        let candidate = entry.code.as_str();
        let candidate_characters = candidate.chars().collect::<Vec<_>>();
        if input.len().abs_diff(candidate_characters.len()) > maximum_distance {
            continue;
        }
        let distance = damerau_levenshtein(&input, &candidate_characters);
        if distance <= maximum_distance {
            candidates.push((distance, entry.declaration_order, candidate));
        }
    }
    candidates.sort_by_key(|(distance, declaration_order, _)| (*distance, *declaration_order));
    candidates
        .into_iter()
        .take(10)
        .map(|(_, _, candidate)| candidate)
        .collect()
}

fn damerau_levenshtein(left: &[char], right: &[char]) -> usize {
    let max_distance = left.len().saturating_add(right.len());
    let mut distances = vec![vec![0; right.len() + 2]; left.len() + 2];
    distances[0][0] = max_distance;
    for row in 0..=left.len() {
        distances[row + 1][0] = max_distance;
        distances[row + 1][1] = row;
    }
    for column in 0..=right.len() {
        distances[0][column + 1] = max_distance;
        distances[1][column + 1] = column;
    }

    let mut last_row = std::collections::BTreeMap::new();
    for (left_index, left_character) in left.iter().enumerate() {
        let mut last_matching_column = 0;
        for (right_index, right_character) in right.iter().enumerate() {
            let row = left_index + 1;
            let column = right_index + 1;
            let prior_row = last_row.get(right_character).copied().unwrap_or(0);
            let prior_column = last_matching_column;
            let substitution_cost = usize::from(left_character != right_character);
            if substitution_cost == 0 {
                last_matching_column = column;
            }
            distances[row + 1][column + 1] = [
                distances[row][column] + substitution_cost,
                distances[row + 1][column] + 1,
                distances[row][column + 1] + 1,
                distances[prior_row][prior_column]
                    + (row - prior_row - 1)
                    + 1
                    + (column - prior_column - 1),
            ]
            .into_iter()
            .min()
            .expect("at least one edit distance");
        }
        last_row.insert(*left_character, left_index + 1);
    }
    distances[left.len() + 1][right.len() + 1]
}

fn run_new(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    run_new_with_creator(
        parsed,
        command,
        cancellation,
        stdout,
        stderr,
        create_new_package_with_cancellation,
    )
}

fn run_new_with_creator(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    create: impl FnOnce(
        &Path,
        &str,
        &[String],
        &CancellationToken,
    ) -> Result<NewPackageReceipt, NewPackageError>,
) -> i32 {
    let mut errors = parsed.usage_errors.clone();
    let package_args = match parse_new_command(command) {
        Ok(arguments) => Some(arguments),
        Err(mut command_errors) => {
            errors.append(&mut command_errors);
            None
        }
    };
    if !errors.is_empty() {
        return report_usage_errors(parsed, "new", errors, stdout, stderr);
    }

    let arguments = package_args.expect("successful command parsing returns arguments");
    let result = create(
        &arguments.destination,
        &arguments.identity,
        &arguments.targets,
        cancellation,
    );
    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    match result {
        Ok(receipt) => report_new_success(
            parsed.globals.format,
            parsed.globals.quiet,
            &receipt,
            stdout,
            stderr,
        ),
        Err(NewPackageError::Interrupted) => report_interrupted(parsed, stdout, stderr),
        Err(error) => report_new_error(
            parsed.globals.format,
            parsed.globals.quiet,
            parsed.globals.color,
            error,
            stdout,
            stderr,
        ),
    }
}

fn report_interrupted(
    parsed: &ParsedArgs,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    if parsed.globals.format == OutputFormat::Json {
        let command_name = parsed
            .command
            .as_ref()
            .map_or_else(|| "sai".to_owned(), command_path);
        if let Err(error) = write_json_diagnostics(stdout, &command_name, &[], 0, 130) {
            report_stdout_failure(stderr, error);
        }
    }
    130
}

fn command_help(parsed: &ParsedArgs) -> String {
    let Some(command) = parsed.command.as_ref() else {
        return render_top_level_help();
    };
    let command_name = command.name.to_string_lossy();
    let command_key = if command_name == "help" {
        command_tail(command)
            .iter()
            .map(|(argument, _)| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let mut path = command_name.into_owned();
        if matches!(path.as_str(), "component" | "bindgen") {
            if let Some((subcommand, _)) = command_tail(command).first() {
                path.push(' ');
                path.push_str(&subcommand.to_string_lossy());
            }
        }
        path
    };
    render_command_help(&command_key).unwrap_or_else(render_top_level_help)
}

fn command_tail(command: &crate::args::ParsedCommand) -> Vec<(OsString, usize)> {
    let global_indices = command
        .global_argument_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    command
        .arguments
        .iter()
        .enumerate()
        .filter(|(index, _)| !global_indices.contains(index))
        .map(|(index, argument)| {
            (
                argument.clone(),
                command.argument_indices.get(index).copied().unwrap_or(0),
            )
        })
        .collect()
}

fn command_path(command: &crate::args::ParsedCommand) -> String {
    let mut path = command.name.to_string_lossy().into_owned();
    if matches!(
        command.name.to_str(),
        Some("component" | "bindgen" | "inspect")
    ) {
        if let Some((subcommand, _)) = command_tail(command).first() {
            path.push(' ');
            path.push_str(&subcommand.to_string_lossy());
        }
    }
    path
}

fn subcommand_usage_error(command: &crate::args::ParsedCommand) -> Option<UsageError> {
    let expected = match command.name.to_str()? {
        "component" => &["build"][..],
        "bindgen" => &["wit"][..],
        "inspect" => &[
            "unsafe",
            "mir",
            "effects",
            "capabilities",
            "layout",
            "wit",
            "deps",
        ][..],
        _ => return None,
    };
    match command_tail(command).first() {
        Some((subcommand, _)) if expected.contains(&subcommand.to_string_lossy().as_ref()) => None,
        Some((subcommand, argument_index)) => Some(UsageError {
            kind: UsageErrorKind::UnknownCommand,
            argument_index: argument_index + 1,
            argument: subcommand.clone(),
        }),
        None => Some(UsageError {
            kind: UsageErrorKind::MissingArgument,
            argument_index: 0,
            argument: OsString::new(),
        }),
    }
}

#[derive(Clone, Copy)]
struct CommandOption {
    takes_value: bool,
    repeatable: bool,
}

fn validate_known_command_options(command: &crate::args::ParsedCommand) -> Vec<UsageError> {
    let global_indices = command
        .global_argument_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let command_name = command.name.to_string_lossy();
    let subcommand = command_tail(command)
        .into_iter()
        .find(|(argument, _)| !crate::args::is_option(argument))
        .map(|(argument, _)| argument.to_string_lossy().into_owned());
    let mut errors = Vec::new();
    let mut seen_values = BTreeSet::new();
    let mut seen_options = BTreeSet::new();
    let mut index = 0;

    while index < command.arguments.len() {
        if global_indices.contains(&index) {
            if command_name == "lsp"
                && matches!(
                    command.arguments[index].to_str(),
                    Some("-C" | "--directory")
                )
            {
                errors.push(command_option_error(
                    command,
                    index,
                    UsageErrorKind::UnknownOption,
                    command.arguments[index].clone(),
                ));
            }
            index += 1;
            continue;
        }

        let argument = &command.arguments[index];
        if argument == "--" || !crate::args::is_option(argument) {
            index += 1;
            continue;
        }
        let parsed_option = if argument == "-o" {
            Some(("-o".to_owned(), None))
        } else {
            crate::args::split_os_option(argument)
        };
        let Some((option, inline_value)) = parsed_option else {
            errors.push(command_option_error(
                command,
                index,
                UsageErrorKind::UnknownOption,
                argument.clone(),
            ));
            index += 1;
            continue;
        };
        let Some(spec) = command_option(&command_name, subcommand.as_deref(), option.as_str())
        else {
            errors.push(command_option_error(
                command,
                index,
                UsageErrorKind::UnknownOption,
                argument.clone(),
            ));
            index += 1;
            continue;
        };
        seen_options.insert(option.clone());

        if spec.takes_value {
            if !spec.repeatable && !seen_values.insert(option.clone()) {
                errors.push(command_option_error(
                    command,
                    index,
                    UsageErrorKind::ConflictingOptions,
                    argument.clone(),
                ));
            }
            let (value, value_index, consumed) = if let Some(value) = inline_value {
                (Some(value), index, 1)
            } else if command
                .arguments
                .get(index + 1)
                .is_some_and(|next| !crate::args::is_option(next))
                && !global_indices.contains(&(index + 1))
            {
                (Some(command.arguments[index + 1].clone()), index + 1, 2)
            } else {
                (None, index, 1)
            };
            if let Some(value) = value {
                if !valid_command_option_value(
                    &command_name,
                    subcommand.as_deref(),
                    option.as_str(),
                    &value,
                ) {
                    errors.push(command_option_error(
                        command,
                        value_index,
                        UsageErrorKind::InvalidOptionValue,
                        if value_index == index {
                            argument.clone()
                        } else {
                            value
                        },
                    ));
                }
                index += consumed;
            } else {
                errors.push(command_option_error(
                    command,
                    index,
                    UsageErrorKind::MissingArgument,
                    argument.clone(),
                ));
                index += 1;
            }
        } else {
            if inline_value.is_some() {
                errors.push(command_option_error(
                    command,
                    index,
                    UsageErrorKind::InvalidOptionValue,
                    argument.clone(),
                ));
            }
            index += 1;
        }
    }

    if !matches!(command_name.as_ref(), "run" | "test") {
        errors.extend(
            command
                .passthrough
                .iter()
                .enumerate()
                .map(|(index, argument)| UsageError {
                    kind: UsageErrorKind::UnexpectedArgument,
                    argument_index: command
                        .passthrough_indices
                        .get(index)
                        .map_or(0, |index| index + 1),
                    argument: argument.clone(),
                }),
        );
    }
    let positionals = collect_command_positionals(command, &subcommand);
    errors.extend(validate_command_shape(
        command,
        subcommand.as_deref(),
        &positionals,
        &seen_options,
    ));
    errors
}

fn collect_command_positionals(
    command: &crate::args::ParsedCommand,
    subcommand: &Option<String>,
) -> Vec<(OsString, usize)> {
    let global_indices = command
        .global_argument_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let command_name = command.name.to_string_lossy();
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < command.arguments.len() {
        if global_indices.contains(&index) {
            index += 1;
            continue;
        }
        let argument = &command.arguments[index];
        if argument == "--" {
            break;
        }
        if !crate::args::is_option(argument) {
            positionals.push((argument.clone(), index));
            index += 1;
            continue;
        }
        let option = if argument == "-o" {
            Some(("-o".to_owned(), None))
        } else {
            crate::args::split_os_option(argument)
        };
        let Some((option, inline_value)) = option else {
            index += 1;
            continue;
        };
        let Some(spec) = command_option(&command_name, subcommand.as_deref(), &option) else {
            index += 1;
            continue;
        };
        if spec.takes_value
            && inline_value.is_none()
            && command
                .arguments
                .get(index + 1)
                .is_some_and(|next| !crate::args::is_option(next))
            && !global_indices.contains(&(index + 1))
        {
            index += 2;
            continue;
        }
        index += 1;
    }
    positionals
}

fn validate_command_shape(
    command: &crate::args::ParsedCommand,
    subcommand: Option<&str>,
    positionals: &[(OsString, usize)],
    seen_options: &BTreeSet<String>,
) -> Vec<UsageError> {
    let command_name = command.name.to_string_lossy();
    let nested_command = matches!(command_name.as_ref(), "component" | "bindgen" | "inspect");
    let positionals = if nested_command && !positionals.is_empty() {
        &positionals[1..]
    } else {
        positionals
    };
    let mut errors = Vec::new();
    let missing = || UsageError {
        kind: UsageErrorKind::MissingArgument,
        argument_index: 0,
        argument: OsString::new(),
    };
    let unexpected = |positional: &(OsString, usize)| UsageError {
        kind: UsageErrorKind::UnexpectedArgument,
        argument_index: command
            .argument_indices
            .get(positional.1)
            .map_or(0, |index| index + 1),
        argument: positional.0.clone(),
    };
    let invalid = |positional: &(OsString, usize)| UsageError {
        kind: UsageErrorKind::InvalidOptionValue,
        argument_index: command
            .argument_indices
            .get(positional.1)
            .map_or(0, |index| index + 1),
        argument: positional.0.clone(),
    };

    match command_name.as_ref() {
        "new" => {}
        "check" | "build" | "lock" | "fetch" | "size" | "lsp" => {
            errors.extend(positionals.iter().map(unexpected));
        }
        "component" => {
            errors.extend(positionals.iter().map(unexpected));
        }
        "bindgen" => {
            errors.extend(positionals.iter().map(unexpected));
            if seen_options.contains("--show") && seen_options.contains("-o") {
                errors.push(UsageError {
                    kind: UsageErrorKind::ConflictingOptions,
                    argument_index: 0,
                    argument: OsString::new(),
                });
            }
        }
        "run" => errors.extend(positionals.iter().map(unexpected)),
        "test" => {}
        "fmt" => {
            let has_stdin = positionals.iter().any(|(argument, _)| argument == "-");
            if seen_options.contains("--stdin-kind") && !has_stdin {
                if let Some(index) = command_option_argument_index(command, "--stdin-kind") {
                    errors.push(command_option_error(
                        command,
                        index,
                        UsageErrorKind::ConflictingOptions,
                        command.arguments[index].clone(),
                    ));
                }
            }
            if has_stdin && positionals.len() > 1 {
                let (argument, index) = &positionals[1];
                errors.push(command_option_error(
                    command,
                    *index,
                    UsageErrorKind::ConflictingOptions,
                    argument.clone(),
                ));
            }
        }
        "update" => {
            let mut package_names = BTreeSet::new();
            for package in positionals {
                let Some(package_selector) = package.0.to_str() else {
                    errors.push(invalid(package));
                    continue;
                };
                let (identity, exact_version) = package_selector
                    .split_once('@')
                    .map_or((package_selector, None), |(identity, version)| {
                        (identity, Some(version))
                    });
                if !sai_driver::package_validation::is_valid_identity(identity)
                    || exact_version.is_some_and(|version| {
                        !sai_driver::package_validation::is_valid_exact_version(version)
                    })
                {
                    errors.push(invalid(package));
                    continue;
                }
                if !package_names.insert(identity.to_owned()) {
                    errors.push(UsageError {
                        kind: UsageErrorKind::ConflictingOptions,
                        argument_index: command
                            .argument_indices
                            .get(package.1)
                            .map_or(0, |index| index + 1),
                        argument: package.0.clone(),
                    });
                }
            }
        }
        "add" => {
            let has_path = seen_options.contains("--path");
            let has_dir = seen_options.contains("--dir");
            let has_version = seen_options.contains("--version");
            if has_path && (has_version || has_dir) {
                if let Some(index) = command_option_argument_index(command, "--path") {
                    errors.push(command_option_error(
                        command,
                        index,
                        UsageErrorKind::ConflictingOptions,
                        command.arguments[index].clone(),
                    ));
                }
            }
            if positionals.is_empty() && !has_path {
                errors.push(missing());
            } else if positionals.len() > 1 || has_path && !positionals.is_empty() {
                if let Some((argument, index)) = positionals.get(1).or(positionals.first()) {
                    errors.push(UsageError {
                        kind: UsageErrorKind::ConflictingOptions,
                        argument_index: command
                            .argument_indices
                            .get(*index)
                            .map_or(0, |index| index + 1),
                        argument: argument.clone(),
                    });
                } else if let Some(index) = command_option_argument_index(command, "--path") {
                    errors.push(command_option_error(
                        command,
                        index,
                        UsageErrorKind::ConflictingOptions,
                        command.arguments[index].clone(),
                    ));
                }
            } else if let Some((source, index)) = positionals.first() {
                if let Some(source_text) = source.to_str() {
                    let is_url = is_git_url_or_path(source_text);
                    if is_url {
                        let kind = match sai_driver::package_validation::validate_git_url(
                            source_text,
                        ) {
                            Ok(()) => None,
                            Err(
                                sai_driver::package_validation::GitUrlValidationError::Insecure,
                            ) => Some(UsageErrorKind::InsecureGitUrl),
                            Err(
                                sai_driver::package_validation::GitUrlValidationError::Unsupported,
                            ) => Some(UsageErrorKind::UnsupportedGitUrl),
                            Err(
                                sai_driver::package_validation::GitUrlValidationError::Credentials,
                            ) => Some(UsageErrorKind::CredentialsInUrl),
                        };
                        if let Some(kind) = kind {
                            errors.push(UsageError {
                                kind,
                                argument_index: command
                                    .argument_indices
                                    .get(*index)
                                    .map_or(0, |index| index + 1),
                                argument: redact_git_url_credentials(source_text).into(),
                            });
                        }
                    } else if !sai_driver::package_validation::is_standard_package_identity(
                        source_text,
                    ) {
                        errors.push(invalid(&(source.clone(), *index)));
                    } else if has_dir {
                        if let Some(index) = command_option_argument_index(command, "--dir") {
                            errors.push(command_option_error(
                                command,
                                index,
                                UsageErrorKind::ConflictingOptions,
                                command.arguments[index].clone(),
                            ));
                        }
                    }
                } else {
                    errors.push(invalid(&(source.clone(), *index)));
                }
            }
        }
        "remove" => match positionals {
            [] => errors.push(missing()),
            [identity] => {
                let valid = identity.0.to_str().is_some_and(|identity| {
                    sai_driver::new_package::validate_new_package_identity(identity).is_ok()
                });
                if !valid {
                    errors.push(invalid(identity));
                }
            }
            [_, extras @ ..] => errors.extend(extras.iter().map(unexpected)),
        },
        "inspect" => match subcommand {
            Some("unsafe" | "capabilities" | "wit" | "deps") => {
                errors.extend(positionals.iter().map(unexpected));
            }
            Some("mir") => {
                if positionals.is_empty() {
                    errors.push(missing());
                } else if positionals.len() > 1 {
                    errors.extend(positionals[1..].iter().map(unexpected));
                }
            }
            Some("effects") => {
                if positionals.len() > 1 {
                    errors.extend(positionals[1..].iter().map(unexpected));
                }
                if seen_options.contains("--why") && positionals.is_empty() {
                    errors.push(missing());
                }
            }
            Some("layout") => {
                if seen_options.contains("--all") && !positionals.is_empty() {
                    errors.extend(positionals.iter().map(unexpected));
                } else if !seen_options.contains("--all") && positionals.is_empty() {
                    errors.push(missing());
                }
            }
            _ => {}
        },
        "graph" => {
            if positionals.is_empty() {
                errors.push(missing());
            } else if !positionals[0].0.to_str().is_some_and(|graph| {
                matches!(
                    graph,
                    "symbols" | "references" | "calls" | "effects" | "capabilities" | "tests"
                )
            }) {
                errors.push(invalid(&positionals[0]));
            }
            if seen_options.contains("--in") && seen_options.contains("--both") {
                errors.push(UsageError {
                    kind: UsageErrorKind::ConflictingOptions,
                    argument_index: 0,
                    argument: OsString::new(),
                });
            }
        }
        "diff" => {
            if positionals.len() > 2 {
                errors.extend(positionals[2..].iter().map(unexpected));
            }
        }
        "edit" => {
            if seen_options.contains("--request") {
                if !positionals.is_empty() {
                    errors.extend(positionals.iter().map(unexpected));
                }
            } else if positionals.len() < 2 {
                errors.push(missing());
            }
        }
        "explain" => {
            if seen_options.contains("--list") {
                errors.extend(positionals.iter().map(unexpected));
            } else if positionals.is_empty() {
                errors.push(missing());
            } else if positionals.len() > 1 {
                errors.extend(positionals[1..].iter().map(unexpected));
            }
        }
        _ => {}
    }
    if command_name == "test"
        && seen_options.contains("--workspace")
        && seen_options.contains("--package")
    {
        errors.push(UsageError {
            kind: UsageErrorKind::ConflictingOptions,
            argument_index: 0,
            argument: OsString::new(),
        });
    }
    errors
}

fn valid_command_option_value(
    command: &str,
    subcommand: Option<&str>,
    option: &str,
    value: &OsString,
) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    match (command, option) {
        (_, "--target") => matches!(
            value,
            "wasm32-core"
                | "wasm64-core"
                | "wasm32-wasi"
                | "wasm64-wasi"
                | "x86_64-linux"
                | "aarch64-linux"
                | "aarch64-macos"
                | "x86_64-windows"
        ),
        (_, "--profile") => matches!(value, "dev" | "release" | "tiny"),
        (_, "--jobs") => value.parse::<usize>().is_ok_and(|jobs| jobs > 0),
        ("build" | "component" | "run" | "test" | "size", "--wasm-feature") => {
            matches!(value, "tail-call" | "simd128")
        }
        ("test", "--seed") => value.parse::<u64>().is_ok(),
        ("add", "--version") => sai_driver::package_validation::is_valid_version_requirement(value),
        ("inspect", "--phase") if subcommand == Some("mir") => matches!(
            value,
            "built" | "drop-elaborated" | "analyzed" | "optimized" | "mono" | "final"
        ),
        ("fmt", "--stdin-kind") => matches!(value, "source" | "package"),
        ("graph", "--depth") => value
            .parse::<u8>()
            .is_ok_and(|depth| (1..=64).contains(&depth)),
        _ => true,
    }
}

fn command_option_error(
    command: &crate::args::ParsedCommand,
    index: usize,
    kind: UsageErrorKind,
    argument: OsString,
) -> UsageError {
    UsageError {
        kind,
        argument_index: command
            .argument_indices
            .get(index)
            .map_or(0, |index| index + 1),
        argument,
    }
}

fn command_option_argument_index(
    command: &crate::args::ParsedCommand,
    option: &str,
) -> Option<usize> {
    command.arguments.iter().position(|argument| {
        crate::args::split_os_option(argument).is_some_and(|(name, _)| name == option)
    })
}

fn is_git_url_or_path(value: &str) -> bool {
    if value.contains("://")
        || value.starts_with("file:")
        || value.starts_with('.')
        || value.starts_with('/')
        || value.contains('/')
        || value.contains('\\')
    {
        return true;
    }
    if value
        .split_once(':')
        .is_some_and(|(authority, path)| authority.contains('@') && !path.is_empty())
    {
        return true;
    }
    value.split_once(':').is_some_and(|(scheme, _)| {
        matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "git" | "ssh" | "ftp" | "git+ssh" | "file"
        )
    })
}

fn redact_git_url_credentials(value: &str) -> String {
    let safe_value = value.split(['?', '#']).next().unwrap_or(value);
    let Some((scheme, remainder)) = safe_value.split_once("://") else {
        return safe_value.rsplit_once('@').map_or_else(
            || safe_value.to_owned(),
            |(_, host_and_path)| format!("<redacted>@{host_and_path}"),
        );
    };
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let Some((_, host)) = authority.rsplit_once('@') else {
        return safe_value.to_owned();
    };
    format!(
        "{scheme}://<redacted>@{host}{}",
        &remainder[authority_end..]
    )
}

fn is_scp_like_git_url(value: &str) -> bool {
    !value.contains("://")
        && value
            .split_once(':')
            .is_some_and(|(authority, path)| authority.contains('@') && !path.starts_with('/'))
}

fn command_option(command: &str, subcommand: Option<&str>, option: &str) -> Option<CommandOption> {
    let value = CommandOption {
        takes_value: true,
        repeatable: false,
    };
    let repeated_value = CommandOption {
        takes_value: true,
        repeatable: true,
    };
    let flag = CommandOption {
        takes_value: false,
        repeatable: true,
    };
    let is_building = matches!(
        command,
        "check"
            | "build"
            | "component"
            | "run"
            | "test"
            | "size"
            | "inspect"
            | "graph"
            | "diff"
            | "edit"
            | "bindgen"
    );
    let is_resolving =
        is_building || matches!(command, "add" | "remove" | "update" | "lock" | "fetch");

    if command == "lsp"
        && matches!(
            option,
            "--target" | "--profile" | "--locked" | "--offline" | "--frozen"
        )
    {
        return Some(if matches!(option, "--locked" | "--offline" | "--frozen") {
            flag
        } else {
            value
        });
    }

    if is_building {
        match option {
            "--target" | "--profile" | "--jobs" => return Some(value),
            "--locked" | "--offline" | "--frozen" => return Some(flag),
            "--approve" if is_resolving && command != "lsp" => return Some(flag),
            "--allow" | "--warn" | "--deny" => return Some(repeated_value),
            "--deny-warnings" => return Some(flag),
            _ => {}
        }
    }

    if (matches!(command, "build" | "run" | "test" | "size")
        || command == "component" && subcommand == Some("build"))
        && option == "--wasm-feature"
    {
        return Some(repeated_value);
    }

    match (command, option) {
        ("check", "--tests" | "--fix")
        | ("run", "--allow-network")
        | ("test", "--workspace" | "--exact" | "--list" | "--no-run")
        | ("fmt", "--check")
        | ("add", "--test")
        | ("update", "--dry-run")
        | ("graph", "--in" | "--both" | "--deps")
        | ("diff", "--size" | "--public")
        | ("edit", "--dry-run" | "--allow-breaking" | "--size")
        | ("explain", "--list")
        | ("lsp", "--stdio") => Some(flag),
        ("run", "--env" | "--dir")
        | ("test", "--package")
        | ("add", "--only" | "--grant")
        | ("graph", "--edge") => Some(repeated_value),
        ("inspect", "--package") if subcommand == Some("unsafe") => Some(repeated_value),
        ("test", "--seed")
        | ("fmt", "--stdin-kind")
        | ("add", "--path" | "--dir" | "--version")
        | ("size", "--why")
        | ("graph", "--depth")
        | ("edit", "--request")
        | ("bindgen", "-o" | "--show") => Some(value),
        ("inspect", "--phase") if subcommand == Some("mir") => Some(value),
        ("inspect", "--why") if subcommand == Some("effects") => Some(value),
        ("inspect", "--tests") if subcommand == Some("deps") => Some(flag),
        ("inspect", "--all") if subcommand == Some("layout") => Some(flag),
        _ => None,
    }
}

fn unimplemented_command(command: &crate::args::ParsedCommand) -> Option<(u8, String)> {
    let command_name = command.name.to_str()?;
    let first_tail = command_tail(command)
        .first()
        .map(|(argument, _)| argument.to_string_lossy().into_owned());
    match command_name {
        "check" => Some((0, "sai check".to_owned())),
        "build" => Some((1, "sai build".to_owned())),
        "run" => Some((4, "sai run".to_owned())),
        "test" => Some((3, "sai test".to_owned())),
        "fmt" => Some((0, "sai fmt".to_owned())),
        "add" | "remove" | "update" | "lock" | "fetch" => Some((3, format!("sai {command_name}"))),
        "size" => Some((2, "sai size".to_owned())),
        "component" if first_tail.as_deref() == Some("build") => {
            Some((4, "sai component build".to_owned()))
        }
        "bindgen" if first_tail.as_deref() == Some("wit") => {
            Some((4, "sai bindgen wit".to_owned()))
        }
        "inspect" => {
            let subcommand = first_tail?;
            let stage = match subcommand.as_str() {
                "unsafe" => 2,
                "mir" | "layout" => 1,
                "effects" | "capabilities" | "deps" => 3,
                "wit" => 4,
                _ => return None,
            };
            Some((stage, format!("sai inspect {subcommand}")))
        }
        "graph" | "diff" | "edit" => Some((6, "semantic-protocol".to_owned())),
        "lsp" => Some((0, "sai lsp".to_owned())),
        _ => None,
    }
}

fn report_feature_not_implemented(
    options: CliOutputOptions,
    command_name: &str,
    stage: u8,
    feature: &str,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let diagnostic = CliDiagnostic {
        code: "FEATURE_NOT_IMPLEMENTED",
        message: format!("stage {stage} feature `{feature}` is not implemented"),
        argument_index: None,
        argument: String::new(),
        help: None,
        data: Some(format!(
            "{{\"stage\":{stage},\"feature\":\"{}\"}}",
            escape_json(feature)
        )),
    };
    match options.format {
        OutputFormat::Human => {
            if let Err(error) = write_human_diagnostics(
                stderr,
                command_name,
                std::slice::from_ref(&diagnostic),
                !options.quiet,
                color_enabled(options.color, io::stderr().is_terminal()),
                1,
                options.max_errors,
            ) {
                report_stderr_failure(error)
            } else {
                1
            }
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_diagnostics(stdout, command_name, &[diagnostic], 1, 1) {
                report_stdout_failure(stderr, error);
                4
            } else {
                1
            }
        }
    }
}

fn report_new_success(
    format: OutputFormat,
    quiet: bool,
    receipt: &NewPackageReceipt,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    match format {
        OutputFormat::Human => {
            if !quiet {
                for file in &receipt.files {
                    let destination_path = Path::new(&receipt.directory).join(&file.path);
                    if let Err(error) = writeln!(
                        stderr,
                        "wrote {} ({} bytes)",
                        human_file_path(&destination_path.to_string_lossy()),
                        file.bytes
                    ) {
                        return report_stderr_failure(error);
                    }
                }
            }
            0
        }
        OutputFormat::Json => {
            if let Err(error) = write_new_json_success(stdout, receipt) {
                report_stdout_failure(stderr, error);
                4
            } else {
                0
            }
        }
    }
}

fn report_new_error(
    format: OutputFormat,
    quiet: bool,
    color_choice: ColorChoice,
    error: NewPackageError,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let (diagnostic, exit_code) = match error {
        NewPackageError::DestinationNotEmpty(_) => (
            CliDiagnostic {
                code: "DESTINATION_NOT_EMPTY",
                message: "destination is not an empty directory".to_owned(),
                argument_index: None,
                argument: String::new(),
                help: None,
                data: None,
            },
            1,
        ),
        NewPackageError::Io { source, .. } => (
            CliDiagnostic {
                code: "OUTPUT_WRITE_FAILED",
                message: "could not write the new package".to_owned(),
                argument_index: None,
                argument: String::new(),
                help: Some(source.to_string()),
                data: None,
            },
            4,
        ),
        other => (
            CliDiagnostic {
                code: "INVALID_OPTION_VALUE",
                message: other.to_string(),
                argument_index: None,
                argument: String::new(),
                help: None,
                data: None,
            },
            2,
        ),
    };

    match format {
        OutputFormat::Human => {
            if let Err(error) = write_human_diagnostics(
                stderr,
                "new",
                std::slice::from_ref(&diagnostic),
                !quiet,
                color_enabled(color_choice, io::stderr().is_terminal()),
                1,
                50,
            ) {
                return report_stderr_failure(error);
            }
            exit_code
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_diagnostics(stdout, "new", &[diagnostic], 1, exit_code) {
                report_stdout_failure(stderr, error);
                4
            } else {
                exit_code
            }
        }
    }
}

fn report_usage_errors(
    parsed: &ParsedArgs,
    command_name: &str,
    mut errors: Vec<UsageError>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    errors.sort_by_key(|error| error.argument_index);
    let total_errors = errors.len();
    let max_errors = parsed.globals.max_errors;
    if max_errors != 0 {
        errors.truncate(max_errors);
    }
    let diagnostics = errors
        .iter()
        .map(|error| usage_diagnostic(error, command_name, parsed.command.as_ref()))
        .collect::<Vec<_>>();
    match parsed.globals.format {
        OutputFormat::Human => {
            let color = color_enabled(parsed.globals.color, io::stderr().is_terminal());
            if let Err(error) = write_human_diagnostics(
                stderr,
                command_name,
                &diagnostics,
                !parsed.globals.quiet,
                color,
                total_errors,
                max_errors,
            ) {
                report_stderr_failure(error)
            } else {
                2
            }
        }
        OutputFormat::Json => {
            if let Err(error) =
                write_json_diagnostics(stdout, command_name, &diagnostics, total_errors, 2)
            {
                report_stdout_failure(stderr, error);
                4
            } else {
                2
            }
        }
    }
}

fn usage_diagnostic(
    error: &UsageError,
    command_name: &str,
    command_args: Option<&crate::args::ParsedCommand>,
) -> CliDiagnostic {
    let raw_argument = error.argument.to_string_lossy();
    let argument = if command_name == "add" && is_git_url_or_path(&raw_argument) {
        redact_git_url_credentials(&raw_argument)
    } else {
        raw_argument.into_owned()
    };
    let command = if command_name == "sai" {
        "sai".to_owned()
    } else {
        format!("sai {command_name}")
    };
    let (code, message, help) = match error.kind {
        UsageErrorKind::UnknownCommand => (
            "UNKNOWN_COMMAND",
            format!("unknown command `{argument}`"),
            Some("run `sai --help` to list available commands".to_owned()),
        ),
        UsageErrorKind::UnknownOption => (
            "UNKNOWN_OPTION",
            format!("unknown option `{argument}` for `{command}`"),
            None,
        ),
        UsageErrorKind::MissingArgument => (
            "MISSING_ARGUMENT",
            format!("missing required argument for `{command}`"),
            None,
        ),
        UsageErrorKind::InvalidOptionValue => {
            let option = command_args.and_then(|command| related_new_option(error, command));
            let display_value = argument
                .split_once('=')
                .filter(|_| argument.starts_with("--"))
                .map_or(argument.as_str(), |(_, value)| value);
            let help = match (command_name, option.as_deref()) {
                ("new", Some("--name")) => Some(
                    "package identities use `namespace:name`; each segment must be 1–64 bytes, start with a lowercase ASCII letter, and contain only lowercase ASCII letters, digits, and single interior hyphens. The `wasi` namespace and `sai:core`/`sai:alloc` identities are reserved".to_owned(),
                ),
                ("new", Some("--target")) => Some(
                    "valid targets: wasm32-core, wasm64-core, wasm32-wasi, wasm64-wasi, x86_64-linux, aarch64-linux, aarch64-macos, x86_64-windows".to_owned(),
                ),
                ("add", None) => Some(
                    "only standard packages shipped with the toolchain can be named by identity; name other dependencies by their repository URL because SAI has no registry".to_owned(),
                ),
                _ => None,
            };
            (
                "INVALID_OPTION_VALUE",
                option.map_or_else(
                    || format!("invalid option value `{display_value}` for `{command}`"),
                    |option| format!("invalid value `{display_value}` for `{option}`"),
                ),
                help,
            )
        }
        UsageErrorKind::InsecureGitUrl => (
            "INSECURE_GIT_URL",
            format!("Git URL `{argument}` uses an insecure transport"),
            Some("use an `https://` or `ssh://` Git URL".to_owned()),
        ),
        UsageErrorKind::UnsupportedGitUrl => (
            "UNSUPPORTED_GIT_URL",
            format!("unsupported Git URL `{argument}`"),
            Some(if is_scp_like_git_url(&argument) {
                "rewrite the scp-like form `user@host:path` as `ssh://user@host/path`, preserving the SSH user name".to_owned()
            } else {
                "use an `https://` or `ssh://` Git URL, or pass a local package with `--path`"
                    .to_owned()
            }),
        ),
        UsageErrorKind::CredentialsInUrl => (
            "CREDENTIALS_IN_URL",
            format!("Git URL `{argument}` contains credentials"),
            Some(
                "remove user information from the URL and use an SSH agent or credential helper"
                    .to_owned(),
            ),
        ),
        UsageErrorKind::UnexpectedArgument => (
            "UNEXPECTED_ARGUMENT",
            format!("unexpected argument `{argument}` for `{command}`"),
            None,
        ),
        UsageErrorKind::ConflictingOptions => (
            "CONFLICTING_OPTIONS",
            format!("conflicting option or repeated value `{argument}` for `{command}`"),
            None,
        ),
    };
    CliDiagnostic {
        code,
        message,
        argument_index: Some(error.argument_index),
        argument,
        help,
        data: None,
    }
}

fn related_new_option(error: &UsageError, command: &crate::args::ParsedCommand) -> Option<String> {
    let original_index = error.argument_index.checked_sub(1)?;
    let current_index = command
        .argument_indices
        .iter()
        .position(|index| *index == original_index)?;
    let global_indices = command
        .global_argument_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if global_indices.contains(&current_index) {
        return None;
    }
    for index in (0..=current_index).rev() {
        if global_indices.contains(&index) {
            continue;
        }
        let argument = command.arguments[index].to_string_lossy();
        if argument == "--name" || argument.starts_with("--name=") {
            return Some("--name".to_owned());
        }
        if argument == "--target" || argument.starts_with("--target=") {
            return Some("--target".to_owned());
        }
        if index != current_index && argument.starts_with('-') {
            break;
        }
    }
    None
}

fn write_human_diagnostics(
    writer: &mut impl Write,
    command_name: &str,
    diagnostics: &[CliDiagnostic],
    show_summary: bool,
    color: bool,
    total_error_count: usize,
    max_errors: usize,
) -> io::Result<()> {
    for (index, diagnostic) in diagnostics.iter().enumerate() {
        if index > 0 {
            writeln!(writer)?;
        }
        if color {
            write!(writer, "\x1b[1;31merror[{}]\x1b[0m: ", diagnostic.code)?;
        } else {
            write!(writer, "error[{}]: ", diagnostic.code)?;
        }
        writeln!(writer, "{}", diagnostic.message)?;
        if let Some(help) = &diagnostic.help {
            writeln!(writer, "  = help: {help}")?;
        }
    }
    if show_summary && total_error_count > 0 {
        let error_count = total_error_count;
        let noun = if error_count == 1 { "error" } else { "errors" };
        let hidden = error_count.saturating_sub(diagnostics.len());
        let more = if hidden > 0 && max_errors != 0 {
            format!("; {hidden} more errors not shown (--max-errors {max_errors})")
        } else {
            String::new()
        };
        if color {
            writeln!(
                writer,
                "\x1b[1;31m{command_name} failed: {error_count} {noun}{more}\x1b[0m"
            )?;
        } else {
            writeln!(writer, "{command_name} failed: {error_count} {noun}{more}")?;
        }
    }
    Ok(())
}

fn write_json_success_header(writer: &mut impl Write, command_name: &str) -> io::Result<()> {
    writeln!(
        writer,
        "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},\"command\":\"{}\",\"root\":null}}",
        env!("CARGO_PKG_VERSION"),
        escape_json(command_name)
    )
}

fn write_new_json_success(writer: &mut impl Write, receipt: &NewPackageReceipt) -> io::Result<()> {
    write_json_success_header(writer, "new")?;
    write!(
        writer,
        "{{\"type\":\"result\",\"kind\":\"new\",\"data\":{{\"package\":\"{}\",\"directory\":\"{}\",\"files\":[",
        escape_json(&receipt.package),
        escape_json(&receipt.directory)
    )?;
    for (index, file) in receipt.files.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        write!(
            writer,
            "{{\"path\":\"{}\",\"bytes\":{},\"sha256\":\"{}\"}}",
            escape_json(&file.path),
            file.bytes,
            file.sha256
        )?;
    }
    writeln!(writer, "]}}}}")?;
    writeln!(
        writer,
        "{{\"type\":\"summary\",\"errors\":0,\"warnings\":0,\"infos\":0,\"shown_errors\":0,\"suppressed\":{{\"cascade\":0,\"duplicate\":0,\"dependency\":0}},\"exit_code\":0}}"
    )
}

fn write_json_diagnostics(
    writer: &mut impl Write,
    command_name: &str,
    diagnostics: &[CliDiagnostic],
    total_error_count: usize,
    exit_code: i32,
) -> io::Result<()> {
    write_json_success_header(writer, command_name)?;
    for diagnostic in diagnostics {
        let data = if let Some(argument_index) = diagnostic.argument_index {
            format!(
                "{{\"argument_index\":{argument_index},\"argument\":\"{}\"}}",
                escape_json(&diagnostic.argument)
            )
        } else if let Some(data) = &diagnostic.data {
            data.clone()
        } else {
            "{}".to_owned()
        };
        let children = diagnostic.help.as_ref().map_or_else(
            || "[]".to_owned(),
            |help| {
                format!(
                    "[{{\"kind\":\"help\",\"message\":\"{}\",\"location\":null}}]",
                    escape_json(help)
                )
            },
        );
        writeln!(
            writer,
            "{{\"type\":\"diagnostic\",\"code\":\"{}\",\"severity\":\"error\",\"level\":null,\"message\":\"{}\",\"primary\":null,\"primary_label\":\"\",\"labels\":[],\"children\":{},\"fixes\":[],\"related\":[],\"data\":{},\"node\":null,\"symbol\":null,\"package\":null}}",
            diagnostic.code,
            escape_json(&diagnostic.message),
            children,
            data
        )?;
    }
    writeln!(
        writer,
        "{{\"type\":\"summary\",\"errors\":{},\"warnings\":0,\"infos\":0,\"shown_errors\":{},\"suppressed\":{{\"cascade\":0,\"duplicate\":0,\"dependency\":0}},\"exit_code\":{exit_code}}}",
        total_error_count,
        diagnostics.len()
    )
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character <= '\u{1f}' => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing to a String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn human_file_path(file_path: &str) -> String {
    let path = Path::new(file_path);
    if path.is_relative() {
        return format_relative_path(path);
    }

    std::env::current_dir()
        .ok()
        .and_then(|current_dir| relative_path(&current_dir, path))
        .and_then(|relative| relative.to_str().map(str::to_owned))
        .map(|path| path.replace('\\', "/"))
        .unwrap_or_else(|| file_path.to_owned())
}

fn format_relative_path(path: &Path) -> String {
    let mut components = Vec::<OsString>::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if components
                    .last()
                    .is_some_and(|last| last != ".." && last != "/")
                {
                    components.pop();
                } else {
                    components.push(OsString::from(".."));
                }
            }
            Component::Normal(component) => components.push(component.to_os_string()),
            Component::Prefix(_) | Component::RootDir => {
                return path.to_string_lossy().replace('\\', "/");
            }
        }
    }
    components
        .iter()
        .map(|component| component.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let (base_prefix, base_components) = absolute_components(base)?;
    let (target_prefix, target_components) = absolute_components(target)?;
    if base_prefix != target_prefix {
        return None;
    }

    let common_length = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in &base_components[common_length..] {
        relative.push("..");
    }
    for component in &target_components[common_length..] {
        relative.push(component);
    }
    Some(relative)
}

fn absolute_components(path: &Path) -> Option<(Option<OsString>, Vec<OsString>)> {
    if !path.is_absolute() {
        return None;
    }

    let mut components = path.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix)) => Some(prefix.as_os_str().to_os_string()),
        Some(Component::RootDir) => None,
        _ => return None,
    };
    if prefix.is_some() && !matches!(components.next(), Some(Component::RootDir)) {
        return None;
    }

    let mut normal_components = Vec::<OsString>::new();
    for component in components {
        match component {
            Component::Normal(component) => normal_components.push(component.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                normal_components.pop();
            }
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some((prefix, normal_components))
}

fn write_stdout_text(stdout: &mut impl Write, value: &str, stderr: &mut impl Write) -> i32 {
    match stdout.write_all(value.as_bytes()) {
        Ok(()) => 0,
        Err(error) => {
            report_stdout_failure(stderr, error);
            4
        }
    }
}

fn report_stdout_failure(stderr: &mut impl Write, error: io::Error) {
    let _ = writeln!(stderr, "sai: cannot write to standard output: {error}");
}

fn report_stderr_failure(_error: io::Error) -> i32 {
    4
}

fn color_enabled(choice: ColorChoice, stream_is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            let force = std::env::var_os("CLICOLOR_FORCE")
                .is_some_and(|value| !value.is_empty() && value != "0");
            if force {
                true
            } else if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
                false
            } else {
                stream_is_terminal && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        EXPLAIN_CODE_SUMMARIES, human_file_path, relative_path, run, run_new_with_creator,
        run_with_cancellation,
    };
    use sai_driver::CancellationToken;

    struct BrokenPipeWriter;

    impl std::io::Write for BrokenPipeWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sai-cli-test-{}-{unique}", std::process::id()));
            fs::create_dir(&path).expect("create temporary test directory");
            Self(path)
        }

        fn child(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn invoke(arguments: &[&str]) -> (i32, String, String) {
        let arguments = arguments
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run(&arguments, &mut stdout, &mut stderr);
        (
            exit_code,
            String::from_utf8(stdout).expect("CLI stdout is UTF-8"),
            String::from_utf8(stderr).expect("CLI stderr is UTF-8"),
        )
    }

    #[test]
    fn new_creates_the_documented_package_and_prints_relative_paths() {
        let temporary = TestDirectory::new();
        let destination = temporary.child("image");
        let destination = destination.to_str().expect("temporary path is UTF-8");
        let (exit_code, stdout, stderr) = invoke(&[
            "new",
            destination,
            "--name",
            "acme:image",
            "--target",
            "wasm32-wasi",
        ]);

        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        let lines = stderr.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("wrote "));
        assert!(lines[0].contains("/image/sai.package ("));
        assert!(lines[1].contains("/image/.gitignore ("));
        assert!(lines[2].contains("/image/src/lib.sai ("));
        assert!(lines.iter().all(|line| !line.starts_with("wrote /")));
        assert_eq!(
            fs::read_to_string(temporary.child("image/sai.package"))
                .expect("read generated manifest"),
            concat!(
                "package acme:image @ 0.1.0\n",
                "compiler ^0.1\n",
                "source \"src\"\n\n",
                "targets { wasm32-wasi }\n"
            )
        );
        assert_eq!(
            fs::read_to_string(temporary.child("image/.gitignore"))
                .expect("read generated gitignore"),
            "/out/\n/.sai/\n"
        );
        assert!(temporary.child("image/src/lib.sai").is_file());
        assert_eq!(
            fs::read_dir(temporary.child("image"))
                .expect("read generated package")
                .count(),
            3
        );
    }

    #[test]
    fn new_quiet_suppresses_progress_without_suppressing_creation() {
        let temporary = TestDirectory::new();
        let destination = temporary.child("quiet-image");
        let destination = destination.to_str().expect("temporary path is UTF-8");
        let (exit_code, stdout, stderr) =
            invoke(&["new", "--quiet", destination, "--name=acme:quiet-image"]);

        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert!(temporary.child("quiet-image/sai.package").is_file());
    }

    #[test]
    fn new_json_writes_header_result_and_summary_records() {
        let temporary = TestDirectory::new();
        let destination = temporary.child("json-image");
        let destination = destination.to_str().expect("temporary path is UTF-8");
        let (exit_code, stdout, stderr) = invoke(&[
            "new",
            "--format=json",
            destination,
            "--name",
            "acme:json-image",
        ]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let records = stdout.lines().collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        assert!(records[0].contains("\"type\":\"header\""));
        assert!(records[1].contains("\"kind\":\"new\""));
        assert!(records[1].contains("\"package\":\"acme:json-image@0.1.0\""));
        assert!(records[1].contains("\"path\":\"sai.package\""));
        assert!(records[1].contains("\"path\":\".gitignore\""));
        assert!(records[1].contains("\"path\":\"src/lib.sai\""));
        assert!(!records[1].contains("\"path\":\"/"));
        assert!(records[1].contains(&format!(
            "\"directory\":\"{}\"",
            destination.replace('\\', "/")
        )));
        assert!(records[1].contains("\"sha256\":\""));
        assert!(records[2].contains("\"type\":\"summary\""));
        assert!(records[2].contains("\"exit_code\":0"));
    }

    #[test]
    fn new_json_reports_nonempty_destination_and_preserves_existing_files() {
        let temporary = TestDirectory::new();
        let destination = temporary.child("occupied");
        fs::create_dir(&destination).expect("create existing destination");
        fs::write(destination.join("keep.txt"), "keep").expect("create sentinel");
        let destination = destination.to_str().expect("temporary path is UTF-8");
        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "new",
            destination,
            "--name",
            "acme:occupied",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"DESTINATION_NOT_EMPTY\""));
        assert!(stdout.contains("\"exit_code\":1"));
        assert_eq!(
            fs::read_to_string(temporary.child("occupied/keep.txt"))
                .expect("read preserved sentinel"),
            "keep"
        );
    }

    #[test]
    fn usage_diagnostics_keep_the_original_argument_index_after_globals() {
        let temporary = TestDirectory::new();
        let destination = temporary.child("unused").to_string_lossy().into_owned();
        let (exit_code, stdout, stderr) = invoke(&[
            "new",
            "--format=json",
            &destination,
            "--name",
            "acme:unused",
            "extra",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNEXPECTED_ARGUMENT\""));
        assert!(stdout.contains("\"argument_index\":6"));
        assert!(!temporary.child("unused").exists());
    }

    #[test]
    fn unknown_command_is_a_structured_usage_diagnostic() {
        let (exit_code, stdout, stderr) = invoke(&["not-a-command", "--format=json"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"command\":\"not-a-command\""));
        assert!(stdout.contains("\"code\":\"UNKNOWN_COMMAND\""));
        assert!(stdout.contains("\"argument_index\":1"));
    }

    #[test]
    fn known_unimplemented_command_uses_the_documented_json_diagnostic() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--format=json"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(stdout.contains("\"stage\":0,\"feature\":\"sai check\""));
        assert!(stdout.contains("\"command\":\"check\""));
        assert!(stdout.contains("\"exit_code\":1"));
    }

    #[test]
    fn known_unimplemented_command_rejects_unknown_options_before_stage_notice() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--format=json", "--bogus"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNKNOWN_OPTION\""));
        assert!(stdout.contains("\"command\":\"check\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn known_unimplemented_command_reports_missing_option_values() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--format=json", "--target"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"MISSING_ARGUMENT\""));
        assert!(stdout.contains("\"argument_index\":3"));
    }

    #[test]
    fn known_unimplemented_command_validates_common_enumerated_values() {
        let (exit_code, stdout, stderr) =
            invoke(&["check", "--format=json", "--target=not-a-target"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(stdout.contains("\"argument\":\"--target=not-a-target\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn reports_global_and_command_usage_errors_together() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--format=invalid", "--bogus"]);

        assert_eq!(exit_code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("error[INVALID_OPTION_VALUE]"));
        assert!(stderr.contains("error[UNKNOWN_OPTION]"));
    }

    #[test]
    fn command_specific_enumerated_values_are_validated_before_stage_notice() {
        let (exit_code, stdout, stderr) = invoke(&[
            "component",
            "build",
            "--format=json",
            "--wasm-feature",
            "threads",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(stdout.contains("\"argument\":\"threads\""));
    }

    #[test]
    fn subcommand_usage_errors_report_the_full_command_path() {
        let (exit_code, stdout, stderr) =
            invoke(&["component", "build", "--format=json", "--bogus"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"command\":\"component build\""));
        assert!(stdout.contains("unknown option `--bogus` for `sai component build`"));
    }

    #[test]
    fn lsp_rejects_the_global_directory_option() {
        let (exit_code, stdout, stderr) = invoke(&["--format=json", "lsp", "-C", "."]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNKNOWN_OPTION\""));
        assert!(stdout.contains("\"argument\":\"-C\""));
    }

    #[test]
    fn lsp_accepts_only_its_documented_resolution_and_initial_config_options() {
        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "lsp",
            "--target",
            "wasm32-wasi",
            "--locked",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"command\":\"lsp\""));
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "lsp", "--approve"]);
        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNKNOWN_OPTION\""));
    }

    #[test]
    fn known_command_requires_its_documented_positionals() {
        for arguments in [
            vec!["remove", "--format=json"],
            vec!["add", "--format=json"],
            vec!["graph", "--format=json"],
            vec!["explain", "--format=json"],
            vec!["edit", "--format=json"],
        ] {
            let (exit_code, stdout, stderr) = invoke(&arguments);

            assert_eq!(exit_code, 2, "{arguments:?}");
            assert!(stderr.is_empty(), "{arguments:?}");
            assert!(
                stdout.contains("\"code\":\"MISSING_ARGUMENT\""),
                "{arguments:?}"
            );
            assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"), "{arguments:?}");
        }
    }

    #[test]
    fn explain_list_writes_the_complete_sorted_json_registry() {
        let (exit_code, stdout, stderr) = invoke(&["--format=json", "explain", "--list"]);
        let records = stdout.lines().collect::<Vec<_>>();

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert_eq!(records.len(), 3);
        assert!(records[0].contains("\"command\":\"explain\""));
        assert!(records[1].contains("\"kind\":\"explain.list\""));
        assert_eq!(
            records[1].matches("\"code\":").count(),
            EXPLAIN_CODE_SUMMARIES.len()
        );
        assert!(records[1].contains("\"code\":\"UNKNOWN_FIELD\",\"class\":\"hard\""));
        assert!(
            records[1].find("\"code\":\"ALIAS_CASE_MISMATCH\"")
                < records[1].find("\"code\":\"WORLD_IMPORT_UNDECLARED\"")
        );
        assert!(records[2].contains("\"exit_code\":0"));
    }

    #[test]
    fn explain_list_writes_one_plain_summary_per_code() {
        let (exit_code, stdout, stderr) = invoke(&["explain", "--list"]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert_eq!(stdout.lines().count(), EXPLAIN_CODE_SUMMARIES.len());
        assert!(stdout.starts_with("ALIAS_CASE_MISMATCH hard "));
        assert!(stdout.contains("\nUNKNOWN_FIELD hard "));
        assert!(stdout.chars().all(|character| character != '\r'));
    }

    #[test]
    fn explain_reports_unknown_codes_with_ranked_candidates() {
        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "explain", "UNKNOWN_DIAGNOSTIC_CDOE"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNKNOWN_DIAGNOSTIC_CODE\""));
        assert!(stdout.contains("\"name\":\"UNKNOWN_DIAGNOSTIC_CDOE\""));
        assert!(stdout.contains("\"candidates\":[\"UNKNOWN_DIAGNOSTIC_CODE\""));
        assert!(stdout.contains("\"exit_code\":1"));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn explain_keeps_unknown_code_diagnostics_single_line_and_json_safe() {
        let arguments = [
            "--format=json".into(),
            "explain".into(),
            "UNKNOWN\nFIELD\"".into(),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run(&arguments, &mut stdout, &mut stderr);
        let stdout = String::from_utf8(stdout).expect("JSON output is UTF-8");

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(stdout.lines().count(), 3);
        assert!(stdout.contains(r#""name":"UNKNOWN\nFIELD\"""#));
        assert!(stdout.contains(r#"unknown diagnostic code `UNKNOWN\\u{A}FIELD\"`"#));
    }

    #[cfg(unix)]
    #[test]
    fn explain_handles_non_unicode_code_arguments() {
        let arguments = vec![
            "--format=json".into(),
            "explain".into(),
            std::ffi::OsString::from_vec(vec![b'X', 0xff]),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run(&arguments, &mut stdout, &mut stderr);
        let stdout = String::from_utf8(stdout).expect("JSON output is UTF-8");

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"name\":\"X�\""));
        assert!(stdout.contains("\"code\":\"UNKNOWN_DIAGNOSTIC_CODE\""));
    }

    #[test]
    fn explain_list_reports_standard_output_failures() {
        let arguments = ["explain", "--list"].map(std::ffi::OsString::from);
        let mut stderr = Vec::new();

        let exit_code = run(&arguments, &mut BrokenPipeWriter, &mut stderr);

        assert_eq!(exit_code, 4);
        assert!(
            String::from_utf8(stderr)
                .expect("stderr is UTF-8")
                .contains("cannot write to standard output")
        );
    }

    #[test]
    fn explain_rejects_invalid_list_shapes_before_running() {
        for arguments in [
            vec!["--format=json", "explain", "--list=invalid"],
            vec!["--format=json", "explain", "--list", "UNKNOWN_FIELD"],
            vec![
                "--format=json",
                "explain",
                "UNKNOWN_FIELD",
                "--target",
                "wasm32-core",
            ],
        ] {
            let (exit_code, stdout, stderr) = invoke(&arguments);

            assert_eq!(exit_code, 2, "{arguments:?}");
            assert!(stderr.is_empty(), "{arguments:?}");
            assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"), "{arguments:?}");
        }
    }

    #[test]
    fn explain_does_not_treat_a_global_option_value_as_list_flag() {
        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "explain",
            "--directory",
            "--list",
            "UNKNOWN_FIELD",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(!stdout.contains("\"kind\":\"explain.list\""));
    }

    #[test]
    fn explain_defers_single_code_markdown_until_full_records_exist() {
        let (exit_code, stdout, stderr) = invoke(&["--format=json", "explain", "UNKNOWN_FIELD"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(stdout.contains("\"feature\":\"sai explain\""));
    }

    #[test]
    fn known_command_rejects_documented_option_conflicts_before_stage_notice() {
        let (exit_code, stdout, stderr) = invoke(&[
            "test",
            "--format=json",
            "--workspace",
            "--package",
            "acme:image",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"CONFLICTING_OPTIONS\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn validates_formatter_stdin_and_package_add_conflicts() {
        for arguments in [
            vec!["fmt", "--format=json", "--stdin-kind", "source"],
            vec!["add", "--format=json", "--path", ".", "--version", "^1.0"],
            vec!["add", "--format=json", "sai:collections", "--dir", "subdir"],
            vec!["add", "--format=json", "acme:json", "other:package"],
        ] {
            let (exit_code, stdout, stderr) = invoke(&arguments);

            assert_eq!(exit_code, 2, "{arguments:?}");
            assert!(stderr.is_empty(), "{arguments:?}");
            assert!(
                stdout.contains("\"code\":\"CONFLICTING_OPTIONS\""),
                "{arguments:?}"
            );
            assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"), "{arguments:?}");
        }
    }

    #[test]
    fn validates_add_version_requirements_and_inline_conflict_locations() {
        let (exit_code, stdout, stderr) =
            invoke(&["add", "--format=json", "acme:json", "--version", "nonsense"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(stdout.contains("\"argument\":\"nonsense\""));

        let (exit_code, stdout, stderr) =
            invoke(&["add", "--format=json", "--path=.", "--version", "^1.0"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"CONFLICTING_OPTIONS\""));
        assert!(stdout.contains("\"argument_index\":3"));
        assert!(stdout.contains("\"argument\":\"--path=.\""));

        let (exit_code, stdout, stderr) =
            invoke(&["fmt", "--format=json", "--stdin-kind=source", "file.sai"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"CONFLICTING_OPTIONS\""));
        assert!(stdout.contains("\"argument_index\":3"));
        assert!(stdout.contains("\"argument\":\"--stdin-kind=source\""));
    }

    #[test]
    fn add_accepts_only_shipped_package_identities_and_valid_git_urls() {
        let (exit_code, stdout, stderr) = invoke(&["add", "--format=json", "sai:collections"]);
        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("FEATURE_NOT_IMPLEMENTED"));

        let (exit_code, stdout, stderr) = invoke(&["add", "--format=json", "acme:json"]);
        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(stdout.contains("no registry"));

        for (url, expected_code) in [
            ("http://example.com/acme/json", "INSECURE_GIT_URL"),
            ("git://example.com/acme/json", "INSECURE_GIT_URL"),
            ("ftp://example.com/acme/json", "UNSUPPORTED_GIT_URL"),
            ("file:///tmp/json", "UNSUPPORTED_GIT_URL"),
            ("git@example.com:acme/json", "UNSUPPORTED_GIT_URL"),
            ("git@example.com:repo", "UNSUPPORTED_GIT_URL"),
            ("../json", "UNSUPPORTED_GIT_URL"),
        ] {
            let (exit_code, stdout, stderr) = invoke(&["add", "--format=json", url]);
            assert_eq!(exit_code, 2, "{url}");
            assert!(stderr.is_empty(), "{url}");
            assert!(
                stdout.contains(&format!("\"code\":\"{expected_code}\"")),
                "{url}: {stdout}"
            );
            assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"), "{url}");
        }

        let (exit_code, stdout, stderr) = invoke(&[
            "add",
            "--format=json",
            "https://example.com/acme/json?token=secret",
        ]);
        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNSUPPORTED_GIT_URL\""));
        assert!(!stdout.contains("token=secret"));

        let (exit_code, stdout, stderr) = invoke(&[
            "add",
            "--format=json",
            "https:user:secret@example.com/acme/json",
        ]);
        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNSUPPORTED_GIT_URL\""));
        assert!(!stdout.contains("secret"));

        for url in [
            "https://user:secret@example.com/acme/json",
            "ssh://git:secret@example.com/acme/json",
        ] {
            let (exit_code, stdout, stderr) = invoke(&["add", "--format=json", url]);
            assert_eq!(exit_code, 2, "{url}");
            assert!(stderr.is_empty(), "{url}");
            assert!(stdout.contains("\"code\":\"CREDENTIALS_IN_URL\""));
            assert!(stdout.contains("<redacted>"));
            assert!(!stdout.contains("secret"));
        }

        let (exit_code, stdout, stderr) =
            invoke(&["add", "--format=json", "https://example.com/acme/json"]);
        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn add_redacts_url_credentials_in_conflict_diagnostics() {
        let (exit_code, stdout, stderr) = invoke(&[
            "add",
            "--format=json",
            "--path",
            "./local",
            "https://user:secret@example.com/acme/repo?token=private",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"CONFLICTING_OPTIONS\""));
        assert!(stdout.contains("<redacted>@example.com/acme/repo"));
        assert!(!stdout.contains("secret"));
        assert!(!stdout.contains("token=private"));
    }

    #[cfg(unix)]
    #[test]
    fn add_rejects_a_non_unicode_source_argument() {
        use std::os::unix::ffi::OsStringExt;

        let source = std::ffi::OsString::from_vec(vec![b'a', b'c', b'm', b'e', b':', 0xff]);
        let arguments = vec!["--format=json".into(), "add".into(), source];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run(&arguments, &mut stdout, &mut stderr);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        let stdout = String::from_utf8(stdout).expect("CLI stdout is UTF-8");
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn scp_like_git_url_help_preserves_the_ssh_user_name() {
        let (exit_code, stdout, stderr) =
            invoke(&["add", "--format=json", "git@example.com:acme/repo"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("ssh://user@host/path"));
        assert!(stdout.contains("preserving the SSH user name"));
        assert!(!stdout.contains("ssh://example.com/acme/repo"));
    }

    #[test]
    fn update_rejects_the_same_package_more_than_once() {
        let (exit_code, stdout, stderr) = invoke(&[
            "update",
            "--format=json",
            "acme:image@1.0.0",
            "acme:image@2.0.0",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"CONFLICTING_OPTIONS\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn update_rejects_invalid_package_identity_and_exact_version() {
        let (exit_code, stdout, stderr) =
            invoke(&["update", "--format=json", "acme:image@not-a-version"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(stdout.contains("\"argument\":\"acme:image@not-a-version\""));
    }

    #[test]
    fn inspect_accepts_documented_options_and_checks_required_values() {
        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "inspect",
            "unsafe",
            "--package",
            "acme:image",
        ]);
        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "inspect", "mir", "--phase=final"]);
        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"MISSING_ARGUMENT\""));

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "inspect", "effects", "--why", "alloc"]);
        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"MISSING_ARGUMENT\""));
    }

    #[test]
    fn human_usage_diagnostics_are_separated_by_blank_lines() {
        let (exit_code, _, stderr) = invoke(&[
            "new",
            "unused",
            "--name",
            "Acme:bad_name",
            "--target",
            "unknown",
            "extra",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.contains("\n\nerror[INVALID_OPTION_VALUE]"));
    }

    #[test]
    fn usage_errors_take_precedence_over_unimplemented_command_diagnostics() {
        let (exit_code, stdout, stderr) = invoke(&["--format=invalid", "check"]);

        assert_eq!(exit_code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("error[INVALID_OPTION_VALUE]"));
        assert!(!stderr.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn interrupted_json_command_emits_only_the_exit_summary() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let arguments =
            ["--format=json", "new", "unused", "--name=acme:unused"].map(std::ffi::OsString::from);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_cancellation(&arguments, &cancellation, &mut stdout, &mut stderr);
        let stdout = String::from_utf8(stdout).unwrap();

        assert_eq!(exit_code, 130);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"command\":\"new\""));
        assert!(stdout.contains("\"type\":\"summary\""));
        assert!(stdout.contains("\"errors\":0"));
        assert!(stdout.contains("\"exit_code\":130"));
        assert!(!stdout.contains("\"type\":\"diagnostic\""));
    }

    #[test]
    fn interrupted_json_keeps_exit_130_when_stdout_is_unavailable() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let arguments = ["--format=json", "new"].map(std::ffi::OsString::from);
        let mut stdout = BrokenPipeWriter;
        let mut stderr = Vec::new();

        let exit_code = run_with_cancellation(&arguments, &cancellation, &mut stdout, &mut stderr);

        assert_eq!(exit_code, 130);
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("cannot write to standard output")
        );
    }

    #[test]
    fn interrupted_human_command_does_not_print_a_summary() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let arguments = ["new", "unused", "--name=acme:unused"].map(std::ffi::OsString::from);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_cancellation(&arguments, &cancellation, &mut stdout, &mut stderr);

        assert_eq!(exit_code, 130);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn cancellation_takes_priority_over_a_concurrent_package_error() {
        let arguments =
            ["--format=json", "new", "unused", "--name=acme:unused"].map(std::ffi::OsString::from);
        let parsed = crate::args::parse_args(&arguments);
        let command = parsed.command.as_ref().unwrap();
        let cancellation = CancellationToken::default();
        let callback_cancellation = cancellation.clone();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_new_with_creator(
            &parsed,
            command,
            &cancellation,
            &mut stdout,
            &mut stderr,
            move |_, _, _, _| {
                callback_cancellation.cancel();
                Err(sai_driver::new_package::NewPackageError::Io {
                    path: PathBuf::from("unused"),
                    source: io::Error::other("simulated filesystem error"),
                })
            },
        );

        assert_eq!(exit_code, 130);
        assert!(stderr.is_empty());
        assert!(
            String::from_utf8(stdout)
                .unwrap()
                .contains("\"exit_code\":130")
        );
    }

    #[test]
    fn stage_six_commands_report_the_protocol_feature_name() {
        let (exit_code, stdout, stderr) = invoke(&["--format=json", "graph", "calls"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"stage\":6,\"feature\":\"semantic-protocol\""));
    }

    #[test]
    fn invalid_subcommands_are_unknown_commands() {
        let (exit_code, stdout, stderr) = invoke(&["component", "bogus", "--format=json"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNKNOWN_COMMAND\""));
        assert!(stdout.contains("\"argument_index\":2,\"argument\":\"bogus\""));
    }

    #[test]
    fn max_errors_limits_rendered_usage_diagnostics_but_not_counts() {
        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--max-errors=1",
            "new",
            "unused",
            "--name",
            "Acme:bad_name",
            "--target",
            "unknown",
            "extra",
        ]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        let records = stdout.lines().collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records
                .iter()
                .filter(|record| record.contains("\"type\":\"diagnostic\""))
                .count(),
            1
        );
        assert!(records[2].contains("\"errors\":3"));
        assert!(records[2].contains("\"shown_errors\":1"));
    }

    #[test]
    fn no_arguments_prints_top_level_help_to_stderr_and_exits_with_usage_error() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run(&[], &mut stdout, &mut stderr);

        assert_eq!(exit_code, 2);
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .expect("stderr is UTF-8")
                .starts_with("Usage: sai [GLOBAL-OPTIONS]")
        );
    }

    #[test]
    fn help_is_specific_to_the_selected_command() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--help"]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.starts_with("Usage: sai check "));

        let (exit_code, stdout, stderr) = invoke(&["help", "build"]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.starts_with("Usage: sai build "));
    }

    #[test]
    fn human_paths_are_relative_and_use_forward_slashes() {
        assert_eq!(
            relative_path(
                Path::new("/workspace/project"),
                Path::new("/workspace/project/a/b")
            ),
            Some(PathBuf::from("a/b"))
        );
        assert_eq!(
            relative_path(
                Path::new("/workspace/project/src"),
                Path::new("/tmp/image/lib.sai")
            ),
            Some(PathBuf::from("../../../tmp/image/lib.sai"))
        );
        assert_eq!(
            human_file_path("image/./src/../sai.package"),
            "image/sai.package"
        );
    }

    #[cfg(unix)]
    #[test]
    fn human_path_normalization_preserves_leading_parent_components() {
        assert_eq!(
            human_file_path("../image/sai.package"),
            "../image/sai.package"
        );
        assert_eq!(
            human_file_path("image/../../image/sai.package"),
            "../image/sai.package"
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_non_unicode_argument_does_not_panic() {
        let argument = std::ffi::OsString::from_vec(vec![b'n', b'e', b'w', 0xff]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run(&[argument], &mut stdout, &mut stderr);

        assert_eq!(exit_code, 2);
        assert!(!stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn update_rejects_a_non_unicode_package_selector() {
        let selector = std::ffi::OsString::from_vec(vec![b'a', b'c', b'm', b'e', b':', 0xff]);
        let arguments = vec!["--format=json".into(), "update".into(), selector];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run(&arguments, &mut stdout, &mut stderr);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        let stdout = String::from_utf8(stdout).expect("CLI stdout is UTF-8");
        assert!(stdout.contains("\"code\":\"INVALID_OPTION_VALUE\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }
}
