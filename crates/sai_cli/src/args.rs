use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// Output format selected by a global option.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

/// Color policy selected by a global option.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

/// Global options recognized before or after a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalOptions {
    pub format: OutputFormat,
    pub color: ColorChoice,
    pub directory: Option<PathBuf>,
    pub quiet: bool,
    pub verbose: bool,
    pub max_errors: usize,
    pub help: bool,
    pub version: bool,
}

impl Default for GlobalOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Human,
            color: ColorChoice::Auto,
            directory: None,
            quiet: false,
            verbose: false,
            max_errors: 50,
            help: false,
            version: false,
        }
    }
}

/// A command name and the unparsed command-specific arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCommand {
    pub name: OsString,
    /// Original tokens after the command and before `--`, in their original
    /// order. Global options remain here so command-specific value parsing
    /// cannot accidentally skip over them.
    pub arguments: Vec<OsString>,
    /// Zero-based positions in `arguments` occupied by global option tokens
    /// and any separate values consumed by those options.
    pub global_argument_indices: Vec<usize>,
    pub passthrough: Vec<OsString>,
}

/// Usage errors found while parsing the shared command-line syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageErrorKind {
    UnknownOption,
    MissingArgument,
    InvalidOptionValue,
    ConflictingOptions,
}

/// One usage error with the argument location defined by the CLI contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageError {
    pub kind: UsageErrorKind,
    /// One-based argv position; zero denotes a missing argument.
    pub argument_index: usize,
    pub argument: OsString,
}

/// Shared command-line syntax parsed before command-specific validation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedArgs {
    pub globals: GlobalOptions,
    pub command: Option<ParsedCommand>,
    pub usage_errors: Vec<UsageError>,
}

#[derive(Clone, Copy)]
enum ValueOption {
    Format,
    Color,
    Directory,
    MaxErrors,
}

impl ValueOption {
    const fn slot(self) -> usize {
        match self {
            Self::Format => 0,
            Self::Color => 1,
            Self::Directory => 2,
            Self::MaxErrors => 3,
        }
    }
}

/// Parses global options and separates command arguments for later validation.
///
/// `arguments` excludes argv[0]. Global options may appear on either side of
/// the command. Unrecognized options before the command are reported here.
/// The original post-command token sequence is preserved, with the positions
/// of recognized global option tokens recorded for command-specific parsing.
pub fn parse_args(arguments: &[OsString]) -> ParsedArgs {
    let mut parsed = ParsedArgs::default();
    let mut seen_values = [None; 4];
    let mut quiet_index = None;
    let mut verbose_index = None;
    let mut quiet_verbose_conflict_reported = false;
    let mut command = None;
    let mut command_arguments = Vec::new();
    let mut global_argument_indices = Vec::new();
    let mut passthrough = Vec::new();
    let mut after_separator = false;
    let mut index = 0;

    while index < arguments.len() {
        let argument = &arguments[index];
        if after_separator {
            if command.is_some() {
                passthrough.push(argument.clone());
            }
            index += 1;
            continue;
        }
        if argument == OsStr::new("--") {
            after_separator = true;
            index += 1;
            continue;
        }

        if is_option(argument) {
            let parsed_global = split_os_option(argument).and_then(|(name, inline_value)| {
                parse_global_option(
                    &name,
                    inline_value.as_deref(),
                    index,
                    arguments,
                    &mut parsed,
                    &mut seen_values,
                    &mut quiet_index,
                    &mut verbose_index,
                    &mut quiet_verbose_conflict_reported,
                )
            });
            if let Some(consumed) = parsed_global {
                if command.is_some() {
                    let start = command_arguments.len();
                    command_arguments.extend_from_slice(&arguments[index..index + consumed]);
                    global_argument_indices.extend(start..command_arguments.len());
                }
                index += consumed;
                continue;
            }
            if command.is_none() {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::UnknownOption,
                    argument_index: index + 1,
                    argument: argument.clone(),
                });
            } else {
                command_arguments.push(argument.clone());
            }
            index += 1;
            continue;
        }

        if command.is_none() {
            command = Some(argument.clone());
        } else {
            command_arguments.push(argument.clone());
        }
        index += 1;
    }

    if let Some(name) = command {
        parsed.command = Some(ParsedCommand {
            name,
            arguments: command_arguments,
            global_argument_indices,
            passthrough,
        });
    } else if !parsed.globals.help && !parsed.globals.version {
        parsed.usage_errors.push(UsageError {
            kind: UsageErrorKind::MissingArgument,
            argument_index: 0,
            argument: OsString::new(),
        });
    }
    parsed
}

#[allow(clippy::too_many_arguments)]
fn parse_global_option(
    name: &str,
    inline_value: Option<&OsStr>,
    index: usize,
    arguments: &[OsString],
    parsed: &mut ParsedArgs,
    seen_values: &mut [Option<usize>; 4],
    quiet_index: &mut Option<usize>,
    verbose_index: &mut Option<usize>,
    quiet_verbose_conflict_reported: &mut bool,
) -> Option<usize> {
    match name {
        "--format" => {
            let first_value = record_value_option(
                ValueOption::Format,
                index,
                &arguments[index],
                seen_values,
                parsed,
            );
            let Some((value, consumed, value_index)) =
                read_value(inline_value, index, arguments, parsed)
            else {
                return Some(1);
            };
            match value.to_str() {
                Some("human") if first_value => parsed.globals.format = OutputFormat::Human,
                Some("json") if first_value => parsed.globals.format = OutputFormat::Json,
                Some("human" | "json") => {}
                _ => parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: value_index,
                    argument: invalid_value_argument(inline_value, &arguments[index], &value),
                }),
            }
            Some(consumed)
        }
        "--color" => {
            let first_value = record_value_option(
                ValueOption::Color,
                index,
                &arguments[index],
                seen_values,
                parsed,
            );
            let Some((value, consumed, value_index)) =
                read_value(inline_value, index, arguments, parsed)
            else {
                return Some(1);
            };
            match value.to_str() {
                Some("auto") if first_value => parsed.globals.color = ColorChoice::Auto,
                Some("always") if first_value => parsed.globals.color = ColorChoice::Always,
                Some("never") if first_value => parsed.globals.color = ColorChoice::Never,
                Some("auto" | "always" | "never") => {}
                _ => parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: value_index,
                    argument: invalid_value_argument(inline_value, &arguments[index], &value),
                }),
            }
            Some(consumed)
        }
        "--directory" | "-C" => {
            let first_value = record_value_option(
                ValueOption::Directory,
                index,
                &arguments[index],
                seen_values,
                parsed,
            );
            let Some((value, consumed, _)) = read_os_value(inline_value, index, arguments, parsed)
            else {
                return Some(1);
            };
            if first_value {
                parsed.globals.directory = Some(PathBuf::from(value));
            }
            Some(consumed)
        }
        "--max-errors" => {
            let first_value = record_value_option(
                ValueOption::MaxErrors,
                index,
                &arguments[index],
                seen_values,
                parsed,
            );
            let Some((value, consumed, value_index)) =
                read_value(inline_value, index, arguments, parsed)
            else {
                return Some(1);
            };
            match value.to_str().and_then(|text| text.parse::<usize>().ok()) {
                Some(value) if first_value => parsed.globals.max_errors = value,
                Some(_) => {}
                None => parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: value_index,
                    argument: invalid_value_argument(inline_value, &arguments[index], &value),
                }),
            }
            Some(consumed)
        }
        "--quiet" | "-q" => {
            if inline_value.is_some() {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: index + 1,
                    argument: arguments[index].clone(),
                });
                return Some(1);
            }
            parsed.globals.quiet = true;
            if verbose_index.is_some() && !*quiet_verbose_conflict_reported {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::ConflictingOptions,
                    argument_index: index + 1,
                    argument: arguments[index].clone(),
                });
                *quiet_verbose_conflict_reported = true;
            }
            quiet_index.get_or_insert(index);
            Some(1)
        }
        "--verbose" | "-v" => {
            if inline_value.is_some() {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: index + 1,
                    argument: arguments[index].clone(),
                });
                return Some(1);
            }
            parsed.globals.verbose = true;
            if quiet_index.is_some() && !*quiet_verbose_conflict_reported {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::ConflictingOptions,
                    argument_index: index + 1,
                    argument: arguments[index].clone(),
                });
                *quiet_verbose_conflict_reported = true;
            }
            verbose_index.get_or_insert(index);
            Some(1)
        }
        "--help" | "-h" => {
            if inline_value.is_some() {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: index + 1,
                    argument: arguments[index].clone(),
                });
                return Some(1);
            }
            parsed.globals.help = true;
            Some(1)
        }
        "--version" => {
            if inline_value.is_some() {
                parsed.usage_errors.push(UsageError {
                    kind: UsageErrorKind::InvalidOptionValue,
                    argument_index: index + 1,
                    argument: arguments[index].clone(),
                });
                return Some(1);
            }
            parsed.globals.version = true;
            Some(1)
        }
        _ => None,
    }
}

fn read_value(
    inline_value: Option<&OsStr>,
    option_index: usize,
    arguments: &[OsString],
    parsed: &mut ParsedArgs,
) -> Option<(OsString, usize, usize)> {
    if let Some(value) = inline_value {
        return Some((value.to_os_string(), 1, option_index + 1));
    }
    let Some(value) = arguments.get(option_index + 1) else {
        parsed.usage_errors.push(UsageError {
            kind: UsageErrorKind::MissingArgument,
            argument_index: 0,
            argument: OsString::new(),
        });
        return None;
    };
    if value == OsStr::new("--") || is_global_option(value) {
        parsed.usage_errors.push(UsageError {
            kind: UsageErrorKind::MissingArgument,
            argument_index: 0,
            argument: OsString::new(),
        });
        return None;
    }
    Some((value.clone(), 2, option_index + 2))
}

fn read_os_value<'a>(
    inline_value: Option<&'a OsStr>,
    option_index: usize,
    arguments: &'a [OsString],
    parsed: &mut ParsedArgs,
) -> Option<(&'a OsStr, usize, usize)> {
    if let Some(value) = inline_value {
        return Some((value, 1, option_index + 1));
    }
    let Some(value) = arguments.get(option_index + 1) else {
        parsed.usage_errors.push(UsageError {
            kind: UsageErrorKind::MissingArgument,
            argument_index: 0,
            argument: OsString::new(),
        });
        return None;
    };
    if *value == OsStr::new("--") || is_global_option(value) {
        parsed.usage_errors.push(UsageError {
            kind: UsageErrorKind::MissingArgument,
            argument_index: 0,
            argument: OsString::new(),
        });
        return None;
    }
    Some((value, 2, option_index + 2))
}

fn is_global_option(argument: &OsStr) -> bool {
    split_os_option(argument).is_some_and(|(name, _)| {
        matches!(
            name.as_str(),
            "--format"
                | "--color"
                | "--directory"
                | "-C"
                | "--max-errors"
                | "--quiet"
                | "-q"
                | "--verbose"
                | "-v"
                | "--help"
                | "-h"
                | "--version"
        )
    })
}

fn invalid_value_argument(
    inline_value: Option<&OsStr>,
    option_argument: &OsString,
    value: &OsString,
) -> OsString {
    if inline_value.is_some() {
        option_argument.clone()
    } else {
        value.clone()
    }
}

fn is_option(argument: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        argument.as_bytes().starts_with(b"-")
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        argument.encode_wide().next() == Some(b'-' as u16)
    }
    #[cfg(not(any(unix, windows)))]
    {
        argument.to_str().is_some_and(|text| text.starts_with('-'))
    }
}

fn split_os_option(argument: &OsStr) -> Option<(String, Option<OsString>)> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let bytes = argument.as_bytes();
        if !bytes.starts_with(b"--")
            && bytes != b"-C"
            && bytes != b"-q"
            && bytes != b"-v"
            && bytes != b"-h"
        {
            return None;
        }
        if bytes.starts_with(b"--") {
            let equals = bytes.iter().position(|byte| *byte == b'=');
            let name_end = equals.unwrap_or(bytes.len());
            let name = std::str::from_utf8(&bytes[..name_end]).ok()?.to_owned();
            let value = equals.map(|equals| OsString::from_vec(bytes[equals + 1..].to_vec()));
            Some((name, value))
        } else {
            Some((std::str::from_utf8(bytes).ok()?.to_owned(), None))
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let wide = argument.encode_wide().collect::<Vec<_>>();
        if !wide.starts_with(&[b'-' as u16, b'-' as u16])
            && wide.as_slice() != [b'-' as u16, b'C' as u16]
            && wide.as_slice() != [b'-' as u16, b'q' as u16]
            && wide.as_slice() != [b'-' as u16, b'v' as u16]
            && wide.as_slice() != [b'-' as u16, b'h' as u16]
        {
            return None;
        }
        if wide.starts_with(&[b'-' as u16, b'-' as u16]) {
            let equals = wide.iter().position(|unit| *unit == b'=' as u16);
            let name_end = equals.unwrap_or(wide.len());
            let name = String::from_utf16(&wide[..name_end]).ok()?;
            let value = equals.map(|equals| OsString::from_wide(&wide[equals + 1..]));
            Some((name, value))
        } else {
            Some((String::from_utf16(&wide).ok()?, None))
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let text = argument.to_str()?;
        if text.starts_with("--") {
            let (name, value) = text
                .split_once('=')
                .map_or((text, None), |(name, value)| (name, Some(value)));
            Some((name.to_owned(), value.map(OsString::from)))
        } else if matches!(text, "-C" | "-q" | "-v" | "-h") {
            Some((text.to_owned(), None))
        } else {
            None
        }
    }
}

fn record_value_option(
    option: ValueOption,
    index: usize,
    argument: &OsString,
    seen_values: &mut [Option<usize>; 4],
    parsed: &mut ParsedArgs,
) -> bool {
    let slot = option.slot();
    if seen_values[slot].is_some() {
        parsed.usage_errors.push(UsageError {
            kind: UsageErrorKind::ConflictingOptions,
            argument_index: index + 1,
            argument: argument.clone(),
        });
        false
    } else {
        seen_values[slot] = Some(index);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{ColorChoice, OutputFormat, UsageErrorKind, parse_args};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_global_options_before_and_after_the_command() {
        let parsed = parse_args(&args(&[
            "--format=json",
            "-C",
            "workspace",
            "check",
            "--color",
            "always",
            "--max-errors=12",
            "-q",
        ]));

        assert!(parsed.usage_errors.is_empty());
        assert_eq!(parsed.globals.format, OutputFormat::Json);
        assert_eq!(parsed.globals.color, ColorChoice::Always);
        assert_eq!(
            parsed.globals.directory.as_deref(),
            Some(std::path::Path::new("workspace"))
        );
        assert_eq!(parsed.globals.max_errors, 12);
        assert!(parsed.globals.quiet);
        let command = parsed.command.unwrap();
        assert_eq!(command.name, "check");
        assert_eq!(
            command.arguments,
            args(&["--color", "always", "--max-errors=12", "-q"])
        );
        assert_eq!(command.global_argument_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn preserves_command_options_and_arguments_for_command_specific_parsing() {
        let parsed = parse_args(&args(&["check", "--target", "wasm32-core", "src/main.sai"]));
        let command = parsed.command.unwrap();

        assert_eq!(
            command.arguments,
            args(&["--target", "wasm32-core", "src/main.sai"])
        );
    }

    #[test]
    fn preserves_everything_after_the_separator_as_passthrough() {
        let parsed = parse_args(&args(&["run", "--quiet", "--", "--help", "arg"]));
        let command = parsed.command.unwrap();

        assert_eq!(command.arguments, args(&["--quiet"]));
        assert_eq!(command.global_argument_indices, vec![0]);
        assert_eq!(command.passthrough, args(&["--help", "arg"]));
        assert!(parsed.globals.quiet);
    }

    #[test]
    fn requires_a_command_before_the_separator() {
        let parsed = parse_args(&args(&["--", "check", "value"]));

        assert!(parsed.command.is_none());
        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| (error.kind, error.argument_index))
                .collect::<Vec<_>>(),
            vec![(UsageErrorKind::MissingArgument, 0)]
        );
    }

    #[test]
    fn accepts_repeated_boolean_options() {
        let parsed = parse_args(&args(&["--help", "-h", "check", "--quiet", "-q"]));
        assert!(parsed.usage_errors.is_empty());
        assert!(parsed.globals.help);
        assert!(parsed.globals.quiet);
    }

    #[test]
    fn reports_repeated_value_options_and_quiet_verbose_conflicts() {
        let parsed = parse_args(&args(&[
            "--format",
            "json",
            "check",
            "--format=human",
            "--quiet",
            "-v",
        ]));

        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| error.kind)
                .collect::<Vec<_>>(),
            vec![
                UsageErrorKind::ConflictingOptions,
                UsageErrorKind::ConflictingOptions,
            ]
        );
        assert_eq!(parsed.usage_errors[0].argument_index, 4);
        assert_eq!(parsed.usage_errors[1].argument_index, 6);
    }

    #[test]
    fn treats_directory_aliases_as_one_repeated_value_option() {
        let parsed = parse_args(&args(&["-C", "first", "check", "--directory=second"]));

        assert_eq!(
            parsed.globals.directory.as_deref(),
            Some(std::path::Path::new("first"))
        );
        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| (error.kind, error.argument_index))
                .collect::<Vec<_>>(),
            vec![(UsageErrorKind::ConflictingOptions, 4)]
        );
    }

    #[test]
    fn reports_both_a_repeated_option_and_its_missing_value() {
        let parsed = parse_args(&args(&["--format=json", "check", "--format"]));

        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| (error.kind, error.argument_index))
                .collect::<Vec<_>>(),
            vec![
                (UsageErrorKind::ConflictingOptions, 3),
                (UsageErrorKind::MissingArgument, 0),
            ]
        );
    }

    #[test]
    fn reports_missing_and_invalid_global_values() {
        let missing = parse_args(&args(&["--format"]));
        assert_eq!(
            missing.usage_errors[0].kind,
            UsageErrorKind::MissingArgument
        );
        assert_eq!(missing.usage_errors[0].argument_index, 0);

        let invalid = parse_args(&args(&["check", "--color", "blue"]));
        assert_eq!(
            invalid.usage_errors[0].kind,
            UsageErrorKind::InvalidOptionValue
        );
        assert_eq!(invalid.usage_errors[0].argument_index, 3);
        assert_eq!(invalid.usage_errors[0].argument, "blue");
    }

    #[test]
    fn reports_missing_value_before_parsing_the_next_global_option() {
        let parsed = parse_args(&args(&["check", "--format", "--quiet", "-v"]));

        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| (error.kind, error.argument_index))
                .collect::<Vec<_>>(),
            vec![
                (UsageErrorKind::MissingArgument, 0),
                (UsageErrorKind::ConflictingOptions, 4),
            ]
        );
        assert!(parsed.globals.quiet);
        assert!(parsed.globals.verbose);
    }

    #[test]
    fn reports_the_complete_option_token_for_invalid_inline_values() {
        let parsed = parse_args(&args(&[
            "check",
            "--format=xml",
            "--color=ultraviolet",
            "--max-errors=many",
        ]));

        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| (error.argument_index, error.argument.clone()))
                .collect::<Vec<_>>(),
            vec![
                (2, OsString::from("--format=xml")),
                (3, OsString::from("--color=ultraviolet")),
                (4, OsString::from("--max-errors=many")),
            ]
        );
    }

    #[test]
    fn preserves_command_option_value_positions_around_global_options() {
        let parsed = parse_args(&args(&["build", "--target", "--quiet", "wasm32-core"]));
        let command = parsed.command.unwrap();

        assert_eq!(
            command.arguments,
            args(&["--target", "--quiet", "wasm32-core"])
        );
        assert_eq!(command.global_argument_indices, vec![1]);
        assert!(parsed.globals.quiet);
    }

    #[test]
    fn treats_the_separator_as_missing_value_boundary_and_preserves_passthrough() {
        let parsed = parse_args(&args(&["check", "--format", "--", "--help"]));
        let command = parsed.command.unwrap();

        assert_eq!(
            parsed
                .usage_errors
                .iter()
                .map(|error| (error.kind, error.argument_index))
                .collect::<Vec<_>>(),
            vec![(UsageErrorKind::MissingArgument, 0)]
        );
        assert!(!parsed.globals.help);
        assert_eq!(command.passthrough, args(&["--help"]));
    }

    #[test]
    fn reports_a_missing_command_except_for_help_and_version() {
        let missing = parse_args(&args(&[]));
        assert_eq!(
            missing
                .usage_errors
                .iter()
                .map(|error| (error.kind, error.argument_index))
                .collect::<Vec<_>>(),
            vec![(UsageErrorKind::MissingArgument, 0)]
        );

        let help = parse_args(&args(&["--help"]));
        assert!(help.command.is_none());
        assert!(help.usage_errors.is_empty());

        let version = parse_args(&args(&["--version"]));
        assert!(version.command.is_none());
        assert!(version.usage_errors.is_empty());
    }

    #[test]
    fn accepts_zero_as_the_unlimited_error_diagnostic_limit() {
        let parsed = parse_args(&args(&["--max-errors=0", "check"]));

        assert!(parsed.usage_errors.is_empty());
        assert_eq!(parsed.globals.max_errors, 0);
    }

    #[test]
    fn rejects_values_attached_to_boolean_global_options() {
        let parsed = parse_args(&args(&["--quiet=false", "check"]));

        assert_eq!(
            parsed.usage_errors[0].kind,
            UsageErrorKind::InvalidOptionValue
        );
        assert_eq!(parsed.usage_errors[0].argument_index, 1);
        assert!(!parsed.globals.quiet);
        assert_eq!(parsed.command.unwrap().name, "check");
    }

    #[cfg(unix)]
    #[test]
    fn invalid_non_unicode_value_is_consumed_without_becoming_the_command() {
        use std::os::unix::ffi::OsStringExt;

        let parsed = parse_args(&[
            OsString::from("--format"),
            OsString::from_vec(vec![0xff]),
            OsString::from("check"),
        ]);

        assert_eq!(
            parsed.usage_errors[0].kind,
            UsageErrorKind::InvalidOptionValue
        );
        assert_eq!(parsed.usage_errors[0].argument_index, 2);
        assert_eq!(parsed.command.unwrap().name, "check");
    }

    #[cfg(unix)]
    #[test]
    fn accepts_non_unicode_inline_directory_values() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let parsed = parse_args(&[
            OsString::from_vec(b"--directory=\xff".to_vec()),
            OsString::from("check"),
        ]);

        assert!(parsed.usage_errors.is_empty());
        assert_eq!(
            parsed
                .globals
                .directory
                .as_deref()
                .unwrap()
                .as_os_str()
                .as_bytes(),
            b"\xff"
        );
        assert_eq!(parsed.command.unwrap().name, "check");
    }

    #[cfg(unix)]
    #[test]
    fn accepts_non_unicode_separate_directory_values() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let parsed = parse_args(&[
            OsString::from("--directory"),
            OsString::from_vec(vec![0xff]),
            OsString::from("check"),
        ]);

        assert!(parsed.usage_errors.is_empty());
        assert_eq!(
            parsed
                .globals
                .directory
                .as_deref()
                .unwrap()
                .as_os_str()
                .as_bytes(),
            &[0xff]
        );
        assert_eq!(parsed.command.unwrap().name, "check");
    }

    #[test]
    fn reports_unknown_options_before_the_command_but_preserves_them_after() {
        let before = parse_args(&args(&["--unknown", "check"]));
        assert_eq!(before.usage_errors[0].kind, UsageErrorKind::UnknownOption);
        assert_eq!(before.usage_errors[0].argument_index, 1);

        let after = parse_args(&args(&["check", "--unknown"]));
        assert!(after.usage_errors.is_empty());
        assert_eq!(after.command.unwrap().arguments, args(&["--unknown"]));
    }

    #[test]
    fn rejects_equals_syntax_on_the_short_directory_alias() {
        let parsed = parse_args(&args(&["-C=workspace", "check"]));

        assert_eq!(parsed.usage_errors.len(), 1);
        assert_eq!(parsed.usage_errors[0].kind, UsageErrorKind::UnknownOption);
        assert_eq!(parsed.usage_errors[0].argument_index, 1);
        assert_eq!(parsed.usage_errors[0].argument, "-C=workspace");
        assert_eq!(parsed.command.unwrap().name, "check");
    }
}
