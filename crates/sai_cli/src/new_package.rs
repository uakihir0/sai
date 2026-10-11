use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::args::{ParsedCommand, UsageError, UsageErrorKind, is_option, split_os_option};

const KNOWN_TARGETS: &[&str] = &[
    "wasm32-core",
    "wasm64-core",
    "wasm32-wasi",
    "wasm64-wasi",
    "x86_64-linux",
    "aarch64-linux",
    "aarch64-macos",
    "x86_64-windows",
];

/// Parsed positionals and options for `sai new`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPackageArgs {
    pub destination: PathBuf,
    pub identity: String,
    /// Empty means the command default, `wasm32-wasi`.
    pub targets: Vec<String>,
}

/// Parses command-specific arguments for `sai new`.
///
/// The shared parser retains global option tokens in the original command
/// argument sequence; their indices are skipped here without shifting values.
pub fn parse_new_command(command: &ParsedCommand) -> Result<NewPackageArgs, Vec<UsageError>> {
    let global_indices = command
        .global_argument_indices
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut errors = Vec::new();
    let mut destination = None;
    let mut identity = None;
    let mut identity_argument = None;
    let mut identity_argument_index = None;
    let mut name_option_seen = false;
    let mut targets = Vec::new();
    let mut target_values = BTreeSet::new();
    let mut index = 0;

    while index < command.arguments.len() {
        if global_indices.contains(&index) {
            index += 1;
            continue;
        }

        let argument = &command.arguments[index];
        if is_option(argument) {
            let Some((option, inline_value)) = split_os_option(argument) else {
                errors.push(command_error(
                    command,
                    index,
                    UsageErrorKind::UnknownOption,
                    argument.clone(),
                ));
                index += 1;
                continue;
            };
            if !matches!(option.as_str(), "--name" | "--target") {
                errors.push(command_error(
                    command,
                    index,
                    UsageErrorKind::UnknownOption,
                    argument.clone(),
                ));
                index += 1;
                continue;
            }

            let option_index = index;
            let duplicate_name = if option == "--name" {
                let duplicate = name_option_seen;
                name_option_seen = true;
                if duplicate {
                    errors.push(command_error(
                        command,
                        option_index,
                        UsageErrorKind::ConflictingOptions,
                        argument.clone(),
                    ));
                }
                duplicate
            } else {
                false
            };
            let inline_value_present = inline_value.is_some();
            let (value, value_index, consumed) = if let Some(value) = inline_value {
                (Some(value), index, 1)
            } else if let Some(value) = command.arguments.get(index + 1) {
                if is_option(value) {
                    (None, index, 1)
                } else {
                    (Some(value.clone()), index + 1, 2)
                }
            } else {
                (None, index, 1)
            };

            let Some(value) = value else {
                errors.push(missing_argument());
                index += consumed;
                continue;
            };

            let value_argv_index = command.argument_indices.get(value_index).copied();
            let reported_value = if inline_value_present {
                argument.clone()
            } else {
                command.arguments[value_index].clone()
            };
            if option == "--name" {
                if !duplicate_name {
                    match value.into_string() {
                        Ok(value) => {
                            identity_argument_index = value_argv_index;
                            identity_argument = Some(reported_value);
                            identity = Some(value);
                        }
                        Err(_) => errors.push(command_error(
                            command,
                            value_index,
                            UsageErrorKind::InvalidOptionValue,
                            reported_value,
                        )),
                    }
                }
            } else {
                match value.into_string() {
                    Ok(value) if !KNOWN_TARGETS.contains(&value.as_str()) => {
                        errors.push(UsageError {
                            kind: UsageErrorKind::InvalidOptionValue,
                            argument_index: value_argv_index.map_or(0, |index| index + 1),
                            argument: reported_value,
                        });
                    }
                    Ok(value) if !target_values.insert(value.clone()) => {
                        errors.push(UsageError {
                            kind: UsageErrorKind::ConflictingOptions,
                            argument_index: value_argv_index.map_or(0, |index| index + 1),
                            argument: reported_value,
                        });
                    }
                    Ok(value) => targets.push(value),
                    Err(_) => errors.push(command_error(
                        command,
                        value_index,
                        UsageErrorKind::InvalidOptionValue,
                        reported_value,
                    )),
                }
            }
            index += consumed;
            continue;
        }

        if destination.is_none() {
            destination = Some((PathBuf::from(argument), index));
        } else {
            errors.push(command_error(
                command,
                index,
                UsageErrorKind::UnexpectedArgument,
                argument.clone(),
            ));
        }
        index += 1;
    }

    for (index, argument) in command.passthrough.iter().enumerate() {
        errors.push(UsageError {
            kind: UsageErrorKind::UnexpectedArgument,
            argument_index: command
                .passthrough_indices
                .get(index)
                .map_or(0, |index| index + 1),
            argument: argument.clone(),
        });
    }

    if destination.is_none() {
        errors.push(missing_argument());
    }
    if !name_option_seen {
        errors.push(missing_argument());
    }

    if let (Some(identity), Some(argument_index), Some(argument)) = (
        identity.as_deref(),
        identity_argument_index,
        identity_argument,
    ) && sai_driver::new_package::validate_new_package_identity(identity).is_err()
    {
        errors.push(UsageError {
            kind: UsageErrorKind::InvalidOptionValue,
            argument_index: argument_index + 1,
            argument,
        });
    }

    if let Some((destination, destination_index)) = &destination {
        if destination.as_os_str().is_empty() || destination.to_str().is_none() {
            errors.push(command_error(
                command,
                *destination_index,
                UsageErrorKind::InvalidOptionValue,
                command.arguments[*destination_index].clone(),
            ));
        }
    }

    if errors.is_empty() {
        let (destination, _) = destination.expect("validated above");
        return Ok(NewPackageArgs {
            destination,
            identity: identity.expect("validated above"),
            targets,
        });
    }

    Err(errors)
}

fn command_error(
    command: &ParsedCommand,
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

fn missing_argument() -> UsageError {
    UsageError {
        kind: UsageErrorKind::MissingArgument,
        argument_index: 0,
        argument: OsString::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{NewPackageArgs, parse_new_command};
    use crate::args::{UsageError, UsageErrorKind, parse_args};

    fn parse_new(values: &[&str]) -> Result<NewPackageArgs, Vec<UsageError>> {
        let arguments = values.iter().map(OsString::from).collect::<Vec<_>>();
        let parsed = parse_args(&arguments);
        assert!(parsed.usage_errors.is_empty());
        parse_new_command(parsed.command.as_ref().unwrap())
    }

    #[test]
    fn parses_positionals_and_repeatable_targets_around_global_options() {
        let parsed = parse_new(&[
            "new",
            "image",
            "--format",
            "json",
            "--target",
            "wasm32-core",
            "--name",
            "acme:image",
            "--target=wasm64-wasi",
        ])
        .unwrap();

        assert_eq!(parsed.destination, PathBuf::from("image"));
        assert_eq!(parsed.identity, "acme:image");
        assert_eq!(parsed.targets, vec!["wasm32-core", "wasm64-wasi"]);
    }

    #[test]
    fn reports_duplicate_target_at_the_repeated_value() {
        let errors = parse_new(&[
            "new",
            "image",
            "--name",
            "acme:image",
            "--target",
            "wasm32-core",
            "--target=wasm32-core",
        ])
        .unwrap_err();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, UsageErrorKind::ConflictingOptions);
        assert_eq!(errors[0].argument_index, 7);
        assert_eq!(errors[0].argument, "--target=wasm32-core");
    }

    #[test]
    fn aggregates_invalid_values_extra_positionals_and_passthrough() {
        let errors = parse_new(&[
            "new",
            "image",
            "--name",
            "acme:image",
            "--target",
            "native",
            "extra",
            "--",
            "passthrough",
        ])
        .unwrap_err();

        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].kind, UsageErrorKind::InvalidOptionValue);
        assert_eq!(errors[0].argument_index, 6);
        assert_eq!(errors[0].argument, "native");
        assert_eq!(errors[1].kind, UsageErrorKind::UnexpectedArgument);
        assert_eq!(errors[1].argument_index, 7);
        assert_eq!(errors[1].argument, "extra");
        assert_eq!(errors[2].kind, UsageErrorKind::UnexpectedArgument);
        assert_eq!(errors[2].argument_index, 9);
        assert_eq!(errors[2].argument, "passthrough");
    }

    #[test]
    fn reports_missing_path_and_required_name() {
        let errors = parse_new(&["new"]).unwrap_err();

        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|error| error.kind == UsageErrorKind::MissingArgument)
        );
        assert!(errors.iter().all(|error| error.argument_index == 0));
    }

    #[test]
    fn reports_reserved_identity_as_invalid_option_value() {
        let errors = parse_new(&["new", "image", "--name=wasi:demo"]).unwrap_err();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, UsageErrorKind::InvalidOptionValue);
        assert_eq!(errors[0].argument_index, 3);
        assert_eq!(errors[0].argument, "--name=wasi:demo");
    }

    #[test]
    fn preserves_missing_option_value_when_a_later_option_follows() {
        let errors = parse_new(&["new", "image", "--name", "--target", "wasm32-core"]).unwrap_err();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, UsageErrorKind::MissingArgument);
        assert_eq!(errors[0].argument_index, 0);
    }
}
