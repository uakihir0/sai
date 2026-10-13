use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::args::{ColorChoice, OutputFormat, ParsedArgs, UsageError, UsageErrorKind, parse_args};
use crate::help::{render_command_help, render_top_level_help, render_version};
use crate::new_package::parse_new_command;
use sai_diagnostics::{EXPLAIN_CODE_SUMMARIES, SourceRef, Value, ValueObject};
use sai_driver::new_package::{
    NewPackageError, NewPackageReceipt, create_new_package_with_cancellation,
};
use sai_driver::{CancellationToken, RootDatabase, TargetCfg};
use sai_package::workspace_lock::{WorkspaceLockGuard, WorkspaceLockMode};
use sai_query::Durability;
use sai_vfs::{
    FileContents, FileKey, PackageFileEntry, PackageFilesSnapshot, PackageKey, PackageRelativePath,
    PackageSourceDirectory, SnapshotFile, SourceFile, VfsInputSnapshot, WorkspaceSnapshot,
};
use sha2::{Digest, Sha256};
use unicode_width::UnicodeWidthChar;

static NEXT_FMT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

struct CliDiagnostic {
    code: &'static str,
    message: String,
    primary: Option<CliLocation>,
    primary_label: String,
    argument_index: Option<usize>,
    argument: String,
    help: Option<String>,
    data: Option<String>,
}

struct CheckDiagnostic {
    diagnostic: CliDiagnostic,
    severity: sai_diagnostics::Severity,
    package: Option<String>,
    related: Vec<CheckRelated>,
    children: Vec<CheckChild>,
}

struct CheckRelated {
    location: CliLocation,
    what: String,
}

struct CheckChild {
    kind: sai_diagnostics::ChildKind,
    message: String,
    location: Option<CliLocation>,
}

struct CliLocation {
    path: String,
    start: CliPosition,
    end: CliPosition,
    source_line: Option<Arc<RenderedSourceLine>>,
    additional_source_lines: Vec<CliAdditionalSnippetRow>,
    start_display_col: usize,
    end_display_col: usize,
}

enum CliAdditionalSnippetRow {
    Gap,
    Source {
        line_number: usize,
        source_line: Arc<RenderedSourceLine>,
        annotation: Option<(usize, usize)>,
    },
}

#[derive(Clone, Copy)]
struct CliPosition {
    byte: usize,
    line: usize,
    col: usize,
    utf16_col: usize,
}

#[derive(Clone, Copy)]
struct CliOutputOptions {
    format: OutputFormat,
    quiet: bool,
    color: ColorChoice,
    max_errors: usize,
}

#[derive(Default)]
struct FmtResult {
    check: bool,
    files: usize,
    changed: Vec<String>,
    refused: Vec<String>,
    output: Option<String>,
}

struct FmtDiagnosticBatch {
    diagnostics: Vec<CliDiagnostic>,
    total_error_count: usize,
}

struct PendingFmtDiagnostic {
    code: &'static str,
    message: Cow<'static, str>,
    span: std::ops::Range<usize>,
    help: Option<String>,
    data: Option<String>,
    order: usize,
}

struct FmtDiagnosticSource<'a> {
    path: &'a str,
    include_source_line: bool,
}

struct CliSourceIndex {
    source: Arc<[u8]>,
    line_starts: Vec<u32>,
    line_ends: HashMap<u32, u32>,
    utf16_checkpoints: HashMap<u32, Vec<(u32, usize)>>,
    rendered_lines: HashMap<u32, Arc<RenderedSourceLine>>,
}

struct RenderedSourceLine {
    text: String,
    checkpoints: Vec<(u32, usize)>,
    window_checkpoints: Vec<(usize, usize)>,
    display_width: usize,
    first_non_whitespace_display_col: usize,
}

impl PartialEq for PendingFmtDiagnostic {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PendingFmtDiagnostic {}

impl PartialOrd for PendingFmtDiagnostic {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingFmtDiagnostic {
    fn cmp(&self, other: &Self) -> Ordering {
        self.span
            .start
            .cmp(&other.span.start)
            .then_with(|| self.code.cmp(other.code))
            .then_with(|| self.message.cmp(&other.message))
            .then_with(|| self.order.cmp(&other.order))
    }
}

struct StdinBroker {
    request_sender: SyncSender<usize>,
    receiver: Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    pending_offset: usize,
    request_pending: bool,
    eof: bool,
    startup_error: Option<io::Error>,
}

struct CancellationAwareStdin {
    broker: Option<Arc<Mutex<StdinBroker>>>,
    cancellation: CancellationToken,
}

static STDIN_BROKER: OnceLock<Arc<Mutex<StdinBroker>>> = OnceLock::new();

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
    run_with_cancellation_and_terminal(arguments, cancellation, false, stdout, stderr)
}

/// Runs the CLI while recording whether stdout is a terminal for safe rendering
/// of source text returned by commands such as `fmt -`.
pub fn run_with_cancellation_and_terminal(
    arguments: &[OsString],
    cancellation: &CancellationToken,
    stdout_is_terminal: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let mut stdin = CancellationAwareStdin {
        broker: None,
        cancellation: cancellation.clone(),
    };
    run_with_cancellation_and_input(
        arguments,
        cancellation,
        &mut stdin,
        stdout,
        stderr,
        stdout_is_terminal,
    )
}

fn run_with_cancellation_and_input(
    arguments: &[OsString],
    cancellation: &CancellationToken,
    stdin: &mut impl Read,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    stdout_is_terminal: bool,
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
        "check" => run_check(&parsed, command, cancellation, stdout, stderr),
        "fmt" => run_fmt(
            &parsed,
            command,
            cancellation,
            stdin,
            stdout,
            stderr,
            stdout_is_terminal,
        ),
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

fn run_check(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let mut usage_errors = parsed.usage_errors.clone();
    usage_errors.extend(validate_known_command_options(command));
    if !usage_errors.is_empty() {
        return report_usage_errors(parsed, "check", usage_errors, stdout, stderr);
    }

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let diagnostic = check_cli_diagnostic(fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                ".",
                "could not resolve the current directory",
                &error,
            ));
            return report_check_diagnostics(
                parsed,
                vec![diagnostic, check_incomplete_diagnostic(command)],
                2,
                0,
                0,
                None,
                stdout,
                stderr,
            );
        }
    };
    let start_directory = parsed.globals.directory.as_deref().map_or_else(
        || cwd.clone(),
        |directory| {
            if directory.is_absolute() {
                directory.to_path_buf()
            } else {
                cwd.join(directory)
            }
        },
    );
    let package_root = match crate::package_discovery::find_root_package(&start_directory, &cwd) {
        Ok(root) => root,
        Err(crate::package_discovery::PackageDiscoveryError::NoPackageFound {
            start_directory,
        }) => {
            let diagnostic = CliDiagnostic {
                code: "NO_PACKAGE_FOUND",
                message: format!(
                    "no package found from start directory `{}`",
                    start_directory.display()
                ),
                primary: None,
                primary_label: String::new(),
                argument_index: None,
                argument: String::new(),
                help: Some("run `sai new` to create a package".to_owned()),
                data: None,
            };
            return report_check_diagnostics(
                parsed,
                vec![check_cli_diagnostic(diagnostic)],
                1,
                0,
                0,
                None,
                stdout,
                stderr,
            );
        }
        Err(error) => {
            let path = fmt_path_reference(&start_directory, &cwd, None);
            let diagnostic = fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                &path,
                "could not discover the package",
                &io::Error::other(error),
            );
            return report_check_diagnostics(
                parsed,
                vec![
                    check_cli_diagnostic(diagnostic),
                    check_incomplete_diagnostic(command),
                ],
                2,
                0,
                0,
                None,
                stdout,
                stderr,
            );
        }
    };

    let mut diagnostics = Vec::new();
    let mut total_errors = 0usize;
    let mut total_warnings = 0usize;
    let mut total_infos = 0usize;
    let mut root_identity = None;
    let mut root_package_key = None;
    let mut package_header_span = None;
    let mut manifest_source = None;
    let include_source_lines = parsed.globals.format == OutputFormat::Human;

    let manifest_path = package_root.join("sai.package");
    let manifest_display_path = fmt_path_reference(&manifest_path, &cwd, Some(&package_root));
    let manifest_selection = crate::fmt_paths::select_fmt_paths(
        std::slice::from_ref(&manifest_path),
        &cwd,
        cancellation,
    );
    if manifest_selection.interrupted || cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    for error in manifest_selection.errors {
        diagnostics.push(check_cli_diagnostic(fmt_io_diagnostic(
            "SOURCE_READ_FAILED",
            &manifest_display_path,
            "could not inspect the package manifest",
            &error.source,
        )));
        total_errors = total_errors.saturating_add(1);
    }
    for (_, missing_path) in manifest_selection.missing {
        diagnostics.push(check_cli_diagnostic(fmt_io_diagnostic(
            "SOURCE_READ_FAILED",
            &fmt_path_reference(&missing_path, &cwd, Some(&package_root)),
            "package manifest disappeared during discovery",
            &io::Error::new(io::ErrorKind::NotFound, "manifest path not found"),
        )));
        total_errors = total_errors.saturating_add(1);
    }

    let mut parsed_manifest = None;
    if let Some(selected) = manifest_selection.files.into_iter().next() {
        let mut file = selected_check_file(selected, manifest_display_path.clone());
        match open_selected_fmt_file(&mut file) {
            Ok(source_file) => {
                match read_file_limited(
                    source_file,
                    sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES,
                    cancellation,
                ) {
                    Ok(source) => {
                        if cancellation.is_cancelled() {
                            return report_interrupted(parsed, stdout, stderr);
                        }
                        let source: Arc<[u8]> = source.into();
                        let parse = sai_package::manifest::parse_for_role_with_error_limit(
                            &source,
                            sai_package::manifest::PackageRole::Root,
                            fmt_error_limit(parsed.globals.max_errors),
                        );
                        let parsed_root_identity = package_identity(&parse);
                        let (manifest_diagnostics, error_count, warning_count) =
                            package_manifest_check_diagnostics(
                                &source,
                                &manifest_display_path,
                                &parse,
                                include_source_lines,
                            );
                        diagnostics.extend(manifest_diagnostics);
                        total_errors = total_errors.saturating_add(error_count);
                        total_warnings = total_warnings.saturating_add(warning_count);

                        if let Some(header) = parse.file().header.as_ref() {
                            root_identity = parsed_root_identity;
                            root_package_key = Some(format!(
                                "{}#{}",
                                header.value.identity,
                                header.value.version.compatibility_line()
                            ));
                            package_header_span = Some(header.span.clone());
                            manifest_source = Some(Arc::clone(&source));
                        }
                        if parse.is_syntax_valid() {
                            parsed_manifest = Some(parse);
                        }
                    }
                    Err(StdinReadError::Interrupted) => {
                        return report_interrupted(parsed, stdout, stderr);
                    }
                    Err(StdinReadError::Io(error)) => {
                        diagnostics.push(check_cli_diagnostic(fmt_io_diagnostic(
                            "SOURCE_READ_FAILED",
                            &manifest_display_path,
                            "could not read the package manifest",
                            &error,
                        )));
                        total_errors = total_errors.saturating_add(1);
                    }
                }
            }
            Err(error) => {
                diagnostics.push(check_cli_diagnostic(fmt_io_diagnostic(
                    "SOURCE_READ_FAILED",
                    &manifest_display_path,
                    "could not open the package manifest",
                    &error,
                )));
                total_errors = total_errors.saturating_add(1);
            }
        }
    }

    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    if let Some(manifest) = parsed_manifest {
        let declared_source = manifest.file().declarations.iter().find_map(|declaration| {
            if let sai_package::manifest::PackageDeclaration::Source(source) = &declaration.value {
                Some(source.as_ref())
            } else {
                None
            }
        });
        if let Some(declared_source) = declared_source {
            match sai_package::package_path::ContentPackagePath::parse(declared_source) {
                Ok(relative_source) => {
                    let source_path = package_root.join(relative_source.as_str());
                    match source_path_contains_symlink(&package_root, relative_source.as_str()) {
                        Ok(true) => {
                            diagnostics.push(module_tree_source_root_symlink_diagnostic(
                                relative_source.as_str(),
                                root_identity.as_deref(),
                            ));
                            total_errors = total_errors.saturating_add(1);
                        }
                        Ok(false) => {
                            match std::fs::canonicalize(&source_path) {
                                Ok(canonical_source_directory)
                                    if canonical_source_directory.starts_with(&package_root)
                                        && std::fs::metadata(&canonical_source_directory)
                                            .is_ok_and(|metadata| metadata.is_dir()) =>
                                {
                                    // Preserve the manifest-declared path for
                                    // diagnostics. The module-tree walker opens
                                    // every component without following links.
                                    let source_directory = source_path.clone();
                                    let selection = crate::fmt_paths::select_fmt_module_paths(
                                        std::slice::from_ref(&source_directory),
                                        &cwd,
                                        cancellation,
                                    );
                                    if selection.interrupted || cancellation.is_cancelled() {
                                        return report_interrupted(parsed, stdout, stderr);
                                    }
                                    let root_module_path = source_directory.join("lib.sai");
                                    let root_is_symlink = selection.module_tree_root_symlink;
                                    let root_inspection_incomplete = root_is_symlink
                                        || selection.errors.iter().any(|error| {
                                            error.path == source_directory
                                                || error.path == root_module_path
                                        })
                                        || selection.missing.iter().any(|(_, path)| {
                                            path == &source_directory || path == &root_module_path
                                        });
                                    if root_is_symlink {
                                        diagnostics.push(
                                            module_tree_source_root_symlink_diagnostic(
                                                relative_source.as_str(),
                                                root_identity.as_deref(),
                                            ),
                                        );
                                        total_errors = total_errors.saturating_add(1);
                                    }
                                    let module_tree_entries = selection.module_tree_entries;
                                    for error in selection.errors {
                                        let path = fmt_path_reference(
                                            &error.path,
                                            &cwd,
                                            Some(&package_root),
                                        );
                                        diagnostics.push(check_diagnostic_with_package(
                                            check_cli_diagnostic(fmt_io_diagnostic(
                                                "SOURCE_READ_FAILED",
                                                &path,
                                                "could not inspect a source directory entry",
                                                &error.source,
                                            )),
                                            root_identity.as_deref(),
                                        ));
                                        total_errors = total_errors.saturating_add(1);
                                    }
                                    for (_, missing_path) in selection.missing {
                                        let path = fmt_path_reference(
                                            &missing_path,
                                            &cwd,
                                            Some(&package_root),
                                        );
                                        diagnostics.push(check_diagnostic_with_package(
                                    check_cli_diagnostic(fmt_io_diagnostic(
                                        "SOURCE_READ_FAILED",
                                        &path,
                                        "declared source directory disappeared during discovery",
                                        &io::Error::new(
                                            io::ErrorKind::NotFound,
                                            "source directory not found",
                                        ),
                                    )),
                                    root_identity.as_deref(),
                                ));
                                        total_errors = total_errors.saturating_add(1);
                                    }

                                    let mut source_inputs = Vec::new();
                                    let mut module_tree_sources = Vec::new();
                                    for selected in selection.files {
                                        if selected.is_package_manifest
                                            || selected
                                                .path
                                                .extension()
                                                .is_none_or(|extension| extension != "sai")
                                            || !is_visible_check_source_file(
                                                &selected.path,
                                                &source_directory,
                                            )
                                        {
                                            continue;
                                        }
                                        if cancellation.is_cancelled() {
                                            return report_interrupted(parsed, stdout, stderr);
                                        }
                                        let source_display_path = fmt_path_reference(
                                            &selected.path,
                                            &cwd,
                                            Some(&package_root),
                                        );
                                        let source_key_path = package_source_file_key_path(
                                            &selected.path,
                                            &source_directory,
                                            relative_source.as_str(),
                                        )
                                        .unwrap_or_else(|| source_display_path.clone());
                                        let mut file = selected_check_file(
                                            selected,
                                            source_display_path.clone(),
                                        );
                                        match open_selected_fmt_file(&mut file) {
                                            Ok(source_file) => {
                                                match read_file_limited(
                                                    source_file,
                                                    sai_vfs::MAX_SOURCE_BYTES,
                                                    cancellation,
                                                ) {
                                                    Ok(source) => {
                                                        let source: Arc<[u8]> = source.into();
                                                        source_inputs.push((
                                                            source_display_path.clone(),
                                                            Arc::clone(&source),
                                                        ));
                                                        module_tree_sources.push((
                                                            source_key_path,
                                                            source_display_path,
                                                            source,
                                                        ));
                                                    }
                                                    Err(StdinReadError::Interrupted) => {
                                                        return report_interrupted(
                                                            parsed, stdout, stderr,
                                                        );
                                                    }
                                                    Err(StdinReadError::Io(error)) => {
                                                        diagnostics.push(check_diagnostic_with_package(
                                                    check_cli_diagnostic(fmt_io_diagnostic(
                                                        "SOURCE_READ_FAILED",
                                                        &source_display_path,
                                                        "could not read a source file",
                                                        &error,
                                                    )),
                                                    root_identity.as_deref(),
                                                ));
                                                        total_errors =
                                                            total_errors.saturating_add(1);
                                                    }
                                                }
                                            }
                                            Err(error) => {
                                                diagnostics.push(check_diagnostic_with_package(
                                                    check_cli_diagnostic(fmt_io_diagnostic(
                                                        "SOURCE_READ_FAILED",
                                                        &source_display_path,
                                                        "could not open a source file",
                                                        &error,
                                                    )),
                                                    root_identity.as_deref(),
                                                ));
                                                total_errors = total_errors.saturating_add(1);
                                            }
                                        }
                                    }
                                    let declared_targets = manifest_target_names(&manifest);
                                    let target_override = check_target_override(command);
                                    let selected_target = target_override
                                        .clone()
                                        .or_else(|| declared_targets.first().cloned());
                                    if let Some(target) = target_override.as_deref()
                                        && !declared_targets.iter().any(|item| item == target)
                                    {
                                        diagnostics.push(check_diagnostic_with_package(
                                            check_target_not_declared_diagnostic(target),
                                            root_identity.as_deref(),
                                        ));
                                        total_errors = total_errors.saturating_add(1);
                                    }
                                    let target_cfg =
                                        selected_target.as_deref().and_then(|target| {
                                            sai_driver::target_cfg_for_check(target).ok()
                                        });
                                    let tree_result = module_tree_check_diagnostics(
                                        root_package_key.as_deref().expect(
                                            "a syntax-valid package has a stable package key",
                                        ),
                                        relative_source.as_str(),
                                        true,
                                        module_tree_entries,
                                        package_header_span
                                            .as_ref()
                                            .expect("syntax-valid root package has a header"),
                                        &manifest_display_path,
                                        manifest_source
                                            .as_ref()
                                            .expect("syntax-valid manifest has a header source"),
                                        &module_tree_sources,
                                        root_identity
                                            .as_deref()
                                            .expect("a syntax-valid package has an identity"),
                                        include_source_lines,
                                        root_inspection_incomplete,
                                        target_cfg.as_ref(),
                                        command_tail(command)
                                            .iter()
                                            .any(|(argument, _)| argument == "--tests"),
                                    );
                                    push_module_tree_check_result(
                                        &mut diagnostics,
                                        &mut total_errors,
                                        &mut total_warnings,
                                        &mut total_infos,
                                        tree_result,
                                        root_identity.as_deref(),
                                    );
                                    for (source_display_path, source) in source_inputs {
                                        if cancellation.is_cancelled() {
                                            return report_interrupted(parsed, stdout, stderr);
                                        }
                                        match sai_fmt::parse_source(&source) {
                                            Ok(parsed_source) => {
                                                let (source_diagnostics, errors, warnings, infos) =
                                                    check_source_diagnostics(
                                                        &source,
                                                        &source_display_path,
                                                        parsed_source.diagnostics(),
                                                        include_source_lines,
                                                        root_identity.as_deref(),
                                                    );
                                                diagnostics.extend(source_diagnostics);
                                                total_errors = total_errors.saturating_add(errors);
                                                total_warnings =
                                                    total_warnings.saturating_add(warnings);
                                                total_infos = total_infos.saturating_add(infos);
                                            }
                                            Err(sai_fmt::SourceInputError::Refused(refusal)) => {
                                                let batch = source_format_diagnostics(
                                                    &source,
                                                    &source_display_path,
                                                    sai_fmt::SourceFormatError::Refused(refusal),
                                                    parsed.globals.max_errors,
                                                    include_source_lines,
                                                );
                                                total_errors = total_errors
                                                    .saturating_add(batch.total_error_count);
                                                diagnostics.extend(
                                                    batch
                                                        .diagnostics
                                                        .into_iter()
                                                        .map(check_cli_diagnostic)
                                                        .map(|diagnostic| {
                                                            check_diagnostic_with_package(
                                                                diagnostic,
                                                                root_identity.as_deref(),
                                                            )
                                                        }),
                                                );
                                            }
                                            Err(sai_fmt::SourceInputError::InternalError) => {
                                                diagnostics.push(check_diagnostic_with_package(
                                                    check_internal_diagnostic(
                                                        "source parser failed internally",
                                                        None,
                                                    ),
                                                    root_identity.as_deref(),
                                                ));
                                                total_errors = total_errors.saturating_add(1);
                                            }
                                        }
                                    }
                                }
                                Ok(source_directory) => {
                                    if !source_directory.starts_with(&package_root) {
                                        diagnostics.push(check_diagnostic_with_package(
                                    check_cli_diagnostic(fmt_io_diagnostic(
                                        "INVALID_PACKAGE_PATH",
                                        &manifest_display_path,
                                        "declared source directory resolves outside the package",
                                        &io::Error::new(
                                            io::ErrorKind::InvalidInput,
                                            "source directory escapes package root",
                                        ),
                                    )),
                                    root_identity.as_deref(),
                                ));
                                        total_errors = total_errors.saturating_add(1);
                                    } else {
                                        match std::fs::metadata(&source_directory) {
                                            Ok(metadata) if !metadata.is_dir() => {
                                                let tree_result = missing_source_root_module_tree(
                                            root_package_key.as_deref().expect(
                                                "a syntax-valid package has a stable package key",
                                            ),
                                            relative_source.as_str(),
                                            package_header_span
                                                .as_ref()
                                                .expect("syntax-valid root package has a header"),
                                            &manifest_display_path,
                                            manifest_source.as_ref().expect(
                                                "syntax-valid manifest has a header source",
                                            ),
                                            root_identity
                                                .as_deref()
                                                .expect("a syntax-valid package has an identity"),
                                            include_source_lines,
                                        );
                                                push_module_tree_check_result(
                                                    &mut diagnostics,
                                                    &mut total_errors,
                                                    &mut total_warnings,
                                                    &mut total_infos,
                                                    tree_result,
                                                    root_identity.as_deref(),
                                                );
                                            }
                                            Err(error)
                                                if error.kind() == io::ErrorKind::NotFound =>
                                            {
                                                let tree_result = missing_source_root_module_tree(
                                            root_package_key.as_deref().expect(
                                                "a syntax-valid package has a stable package key",
                                            ),
                                            relative_source.as_str(),
                                            package_header_span
                                                .as_ref()
                                                .expect("syntax-valid root package has a header"),
                                            &manifest_display_path,
                                            manifest_source.as_ref().expect(
                                                "syntax-valid manifest has a header source",
                                            ),
                                            root_identity
                                                .as_deref()
                                                .expect("a syntax-valid package has an identity"),
                                            include_source_lines,
                                        );
                                                push_module_tree_check_result(
                                                    &mut diagnostics,
                                                    &mut total_errors,
                                                    &mut total_warnings,
                                                    &mut total_infos,
                                                    tree_result,
                                                    root_identity.as_deref(),
                                                );
                                            }
                                            Ok(_) => {
                                                diagnostics.push(check_diagnostic_with_package(
                                            check_cli_diagnostic(fmt_io_diagnostic(
                                                "SOURCE_READ_FAILED",
                                                &fmt_path_reference(
                                                    &source_directory,
                                                    &cwd,
                                                    Some(&package_root),
                                                ),
                                                "could not inspect the declared source directory",
                                                &io::Error::other(
                                                    "source directory changed during inspection",
                                                ),
                                            )),
                                            root_identity.as_deref(),
                                        ));
                                                total_errors = total_errors.saturating_add(1);
                                            }
                                            Err(error) => {
                                                diagnostics.push(check_diagnostic_with_package(
                                            check_cli_diagnostic(fmt_io_diagnostic(
                                                "SOURCE_READ_FAILED",
                                                &fmt_path_reference(
                                                    &source_directory,
                                                    &cwd,
                                                    Some(&package_root),
                                                ),
                                                "could not inspect the declared source directory",
                                                &error,
                                            )),
                                            root_identity.as_deref(),
                                        ));
                                                total_errors = total_errors.saturating_add(1);
                                            }
                                        }
                                    }
                                }
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                                    ) =>
                                {
                                    let tree_result = missing_source_root_module_tree(
                                        root_package_key.as_deref().expect(
                                            "a syntax-valid package has a stable package key",
                                        ),
                                        relative_source.as_str(),
                                        package_header_span
                                            .as_ref()
                                            .expect("syntax-valid root package has a header"),
                                        &manifest_display_path,
                                        manifest_source
                                            .as_ref()
                                            .expect("syntax-valid manifest has a header source"),
                                        root_identity
                                            .as_deref()
                                            .expect("a syntax-valid package has an identity"),
                                        include_source_lines,
                                    );
                                    push_module_tree_check_result(
                                        &mut diagnostics,
                                        &mut total_errors,
                                        &mut total_warnings,
                                        &mut total_infos,
                                        tree_result,
                                        root_identity.as_deref(),
                                    );
                                }
                                Err(error) => {
                                    diagnostics.push(check_diagnostic_with_package(
                                        check_cli_diagnostic(fmt_io_diagnostic(
                                            "SOURCE_READ_FAILED",
                                            &fmt_path_reference(
                                                &source_path,
                                                &cwd,
                                                Some(&package_root),
                                            ),
                                            "could not inspect the declared source directory",
                                            &error,
                                        )),
                                        root_identity.as_deref(),
                                    ));
                                    total_errors = total_errors.saturating_add(1);
                                }
                            }
                        }
                        Err(error) => {
                            diagnostics.push(check_diagnostic_with_package(
                                check_cli_diagnostic(fmt_io_diagnostic(
                                    "SOURCE_READ_FAILED",
                                    &fmt_path_reference(&source_path, &cwd, Some(&package_root)),
                                    "could not inspect the declared source directory",
                                    &error,
                                )),
                                root_identity.as_deref(),
                            ));
                            total_errors = total_errors.saturating_add(1);
                        }
                    }
                }
                Err(_) => {
                    diagnostics.push(check_diagnostic_with_package(
                        check_cli_diagnostic(fmt_io_diagnostic(
                            "INVALID_PACKAGE_PATH",
                            &manifest_display_path,
                            "invalid declared source directory",
                            &io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "source path is not a valid package-relative path",
                            ),
                        )),
                        root_identity.as_deref(),
                    ));
                    total_errors = total_errors.saturating_add(1);
                }
            }
        }
    }

    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    diagnostics.push(check_incomplete_diagnostic(command));
    total_errors = total_errors.saturating_add(1);
    report_check_diagnostics(
        parsed,
        diagnostics,
        total_errors,
        total_warnings,
        total_infos,
        root_identity.as_deref(),
        stdout,
        stderr,
    )
}

fn selected_check_file(
    selected: crate::fmt_paths::SelectedFmtPath,
    source_path: String,
) -> SelectedFmtFile {
    SelectedFmtFile {
        path: selected.path,
        is_package_manifest: selected.is_package_manifest,
        source_path,
        package_root: None,
        package_root_identities: Vec::new(),
        parent: selected.parent,
        name: selected.name,
        identity: selected.identity,
        identity_guard: selected.identity_guard,
    }
}

fn source_path_contains_symlink(package_root: &Path, source_directory: &str) -> io::Result<bool> {
    if source_directory == "." {
        return Ok(false);
    }

    let mut path = package_root.to_path_buf();
    for component in source_directory.split('/') {
        path.push(component);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            return Ok(true);
        }
        if !metadata.is_dir() {
            return Ok(false);
        }
    }
    Ok(false)
}

fn module_tree_source_root_symlink_diagnostic(
    source_directory: &str,
    package_identity: Option<&str>,
) -> CheckDiagnostic {
    let mut data = ValueObject::new();
    data.insert(
        "path".to_owned(),
        Value::String(source_directory.to_owned()),
    );
    check_diagnostic_with_package(
        check_cli_diagnostic(CliDiagnostic {
            code: "SYMLINK_IN_SOURCE",
            message: "symbolic link in source directory".to_owned(),
            primary: None,
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: Some(source_value_object_json(&data)),
        }),
        package_identity,
    )
}

fn manifest_target_names(manifest: &sai_package::manifest::PackageParseOutput) -> Vec<String> {
    manifest
        .file()
        .declarations
        .iter()
        .find_map(|declaration| {
            if let sai_package::manifest::PackageDeclaration::Targets(targets) = &declaration.value
            {
                Some(
                    targets
                        .iter()
                        .map(|target| target.value.to_string())
                        .collect(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn check_target_override(command: &crate::args::ParsedCommand) -> Option<String> {
    let tail = command_tail(command);
    let mut index = 0;
    while index < tail.len() {
        let argument = tail[index].0.to_str()?;
        if argument == "--" {
            break;
        }
        if let Some(target) = argument.strip_prefix("--target=") {
            return Some(target.to_owned());
        }
        if argument == "--target" {
            return tail.get(index + 1)?.0.to_str().map(str::to_owned);
        }
        index += 1;
    }
    None
}

fn check_target_not_declared_diagnostic(target: &str) -> CheckDiagnostic {
    let mut data = ValueObject::new();
    data.insert("target".to_owned(), Value::String(target.to_owned()));
    check_cli_diagnostic(CliDiagnostic {
        code: "TARGET_NOT_DECLARED",
        message: format!("target `{target}` is not declared by this package"),
        primary: None,
        primary_label: String::new(),
        argument_index: None,
        argument: String::new(),
        help: None,
        data: Some(source_value_object_json(&data)),
    })
}

fn module_tree_check_diagnostics(
    package_key: &str,
    source_directory: &str,
    source_directory_exists: bool,
    entries: Vec<PackageFileEntry>,
    package_header_span: &std::ops::Range<usize>,
    manifest_path: &str,
    manifest_source: &Arc<[u8]>,
    source_inputs: &[(String, String, Arc<[u8]>)],
    package_identity: &str,
    include_source_lines: bool,
    root_inspection_incomplete: bool,
    target_cfg: Option<&TargetCfg>,
    is_test_build: bool,
) -> Result<(Vec<CheckDiagnostic>, usize, usize, usize), String> {
    let package_key_value =
        PackageKey::new(package_key).map_err(|error| format!("invalid package key: {error:?}"))?;
    let source_directory_value = PackageSourceDirectory::new(source_directory.to_owned())
        .map_err(|error| format!("invalid source directory: {error:?}"))?;
    let package_files = PackageFilesSnapshot::new(source_directory_exists, entries)
        .map_err(|error| format!("invalid package source inventory: {error:?}"))?;

    let mut database = RootDatabase::new()
        .map_err(|error| format!("could not initialize query database: {error:?}"))?;
    let package_id = database
        .intern_package(package_key_value.as_str())
        .map_err(|error| format!("could not intern package key: {error:?}"))?;
    let vfs_files = source_inputs
        .iter()
        .filter_map(|(key_path, _, source)| {
            FileKey::new(package_key_value.as_str(), key_path.clone())
                .ok()
                .map(|key| {
                    (
                        key,
                        FileContents::Bytes(Arc::clone(source)),
                        Durability::Low,
                    )
                })
        })
        .collect::<Vec<_>>();
    let input_snapshot = VfsInputSnapshot::new(vfs_files)
        .map_err(|error| format!("invalid package source inputs: {error:?}"))?
        .with_package_files([(package_key_value.clone(), package_files, Durability::Low)])
        .map_err(|error| format!("invalid package source inventory inputs: {error:?}"))?;
    database
        .replace_vfs_snapshot(None, &input_snapshot)
        .map_err(|error| format!("could not set package source inputs: {error:?}"))?;

    let manifest_file_id = database
        .intern_package_file(
            package_id,
            PackageRelativePath::new("sai.package")
                .map_err(|error| format!("invalid package manifest path: {error:?}"))?,
        )
        .map_err(|error| format!("could not intern package manifest: {error:?}"))?;
    let start = u32::try_from(package_header_span.start)
        .map_err(|_| "package header span start exceeds the source limit".to_owned())?;
    let end = u32::try_from(package_header_span.end)
        .map_err(|_| "package header span end exceeds the source limit".to_owned())?;
    let range = sai_diagnostics::TextRange::new(
        sai_diagnostics::TextSize::new(start),
        sai_diagnostics::TextSize::new(end),
    )
    .ok_or_else(|| "package header span is reversed".to_owned())?;
    let package_field = sai_diagnostics::Location {
        file: SourceRef {
            path: manifest_path.to_owned(),
            package: Some(package_identity.to_owned()),
            snapshot: None,
        },
        span: sai_diagnostics::Span::new(manifest_file_id, range),
    };
    let module_tree = database
        .module_tree(package_id, source_directory_value, Some(package_field))
        .map_err(|error| format!("module-tree query failed: {error:?}"))?;

    let mut source_by_key = HashMap::new();
    source_by_key.insert(
        manifest_path.to_owned(),
        (manifest_path.to_owned(), Arc::clone(manifest_source)),
    );
    for (key_path, display_path, source) in source_inputs {
        source_by_key.insert(key_path.clone(), (display_path.clone(), Arc::clone(source)));
    }

    let mut diagnostics = Vec::with_capacity(module_tree.diagnostics.len());
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut infos = 0usize;
    for diagnostic in module_tree.diagnostics.iter() {
        if root_inspection_incomplete && diagnostic.code.as_str() == "MISSING_ROOT_MODULE" {
            continue;
        }
        let mut data = diagnostic.data.clone();
        if diagnostic.code.as_str() == "MISSING_ROOT_MODULE"
            && let Some(Value::String(path)) = data.get_mut("path")
        {
            *path = if source_directory == "." {
                path.clone()
            } else {
                format!("{source_directory}/{path}")
            };
        }
        let help = diagnostic
            .children()
            .iter()
            .find(|child| child.kind == sai_diagnostics::ChildKind::Help)
            .map(|child| child.message.clone());
        let mut cli_diagnostic = if let Some(primary) = diagnostic.primary() {
            let (display_path, source) = source_by_key
                .get(&primary.file.path)
                .cloned()
                .unwrap_or_else(|| (primary.file.path.clone(), Arc::from([])));
            let mut source_index = CliSourceIndex::new(source);
            cli_source_diagnostic(
                &mut source_index,
                FmtDiagnosticSource {
                    path: &display_path,
                    include_source_line: include_source_lines,
                },
                diagnostic.code.as_str(),
                diagnostic.message.clone(),
                primary.span.range.start().to_usize()..primary.span.range.end().to_usize(),
                (!data.is_empty()).then(|| source_value_object_json(&data)),
                help,
            )
        } else {
            CliDiagnostic {
                code: diagnostic.code.as_str(),
                message: diagnostic.message.clone(),
                primary: None,
                primary_label: String::new(),
                argument_index: None,
                argument: String::new(),
                help,
                data: (!data.is_empty()).then(|| source_value_object_json(&data)),
            }
        };
        cli_diagnostic.primary_label = if diagnostic.code.as_str() == "MISSING_ROOT_MODULE" {
            "package".to_owned()
        } else {
            diagnostic.primary_label.clone()
        };
        match diagnostic.severity {
            sai_diagnostics::Severity::Error => errors = errors.saturating_add(1),
            sai_diagnostics::Severity::Warning => warnings = warnings.saturating_add(1),
            sai_diagnostics::Severity::Info => infos = infos.saturating_add(1),
        }
        diagnostics.push(CheckDiagnostic {
            diagnostic: cli_diagnostic,
            severity: diagnostic.severity,
            package: Some(package_identity.to_owned()),
            related: Vec::new(),
            children: check_additional_children(diagnostic, &source_by_key, include_source_lines),
        });
    }

    if let Some(target_cfg) = target_cfg
        && !module_tree.modules.is_empty()
    {
        let mut snapshot_files = Vec::with_capacity(module_tree.modules.len());
        let mut complete_snapshot = true;
        for module in &module_tree.modules {
            let file_key = database
                .file_key(module.file)
                .ok_or_else(|| format!("module-tree file {} is not interned", module.file.raw()))?;
            let Some((_, source)) = source_by_key.get(file_key.path()) else {
                complete_snapshot = false;
                break;
            };
            let source = SourceFile::decode(module.file, source)
                .map_err(|error| format!("could not decode {}: {error:?}", file_key.path()))?;
            snapshot_files.push(SnapshotFile::new(
                Arc::new(source),
                file_key.path().to_owned(),
                Some(package_identity.to_owned()),
                None,
            ));
        }
        if complete_snapshot {
            let workspace_snapshot = WorkspaceSnapshot::new(snapshot_files)
                .map_err(|error| format!("could not create source snapshot: {error:?}"))?;
            let phase1 = database
                .collect_phase1_package_scopes_with_manifest(
                    &module_tree,
                    &workspace_snapshot,
                    target_cfg,
                    sai_driver::Phase1BuildContext {
                        is_root_core_module_build: matches!(
                            target_cfg.target_name(),
                            "wasm32-core" | "wasm64-core"
                        ),
                        is_test_build,
                        analyzes_tests: is_test_build,
                    },
                    Arc::clone(manifest_source),
                    sai_package::manifest::PackageRole::Root,
                )
                .map_err(|error| format!("Phase 1 collection failed: {error:?}"))?;
            let (phase1_diagnostics, phase1_errors, phase1_warnings, phase1_infos) =
                phase1_check_diagnostics(
                    &phase1.diagnostics,
                    &source_by_key,
                    package_identity,
                    include_source_lines,
                );
            diagnostics.extend(phase1_diagnostics);
            errors = errors.saturating_add(phase1_errors);
            warnings = warnings.saturating_add(phase1_warnings);
            infos = infos.saturating_add(phase1_infos);
        }
    }
    Ok((diagnostics, errors, warnings, infos))
}

fn phase1_check_diagnostics(
    parsed: &[sai_diagnostics::Diagnostic],
    source_by_key: &HashMap<String, (String, Arc<[u8]>)>,
    package_identity: &str,
    include_source_lines: bool,
) -> (Vec<CheckDiagnostic>, usize, usize, usize) {
    let mut diagnostics = Vec::with_capacity(parsed.len());
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut infos = 0usize;
    for diagnostic in parsed {
        let primary = diagnostic.primary().map(|location| {
            let (display_path, source) = source_by_key
                .get(&location.file.path)
                .cloned()
                .unwrap_or_else(|| (location.file.path.clone(), Arc::from([])));
            let mut source_index = CliSourceIndex::new(source);
            source_index.location(
                &display_path,
                location.span.range.start().to_usize()..location.span.range.end().to_usize(),
                include_source_lines,
            )
        });
        let mut cli_diagnostic = CliDiagnostic {
            code: diagnostic.code.as_str(),
            message: diagnostic.message.clone(),
            primary,
            primary_label: diagnostic.primary_label.clone(),
            argument_index: None,
            argument: String::new(),
            help: diagnostic
                .children()
                .iter()
                .find(|child| child.kind == sai_diagnostics::ChildKind::Help)
                .map(|child| child.message.clone()),
            data: (!diagnostic.data.is_empty()).then(|| source_value_object_json(&diagnostic.data)),
        };
        if cli_diagnostic.primary.is_none() {
            cli_diagnostic.primary_label.clear();
        }
        let related = diagnostic
            .related()
            .iter()
            .filter_map(|related| {
                let sai_diagnostics::Related::DeclaredHere { location, what } = related else {
                    return None;
                };
                let (display_path, source) = source_by_key
                    .get(&location.file.path)
                    .cloned()
                    .unwrap_or_else(|| (location.file.path.clone(), Arc::from([])));
                let mut source_index = CliSourceIndex::new(source);
                Some(CheckRelated {
                    location: source_index.location(
                        &display_path,
                        location.span.range.start().to_usize()
                            ..location.span.range.end().to_usize(),
                        include_source_lines,
                    ),
                    what: what.clone(),
                })
            })
            .collect();
        let children = check_additional_children(diagnostic, source_by_key, include_source_lines);
        match diagnostic.severity {
            sai_diagnostics::Severity::Error => errors = errors.saturating_add(1),
            sai_diagnostics::Severity::Warning => warnings = warnings.saturating_add(1),
            sai_diagnostics::Severity::Info => infos = infos.saturating_add(1),
        }
        diagnostics.push(CheckDiagnostic {
            diagnostic: cli_diagnostic,
            severity: diagnostic.severity,
            package: Some(package_identity.to_owned()),
            related,
            children,
        });
    }
    (diagnostics, errors, warnings, infos)
}

fn check_additional_children(
    diagnostic: &sai_diagnostics::Diagnostic,
    source_by_key: &HashMap<String, (String, Arc<[u8]>)>,
    include_source_lines: bool,
) -> Vec<CheckChild> {
    let mut first_help_is_primary = false;
    diagnostic
        .children()
        .iter()
        .filter(|child| {
            if child.kind != sai_diagnostics::ChildKind::Help {
                return true;
            }
            if first_help_is_primary {
                true
            } else {
                first_help_is_primary = true;
                false
            }
        })
        .map(|child| {
            let location = child.location.as_ref().map(|location| {
                let (display_path, source) = source_by_key
                    .get(&location.file.path)
                    .cloned()
                    .unwrap_or_else(|| (location.file.path.clone(), Arc::from([])));
                let mut source_index = CliSourceIndex::new(source);
                source_index.location(
                    &display_path,
                    location.span.range.start().to_usize()..location.span.range.end().to_usize(),
                    include_source_lines,
                )
            });
            CheckChild {
                kind: child.kind,
                message: child.message.clone(),
                location,
            }
        })
        .collect()
}

fn missing_source_root_module_tree(
    package_key: &str,
    source_directory: &str,
    package_header_span: &std::ops::Range<usize>,
    manifest_path: &str,
    manifest_source: &Arc<[u8]>,
    package_identity: &str,
    include_source_lines: bool,
) -> Result<(Vec<CheckDiagnostic>, usize, usize, usize), String> {
    module_tree_check_diagnostics(
        package_key,
        source_directory,
        false,
        Vec::new(),
        package_header_span,
        manifest_path,
        manifest_source,
        &[],
        package_identity,
        include_source_lines,
        false,
        None,
        false,
    )
}

fn push_module_tree_check_result(
    diagnostics: &mut Vec<CheckDiagnostic>,
    total_error_count: &mut usize,
    total_warning_count: &mut usize,
    total_info_count: &mut usize,
    result: Result<(Vec<CheckDiagnostic>, usize, usize, usize), String>,
    package_identity: Option<&str>,
) {
    match result {
        Ok((tree_diagnostics, errors, warnings, infos)) => {
            diagnostics.extend(tree_diagnostics);
            *total_error_count = total_error_count.saturating_add(errors);
            *total_warning_count = total_warning_count.saturating_add(warnings);
            *total_info_count = total_info_count.saturating_add(infos);
        }
        Err(error) => {
            diagnostics.push(check_diagnostic_with_package(
                check_internal_diagnostic(
                    &format!("module-tree integration failed: {error}"),
                    None,
                ),
                package_identity,
            ));
            *total_error_count = total_error_count.saturating_add(1);
        }
    }
}

fn package_source_file_key_path(
    file_path: &Path,
    source_directory: &Path,
    declared_source_directory: &str,
) -> Option<String> {
    let relative_path = file_path.strip_prefix(source_directory).ok()?.to_str()?;
    let relative_path = relative_path.replace(std::path::MAIN_SEPARATOR, "/");
    Some(if declared_source_directory == "." {
        relative_path
    } else {
        format!("{declared_source_directory}/{relative_path}")
    })
}

fn is_visible_check_source_file(path: &Path, source_directory: &Path) -> bool {
    let Ok(relative_path) = path.strip_prefix(source_directory) else {
        return false;
    };
    !relative_path
        .components()
        .any(|component| component.as_os_str().as_encoded_bytes().starts_with(b"."))
}

fn package_identity(parsed: &sai_package::manifest::PackageParseOutput) -> Option<String> {
    parsed
        .file()
        .header
        .as_ref()
        .map(|header| format!("{}@{}", header.value.identity, header.value.version))
}

fn package_manifest_check_diagnostics(
    source: &Arc<[u8]>,
    source_path: &str,
    parsed: &sai_package::manifest::PackageParseOutput,
    include_source_lines: bool,
) -> (Vec<CheckDiagnostic>, usize, usize) {
    let mut source_index = CliSourceIndex::new(Arc::clone(source));
    let package_identity = package_identity(parsed);
    let mut diagnostics = Vec::new();
    for error in parsed.lexical_errors() {
        let (code, message) = package_lex_error_fields(error.kind);
        diagnostics.push(CheckDiagnostic {
            diagnostic: cli_source_diagnostic(
                &mut source_index,
                FmtDiagnosticSource {
                    path: source_path,
                    include_source_line: include_source_lines,
                },
                code,
                message.to_owned(),
                error.span.clone(),
                None,
                None,
            ),
            severity: sai_diagnostics::Severity::Error,
            package: package_identity.clone(),
            related: Vec::new(),
            children: Vec::new(),
        });
    }
    for error in parsed.errors() {
        let (code, message, data) = package_parse_error_fields(&error.kind);
        diagnostics.push(CheckDiagnostic {
            diagnostic: cli_source_diagnostic(
                &mut source_index,
                FmtDiagnosticSource {
                    path: source_path,
                    include_source_line: include_source_lines,
                },
                code,
                message.into_owned(),
                error.span.clone(),
                data,
                None,
            ),
            severity: sai_diagnostics::Severity::Error,
            package: package_identity.clone(),
            related: Vec::new(),
            children: Vec::new(),
        });
    }
    for warning in parsed.warnings() {
        let (code, message) = match warning.kind {
            sai_package::manifest::PackageParseWarningKind::DuplicateListItem => {
                ("DUPLICATE_LIST_ITEM", "duplicate list item")
            }
        };
        diagnostics.push(CheckDiagnostic {
            diagnostic: cli_source_diagnostic(
                &mut source_index,
                FmtDiagnosticSource {
                    path: source_path,
                    include_source_line: include_source_lines,
                },
                code,
                message.to_owned(),
                warning.span.clone(),
                None,
                None,
            ),
            severity: sai_diagnostics::Severity::Warning,
            package: package_identity.clone(),
            related: Vec::new(),
            children: Vec::new(),
        });
    }
    (
        diagnostics,
        parsed
            .lexical_error_count()
            .saturating_add(parsed.error_count()),
        parsed.warnings().len(),
    )
}

fn check_source_diagnostics(
    source: &Arc<[u8]>,
    source_path: &str,
    parsed: &[sai_diagnostics::Diagnostic],
    include_source_lines: bool,
    package_identity: Option<&str>,
) -> (Vec<CheckDiagnostic>, usize, usize, usize) {
    let decoded_source = sai_vfs::SourceFile::decode(sai_vfs::FileId::new(0), source).ok();
    let mut source_index = CliSourceIndex::new(Arc::clone(source));
    let mut diagnostics = Vec::with_capacity(parsed.len());
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut infos = 0usize;
    for diagnostic in parsed {
        let primary = diagnostic.primary();
        let mut cli_diagnostic = if let Some(primary) = primary {
            let span = decoded_source_range_to_original(
                decoded_source.as_ref(),
                primary.span.range.start().to_usize()..primary.span.range.end().to_usize(),
            );
            cli_source_diagnostic(
                &mut source_index,
                FmtDiagnosticSource {
                    path: source_path,
                    include_source_line: include_source_lines,
                },
                diagnostic.code.as_str(),
                diagnostic.message.clone(),
                span,
                (!diagnostic.data.is_empty()).then(|| source_value_object_json(&diagnostic.data)),
                diagnostic
                    .children()
                    .iter()
                    .find(|child| child.kind == sai_diagnostics::ChildKind::Help)
                    .map(|child| child.message.clone()),
            )
        } else {
            CliDiagnostic {
                code: diagnostic.code.as_str(),
                message: diagnostic.message.clone(),
                primary: None,
                primary_label: String::new(),
                argument_index: None,
                argument: String::new(),
                help: diagnostic
                    .children()
                    .iter()
                    .find(|child| child.kind == sai_diagnostics::ChildKind::Help)
                    .map(|child| child.message.clone()),
                data: (!diagnostic.data.is_empty())
                    .then(|| source_value_object_json(&diagnostic.data)),
            }
        };
        cli_diagnostic.primary_label = diagnostic.primary_label.clone();
        let mut first_help_is_primary = false;
        let children = diagnostic
            .children()
            .iter()
            .filter(|child| {
                if child.kind != sai_diagnostics::ChildKind::Help {
                    return true;
                }
                if first_help_is_primary {
                    true
                } else {
                    first_help_is_primary = true;
                    false
                }
            })
            .map(|child| {
                let location = child.location.as_ref().map(|location| {
                    let range = decoded_source_range_to_original(
                        decoded_source.as_ref(),
                        location.span.range.start().to_usize()
                            ..location.span.range.end().to_usize(),
                    );
                    source_index.location(source_path, range, include_source_lines)
                });
                CheckChild {
                    kind: child.kind,
                    message: child.message.clone(),
                    location,
                }
            })
            .collect();
        match diagnostic.severity {
            sai_diagnostics::Severity::Error => errors = errors.saturating_add(1),
            sai_diagnostics::Severity::Warning => warnings = warnings.saturating_add(1),
            sai_diagnostics::Severity::Info => infos = infos.saturating_add(1),
        }
        diagnostics.push(CheckDiagnostic {
            diagnostic: cli_diagnostic,
            severity: diagnostic.severity,
            package: package_identity.map(str::to_owned),
            related: Vec::new(),
            children,
        });
    }
    (diagnostics, errors, warnings, infos)
}

fn check_cli_diagnostic(diagnostic: CliDiagnostic) -> CheckDiagnostic {
    CheckDiagnostic {
        diagnostic,
        severity: sai_diagnostics::Severity::Error,
        package: None,
        related: Vec::new(),
        children: Vec::new(),
    }
}

fn check_diagnostic_with_package(
    mut diagnostic: CheckDiagnostic,
    package_identity: Option<&str>,
) -> CheckDiagnostic {
    diagnostic.package = package_identity.map(str::to_owned);
    diagnostic
}

fn check_internal_diagnostic(description: &str, primary: Option<CliLocation>) -> CheckDiagnostic {
    check_cli_diagnostic(internal_fmt_diagnostic(description.to_owned(), primary))
}

fn check_incomplete_diagnostic(command: &crate::args::ParsedCommand) -> CheckDiagnostic {
    let mut unsupported = vec![
        "import and use resolution",
        "type checking",
        "dependency and workspace analysis",
        "lints",
    ];
    let tail = command_tail(command);
    if tail.iter().any(|(argument, _)| argument == "--tests") {
        unsupported.push("test-only analysis");
    }
    if tail.iter().any(|(argument, _)| argument == "--fix") {
        unsupported.push("`--fix`");
    }
    let feature = "sai check";
    check_cli_diagnostic(CliDiagnostic {
        code: "FEATURE_NOT_IMPLEMENTED",
        message: format!("stage 0 feature `{feature}` is not implemented"),
        primary: None,
        primary_label: String::new(),
        argument_index: None,
        argument: String::new(),
        help: Some(format!(
            "root-package syntax parsing, module-tree validation, target filtering, Phase 1 declarations, and core export-name checks are implemented; {} remain unavailable",
            unsupported.join(", ")
        )),
        data: Some(format!(
            "{{\"stage\":0,\"feature\":\"{}\"}}",
            escape_json(feature)
        )),
    })
}

fn report_check_diagnostics(
    parsed: &ParsedArgs,
    mut diagnostics: Vec<CheckDiagnostic>,
    total_error_count: usize,
    total_warning_count: usize,
    total_info_count: usize,
    root_identity: Option<&str>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    diagnostics.sort_by(compare_check_diagnostics);
    let mut shown_error_count = 0usize;
    let diagnostics = diagnostics
        .into_iter()
        .filter(|diagnostic| {
            if diagnostic.severity != sai_diagnostics::Severity::Error {
                return true;
            }
            if parsed.globals.max_errors != 0 && shown_error_count >= parsed.globals.max_errors {
                return false;
            }
            shown_error_count = shown_error_count.saturating_add(1);
            true
        })
        .collect::<Vec<_>>();
    let exit_code = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.diagnostic.code == "INTERNAL_COMPILER_ERROR")
    {
        101
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.diagnostic.code == "NO_PACKAGE_FOUND")
    {
        2
    } else {
        1
    };
    match parsed.globals.format {
        OutputFormat::Human => {
            let mut rendered = String::new();
            for diagnostic in &diagnostics {
                let code = sai_diagnostics::Code::from_name(diagnostic.diagnostic.code)
                    .unwrap_or(UNREGISTERED_CODE_PLACEHOLDER);
                let mut shared = shared_check_diagnostic(diagnostic, code);
                if code == UNREGISTERED_CODE_PLACEHOLDER {
                    shared.code = UNREGISTERED_CODE_PLACEHOLDER;
                }
                let snippets = CliSnippets {
                    diagnostic: &diagnostic.diagnostic,
                    related: &diagnostic.related,
                    children: &diagnostic.children,
                };
                let text = sai_diagnostics::render_diagnostic(
                    &shared,
                    &snippets,
                    color_enabled(parsed.globals.color, io::stderr().is_terminal()),
                );
                if code == UNREGISTERED_CODE_PLACEHOLDER {
                    rendered.push_str(&text.replacen(
                        UNREGISTERED_CODE_PLACEHOLDER.as_str(),
                        diagnostic.diagnostic.code,
                        1,
                    ));
                } else {
                    rendered.push_str(&text);
                }
            }
            if !parsed.globals.quiet {
                rendered.push_str(&sai_diagnostics::render_summary(
                    "check",
                    sai_diagnostics::SummaryCounts {
                        errors: total_error_count,
                        warnings: total_warning_count,
                        infos: total_info_count,
                        hidden_errors: total_error_count.saturating_sub(shown_error_count),
                        max_errors: parsed.globals.max_errors,
                    },
                    color_enabled(parsed.globals.color, io::stderr().is_terminal()),
                ));
            }
            match stderr.write_all(rendered.as_bytes()) {
                Ok(()) => exit_code,
                Err(error) => report_stderr_failure(error),
            }
        }
        OutputFormat::Json => {
            let result = (|| -> io::Result<()> {
                write_json_check_header(stdout, root_identity)?;
                write_json_check_diagnostic_records(stdout, &diagnostics)?;
                writeln!(
                    stdout,
                    "{{\"type\":\"summary\",\"errors\":{},\"warnings\":{},\"infos\":{},\"shown_errors\":{},\"suppressed\":{{\"cascade\":0,\"duplicate\":0,\"dependency\":0}},\"exit_code\":{exit_code}}}",
                    total_error_count, total_warning_count, total_info_count, shown_error_count
                )
            })();
            match result {
                Ok(()) => exit_code,
                Err(error) => {
                    report_stdout_failure(stderr, error);
                    4
                }
            }
        }
    }
}

fn compare_check_diagnostics(left: &CheckDiagnostic, right: &CheckDiagnostic) -> Ordering {
    let location_order = match (
        left.diagnostic.primary.as_ref(),
        right.diagnostic.primary.as_ref(),
    ) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => check_location_order(left, right),
    };
    location_order
        .then_with(|| check_severity_rank(left.severity).cmp(&check_severity_rank(right.severity)))
        .then_with(|| {
            left.diagnostic
                .code
                .as_bytes()
                .cmp(right.diagnostic.code.as_bytes())
        })
        .then_with(|| {
            left.diagnostic
                .message
                .as_bytes()
                .cmp(right.diagnostic.message.as_bytes())
        })
        .then_with(|| {
            check_diagnostic_record_bytes(left).cmp(&check_diagnostic_record_bytes(right))
        })
}

fn check_location_order(left: &CliLocation, right: &CliLocation) -> Ordering {
    let file_class = |path: &str| match path {
        "sai.package" => 0,
        "sai.lock" => 1,
        _ => 2,
    };
    let left_class = file_class(&left.path);
    let right_class = file_class(&right.path);
    left_class
        .cmp(&right_class)
        .then_with(|| {
            if left_class == 2 && right_class == 2 {
                left.path.as_bytes().cmp(right.path.as_bytes())
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| left.start.byte.cmp(&right.start.byte))
        .then_with(|| left.end.byte.cmp(&right.end.byte))
}

fn check_severity_rank(severity: sai_diagnostics::Severity) -> u8 {
    match severity {
        sai_diagnostics::Severity::Error => 0,
        sai_diagnostics::Severity::Warning => 1,
        sai_diagnostics::Severity::Info => 2,
    }
}

fn check_diagnostic_record_bytes(diagnostic: &CheckDiagnostic) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_json_check_diagnostic_records(&mut bytes, std::slice::from_ref(diagnostic))
        .expect("writing a check diagnostic to a byte vector cannot fail");
    bytes
}

fn write_json_check_diagnostic_records(
    writer: &mut impl Write,
    diagnostics: &[CheckDiagnostic],
) -> io::Result<()> {
    for check_diagnostic in diagnostics {
        let diagnostic = &check_diagnostic.diagnostic;
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
        let mut child_records = Vec::new();
        if let Some(help) = &diagnostic.help {
            child_records.push(format!(
                "{{\"kind\":\"help\",\"message\":\"{}\",\"location\":null}}",
                escape_json(help)
            ));
        }
        child_records.extend(check_diagnostic.children.iter().map(|child| {
            let kind = match child.kind {
                sai_diagnostics::ChildKind::Note => "note",
                sai_diagnostics::ChildKind::Help => "help",
            };
            let location = child.location.as_ref().map_or_else(
                || "null".to_owned(),
                |location| {
                    json_location_with_package(location, check_diagnostic.package.as_deref())
                },
            );
            format!(
                "{{\"kind\":\"{kind}\",\"message\":\"{}\",\"location\":{location}}}",
                escape_json(&child.message)
            )
        }));
        let children = format!("[{}]", child_records.join(","));
        let primary = diagnostic.primary.as_ref().map_or_else(
            || "null".to_owned(),
            |location| json_location_with_package(location, check_diagnostic.package.as_deref()),
        );
        let package = check_diagnostic.package.as_deref().map_or_else(
            || "null".to_owned(),
            |identity| format!("\"{}\"", escape_json(identity)),
        );
        let severity = match check_diagnostic.severity {
            sai_diagnostics::Severity::Error => "error",
            sai_diagnostics::Severity::Warning => "warning",
            sai_diagnostics::Severity::Info => "info",
        };
        let related = format!(
            "[{}]",
            check_diagnostic
                .related
                .iter()
                .map(|related| format!(
                    "{{\"kind\":\"declared_here\",\"location\":{},\"what\":\"{}\"}}",
                    json_location_with_package(
                        &related.location,
                        check_diagnostic.package.as_deref(),
                    ),
                    escape_json(&related.what),
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        writeln!(
            writer,
            "{{\"type\":\"diagnostic\",\"code\":\"{}\",\"severity\":\"{severity}\",\"level\":null,\"message\":\"{}\",\"primary\":{},\"primary_label\":\"{}\",\"labels\":[],\"children\":{},\"fixes\":[],\"related\":{related},\"data\":{},\"node\":null,\"symbol\":null,\"package\":{package}}}",
            diagnostic.code,
            escape_json(&diagnostic.message),
            primary,
            escape_json(&diagnostic.primary_label),
            children,
            data
        )?;
    }
    Ok(())
}

fn run_fmt(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    cancellation: &CancellationToken,
    stdin: &mut impl Read,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    stdout_is_terminal: bool,
) -> i32 {
    let mut usage_errors = parsed.usage_errors.clone();
    usage_errors.extend(validate_known_command_options(command));
    if !usage_errors.is_empty() {
        return report_usage_errors(parsed, "fmt", usage_errors, stdout, stderr);
    }

    let positionals = collect_command_positionals(command, &None);
    if positionals.len() != 1 || positionals[0].0 != "-" {
        if positionals.is_empty() {
            let mut retry_state = FmtPathRetryState {
                remaining_retries: 8,
                waiting_reported: false,
            };
            return run_fmt_workspace(
                parsed,
                command,
                cancellation,
                stdout,
                stderr,
                &mut retry_state,
            );
        }
        let mut retry_state = FmtPathRetryState {
            remaining_retries: 8,
            waiting_reported: false,
        };
        return run_fmt_paths(
            parsed,
            command,
            &positionals,
            cancellation,
            stdout,
            stderr,
            &mut retry_state,
        );
    }

    let tail = command_tail(command);
    let check = tail.iter().any(|(argument, _)| argument == "--check");
    let mut stdin_kind = "source";
    let mut index = 0;
    while index < tail.len() {
        let argument = tail[index].0.to_string_lossy();
        if let Some(value) = argument.strip_prefix("--stdin-kind=") {
            stdin_kind = if value == "package" {
                "package"
            } else {
                "source"
            };
        } else if argument == "--stdin-kind" {
            if let Some((value, _)) = tail.get(index + 1) {
                stdin_kind = if value == "package" {
                    "package"
                } else {
                    "source"
                };
                index += 1;
            }
        }
        index += 1;
    }
    if stdin_kind == "source" {
        return run_fmt_source_stdin(
            parsed,
            cancellation,
            stdin,
            stdout,
            stderr,
            stdout_is_terminal,
            check,
        );
    }

    let source = match read_stdin_limited(
        stdin,
        sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES,
        cancellation,
    ) {
        Ok(source) => source,
        Err(StdinReadError::Interrupted) => {
            return report_interrupted(parsed, stdout, stderr);
        }
        Err(StdinReadError::Io(error)) => {
            let diagnostic = CliDiagnostic {
                code: "SOURCE_READ_FAILED",
                message: "could not read standard input".to_owned(),
                primary: None,
                primary_label: String::new(),
                argument_index: None,
                argument: String::new(),
                help: Some(error.to_string()),
                data: Some(format!(
                    "{{\"path\":\"<stdin>\",\"reason\":\"{}\"}}",
                    escape_json(&error.to_string())
                )),
            };
            return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
        }
    };
    let source: Arc<[u8]> = source.into();

    let max_errors = if parsed.globals.max_errors == 0 {
        usize::MAX
    } else {
        parsed.globals.max_errors
    };
    let formatting_result = sai_fmt::format_package_with_error_limit(&source, max_errors);
    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    let formatted = match formatting_result {
        Ok(formatted) => formatted,
        Err(error) => {
            let refused = matches!(error, sai_fmt::PackageFormatError::Refused(_));
            let diagnostics = package_format_diagnostics(
                &source,
                "<stdin>",
                error,
                parsed.globals.max_errors,
                parsed.globals.format == OutputFormat::Human,
            );
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
            return report_fmt_diagnostics(
                parsed,
                diagnostics.diagnostics,
                diagnostics.total_error_count,
                refused.then_some(FmtResult {
                    check,
                    files: 1,
                    refused: refused.then(|| "<stdin>".to_owned()).into_iter().collect(),
                    ..FmtResult::default()
                }),
                stdout,
                stderr,
            );
        }
    };
    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    let differs = source.as_ref() != formatted.as_bytes();
    if check && differs {
        return report_fmt_diagnostics(
            parsed,
            vec![unformatted_file_diagnostic(
                &source,
                formatted.as_bytes(),
                "<stdin>",
                parsed.globals.format == OutputFormat::Human,
            )],
            1,
            Some(FmtResult {
                check,
                files: 1,
                changed: vec!["<stdin>".to_owned()],
                ..FmtResult::default()
            }),
            stdout,
            stderr,
        );
    }

    match parsed.globals.format {
        OutputFormat::Human => {
            if check {
                0
            } else if stdout_is_terminal {
                let safe_output = escape_terminal_controls(&formatted);
                write_stdout_text(stdout, &safe_output, stderr)
            } else {
                write_stdout_text(stdout, &formatted, stderr)
            }
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_fmt_success(
                stdout,
                FmtResult {
                    check,
                    files: 1,
                    changed: differs.then(|| "<stdin>".to_owned()).into_iter().collect(),
                    output: (!check).then_some(formatted),
                    ..FmtResult::default()
                },
            ) {
                report_stdout_failure(stderr, error);
                4
            } else {
                0
            }
        }
    }
}

fn run_fmt_source_stdin(
    parsed: &ParsedArgs,
    cancellation: &CancellationToken,
    stdin: &mut impl Read,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    stdout_is_terminal: bool,
    check: bool,
) -> i32 {
    let source = match read_source_limited(stdin, sai_vfs::MAX_SOURCE_BYTES, cancellation) {
        Ok(source) => source,
        Err(SourceInputReadError::Interrupted) => {
            return report_interrupted(parsed, stdout, stderr);
        }
        Err(SourceInputReadError::Io(error)) => {
            let diagnostic = CliDiagnostic {
                code: "SOURCE_READ_FAILED",
                message: "could not read standard input".to_owned(),
                primary: None,
                primary_label: String::new(),
                argument_index: None,
                argument: String::new(),
                help: Some(error.to_string()),
                data: Some(format!(
                    "{{\"path\":\"<stdin>\",\"reason\":\"{}\"}}",
                    escape_json(&error.to_string())
                )),
            };
            return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
        }
        Err(SourceInputReadError::TooLarge(byte_len)) => {
            let diagnostic = source_limit_diagnostic(byte_len, "<stdin>");
            return report_fmt_diagnostics(
                parsed,
                vec![diagnostic],
                1,
                Some(FmtResult {
                    check,
                    files: 1,
                    refused: vec!["<stdin>".to_owned()],
                    ..FmtResult::default()
                }),
                stdout,
                stderr,
            );
        }
    };
    let source: Arc<[u8]> = source.into();
    let formatted = match sai_fmt::format_source(&source) {
        Ok(formatted) => formatted,
        Err(error) => {
            let result = match &error {
                sai_fmt::SourceFormatError::Refused(_) => Some(FmtResult {
                    check,
                    files: 1,
                    refused: vec!["<stdin>".to_owned()],
                    ..FmtResult::default()
                }),
                sai_fmt::SourceFormatError::UnsupportedShape => Some(FmtResult {
                    check,
                    files: 1,
                    ..FmtResult::default()
                }),
                _ => None,
            };
            let batch = source_format_diagnostics(
                &source,
                "<stdin>",
                error,
                parsed.globals.max_errors,
                parsed.globals.format == OutputFormat::Human,
            );
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
            return report_fmt_diagnostics(
                parsed,
                batch.diagnostics,
                batch.total_error_count,
                result,
                stdout,
                stderr,
            );
        }
    };
    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    let differs = source.as_ref() != formatted.as_bytes();
    if check && differs {
        return report_fmt_diagnostics(
            parsed,
            vec![unformatted_file_diagnostic(
                &source,
                formatted.as_bytes(),
                "<stdin>",
                parsed.globals.format == OutputFormat::Human,
            )],
            1,
            Some(FmtResult {
                check,
                files: 1,
                changed: vec!["<stdin>".to_owned()],
                ..FmtResult::default()
            }),
            stdout,
            stderr,
        );
    }
    match parsed.globals.format {
        OutputFormat::Human => {
            if check {
                0
            } else if stdout_is_terminal {
                let safe_output = escape_terminal_controls(&formatted);
                write_stdout_text(stdout, &safe_output, stderr)
            } else {
                write_stdout_text(stdout, &formatted, stderr)
            }
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_fmt_success(
                stdout,
                FmtResult {
                    check,
                    files: 1,
                    changed: differs.then(|| "<stdin>".to_owned()).into_iter().collect(),
                    output: (!check).then_some(formatted),
                    ..FmtResult::default()
                },
            ) {
                report_stdout_failure(stderr, error);
                4
            } else {
                0
            }
        }
    }
}

fn source_format_diagnostics(
    source: &Arc<[u8]>,
    source_path: &str,
    error: sai_fmt::SourceFormatError,
    max_errors: usize,
    include_source_lines: bool,
) -> FmtDiagnosticBatch {
    match error {
        sai_fmt::SourceFormatError::Refused(refusal) => {
            let total_error_count = refusal
                .input_errors
                .len()
                .saturating_add(refusal.lexical_errors.len())
                .saturating_add(refusal.syntax_errors.len());
            let mut selected = BinaryHeap::new();
            let mut order = 0usize;
            for diagnostic in refusal
                .input_errors
                .into_iter()
                .chain(refusal.lexical_errors)
                .chain(refusal.syntax_errors)
            {
                let span = diagnostic.primary().map_or(0..0, |location| {
                    location.span.range.start().to_usize()..location.span.range.end().to_usize()
                });
                let data = (!diagnostic.data.is_empty())
                    .then(|| source_value_object_json(&diagnostic.data));
                retain_pending_fmt_diagnostic(
                    &mut selected,
                    PendingFmtDiagnostic {
                        code: diagnostic.code.as_str(),
                        message: Cow::Owned(diagnostic.message),
                        span,
                        help: None,
                        data,
                        order,
                    },
                    max_errors,
                );
                order = order.saturating_add(1);
            }
            let decoded_source = sai_vfs::SourceFile::decode(sai_vfs::FileId::new(0), source).ok();
            let mut source_index = CliSourceIndex::new(Arc::clone(source));
            let diagnostics = selected
                .into_sorted_vec()
                .into_iter()
                .map(|diagnostic| {
                    let span =
                        decoded_source_range_to_original(decoded_source.as_ref(), diagnostic.span);
                    cli_source_diagnostic(
                        &mut source_index,
                        FmtDiagnosticSource {
                            path: source_path,
                            include_source_line: include_source_lines,
                        },
                        diagnostic.code,
                        diagnostic.message.into_owned(),
                        span,
                        diagnostic.data,
                        diagnostic.help,
                    )
                })
                .collect();
            FmtDiagnosticBatch {
                diagnostics,
                total_error_count,
            }
        }
        sai_fmt::SourceFormatError::UnsupportedShape => FmtDiagnosticBatch {
            diagnostics: vec![fmt_unsupported_source_shape_diagnostic(
                source,
                source_path,
                include_source_lines,
            )],
            total_error_count: 1,
        },
        sai_fmt::SourceFormatError::VerificationFailed(failure) => {
            let check = match failure.check {
                sai_fmt::SourceVerificationCheck::OutputSyntax => "output does not parse",
                sai_fmt::SourceVerificationCheck::SyntaxTreePreservation => "syntax tree changed",
                sai_fmt::SourceVerificationCheck::CommentPreservation => "comments changed",
                sai_fmt::SourceVerificationCheck::Idempotency => "formatting is not idempotent",
            };
            let mut source_index = CliSourceIndex::new(Arc::clone(source));
            let primary = failure.input_byte_offset.map(|offset| {
                source_index.location(source_path, offset..offset, include_source_lines)
            });
            FmtDiagnosticBatch {
                diagnostics: vec![internal_fmt_diagnostic(
                    format!("formatter verification failed: {check}"),
                    primary,
                )],
                total_error_count: 1,
            }
        }
        sai_fmt::SourceFormatError::InternalError => FmtDiagnosticBatch {
            diagnostics: vec![internal_fmt_diagnostic(
                "source formatter could not render input".to_owned(),
                None,
            )],
            total_error_count: 1,
        },
        sai_fmt::SourceFormatError::Print(error) => FmtDiagnosticBatch {
            diagnostics: vec![internal_fmt_diagnostic(
                format!("source formatter printer failed: {error:?}"),
                None,
            )],
            total_error_count: 1,
        },
    }
}

fn decoded_source_range_to_original(
    decoded_source: Option<&sai_vfs::SourceFile>,
    decoded_span: std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let Some(decoded_source) = decoded_source else {
        return decoded_span;
    };
    let Some(start) = sai_vfs::TextSize::from_usize(decoded_span.start)
        .and_then(|offset| decoded_source.to_original_offset(offset))
    else {
        return decoded_span;
    };
    let Some(end) = sai_vfs::TextSize::from_usize(decoded_span.end)
        .and_then(|offset| decoded_source.to_original_offset(offset))
    else {
        return decoded_span;
    };
    start.to_usize()..end.to_usize()
}

fn fmt_unsupported_source_shape_diagnostic(
    source: &Arc<[u8]>,
    source_path: &str,
    include_source_lines: bool,
) -> CliDiagnostic {
    let mut source_index = CliSourceIndex::new(Arc::clone(source));
    CliDiagnostic {
        code: "FEATURE_NOT_IMPLEMENTED",
        message: "this SAI source syntax is not supported yet".to_owned(),
        primary: Some(source_index.location(source_path, 0..0, include_source_lines)),
        primary_label: String::new(),
        argument_index: None,
        argument: source_path.to_owned(),
        help: None,
        data: Some("{\"stage\":0,\"feature\":\"sai fmt source syntax\"}".to_owned()),
    }
}

fn source_value_object_json(object: &ValueObject) -> String {
    let members = object
        .iter()
        .map(|(key, value)| format!("\"{}\":{}", escape_json(key), source_value_json(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{members}}}")
}

fn source_value_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::String(value) => format!("\"{}\"", escape_json(value)),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(source_value_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => source_value_object_json(object),
    }
}

struct SelectedFmtFile {
    path: PathBuf,
    is_package_manifest: bool,
    source_path: String,
    package_root: Option<PathBuf>,
    package_root_identities: Vec<sai_windows_fs::FileIdentity>,
    parent: Option<Arc<cap_std::fs::Dir>>,
    name: OsString,
    identity: sai_windows_fs::FileIdentity,
    identity_guard: Option<std::fs::File>,
}

#[derive(Clone, Debug)]
struct FmtPackageRoot {
    path: PathBuf,
    identity: sai_windows_fs::FileIdentity,
    _directory: Option<Arc<cap_std::fs::Dir>>,
}

#[derive(Debug, PartialEq, Eq)]
struct FmtPathSelectionSignature {
    files: Vec<(
        PathBuf,
        sai_windows_fs::FileIdentity,
        sai_windows_fs::FileIdentity,
    )>,
    resolved_manifest_paths: Vec<(PathBuf, PathBuf)>,
    missing: Vec<(usize, PathBuf)>,
    errors: Vec<(PathBuf, io::ErrorKind, Option<i32>)>,
}

struct FmtPathRetryState {
    remaining_retries: usize,
    waiting_reported: bool,
}

#[derive(Debug, Eq, PartialEq)]
struct FmtWorkspaceSignature {
    root: Option<PathBuf>,
    input_paths: Vec<PathBuf>,
    manifests: Vec<crate::fmt_workspace::FmtManifestSnapshot>,
    source_directories: Vec<crate::fmt_workspace::FmtWorkspaceSourceDirectory>,
    errors: Vec<(PathBuf, io::ErrorKind, Option<i32>)>,
    total_error_count: usize,
}

struct FmtPathRunInput<'a> {
    positionals: &'a [(OsString, usize)],
    input_paths: &'a [PathBuf],
    initial_errors: Vec<crate::fmt_paths::FmtPathSelectionError>,
    initial_diagnostics: Vec<CliDiagnostic>,
    initial_error_count: usize,
    workspace_discovery_signature: Option<&'a FmtWorkspaceSignature>,
    workspace_start_directory: Option<&'a Path>,
}

struct FmtWorkspaceLocation<'a> {
    start_directory: &'a Path,
    cwd: &'a Path,
}

struct FmtRetryRequest<'a> {
    positionals: &'a [(OsString, usize)],
    workspace_start_directory: Option<&'a Path>,
    cwd: &'a Path,
}

fn fmt_error_limit(max_errors: usize) -> usize {
    if max_errors == 0 {
        usize::MAX
    } else {
        max_errors
    }
}

fn fmt_workspace_signature(
    discovery: &crate::fmt_workspace::FmtWorkspaceDiscovery,
) -> FmtWorkspaceSignature {
    FmtWorkspaceSignature {
        root: discovery.workspace_root.clone(),
        input_paths: discovery.input_paths.clone(),
        manifests: discovery.manifest_snapshots.clone(),
        source_directories: discovery.source_directories.clone(),
        errors: discovery
            .errors
            .iter()
            .map(|error| {
                (
                    error.path.clone(),
                    error.source.kind(),
                    error.source.raw_os_error(),
                )
            })
            .collect(),
        total_error_count: discovery.total_error_count,
    }
}

fn fmt_workspace_diagnostics(
    discovery: &crate::fmt_workspace::FmtWorkspaceDiscovery,
    cwd: &Path,
    diagnostic_base: &Path,
    include_source_lines: bool,
) -> Vec<CliDiagnostic> {
    let mut issues = discovery.diagnostics.iter().collect::<Vec<_>>();
    issues.sort_by(|left, right| {
        left.manifest_path
            .as_deref()
            .unwrap_or(&left.display_path)
            .as_os_str()
            .as_encoded_bytes()
            .cmp(
                right
                    .manifest_path
                    .as_deref()
                    .unwrap_or(&right.display_path)
                    .as_os_str()
                    .as_encoded_bytes(),
            )
            .then_with(|| {
                left.span
                    .as_ref()
                    .map_or(usize::MAX, |span| span.start)
                    .cmp(&right.span.as_ref().map_or(usize::MAX, |span| span.start))
            })
    });
    let mut current_source: Option<(PathBuf, Option<CliSourceIndex>)> = None;
    let mut diagnostics = Vec::with_capacity(issues.len());
    for issue in issues {
        let source_path = fmt_path_reference(&issue.display_path, cwd, Some(diagnostic_base));
        if let Some(manifest_path) = issue.manifest_path.as_ref()
            && current_source
                .as_ref()
                .is_none_or(|(path, _)| path != manifest_path)
        {
            let source_index = discovery
                .manifest_snapshots
                .iter()
                .find(|snapshot| snapshot.path == *manifest_path && !snapshot.oversized)
                .and_then(|snapshot| {
                    let expected_digest = snapshot.digest?;
                    let source = read_fmt_manifest_for_diagnostic(manifest_path).ok()?;
                    let digest: [u8; 32] = Sha256::digest(&source).into();
                    (digest == expected_digest).then(|| CliSourceIndex::new(Arc::from(source)))
                });
            current_source = Some((manifest_path.clone(), source_index));
        }
        let source_index = issue.manifest_path.as_ref().and_then(|manifest_path| {
            current_source
                .as_mut()
                .filter(|(path, _)| path == manifest_path)
                .and_then(|(_, source)| source.as_mut())
        });
        if let (Some(source_index), Some(span)) = (source_index, issue.span.clone()) {
            diagnostics.push(cli_source_diagnostic(
                source_index,
                FmtDiagnosticSource {
                    path: &source_path,
                    include_source_line: include_source_lines,
                },
                issue.code,
                issue.message.clone(),
                span,
                None,
                None,
            ));
        } else {
            diagnostics.push(fmt_io_diagnostic(
                issue.code,
                &source_path,
                &issue.message,
                &io::Error::other("workspace discovery may be incomplete"),
            ));
        }
    }
    diagnostics
}

fn read_fmt_manifest_for_diagnostic(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut file = file.take(
        sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES
            .saturating_add(1)
            .try_into()
            .unwrap_or(u64::MAX),
    );
    let mut source =
        Vec::with_capacity(sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES.min(8192));
    file.read_to_end(&mut source)?;
    Ok(source)
}

fn fmt_workspace_diagnostic_base<'a>(
    workspace_root: &'a Path,
    cwd: &'a Path,
    format: OutputFormat,
) -> &'a Path {
    if format == OutputFormat::Human && cwd.starts_with(workspace_root) {
        cwd
    } else {
        workspace_root
    }
}

fn fmt_package_roots_for_input(
    path: &Path,
    is_package_manifest: bool,
    workspace_discovery_signature: Option<&FmtWorkspaceSignature>,
    cwd: &Path,
) -> Result<Vec<PathBuf>, crate::package_discovery::PackageDiscoveryError> {
    let mut workspace_roots = workspace_discovery_signature
        .into_iter()
        .flat_map(|signature| signature.manifests.iter())
        .filter(|manifest| is_package_manifest && manifest.path == path)
        .filter_map(|manifest| manifest.package_directory.clone())
        .collect::<Vec<_>>();
    if is_package_manifest && workspace_discovery_signature.is_some() && workspace_roots.is_empty()
    {
        return Err(
            crate::package_discovery::PackageDiscoveryError::ManifestMetadataIo {
                path: path.to_path_buf(),
                source: io::Error::other("workspace manifest changed during discovery"),
            },
        );
    }
    if is_package_manifest && workspace_discovery_signature.is_some() {
        match crate::package_discovery::find_root_package(path.parent().unwrap_or(path), cwd) {
            Ok(root) => workspace_roots.push(root),
            Err(crate::package_discovery::PackageDiscoveryError::NoPackageFound { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    if !is_package_manifest {
        workspace_roots.extend(
            workspace_discovery_signature
                .into_iter()
                .flat_map(|signature| signature.source_directories.iter())
                .filter(|source| path.starts_with(&source.directory))
                .map(|source| source.package_directory.clone()),
        );
        if workspace_discovery_signature.is_some() {
            match crate::package_discovery::find_root_package(path.parent().unwrap_or(path), cwd) {
                Ok(root) => workspace_roots.push(root),
                Err(crate::package_discovery::PackageDiscoveryError::NoPackageFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }
    workspace_roots.sort_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    });
    workspace_roots.dedup();
    if !workspace_roots.is_empty() {
        return Ok(workspace_roots);
    }
    crate::package_discovery::find_root_package(path.parent().unwrap_or(path), cwd)
        .map(|root| vec![root])
}

fn fmt_workspace_display_path(
    path: &Path,
    is_package_manifest: bool,
    workspace_discovery_signature: Option<&FmtWorkspaceSignature>,
) -> PathBuf {
    if is_package_manifest {
        if let Some(package_directory) = workspace_discovery_signature
            .into_iter()
            .flat_map(|signature| signature.manifests.iter())
            .filter(|manifest| manifest.path == path)
            .filter_map(|manifest| manifest.package_directory.as_ref())
            .min_by(|left, right| {
                left.as_os_str()
                    .as_encoded_bytes()
                    .cmp(right.as_os_str().as_encoded_bytes())
            })
        {
            return package_directory.join("sai.package");
        }
        if let Some(root) =
            workspace_discovery_signature.and_then(|signature| signature.root.as_ref())
        {
            return root.join("sai.package");
        }
    }
    path.to_path_buf()
}

fn fmt_workspace_selection_error_path(
    path: &Path,
    workspace_discovery_signature: &FmtWorkspaceSignature,
) -> PathBuf {
    let manifest_owners = workspace_discovery_signature
        .manifests
        .iter()
        .filter(|manifest| manifest.path == path)
        .filter_map(|manifest| manifest.package_directory.as_ref())
        .collect::<Vec<_>>();
    if let Some(owner) = manifest_owners.into_iter().min_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    }) {
        return owner.join("sai.package");
    }
    if workspace_discovery_signature
        .root
        .as_deref()
        .is_some_and(|root| !path.starts_with(root))
    {
        return workspace_discovery_signature
            .root
            .as_deref()
            .expect("checked workspace root")
            .join("sai.package");
    }
    path.to_path_buf()
}

fn open_fmt_package_root(
    path: &Path,
    selected_parent: Option<(&Path, &Arc<cap_std::fs::Dir>, sai_windows_fs::FileIdentity)>,
) -> io::Result<FmtPackageRoot> {
    let path = std::fs::canonicalize(path)?;
    if let Some((parent_path, directory, identity)) = selected_parent {
        if path == parent_path {
            return Ok(FmtPackageRoot {
                path: path.to_path_buf(),
                identity,
                _directory: Some(Arc::clone(directory)),
            });
        }
    }
    let directory = sai_windows_fs::open_directory_path_without_following(&path)?;
    let identity = sai_windows_fs::file_identity(&directory)?;
    Ok(FmtPackageRoot {
        path: path.to_path_buf(),
        identity,
        _directory: Some(Arc::new(cap_std::fs::Dir::from_std_file(directory))),
    })
}

fn inspect_fmt_package_root(path: &Path) -> io::Result<FmtPackageRoot> {
    let path = std::fs::canonicalize(path)?;
    let directory = sai_windows_fs::open_directory_path_without_following(&path)?;
    let identity = sai_windows_fs::file_identity(&directory)?;
    Ok(FmtPackageRoot {
        path: path.to_path_buf(),
        identity,
        _directory: None,
    })
}

fn normalize_fmt_package_roots(mut roots: Vec<FmtPackageRoot>) -> Vec<FmtPackageRoot> {
    roots.sort_by(|left, right| {
        left.path
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.path.as_os_str().as_encoded_bytes())
    });
    let mut identities = HashSet::new();
    roots.retain(|root| identities.insert(root.identity));
    roots
}

fn fmt_package_root_signature(
    roots: &[FmtPackageRoot],
) -> Vec<(PathBuf, sai_windows_fs::FileIdentity)> {
    roots
        .iter()
        .map(|root| (root.path.clone(), root.identity))
        .collect()
}

fn fmt_path_selection_signature(
    selection: &crate::fmt_paths::FmtPathSelection,
) -> FmtPathSelectionSignature {
    FmtPathSelectionSignature {
        files: selection
            .files
            .iter()
            .map(|file| (file.path.clone(), file.identity, file.parent_identity))
            .collect(),
        resolved_manifest_paths: selection.resolved_manifest_paths.clone(),
        missing: selection.missing.clone(),
        errors: selection
            .errors
            .iter()
            .map(|error| {
                (
                    error.path.clone(),
                    error.source.kind(),
                    error.source.raw_os_error(),
                )
            })
            .collect(),
    }
}

fn fmt_workspace_manifest_resolutions_match(
    selection: &crate::fmt_paths::FmtPathSelection,
    workspace_discovery_signature: &FmtWorkspaceSignature,
) -> bool {
    let mut expected = workspace_discovery_signature
        .manifests
        .iter()
        .filter(|manifest| manifest.package_directory.is_some())
        .map(|manifest| (manifest.logical_path.clone(), manifest.path.clone()))
        .collect::<Vec<_>>();
    let mut actual = selection.resolved_manifest_paths.clone();
    let path_order = |left: &PathBuf, right: &PathBuf| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    };
    let resolution_order = |left: &(PathBuf, PathBuf), right: &(PathBuf, PathBuf)| {
        path_order(&left.0, &right.0).then_with(|| path_order(&left.1, &right.1))
    };
    expected.sort_by(&resolution_order);
    actual.sort_by(resolution_order);
    expected == actual
}

fn fmt_workspace_manifest_source_matches_discovery(
    path: &Path,
    source: &[u8],
    workspace_discovery_signature: &FmtWorkspaceSignature,
) -> bool {
    let snapshots = workspace_discovery_signature
        .manifests
        .iter()
        .filter(|manifest| manifest.path == path && manifest.package_directory.is_some())
        .collect::<Vec<_>>();
    if snapshots.is_empty() {
        return false;
    }
    let source_digest: [u8; 32] = Sha256::digest(source).into();
    snapshots.iter().all(|snapshot| {
        !snapshot.oversized
            && snapshot
                .digest
                .is_some_and(|digest| digest == source_digest)
    })
}

fn open_selected_fmt_file(file: &mut SelectedFmtFile) -> io::Result<cap_std::fs::File> {
    if let Some(opened_file) = file.identity_guard.as_ref() {
        if sai_windows_fs::file_identity(opened_file)? != file.identity {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "formatter input identity changed after selection",
            ));
        }
        return Ok(cap_std::fs::File::from_std(opened_file.try_clone()?));
    }
    let parent = file.parent.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "formatter input directory capability was not retained",
        )
    })?;
    let parent_handle = parent.try_clone()?.into_std_file();
    let parent_path = file.path.parent().unwrap_or(&file.path);
    let opened_file =
        sai_windows_fs::open_file_without_following(&parent_handle, parent_path, &file.name)?;
    if sai_windows_fs::file_identity(&opened_file)? != file.identity {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "formatter input identity changed after selection",
        ));
    }
    Ok(cap_std::fs::File::from_std(opened_file))
}

fn run_fmt_workspace(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    retry_state: &mut FmtPathRetryState,
) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let diagnostic = fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                ".",
                "could not resolve the current directory",
                &error,
            );
            return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
        }
    };
    let start_directory = parsed.globals.directory.as_deref().map_or_else(
        || cwd.clone(),
        |directory| {
            if directory.is_absolute() {
                directory.to_path_buf()
            } else {
                cwd.join(directory)
            }
        },
    );
    run_fmt_workspace_at(
        parsed,
        command,
        FmtWorkspaceLocation {
            start_directory: &start_directory,
            cwd: &cwd,
        },
        cancellation,
        stdout,
        stderr,
        retry_state,
    )
}

fn run_fmt_workspace_at(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    location: FmtWorkspaceLocation<'_>,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    retry_state: &mut FmtPathRetryState,
) -> i32 {
    let FmtWorkspaceLocation {
        start_directory,
        cwd,
    } = location;
    let workspace_root = match crate::package_discovery::find_root_package(start_directory, cwd) {
        Ok(root) => root,
        Err(crate::package_discovery::PackageDiscoveryError::NoPackageFound {
            start_directory,
        }) => {
            return report_fmt_no_package_found(parsed, &start_directory, stdout, stderr);
        }
        Err(error) => {
            let source_path = fmt_path_reference(start_directory, cwd, None);
            let diagnostic = fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                &source_path,
                "could not discover the formatter workspace",
                &io::Error::other(error),
            );
            return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
        }
    };
    let error_limit = fmt_error_limit(parsed.globals.max_errors);
    let diagnostic_base =
        fmt_workspace_diagnostic_base(&workspace_root, cwd, parsed.globals.format);
    let mut discovery = crate::fmt_workspace::discover_fmt_workspace_with_error_limit_and_base(
        &workspace_root,
        cancellation,
        error_limit,
        diagnostic_base,
    );
    if discovery.interrupted || cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    let input_paths = discovery.input_paths.clone();
    let signature = fmt_workspace_signature(&discovery);
    let initial_error_count = discovery.total_error_count;
    let initial_errors = std::mem::take(&mut discovery.errors);
    let initial_diagnostics = fmt_workspace_diagnostics(
        &discovery,
        cwd,
        diagnostic_base,
        parsed.globals.format == OutputFormat::Human,
    );
    run_fmt_paths_for_inputs(
        parsed,
        command,
        FmtPathRunInput {
            positionals: &[],
            input_paths: &input_paths,
            initial_errors,
            initial_diagnostics,
            initial_error_count,
            workspace_discovery_signature: Some(&signature),
            workspace_start_directory: Some(start_directory),
        },
        cancellation,
        stdout,
        stderr,
        retry_state,
    )
}

fn report_fmt_no_package_found(
    parsed: &ParsedArgs,
    start_directory: &Path,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let diagnostic = CliDiagnostic {
        code: "NO_PACKAGE_FOUND",
        message: format!(
            "no package found from start directory `{}`",
            start_directory.display()
        ),
        primary: None,
        primary_label: String::new(),
        argument_index: None,
        argument: String::new(),
        help: Some("run `sai new` to create a package".to_owned()),
        data: None,
    };
    match parsed.globals.format {
        OutputFormat::Human => {
            if let Err(error) = write_human_diagnostics(
                stderr,
                "fmt",
                std::slice::from_ref(&diagnostic),
                !parsed.globals.quiet,
                color_enabled(parsed.globals.color, io::stderr().is_terminal()),
                1,
                parsed.globals.max_errors,
            ) {
                report_stderr_failure(error)
            } else {
                2
            }
        }
        OutputFormat::Json => {
            if let Err(error) = write_json_fmt_diagnostics(stdout, &[diagnostic], 1, 2, None) {
                report_stdout_failure(stderr, error);
                4
            } else {
                2
            }
        }
    }
}

fn retry_fmt_run(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    request: FmtRetryRequest<'_>,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    retry_state: &mut FmtPathRetryState,
) -> i32 {
    let FmtRetryRequest {
        positionals,
        workspace_start_directory,
        cwd,
    } = request;
    if let Some(start_directory) = workspace_start_directory {
        run_fmt_workspace_at(
            parsed,
            command,
            FmtWorkspaceLocation {
                start_directory,
                cwd,
            },
            cancellation,
            stdout,
            stderr,
            retry_state,
        )
    } else {
        run_fmt_paths(
            parsed,
            command,
            positionals,
            cancellation,
            stdout,
            stderr,
            retry_state,
        )
    }
}

fn run_fmt_paths(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    positionals: &[(OsString, usize)],
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    retry_state: &mut FmtPathRetryState,
) -> i32 {
    let input_paths = positionals
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    run_fmt_paths_for_inputs(
        parsed,
        command,
        FmtPathRunInput {
            positionals,
            input_paths: &input_paths,
            initial_errors: Vec::new(),
            initial_diagnostics: Vec::new(),
            initial_error_count: 0,
            workspace_discovery_signature: None,
            workspace_start_directory: None,
        },
        cancellation,
        stdout,
        stderr,
        retry_state,
    )
}

fn run_fmt_paths_for_inputs(
    parsed: &ParsedArgs,
    command: &crate::args::ParsedCommand,
    run_input: FmtPathRunInput<'_>,
    cancellation: &CancellationToken,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    retry_state: &mut FmtPathRetryState,
) -> i32 {
    let FmtPathRunInput {
        positionals,
        input_paths,
        initial_errors,
        initial_diagnostics,
        initial_error_count,
        workspace_discovery_signature,
        workspace_start_directory,
    } = run_input;
    let check = command_tail(command)
        .iter()
        .any(|(argument, _)| argument == "--check");
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let diagnostic = fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                ".",
                "could not resolve the current directory",
                &error,
            );
            return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
        }
    };
    let workspace_path_base = workspace_discovery_signature
        .and_then(|signature| signature.root.as_deref())
        .map(|root| fmt_workspace_diagnostic_base(root, &cwd, parsed.globals.format));
    let mut selection = if workspace_discovery_signature.is_some() {
        crate::fmt_paths::select_fmt_workspace_paths(input_paths, &cwd, cancellation)
    } else {
        crate::fmt_paths::select_fmt_paths(input_paths, &cwd, cancellation)
    };
    if selection.interrupted {
        return report_interrupted(parsed, stdout, stderr);
    }
    let selection_signature = fmt_path_selection_signature(&selection);
    let selected_error_count = selection.errors.len();
    selection.errors.extend(initial_errors);
    let mut total_error_count = initial_error_count.saturating_add(selected_error_count);
    let mut diagnostics = initial_diagnostics;
    let mut result = FmtResult {
        check,
        files: selection.files.len(),
        ..FmtResult::default()
    };
    let mut human_changed_paths = Vec::new();

    for (input_index, missing_path) in selection.missing {
        if let Some((argument, command_index)) = positionals.get(input_index) {
            let argument_index = command
                .argument_indices
                .get(*command_index)
                .map_or(0, |index| index + 1);
            diagnostics.push(CliDiagnostic {
                code: "PATH_NOT_FOUND",
                message: format!("path `{}` does not exist", argument.to_string_lossy()),
                primary: None,
                primary_label: String::new(),
                argument_index: Some(argument_index),
                argument: argument.to_string_lossy().into_owned(),
                help: None,
                data: None,
            });
        } else {
            let source_path = fmt_run_path_reference(&missing_path, &cwd, workspace_path_base);
            diagnostics.push(fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                &source_path,
                "a discovered formatter input no longer exists",
                &io::Error::new(io::ErrorKind::NotFound, "input path not found"),
            ));
        }
        total_error_count = total_error_count.saturating_add(1);
    }
    for error in selection.errors {
        let display_path = workspace_discovery_signature
            .map(|signature| fmt_workspace_selection_error_path(&error.path, signature))
            .unwrap_or_else(|| error.path.clone());
        let source_path = fmt_run_path_reference(&display_path, &cwd, workspace_path_base);
        diagnostics.push(fmt_io_diagnostic(
            "SOURCE_READ_FAILED",
            &source_path,
            "could not inspect a formatter input path",
            &error.source,
        ));
    }

    let mut files = Vec::with_capacity(selection.files.len());
    let mut roots_by_path = HashMap::<PathBuf, FmtPackageRoot>::new();
    let mut blocked_files = HashSet::new();
    for selected in selection.files {
        if cancellation.is_cancelled() {
            return report_interrupted(parsed, stdout, stderr);
        }
        let crate::fmt_paths::SelectedFmtPath {
            path,
            is_package_manifest,
            parent,
            name,
            identity,
            identity_guard,
            parent_identity,
        } = selected;
        let start_directory = path.parent().unwrap_or(&path);
        let discovered_package_roots = fmt_package_roots_for_input(
            &path,
            is_package_manifest,
            workspace_discovery_signature,
            &cwd,
        );
        let (package_root, package_root_identities) = match discovered_package_roots {
            Ok(roots) => {
                let mut package_root = None;
                let mut package_root_identities = Vec::with_capacity(roots.len());
                for root in roots {
                    let inspected_root = if let Some(root) = roots_by_path.get(&root) {
                        Some(root.clone())
                    } else {
                        let selected_parent = (start_directory == root)
                            .then(|| {
                                parent
                                    .as_ref()
                                    .map(|directory| (start_directory, directory, parent_identity))
                            })
                            .flatten();
                        match open_fmt_package_root(&root, selected_parent) {
                            Ok(root_capability) => {
                                roots_by_path.insert(root.clone(), root_capability.clone());
                                Some(root_capability)
                            }
                            Err(error) => {
                                let display_path = fmt_workspace_display_path(
                                    &path,
                                    is_package_manifest,
                                    workspace_discovery_signature,
                                );
                                let source_path = fmt_run_path_reference(
                                    &display_path,
                                    &cwd,
                                    workspace_path_base,
                                );
                                diagnostics.push(fmt_io_diagnostic(
                                    "WORKSPACE_LOCK_FAILED",
                                    &source_path,
                                    "could not identify the package lock for a formatter input",
                                    &error,
                                ));
                                total_error_count = total_error_count.saturating_add(1);
                                blocked_files.insert(path.clone());
                                None
                            }
                        }
                    };
                    if let Some(inspected_root) = inspected_root {
                        package_root.get_or_insert_with(|| root.clone());
                        package_root_identities.push(inspected_root.identity);
                    } else {
                        break;
                    }
                }
                (package_root, package_root_identities)
            }
            Err(crate::package_discovery::PackageDiscoveryError::NoPackageFound { .. }) => {
                (None, Vec::new())
            }
            Err(_error) => {
                let display_path = fmt_workspace_display_path(
                    &path,
                    is_package_manifest,
                    workspace_discovery_signature,
                );
                let source_path = fmt_run_path_reference(&display_path, &cwd, workspace_path_base);
                diagnostics.push(fmt_io_diagnostic(
                    "WORKSPACE_LOCK_FAILED",
                    &source_path,
                    "could not identify the package lock for a formatter input",
                    &io::Error::other("package discovery failed"),
                ));
                total_error_count = total_error_count.saturating_add(1);
                blocked_files.insert(path.clone());
                (None, Vec::new())
            }
        };
        files.push(SelectedFmtFile {
            path,
            is_package_manifest,
            source_path: String::new(),
            package_root,
            package_root_identities,
            parent,
            name,
            identity,
            identity_guard,
        });
    }
    let roots = normalize_fmt_package_roots(roots_by_path.into_values().collect());
    let locked_roots = fmt_package_root_signature(&roots);
    let workspace_root = workspace_discovery_signature
        .and_then(|signature| signature.root.clone())
        .or_else(|| crate::package_discovery::find_root_package(&cwd, &cwd).ok());
    for file in &mut files {
        let owning_manifest_directory = (workspace_discovery_signature.is_some()
            && file.is_package_manifest)
            .then(|| {
                fmt_workspace_display_path(&file.path, true, workspace_discovery_signature)
                    .parent()
                    .map(Path::to_path_buf)
            })
            .flatten();
        let display_path = owning_manifest_directory
            .as_ref()
            .map(|directory| directory.join("sai.package"))
            .unwrap_or_else(|| file.path.clone());
        let source_root = workspace_path_base
            .or(owning_manifest_directory.as_deref())
            .or_else(|| {
                workspace_root
                    .as_deref()
                    .filter(|root| display_path.starts_with(root))
            })
            .or_else(|| {
                (roots.len() == 1)
                    .then_some(file.package_root.as_deref())
                    .flatten()
            });
        file.source_path = fmt_path_reference(&display_path, &cwd, source_root);
    }
    files.sort_by(|left, right| {
        left.path
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.path.as_os_str().as_encoded_bytes())
    });

    let lock_mode = if check {
        WorkspaceLockMode::Shared
    } else {
        WorkspaceLockMode::Exclusive
    };
    let mut held_locks = Vec::with_capacity(roots.len());
    let mut failed_roots = HashSet::new();
    for root in &roots {
        loop {
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
            match WorkspaceLockGuard::try_acquire(&root.path, lock_mode) {
                Ok(Some(guard)) => {
                    held_locks.push(guard);
                    break;
                }
                Ok(None) => {
                    if !retry_state.waiting_reported
                        && parsed.globals.format == OutputFormat::Human
                        && !parsed.globals.quiet
                    {
                        if let Err(error) = stderr.write_all(b"waiting for .sai/lock\n") {
                            return report_stderr_failure(error);
                        }
                        retry_state.waiting_reported = true;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_error) => {
                    let logical_workspace_manifest = workspace_discovery_signature
                        .filter(|signature| {
                            signature.root.as_deref().is_some_and(|workspace_root| {
                                !root.path.starts_with(workspace_root)
                            })
                        })
                        .and_then(|signature| {
                            files
                                .iter()
                                .find(|file| {
                                    file.is_package_manifest
                                        && file.package_root_identities.contains(&root.identity)
                                })
                                .map(|file| {
                                    fmt_workspace_display_path(&file.path, true, Some(signature))
                                })
                                .or_else(|| {
                                    signature
                                        .root
                                        .as_deref()
                                        .map(|workspace_root| workspace_root.join("sai.package"))
                                })
                        });
                    let package_path =
                        logical_workspace_manifest.unwrap_or_else(|| root.path.join("sai.package"));
                    let package_path =
                        fmt_run_path_reference(&package_path, &cwd, workspace_path_base);
                    diagnostics.push(fmt_io_diagnostic(
                        "WORKSPACE_LOCK_FAILED",
                        &package_path,
                        "could not acquire the package workspace lock",
                        &io::Error::other("workspace lock is unavailable"),
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                    failed_roots.insert(root.identity);
                    break;
                }
            }
        }
    }

    let current_selection = if workspace_discovery_signature.is_some() {
        crate::fmt_paths::select_fmt_workspace_paths_for_comparison(input_paths, &cwd, cancellation)
    } else {
        crate::fmt_paths::select_fmt_paths_for_comparison(input_paths, &cwd, cancellation)
    };
    if current_selection.interrupted || cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }
    let mut current_roots_by_path = HashMap::new();
    let mut current_discovery_failed = workspace_discovery_signature.is_some_and(|signature| {
        !fmt_workspace_manifest_resolutions_match(&current_selection, signature)
    });
    for selected in &current_selection.files {
        let path = &selected.path;
        match fmt_package_roots_for_input(
            path,
            selected.is_package_manifest,
            workspace_discovery_signature,
            &cwd,
        ) {
            Ok(roots) => {
                for root in roots {
                    match current_roots_by_path.entry(root) {
                        std::collections::hash_map::Entry::Occupied(_) => {}
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            match inspect_fmt_package_root(entry.key()) {
                                Ok(root_capability) => {
                                    entry.insert(root_capability);
                                }
                                Err(_) => current_discovery_failed = true,
                            }
                        }
                    }
                }
            }
            Err(crate::package_discovery::PackageDiscoveryError::NoPackageFound { .. }) => {}
            Err(_) => current_discovery_failed = true,
        }
    }
    let current_roots = normalize_fmt_package_roots(current_roots_by_path.into_values().collect());
    if current_discovery_failed
        || fmt_package_root_signature(&current_roots) != locked_roots
        || fmt_path_selection_signature(&current_selection) != selection_signature
    {
        drop(files);
        drop(current_selection);
        drop(held_locks);
        if retry_state.remaining_retries > 0 {
            retry_state.remaining_retries -= 1;
            return retry_fmt_run(
                parsed,
                command,
                FmtRetryRequest {
                    positionals,
                    workspace_start_directory,
                    cwd: &cwd,
                },
                cancellation,
                stdout,
                stderr,
                retry_state,
            );
        }
        let diagnostic = fmt_io_diagnostic(
            "SOURCE_READ_FAILED",
            ".",
            "formatter inputs kept changing while package locks were acquired",
            &io::Error::new(
                io::ErrorKind::WouldBlock,
                "formatter input set did not stabilize",
            ),
        );
        return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
    }
    drop(current_selection);

    if let (Some(expected), Some(start_directory)) =
        (workspace_discovery_signature, workspace_start_directory)
    {
        let current_discovery =
            match crate::package_discovery::find_root_package(start_directory, &cwd) {
                Ok(root) => {
                    let diagnostic_base =
                        fmt_workspace_diagnostic_base(&root, &cwd, parsed.globals.format);
                    crate::fmt_workspace::discover_fmt_workspace_with_error_limit_and_base(
                        &root,
                        cancellation,
                        fmt_error_limit(parsed.globals.max_errors),
                        diagnostic_base,
                    )
                }
                Err(_) => crate::fmt_workspace::FmtWorkspaceDiscovery::default(),
            };
        if cancellation.is_cancelled() || current_discovery.interrupted {
            return report_interrupted(parsed, stdout, stderr);
        }
        if fmt_workspace_signature(&current_discovery) != *expected {
            drop(files);
            drop(held_locks);
            if retry_state.remaining_retries > 0 {
                retry_state.remaining_retries -= 1;
                return retry_fmt_run(
                    parsed,
                    command,
                    FmtRetryRequest {
                        positionals,
                        workspace_start_directory,
                        cwd: &cwd,
                    },
                    cancellation,
                    stdout,
                    stderr,
                    retry_state,
                );
            }
            let diagnostic = fmt_io_diagnostic(
                "SOURCE_READ_FAILED",
                ".",
                "formatter workspace kept changing during discovery",
                &io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "formatter workspace did not stabilize",
                ),
            );
            return report_fmt_diagnostics(parsed, vec![diagnostic], 1, None, stdout, stderr);
        }
    }

    let max_errors = if parsed.globals.max_errors == 0 {
        usize::MAX
    } else {
        parsed.globals.max_errors
    };
    for mut file in files {
        if cancellation.is_cancelled() {
            return report_interrupted(parsed, stdout, stderr);
        }
        if blocked_files.contains(&file.path)
            || file
                .package_root_identities
                .iter()
                .any(|identity| failed_roots.contains(identity))
        {
            result.refused.push(file.source_path);
            continue;
        }
        if file.is_package_manifest {
            let source_file = match open_selected_fmt_file(&mut file) {
                Ok(file) => file,
                Err(error) => {
                    diagnostics.push(fmt_io_diagnostic(
                        "SOURCE_READ_FAILED",
                        &file.source_path,
                        "could not open formatter input",
                        &error,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                    continue;
                }
            };
            let permissions = match source_file.metadata() {
                Ok(metadata) => metadata.permissions(),
                Err(error) => {
                    diagnostics.push(fmt_io_diagnostic(
                        "SOURCE_READ_FAILED",
                        &file.source_path,
                        "could not inspect formatter input permissions",
                        &error,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                    continue;
                }
            };
            let source = match read_file_limited(
                source_file,
                sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES,
                cancellation,
            ) {
                Ok(source) => source,
                Err(StdinReadError::Interrupted) => {
                    return report_interrupted(parsed, stdout, stderr);
                }
                Err(StdinReadError::Io(error)) => {
                    diagnostics.push(fmt_io_diagnostic(
                        "SOURCE_READ_FAILED",
                        &file.source_path,
                        "could not read formatter input",
                        &error,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                    continue;
                }
            };
            let source: Arc<[u8]> = source.into();
            if let Some(signature) = workspace_discovery_signature
                && !fmt_workspace_manifest_source_matches_discovery(&file.path, &source, signature)
            {
                result.refused.push(file.source_path.clone());
                diagnostics.push(fmt_io_diagnostic(
                    "SOURCE_READ_FAILED",
                    &file.source_path,
                    "workspace manifest changed after discovery; refusing to format it",
                    &io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "workspace manifest does not match its discovery snapshot",
                    ),
                ));
                total_error_count = total_error_count.saturating_add(1);
                continue;
            }
            let formatting_result = sai_fmt::format_package_with_error_limit(&source, max_errors);
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
            let formatted = match formatting_result {
                Ok(formatted) => formatted,
                Err(error) => {
                    let refused = matches!(error, sai_fmt::PackageFormatError::Refused(_));
                    if refused {
                        result.refused.push(file.source_path.clone());
                    }
                    let batch = package_format_diagnostics(
                        &source,
                        &file.source_path,
                        error,
                        parsed.globals.max_errors,
                        parsed.globals.format == OutputFormat::Human,
                    );
                    total_error_count = total_error_count.saturating_add(batch.total_error_count);
                    diagnostics.extend(batch.diagnostics);
                    continue;
                }
            };
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
            let differs = source.as_ref() != formatted.as_bytes();
            if differs {
                if !check {
                    if cancellation.is_cancelled() {
                        return report_interrupted(parsed, stdout, stderr);
                    }
                    match write_file_atomically_in_directory(
                        file.parent
                            .as_deref()
                            .expect("package manifest retains its parent directory"),
                        &file.name,
                        file.identity,
                        source.as_ref(),
                        permissions,
                        formatted.as_bytes(),
                    ) {
                        Ok(()) => {
                            result.changed.push(file.source_path.clone());
                            human_changed_paths.push(fmt_human_changed_path(
                                &file,
                                workspace_discovery_signature.is_some(),
                            ));
                        }
                        Err(error) => {
                            if error.installed {
                                result.changed.push(file.source_path.clone());
                                human_changed_paths.push(fmt_human_changed_path(
                                    &file,
                                    workspace_discovery_signature.is_some(),
                                ));
                            }
                            diagnostics.push(fmt_io_diagnostic(
                                "OUTPUT_WRITE_FAILED",
                                &file.source_path,
                                "could not write formatted file",
                                &error.source,
                            ));
                            total_error_count = total_error_count.saturating_add(1);
                        }
                    }
                } else {
                    result.changed.push(file.source_path.clone());
                    diagnostics.push(unformatted_file_diagnostic(
                        &source,
                        formatted.as_bytes(),
                        &file.source_path,
                        parsed.globals.format == OutputFormat::Human,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                }
            }
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
        } else {
            let source_file = match open_selected_fmt_file(&mut file) {
                Ok(file) => file,
                Err(error) => {
                    diagnostics.push(fmt_io_diagnostic(
                        "SOURCE_READ_FAILED",
                        &file.source_path,
                        "could not open formatter input",
                        &error,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                    continue;
                }
            };
            let permissions = match source_file.metadata() {
                Ok(metadata) => metadata.permissions(),
                Err(error) => {
                    diagnostics.push(fmt_io_diagnostic(
                        "SOURCE_READ_FAILED",
                        &file.source_path,
                        "could not inspect formatter input permissions",
                        &error,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                    continue;
                }
            };
            let source =
                match read_source_limited(source_file, sai_vfs::MAX_SOURCE_BYTES, cancellation) {
                    Ok(source) => source,
                    Err(SourceInputReadError::Interrupted) => {
                        return report_interrupted(parsed, stdout, stderr);
                    }
                    Err(SourceInputReadError::Io(error)) => {
                        diagnostics.push(fmt_io_diagnostic(
                            "SOURCE_READ_FAILED",
                            &file.source_path,
                            "could not read formatter input",
                            &error,
                        ));
                        total_error_count = total_error_count.saturating_add(1);
                        continue;
                    }
                    Err(SourceInputReadError::TooLarge(byte_len)) => {
                        result.refused.push(file.source_path.clone());
                        diagnostics.push(source_limit_diagnostic(byte_len, &file.source_path));
                        total_error_count = total_error_count.saturating_add(1);
                        continue;
                    }
                };
            let source: Arc<[u8]> = source.into();
            let formatted = match sai_fmt::format_source(&source) {
                Ok(formatted) => formatted,
                Err(error) => {
                    if matches!(error, sai_fmt::SourceFormatError::Refused(_)) {
                        result.refused.push(file.source_path.clone());
                    }
                    let batch = source_format_diagnostics(
                        &source,
                        &file.source_path,
                        error,
                        parsed.globals.max_errors,
                        parsed.globals.format == OutputFormat::Human,
                    );
                    total_error_count = total_error_count.saturating_add(batch.total_error_count);
                    diagnostics.extend(batch.diagnostics);
                    continue;
                }
            };
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
            let differs = source.as_ref() != formatted.as_bytes();
            if differs {
                if !check {
                    if cancellation.is_cancelled() {
                        return report_interrupted(parsed, stdout, stderr);
                    }
                    match write_file_atomically_in_directory(
                        file.parent
                            .as_deref()
                            .expect("source file retains its parent directory"),
                        &file.name,
                        file.identity,
                        source.as_ref(),
                        permissions,
                        formatted.as_bytes(),
                    ) {
                        Ok(()) => {
                            result.changed.push(file.source_path.clone());
                            human_changed_paths.push(fmt_human_changed_path(
                                &file,
                                workspace_discovery_signature.is_some(),
                            ));
                        }
                        Err(error) => {
                            if error.installed {
                                result.changed.push(file.source_path.clone());
                                human_changed_paths.push(fmt_human_changed_path(
                                    &file,
                                    workspace_discovery_signature.is_some(),
                                ));
                            }
                            diagnostics.push(fmt_io_diagnostic(
                                "OUTPUT_WRITE_FAILED",
                                &file.source_path,
                                "could not write formatted file",
                                &error.source,
                            ));
                            total_error_count = total_error_count.saturating_add(1);
                        }
                    }
                } else {
                    result.changed.push(file.source_path.clone());
                    diagnostics.push(unformatted_file_diagnostic(
                        &source,
                        formatted.as_bytes(),
                        &file.source_path,
                        parsed.globals.format == OutputFormat::Human,
                    ));
                    total_error_count = total_error_count.saturating_add(1);
                }
            }
            if cancellation.is_cancelled() {
                return report_interrupted(parsed, stdout, stderr);
            }
        }
    }
    if cancellation.is_cancelled() {
        return report_interrupted(parsed, stdout, stderr);
    }

    if parsed.globals.format == OutputFormat::Human && !parsed.globals.quiet {
        for path in human_changed_paths {
            if check {
                continue;
            }
            let line = format!("formatted {path}\n");
            let line = escape_terminal_controls(&line);
            if let Err(error) = stderr.write_all(line.as_bytes()) {
                report_stderr_failure(error);
                return 4;
            }
        }
    }

    if !diagnostics.is_empty() {
        return report_fmt_diagnostics(
            parsed,
            diagnostics,
            total_error_count,
            Some(result),
            stdout,
            stderr,
        );
    }
    match parsed.globals.format {
        OutputFormat::Human => 0,
        OutputFormat::Json => {
            if let Err(error) = write_json_fmt_success(stdout, result) {
                report_stdout_failure(stderr, error);
                4
            } else {
                0
            }
        }
    }
}

fn fmt_path_reference(path: &Path, cwd: &Path, package_root: Option<&Path>) -> String {
    let relative = package_root
        .and_then(|root| relative_path(root, path))
        .or_else(|| relative_path(cwd, path));
    relative.map_or_else(
        || {
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned()
        },
        |relative| format_relative_path(&relative),
    )
}

fn fmt_run_path_reference(path: &Path, cwd: &Path, workspace_base: Option<&Path>) -> String {
    fmt_path_reference(path, cwd, workspace_base)
}

fn fmt_io_diagnostic(
    code: &'static str,
    path: &str,
    message: &str,
    error: &io::Error,
) -> CliDiagnostic {
    CliDiagnostic {
        code,
        message: format!("{message}: {}", error),
        primary: None,
        primary_label: String::new(),
        argument_index: None,
        argument: path.to_owned(),
        help: None,
        data: Some(format!(
            "{{\"path\":\"{}\",\"reason\":\"{}\"}}",
            escape_json(path),
            escape_json(&error.to_string())
        )),
    }
}

fn read_file_limited(
    mut file: impl Read,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, StdinReadError> {
    let max_bytes = limit.saturating_add(1);
    let mut source = Vec::with_capacity(max_bytes.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        if cancellation.is_cancelled() {
            return Err(StdinReadError::Interrupted);
        }
        let remaining = max_bytes.saturating_sub(source.len());
        if remaining == 0 {
            return Ok(source);
        }
        let read_len = remaining.min(buffer.len());
        match file.read(&mut buffer[..read_len]) {
            Ok(0) => return Ok(source),
            Ok(bytes_read) => source.extend_from_slice(&buffer[..bytes_read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(StdinReadError::Io(error)),
        }
    }
}

#[derive(Debug)]
struct AtomicFmtWriteError {
    source: io::Error,
    installed: bool,
}

fn write_file_atomically_in_directory(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
    expected_identity: sai_windows_fs::FileIdentity,
    expected_contents: &[u8],
    permissions: cap_std::fs::Permissions,
    contents: &[u8],
) -> Result<(), AtomicFmtWriteError> {
    for _ in 0..8 {
        let serial = NEXT_FMT_TEMPORARY_FILE.fetch_add(1, AtomicOrdering::Relaxed);
        let temporary_name =
            OsString::from(format!(".sai-fmt-{}-{serial}.tmp", std::process::id()));
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let temporary_parent = parent.try_clone().map_err(|source| AtomicFmtWriteError {
            source,
            installed: false,
        })?;
        let mut temporary_file = match parent.open_with(&temporary_name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(AtomicFmtWriteError {
                    source,
                    installed: false,
                });
            }
        };
        let mut temporary = TemporaryFmtFile {
            parent: temporary_parent,
            name: temporary_name.clone(),
            published: false,
        };
        temporary_file
            .write_all(contents)
            .map_err(|source| AtomicFmtWriteError {
                source,
                installed: false,
            })?;
        temporary_file
            .set_permissions(permissions.clone())
            .map_err(|source| AtomicFmtWriteError {
                source,
                installed: false,
            })?;
        temporary_file
            .sync_all()
            .map_err(|source| AtomicFmtWriteError {
                source,
                installed: false,
            })?;
        drop(temporary_file);
        let parent_handle = parent
            .try_clone()
            .map_err(|source| AtomicFmtWriteError {
                source,
                installed: false,
            })?
            .into_std_file();
        sai_windows_fs::replace_file_in_directory_if_unchanged(
            &parent_handle,
            &temporary_name,
            name,
            expected_identity,
            expected_contents,
        )
        .map_err(|source| AtomicFmtWriteError {
            source,
            installed: false,
        })?;
        temporary.published = true;
        sync_fmt_parent_directory(parent).map_err(|source| AtomicFmtWriteError {
            source,
            installed: true,
        })?;
        return Ok(());
    }
    Err(AtomicFmtWriteError {
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique formatter temporary file",
        ),
        installed: false,
    })
}

struct TemporaryFmtFile {
    parent: cap_std::fs::Dir,
    name: OsString,
    published: bool,
}

impl Drop for TemporaryFmtFile {
    fn drop(&mut self) {
        if !self.published {
            if let Ok(parent) = self.parent.try_clone() {
                let _ =
                    sai_windows_fs::remove_file_in_directory(&parent.into_std_file(), &self.name);
            }
        }
    }
}

fn sync_fmt_parent_directory(parent: &cap_std::fs::Dir) -> io::Result<()> {
    #[cfg(unix)]
    {
        match parent
            .try_clone()
            .map(|directory| directory.into_std_file())
            .and_then(|directory| directory.sync_all())
        {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
                ) => {}
            Err(error) => return Err(error),
        }
    }
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

#[derive(Debug)]
enum StdinReadError {
    Interrupted,
    Io(io::Error),
}

fn read_stdin_limited(
    stdin: &mut impl Read,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, StdinReadError> {
    let mut source = Vec::new();
    let mut buffer = [0_u8; 8192];
    let max_bytes = limit.saturating_add(1);

    loop {
        if cancellation.is_cancelled() {
            return Err(StdinReadError::Interrupted);
        }
        let remaining = max_bytes.saturating_sub(source.len());
        let read_len = remaining.min(buffer.len());
        if read_len == 0 {
            return Ok(source);
        }
        match stdin.read(&mut buffer[..read_len]) {
            Ok(0) => {
                if cancellation.is_cancelled() {
                    return Err(StdinReadError::Interrupted);
                }
                return Ok(source);
            }
            Ok(bytes_read) => {
                source.extend_from_slice(&buffer[..bytes_read]);
                if cancellation.is_cancelled() {
                    return Err(StdinReadError::Interrupted);
                }
                if source.len() >= max_bytes {
                    return Ok(source);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                if cancellation.is_cancelled() {
                    return Err(StdinReadError::Interrupted);
                }
            }
            Err(error) => {
                if cancellation.is_cancelled() {
                    return Err(StdinReadError::Interrupted);
                }
                return Err(StdinReadError::Io(error));
            }
        }
    }
}

enum SourceInputReadError {
    Interrupted,
    Io(io::Error),
    TooLarge(usize),
}

fn read_source_limited(
    mut input: impl Read,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, SourceInputReadError> {
    let mut source = Vec::with_capacity(limit.min(8192));
    let mut total_bytes = 0usize;
    let mut oversized = false;
    let mut buffer = [0_u8; 8192];

    loop {
        if cancellation.is_cancelled() {
            return Err(SourceInputReadError::Interrupted);
        }
        match input.read(&mut buffer) {
            Ok(0) => {
                if cancellation.is_cancelled() {
                    return Err(SourceInputReadError::Interrupted);
                }
                return if oversized {
                    Err(SourceInputReadError::TooLarge(total_bytes))
                } else {
                    Ok(source)
                };
            }
            Ok(bytes_read) => {
                total_bytes = total_bytes.saturating_add(bytes_read);
                let remaining = limit.saturating_sub(source.len());
                let retained_bytes = bytes_read.min(remaining);
                source.extend_from_slice(&buffer[..retained_bytes]);
                oversized |= retained_bytes < bytes_read;
                if cancellation.is_cancelled() {
                    return Err(SourceInputReadError::Interrupted);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                if cancellation.is_cancelled() {
                    return Err(SourceInputReadError::Interrupted);
                }
            }
            Err(error) => {
                if cancellation.is_cancelled() {
                    return Err(SourceInputReadError::Interrupted);
                }
                return Err(SourceInputReadError::Io(error));
            }
        }
    }
}

fn source_limit_diagnostic(byte_len: usize, source_path: &str) -> CliDiagnostic {
    let limit_mib = sai_vfs::MAX_SOURCE_BYTES / (1024 * 1024);
    let mut source_index = CliSourceIndex::new(Arc::from([]));
    cli_source_diagnostic(
        &mut source_index,
        FmtDiagnosticSource {
            path: source_path,
            include_source_line: false,
        },
        "IMPLEMENTATION_LIMIT_EXCEEDED",
        format!("source file is {byte_len} bytes, exceeding the {limit_mib} MiB limit"),
        0..0,
        Some(format!(
            "{{\"limit\":\"size of a source file\",\"value\":{byte_len}}}"
        )),
        None,
    )
}

impl StdinBroker {
    fn spawn(input: Box<dyn Read + Send>) -> Self {
        let (request_sender, request_receiver) = mpsc::sync_channel::<usize>(1);
        let (sender, receiver) = mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
        let spawn_result = thread::Builder::new()
            .name("sai-stdin-reader".to_owned())
            .spawn(move || {
                let mut input = input;
                loop {
                    let Ok(requested_len) = request_receiver.recv() else {
                        return;
                    };
                    let mut buffer = vec![0_u8; requested_len.min(8192)];
                    loop {
                        match input.read(&mut buffer) {
                            Ok(0) => {
                                let _ = sender.send(Ok(Vec::new()));
                                return;
                            }
                            Ok(bytes_read) => {
                                if sender.send(Ok(buffer[..bytes_read].to_vec())).is_err() {
                                    return;
                                }
                                break;
                            }
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                            Err(error) => {
                                let _ = sender.send(Err(error));
                                return;
                            }
                        }
                    }
                }
            });

        Self {
            request_sender,
            receiver,
            pending: Vec::new(),
            pending_offset: 0,
            request_pending: false,
            eof: false,
            startup_error: spawn_result.err(),
        }
    }
}

impl Read for CancellationAwareStdin {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.broker.is_none() {
            self.broker = Some(Arc::clone(STDIN_BROKER.get_or_init(|| {
                Arc::new(Mutex::new(StdinBroker::spawn(Box::new(io::stdin()))))
            })));
        }
        let broker = Arc::clone(self.broker.as_ref().expect("stdin broker is initialized"));

        loop {
            if self.cancellation.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let mut broker = broker
                .lock()
                .map_err(|_| io::Error::other("standard-input reader state was poisoned"))?;
            if let Some(error) = broker.startup_error.take() {
                return Err(error);
            }
            if broker.pending_offset < broker.pending.len() {
                let remaining = broker.pending.len() - broker.pending_offset;
                let bytes_read = buffer.len().min(remaining);
                let pending_offset = broker.pending_offset;
                buffer[..bytes_read]
                    .copy_from_slice(&broker.pending[pending_offset..pending_offset + bytes_read]);
                broker.pending_offset += bytes_read;
                if broker.pending_offset == broker.pending.len() {
                    broker.pending.clear();
                    broker.pending_offset = 0;
                }
                return Ok(bytes_read);
            }
            if broker.eof {
                return Ok(0);
            }

            if !broker.request_pending {
                broker
                    .request_sender
                    .send(buffer.len().min(8192))
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "standard-input reader stopped unexpectedly",
                        )
                    })?;
                broker.request_pending = true;
            }
            match broker.receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(chunk)) if chunk.is_empty() => {
                    broker.request_pending = false;
                    broker.eof = true;
                    return Ok(0);
                }
                Ok(Ok(chunk)) => {
                    broker.request_pending = false;
                    broker.pending = chunk;
                    broker.pending_offset = 0;
                }
                Ok(Err(error)) => {
                    broker.request_pending = false;
                    return Err(error);
                }
                Err(RecvTimeoutError::Timeout) => {
                    drop(broker);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "standard-input reader stopped unexpectedly",
                    ));
                }
            }
        }
    }
}

fn write_json_fmt_success(writer: &mut impl Write, result: FmtResult) -> io::Result<()> {
    write_json_success_header(writer, "fmt")?;
    writeln!(writer, "{}", json_fmt_result(&result))?;
    write_json_summary(writer, 0, 0, 0)
}

fn report_fmt_diagnostics(
    parsed: &ParsedArgs,
    mut diagnostics: Vec<CliDiagnostic>,
    total_error_count: usize,
    result: Option<FmtResult>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    diagnostics.sort_by(|left, right| {
        let left_path = left
            .primary
            .as_ref()
            .map_or_else(|| left.argument.clone(), |location| location.path.clone());
        let right_path = right
            .primary
            .as_ref()
            .map_or_else(|| right.argument.clone(), |location| location.path.clone());
        let left_offset = left
            .primary
            .as_ref()
            .map_or(usize::MAX, |location| location.start.byte);
        let right_offset = right
            .primary
            .as_ref()
            .map_or(usize::MAX, |location| location.start.byte);
        left_path
            .as_bytes()
            .cmp(right_path.as_bytes())
            .then_with(|| left_offset.cmp(&right_offset))
            .then_with(|| left.code.cmp(right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    let total = total_error_count;
    let exit_code = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "INTERNAL_COMPILER_ERROR")
    {
        101
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "OUTPUT_WRITE_FAILED")
    {
        4
    } else {
        1
    };
    if parsed.globals.max_errors != 0 {
        diagnostics.truncate(parsed.globals.max_errors);
    }
    match parsed.globals.format {
        OutputFormat::Human => {
            if let Err(error) = write_human_diagnostics(
                stderr,
                "fmt",
                &diagnostics,
                !parsed.globals.quiet,
                color_enabled(parsed.globals.color, io::stderr().is_terminal()),
                total,
                parsed.globals.max_errors,
            ) {
                report_stderr_failure(error)
            } else {
                exit_code
            }
        }
        OutputFormat::Json => {
            if let Err(error) =
                write_json_fmt_diagnostics(stdout, &diagnostics, total, exit_code, result.as_ref())
            {
                report_stdout_failure(stderr, error);
                4
            } else {
                exit_code
            }
        }
    }
}

fn json_fmt_result(result: &FmtResult) -> String {
    let changed = json_string_array(&result.changed);
    let refused = json_string_array(&result.refused);
    let output = result.output.as_ref().map_or_else(
        || "null".to_owned(),
        |text| format!("\"{}\"", escape_json(text)),
    );
    format!(
        "{{\"type\":\"result\",\"kind\":\"fmt\",\"data\":{{\"check\":{},\"files\":{},\"changed\":{},\"refused\":{},\"output\":{}}}}}",
        result.check, result.files, changed, refused, output
    )
}

fn json_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{}\"", escape_json(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn write_json_fmt_diagnostics(
    writer: &mut impl Write,
    diagnostics: &[CliDiagnostic],
    total_error_count: usize,
    exit_code: i32,
    result: Option<&FmtResult>,
) -> io::Result<()> {
    write_json_success_header(writer, "fmt")?;
    write_json_diagnostic_records(writer, diagnostics)?;
    if let Some(result) = result {
        writeln!(writer, "{}", json_fmt_result(result))?;
    }
    write_json_summary(writer, total_error_count, diagnostics.len(), exit_code)
}

fn package_format_diagnostics(
    source: &Arc<[u8]>,
    source_path: &str,
    error: sai_fmt::PackageFormatError,
    max_errors: usize,
    include_source_lines: bool,
) -> FmtDiagnosticBatch {
    match error {
        sai_fmt::PackageFormatError::Refused(refusal) => {
            let total_error_count = refusal
                .lexical_error_count
                .saturating_add(refusal.syntax_error_count);
            let mut selected = BinaryHeap::new();
            let mut order = 0usize;
            for error in refusal.lexical_errors {
                let (code, message) = package_lex_error_fields(error.kind);
                retain_pending_fmt_diagnostic(
                    &mut selected,
                    PendingFmtDiagnostic {
                        code,
                        message: Cow::Borrowed(message),
                        span: error.span,
                        help: None,
                        data: None,
                        order,
                    },
                    max_errors,
                );
                order = order.saturating_add(1);
            }
            for error in refusal.syntax_errors {
                let (code, message, data) = package_parse_error_fields(&error.kind);
                retain_pending_fmt_diagnostic(
                    &mut selected,
                    PendingFmtDiagnostic {
                        code,
                        message,
                        span: error.span,
                        help: None,
                        data,
                        order,
                    },
                    max_errors,
                );
                order = order.saturating_add(1);
            }
            let mut source_index = CliSourceIndex::new(Arc::clone(source));
            let diagnostics = selected
                .into_sorted_vec()
                .into_iter()
                .map(|pending| {
                    cli_source_diagnostic(
                        &mut source_index,
                        FmtDiagnosticSource {
                            path: source_path,
                            include_source_line: include_source_lines,
                        },
                        pending.code,
                        pending.message.into_owned(),
                        pending.span,
                        pending.data,
                        pending.help,
                    )
                })
                .collect();
            FmtDiagnosticBatch {
                diagnostics,
                total_error_count,
            }
        }
        sai_fmt::PackageFormatError::VerificationFailed(failure) => {
            let check = match failure.check {
                sai_fmt::PackageVerificationCheck::OutputSyntax => "output does not parse",
                sai_fmt::PackageVerificationCheck::TokenPreservation => "token sequence changed",
                sai_fmt::PackageVerificationCheck::DiagnosticPreservation => {
                    "parser diagnostics changed"
                }
                sai_fmt::PackageVerificationCheck::CommentPreservation => "comments changed",
                sai_fmt::PackageVerificationCheck::Idempotency => "formatting is not idempotent",
            };
            let mut source_index = CliSourceIndex::new(Arc::clone(source));
            let primary = failure.input_byte_offset.map(|offset| {
                source_index.location(source_path, offset..offset, include_source_lines)
            });
            FmtDiagnosticBatch {
                diagnostics: vec![internal_fmt_diagnostic(
                    format!("formatter verification failed: {check}"),
                    primary,
                )],
                total_error_count: 1,
            }
        }
        sai_fmt::PackageFormatError::InternalError => FmtDiagnosticBatch {
            diagnostics: vec![internal_fmt_diagnostic(
                "package formatter could not render input".to_owned(),
                None,
            )],
            total_error_count: 1,
        },
        sai_fmt::PackageFormatError::Print(error) => FmtDiagnosticBatch {
            diagnostics: vec![internal_fmt_diagnostic(
                format!("package formatter printer failed: {error:?}"),
                None,
            )],
            total_error_count: 1,
        },
    }
}

fn package_lex_error_fields(
    kind: sai_package::lexer::PackageLexErrorKind,
) -> (&'static str, &'static str) {
    use sai_package::lexer::PackageLexErrorKind as Kind;
    match kind {
        Kind::InvalidUtf8 => ("INVALID_UTF8", "manifest is not valid UTF-8"),
        Kind::InvalidLineEnding => ("INVALID_LINE_ENDING", "lone carriage return"),
        Kind::ForbiddenCharacter => ("FORBIDDEN_CHARACTER", "forbidden character in manifest"),
        Kind::UnexpectedCharacter => ("UNEXPECTED_CHARACTER", "unexpected character in manifest"),
        Kind::UnterminatedComment => ("UNTERMINATED_COMMENT", "unterminated comment"),
        Kind::UnterminatedString => ("UNTERMINATED_STRING", "unterminated string"),
        Kind::InvalidEscape => ("INVALID_ESCAPE", "invalid string escape"),
        Kind::UnsupportedStringForm => (
            "UNSUPPORTED_STRING_FORM",
            "unsupported string form in manifest",
        ),
        Kind::InvalidVersion => ("INVALID_VERSION", "invalid package version"),
        Kind::BuildMetadataNotAllowed => (
            "BUILD_METADATA_NOT_ALLOWED",
            "build metadata is not allowed in a package version",
        ),
        Kind::NoSemicolon => ("NO_SEMICOLON", "semicolons are not allowed in manifests"),
    }
}

fn retain_pending_fmt_diagnostic(
    selected: &mut BinaryHeap<PendingFmtDiagnostic>,
    diagnostic: PendingFmtDiagnostic,
    max_errors: usize,
) {
    if max_errors == 0 || selected.len() < max_errors {
        selected.push(diagnostic);
    } else if selected.peek().is_some_and(|largest| diagnostic < *largest) {
        selected.pop();
        selected.push(diagnostic);
    }
}

fn package_parse_error_fields(
    kind: &sai_package::manifest::PackageParseErrorKind,
) -> (&'static str, Cow<'static, str>, Option<String>) {
    use sai_package::manifest::PackageParseErrorKind as Kind;
    match kind {
        Kind::MissingPackageDeclaration => (
            "MISSING_PACKAGE_DECL",
            Cow::Borrowed("missing `package` declaration"),
            None,
        ),
        Kind::PackageDeclarationNotFirst => (
            "PACKAGE_DECL_NOT_FIRST",
            Cow::Borrowed("`package` declaration must be first"),
            None,
        ),
        Kind::DuplicateDeclaration(name) => (
            "DUPLICATE_DECLARATION",
            Cow::Owned(format!("duplicate `{name}` declaration")),
            None,
        ),
        Kind::UnknownDeclaration { name, .. } => (
            "UNKNOWN_DECLARATION",
            Cow::Owned(format!("unknown package declaration `{name}`")),
            None,
        ),
        Kind::UnknownArtifactKind(name) => (
            "UNKNOWN_ARTIFACT_KIND",
            Cow::Owned(format!("unknown artifact kind `{name}`")),
            None,
        ),
        Kind::ExpectedToken(token) => {
            let expected = if token.as_ref() == "newline between declarations" {
                "NEWLINE".to_owned()
            } else {
                token.to_string()
            };
            (
                "EXPECTED_TOKEN",
                Cow::Owned(format!("expected {token}")),
                Some(format!(
                    "{{\"expected_tokens\":[\"{}\"]}}",
                    escape_json(&expected)
                )),
            )
        }
        Kind::ExpectedListSeparator(tokens) => (
            "EXPECTED_TOKEN",
            Cow::Borrowed("expected a list separator"),
            Some(format!(
                "{{\"expected_tokens\":[{}]}}",
                tokens
                    .iter()
                    .map(|token| format!("\"{}\"", escape_json(token)))
                    .collect::<Vec<_>>()
                    .join(",")
            )),
        ),
        Kind::ExpectedDeclaration => (
            "EXPECTED_TOKEN",
            Cow::Borrowed("expected a package declaration"),
            Some("{\"expected_tokens\":[\"package\",\"compiler\",\"source\",\"wit\",\"artifact\",\"targets\",\"export\",\"requires\",\"allow\",\"deny\",\"policy\",\"lints\",\"profile\",\"allocator\",\"overrides\",\"description\",\"license\",\"authors\",\"repository\"]}".to_owned()),
        ),
        Kind::ExpectedProfileItem => (
            "EXPECTED_TOKEN",
            Cow::Borrowed("expected a profile item"),
            Some("{\"expected_tokens\":[\"allow\",\"budget\"]}".to_owned()),
        ),
        Kind::InvalidPackageIdentity(_) => (
            "INVALID_PACKAGE_NAME",
            Cow::Borrowed("invalid package identity"),
            None,
        ),
        Kind::InvalidPackageVersion(_) => (
            "INVALID_VERSION",
            Cow::Borrowed("invalid package version"),
            None,
        ),
        Kind::InvalidContentPath(_)
        | Kind::InvalidDependencyPath(_)
        | Kind::InvalidRepositoryDirectory(_) => (
            "INVALID_PACKAGE_PATH",
            Cow::Borrowed("invalid package path"),
            None,
        ),
        Kind::ConflictingLintLevel => (
            "CONFLICTING_LINT_LEVEL",
            Cow::Borrowed("conflicting lint levels"),
            None,
        ),
        Kind::InvalidVersionRequirement(_) => (
            "INVALID_VERSION_REQUIREMENT",
            Cow::Borrowed("invalid version requirement"),
            None,
        ),
        Kind::ManifestTooLarge => (
            "PACKAGE_TOO_LARGE",
            Cow::Borrowed("package manifest exceeds the supported size limit"),
            None,
        ),
    }
}

fn internal_fmt_diagnostic(description: String, primary: Option<CliLocation>) -> CliDiagnostic {
    let data = format!(
        "{{\"component\":\"sai_fmt\",\"panic_message\":\"{}\",\"query_stack\":[],\"compiler_version\":\"{}\"}}",
        escape_json(&description),
        env!("CARGO_PKG_VERSION")
    );
    CliDiagnostic {
        code: "INTERNAL_COMPILER_ERROR",
        message: format!("internal compiler error: {description}"),
        primary,
        primary_label: String::new(),
        argument_index: None,
        argument: String::new(),
        help: None,
        data: Some(data),
    }
}

fn cli_source_diagnostic(
    source: &mut CliSourceIndex,
    source_location: FmtDiagnosticSource<'_>,
    code: &'static str,
    message: String,
    span: std::ops::Range<usize>,
    data: Option<String>,
    help: Option<String>,
) -> CliDiagnostic {
    CliDiagnostic {
        code,
        message,
        primary: Some(source.location(
            source_location.path,
            span,
            source_location.include_source_line,
        )),
        primary_label: String::new(),
        argument_index: None,
        argument: String::new(),
        help,
        data,
    }
}

#[cfg(test)]
fn cli_position(source: &[u8], offset: usize) -> CliPosition {
    let mut index = CliSourceIndex::new(Arc::from(source));
    index.position(offset)
}

impl CliSourceIndex {
    fn new(source: Arc<[u8]>) -> Self {
        let bom_len = usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3;
        let mut line_starts = vec![u32::try_from(bom_len).unwrap_or(0)];
        let mut cursor = bom_len;
        while cursor < source.len() {
            match source[cursor] {
                b'\n' => {
                    cursor += 1;
                    line_starts.push(u32::try_from(cursor).unwrap_or(u32::MAX));
                }
                b'\r' if source.get(cursor + 1) == Some(&b'\n') => {
                    cursor += 2;
                    line_starts.push(u32::try_from(cursor).unwrap_or(u32::MAX));
                }
                b'\r' => {
                    cursor += 1;
                    line_starts.push(u32::try_from(cursor).unwrap_or(u32::MAX));
                }
                _ => cursor += 1,
            }
        }
        Self {
            source,
            line_starts,
            line_ends: HashMap::new(),
            utf16_checkpoints: HashMap::new(),
            rendered_lines: HashMap::new(),
        }
    }

    fn position(&mut self, offset: usize) -> CliPosition {
        let offset = offset.min(self.source.len());
        let line_index = self
            .line_starts
            .partition_point(|start| (*start as usize) <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line_index] as usize;
        let next_line_start = self
            .line_starts
            .get(line_index + 1)
            .copied()
            .map_or(self.source.len(), |start| start as usize);
        let utf16_col = if offset <= line_start {
            1
        } else {
            let line = &self.source[line_start..next_line_start];
            let checkpoints = self
                .utf16_checkpoints
                .entry(line_start as u32)
                .or_insert_with(|| build_utf16_checkpoints(line));
            let relative_offset = offset.saturating_sub(line_start);
            let checkpoint_index = checkpoints
                .partition_point(|(byte, _)| (*byte as usize) <= relative_offset)
                .saturating_sub(1);
            let (checkpoint_byte, checkpoint_width) = checkpoints[checkpoint_index];
            let checkpoint_byte = checkpoint_byte as usize;
            let suffix_end = relative_offset.min(line.len());
            1 + checkpoint_width
                + utf16_column_width(&line[checkpoint_byte.min(suffix_end)..suffix_end])
        };
        CliPosition {
            byte: offset,
            line: line_index + 1,
            col: offset.saturating_sub(line_start) + 1,
            utf16_col,
        }
    }

    fn line_bounds(&mut self, offset: usize) -> (usize, u32, u32) {
        let offset = offset.min(self.source.len());
        let line_index = self
            .line_starts
            .partition_point(|start| (*start as usize) <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line_index];
        let content_end = if let Some(end) = self.line_ends.get(&line_start) {
            *end
        } else {
            let start = line_start as usize;
            let end = self.source[start..]
                .iter()
                .position(|byte| matches!(*byte, b'\r' | b'\n'))
                .map_or(self.source.len(), |relative| start + relative);
            let end = u32::try_from(end).unwrap_or(u32::MAX);
            self.line_ends.insert(line_start, end);
            end
        };
        (line_index + 1, line_start, content_end)
    }

    fn rendered_line(&mut self, line_start: u32, content_end: u32) -> Arc<RenderedSourceLine> {
        if let Some(line) = self.rendered_lines.get(&line_start) {
            return Arc::clone(line);
        }
        let rendered_start =
            if line_start == self.line_starts[0] && self.source.starts_with(b"\xef\xbb\xbf") {
                0
            } else {
                line_start as usize
            };
        let content_end = content_end as usize;
        let content_end = if self.source.get(content_end) == Some(&b'\r')
            && self.source.get(content_end + 1) != Some(&b'\n')
        {
            content_end + 1
        } else {
            content_end
        };
        let line = Arc::new(RenderedSourceLine::new(
            &self.source[rendered_start..content_end],
        ));
        self.rendered_lines.insert(line_start, Arc::clone(&line));
        line
    }

    fn location(
        &mut self,
        path: &str,
        span: std::ops::Range<usize>,
        include_source_line: bool,
    ) -> CliLocation {
        let start = span.start.min(self.source.len());
        let end = span.end.max(start).min(self.source.len());
        let lone_cr_span = end == start.saturating_add(1)
            && self.source.get(start) == Some(&b'\r')
            && self.source.get(end) != Some(&b'\n');
        let (start_line, line_start, content_end) = self.line_bounds(start);
        let start_position = self.position(start);
        let end_position = self.position(end);
        let mut additional_source_lines = Vec::new();
        let (source_line, start_display_col, end_display_col) = if include_source_line {
            let rendered_content_end = if lone_cr_span {
                content_end.saturating_add(1).min(self.source.len() as u32)
            } else {
                content_end
            };
            let line = self.rendered_line(line_start, rendered_content_end);
            let rendered_start =
                if line_start == self.line_starts[0] && self.source.starts_with(b"\xef\xbb\xbf") {
                    0
                } else {
                    line_start as usize
                };
            let start_prefix_end = start.min(content_end as usize);
            let start_display_col = line
                .display_column(&self.source[rendered_start..start_prefix_end.max(rendered_start)]);
            let end_display_col = if lone_cr_span {
                line.display_column(&self.source[rendered_start..end])
            } else if end_position.line == start_line {
                line.display_column(
                    &self.source[rendered_start..end.min(content_end as usize).max(rendered_start)],
                )
            } else {
                let end_line_index = end_position.line.saturating_sub(1);
                let end_line_start = self.line_starts[end_line_index];
                let (_, end_line_start, end_content_end) =
                    self.line_bounds(end_line_start as usize);
                let end_line = self.rendered_line(end_line_start, end_content_end);
                let end_rendered_start = if end_line_start == self.line_starts[0]
                    && self.source.starts_with(b"\xef\xbb\xbf")
                {
                    0
                } else {
                    end_line_start as usize
                };
                let end_prefix_end = end.min(end_content_end as usize).max(end_rendered_start);
                let end_display_col =
                    end_line.display_column(&self.source[end_rendered_start..end_prefix_end]);
                let middle_line_count = end_position
                    .line
                    .saturating_sub(start_line)
                    .saturating_sub(1);
                if middle_line_count > 2 {
                    additional_source_lines.push(CliAdditionalSnippetRow::Gap);
                } else {
                    for line_number in (start_line + 1)..end_position.line {
                        let line_start = self.line_starts[line_number - 1];
                        let (_, line_start, content_end) = self.line_bounds(line_start as usize);
                        let source_line = self.rendered_line(line_start, content_end);
                        additional_source_lines.push(CliAdditionalSnippetRow::Source {
                            line_number,
                            source_line,
                            annotation: None,
                        });
                    }
                }
                additional_source_lines.push(CliAdditionalSnippetRow::Source {
                    line_number: end_position.line,
                    annotation: Some((
                        end_line.first_non_whitespace_display_col(),
                        end_display_col,
                    )),
                    source_line: end_line,
                });
                end_display_col
            };
            (Some(line), start_display_col, end_display_col)
        } else {
            (None, 0, 0)
        };
        CliLocation {
            path: path.to_owned(),
            start: start_position,
            end: end_position,
            source_line,
            additional_source_lines,
            start_display_col,
            end_display_col,
        }
    }
}

fn build_utf16_checkpoints(source: &[u8]) -> Vec<(u32, usize)> {
    let mut checkpoints = vec![(0, 0)];
    let mut byte_offset = 0usize;
    let mut utf16_width = 0usize;
    let mut last_checkpoint = 0usize;

    while byte_offset < source.len() {
        let remaining = &source[byte_offset..];
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                for character in valid.chars() {
                    byte_offset += character.len_utf8();
                    utf16_width += character.len_utf16();
                    if byte_offset.saturating_sub(last_checkpoint) >= 256 {
                        checkpoints
                            .push((u32::try_from(byte_offset).unwrap_or(u32::MAX), utf16_width));
                        last_checkpoint = byte_offset;
                    }
                }
                break;
            }
            Err(error) if error.valid_up_to() > 0 => {
                let valid = std::str::from_utf8(&remaining[..error.valid_up_to()])
                    .expect("valid_up_to identifies a valid UTF-8 prefix");
                for character in valid.chars() {
                    byte_offset += character.len_utf8();
                    utf16_width += character.len_utf16();
                    if byte_offset.saturating_sub(last_checkpoint) >= 256 {
                        checkpoints
                            .push((u32::try_from(byte_offset).unwrap_or(u32::MAX), utf16_width));
                        last_checkpoint = byte_offset;
                    }
                }
            }
            Err(error) => {
                let invalid_len = error.error_len().unwrap_or(remaining.len());
                byte_offset = byte_offset.saturating_add(invalid_len);
                utf16_width = utf16_width.saturating_add(invalid_len);
                if byte_offset.saturating_sub(last_checkpoint) >= 256 {
                    checkpoints.push((u32::try_from(byte_offset).unwrap_or(u32::MAX), utf16_width));
                    last_checkpoint = byte_offset;
                }
            }
        }
    }
    checkpoints
}

impl RenderedSourceLine {
    fn new(source: &[u8]) -> Self {
        let mut text = String::with_capacity(source.len());
        let mut checkpoints = vec![(0, 0)];
        let mut window_checkpoints = vec![(0, 0)];
        let mut byte_offset = 0usize;
        let mut display_width = 0usize;
        let mut last_checkpoint = 0usize;

        while byte_offset < source.len() {
            match std::str::from_utf8(&source[byte_offset..]) {
                Ok(valid) => {
                    for character in valid.chars() {
                        append_source_storage(character, &mut text, &mut display_width);
                        byte_offset += character.len_utf8();
                        add_display_checkpoint(
                            &mut checkpoints,
                            &mut window_checkpoints,
                            &mut last_checkpoint,
                            byte_offset,
                            display_width,
                            text.len(),
                        );
                    }
                }
                Err(error) => {
                    let valid_len = error.valid_up_to();
                    if valid_len > 0 {
                        let valid =
                            std::str::from_utf8(&source[byte_offset..byte_offset + valid_len])
                                .expect("valid_up_to identifies a valid UTF-8 prefix");
                        for character in valid.chars() {
                            append_source_storage(character, &mut text, &mut display_width);
                            byte_offset += character.len_utf8();
                            add_display_checkpoint(
                                &mut checkpoints,
                                &mut window_checkpoints,
                                &mut last_checkpoint,
                                byte_offset,
                                display_width,
                                text.len(),
                            );
                        }
                    }
                    let invalid_len = error
                        .error_len()
                        .unwrap_or_else(|| source.len().saturating_sub(byte_offset));
                    append_source_storage('\u{fffd}', &mut text, &mut display_width);
                    byte_offset = byte_offset.saturating_add(invalid_len);
                    add_display_checkpoint(
                        &mut checkpoints,
                        &mut window_checkpoints,
                        &mut last_checkpoint,
                        byte_offset,
                        display_width,
                        text.len(),
                    );
                }
            }
        }
        if last_checkpoint != byte_offset {
            checkpoints.push((
                u32::try_from(byte_offset).unwrap_or(u32::MAX),
                display_width,
            ));
            window_checkpoints.push((display_width, text.len()));
        }
        let first_non_whitespace_display_col = first_non_whitespace_display_col(&text);
        Self {
            text,
            checkpoints,
            window_checkpoints,
            display_width,
            first_non_whitespace_display_col,
        }
    }

    fn display_column(&self, mut prefix: &[u8]) -> usize {
        let prefix_len = prefix.len();
        let checkpoint_index = self
            .checkpoints
            .partition_point(|(byte, _)| (*byte as usize) <= prefix_len)
            .saturating_sub(1);
        let (checkpoint_byte, checkpoint_width) = self.checkpoints[checkpoint_index];
        prefix = &prefix[checkpoint_byte as usize..];
        display_width_after(prefix, checkpoint_width)
    }

    fn first_non_whitespace_display_col(&self) -> usize {
        self.first_non_whitespace_display_col
    }

    fn window(
        &self,
        annotation_start: usize,
        annotation_end: usize,
    ) -> (String, usize, bool, bool, usize) {
        const MAX_DISPLAY_WIDTH: usize = 140;
        if self.display_width <= MAX_DISPLAY_WIDTH {
            let text = self.slice_display_columns(0, self.display_width);
            return (text, 0, false, false, self.display_width);
        }
        let max_start = self.display_width - MAX_DISPLAY_WIDTH;
        let mut start = annotation_start.saturating_sub(20);
        let annotation_last = annotation_end.saturating_sub(1);
        start = start.max(annotation_last.saturating_sub(MAX_DISPLAY_WIDTH - 1));
        start = start.min(max_start);
        start = self.round_down_to_display_boundary(start);
        let mut content_end = (start + MAX_DISPLAY_WIDTH).min(self.display_width);
        if annotation_end > content_end {
            start = self.round_up_to_display_boundary(start.saturating_add(1));
            content_end = (start + MAX_DISPLAY_WIDTH).min(self.display_width);
        }
        let left_clipped = start > 0;
        let right_clipped = content_end < self.display_width;
        let text = self.slice_display_columns(start, content_end);
        let width = display_width_after(text.as_bytes(), 0);
        (text, start, left_clipped, right_clipped, width)
    }

    fn round_down_to_display_boundary(&self, target: usize) -> usize {
        let (mut column, text_offset) = self.window_checkpoint_before(target);
        for character in self.text[text_offset..].chars() {
            let next = column.saturating_add(source_character_width(character, column));
            if next > target {
                return column;
            }
            column = next;
        }
        column
    }

    fn round_up_to_display_boundary(&self, target: usize) -> usize {
        let (mut column, text_offset) = self.window_checkpoint_before(target);
        for character in self.text[text_offset..].chars() {
            column = column.saturating_add(source_character_width(character, column));
            if column >= target {
                return column;
            }
        }
        column
    }

    fn slice_display_columns(&self, start: usize, end: usize) -> String {
        if end <= start {
            return String::new();
        }
        let (mut column, text_offset) = self.window_checkpoint_before(start);
        let mut output = String::new();
        for character in self.text[text_offset..].chars() {
            let width = source_character_width(character, column);
            let next_column = column.saturating_add(width);
            if width == 0 {
                let at_visible_eof = column == end && end >= self.display_width;
                if column >= start && (column < end || at_visible_eof) {
                    output.push(character);
                }
            } else {
                if column >= end {
                    break;
                }
                if column >= start && next_column <= end {
                    let mut rendered_column = column;
                    append_source_character(character, &mut output, &mut rendered_column);
                }
            }
            column = next_column;
        }
        output
    }

    fn window_checkpoint_before(&self, target: usize) -> (usize, usize) {
        let checkpoint_index = self
            .window_checkpoints
            .partition_point(|(column, _)| *column < target)
            .saturating_sub(1);
        self.window_checkpoints[checkpoint_index]
    }
}

fn first_non_whitespace_display_col(text: &str) -> usize {
    let mut column = 0usize;
    for character in text.chars() {
        if !character.is_whitespace() || is_forbidden_display_character(character) {
            return column;
        }
        column = column.saturating_add(source_character_width(character, column));
    }
    column
}

fn source_character_width(character: char, display_column: usize) -> usize {
    if character == '\t' {
        4 - (display_column % 4)
    } else if is_forbidden_display_character(character) {
        format!("<U+{:04X}>", character as u32).len()
    } else {
        character_width(character)
    }
}

fn append_source_storage(character: char, text: &mut String, display_width: &mut usize) {
    text.push(character);
    *display_width += source_character_width(character, *display_width);
}

fn escape_terminal_controls(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|character| {
        !matches!(character, '\t' | '\n') && is_forbidden_display_character(character)
    }) {
        return Cow::Borrowed(text);
    }

    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\t' | '\n') || !is_forbidden_display_character(character) {
            safe.push(character);
        } else {
            safe.push_str(&format!("<U+{:04X}>", character as u32));
        }
    }
    Cow::Owned(safe)
}

fn utf16_column_width(mut bytes: &[u8]) -> usize {
    let mut width = 0usize;
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                width += valid.encode_utf16().count();
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    let valid = std::str::from_utf8(&bytes[..valid_len])
                        .expect("valid_up_to identifies a valid UTF-8 prefix");
                    width += valid.encode_utf16().count();
                }
                let invalid_len = error
                    .error_len()
                    .unwrap_or_else(|| bytes.len().saturating_sub(valid_len));
                width += invalid_len;
                bytes = &bytes[valid_len + invalid_len..];
            }
        }
    }
    width
}

fn add_display_checkpoint(
    checkpoints: &mut Vec<(u32, usize)>,
    window_checkpoints: &mut Vec<(usize, usize)>,
    last_checkpoint: &mut usize,
    byte_offset: usize,
    display_width: usize,
    text_byte_offset: usize,
) {
    if byte_offset.saturating_sub(*last_checkpoint) >= 256 {
        checkpoints.push((
            u32::try_from(byte_offset).unwrap_or(u32::MAX),
            display_width,
        ));
        window_checkpoints.push((display_width, text_byte_offset));
        *last_checkpoint = byte_offset;
    }
}

fn append_source_character(character: char, text: &mut String, display_width: &mut usize) {
    if character == '\t' {
        let spaces = 4 - (*display_width % 4);
        text.extend(std::iter::repeat_n(' ', spaces));
        *display_width += spaces;
    } else if is_forbidden_display_character(character) {
        let visible = format!("<U+{:04X}>", character as u32);
        *display_width += visible.len();
        text.push_str(&visible);
    } else {
        text.push(character);
        *display_width += character_width(character);
    }
}

fn is_forbidden_display_character(character: char) -> bool {
    character.is_control()
        || character == '\u{200b}'
        || character == '\u{feff}'
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{206f}'
        )
        || (character.is_whitespace() && !matches!(character, ' ' | '\t' | '\n' | '\r'))
}

fn character_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(1)
}

fn display_width_after(mut bytes: &[u8], mut width: usize) -> usize {
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                for character in valid.chars() {
                    if character == '\t' {
                        width += 4 - (width % 4);
                    } else if is_forbidden_display_character(character) {
                        width += format!("<U+{:04X}>", character as u32).len();
                    } else {
                        width += character_width(character);
                    }
                }
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    let valid = std::str::from_utf8(&bytes[..valid_len])
                        .expect("valid_up_to identifies a valid UTF-8 prefix");
                    for character in valid.chars() {
                        if character == '\t' {
                            width += 4 - (width % 4);
                        } else if is_forbidden_display_character(character) {
                            width += format!("<U+{:04X}>", character as u32).len();
                        } else {
                            width += character_width(character);
                        }
                    }
                }
                let invalid_len = error
                    .error_len()
                    .unwrap_or_else(|| bytes.len().saturating_sub(valid_len));
                width += character_width('\u{fffd}');
                bytes = &bytes[valid_len + invalid_len..];
            }
        }
    }
    width
}

fn unformatted_file_diagnostic(
    source: &Arc<[u8]>,
    formatted: &[u8],
    source_path: &str,
    include_source_line: bool,
) -> CliDiagnostic {
    let mut input_lines = source.split_inclusive(|byte| *byte == b'\n');
    let mut output_lines = formatted.split_inclusive(|byte| *byte == b'\n');
    let mut line_index = 0;
    let expected = loop {
        match (input_lines.next(), output_lines.next()) {
            (Some(input), Some(output)) if input == output => line_index += 1,
            (_, output) => break output,
        }
    };
    let (start, end) = source_line_span(source, line_index);
    let expected = expected
        .map(|line| String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned());
    let mut source_index = CliSourceIndex::new(Arc::clone(source));
    CliDiagnostic {
        code: "UNFORMATTED_FILE",
        message: "file is not formatted".to_owned(),
        primary: Some(source_index.location(source_path, start..end, include_source_line)),
        primary_label: String::new(),
        argument_index: None,
        argument: String::new(),
        help: Some("run `sai fmt <path>` to format this file".to_owned()),
        data: Some(format!(
            "{{\"line\":{},\"expected\":{}}}",
            line_index + 1,
            expected.map_or_else(
                || "null".to_owned(),
                |line| format!("\"{}\"", escape_json(&line))
            )
        )),
    }
}

fn source_line_span(source: &[u8], line_index: usize) -> (usize, usize) {
    let mut start = 0;
    for _ in 0..line_index {
        let Some(newline) = source[start..].iter().position(|byte| *byte == b'\n') else {
            return (source.len(), source.len());
        };
        start += newline + 1;
    }
    if start >= source.len() {
        return (source.len(), source.len());
    }
    let end = source[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(source.len(), |index| start + index);
    let content_end = if end > start && source[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    (start, content_end)
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
        primary: None,
        primary_label: String::new(),
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
        primary: None,
        primary_label: String::new(),
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
                primary: None,
                primary_label: String::new(),
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
                primary: None,
                primary_label: String::new(),
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
                primary: None,
                primary_label: String::new(),
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
        primary: None,
        primary_label: String::new(),
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
    let mut rendered = String::new();
    for diagnostic in diagnostics {
        let snippets = CliSnippets {
            diagnostic,
            related: &[],
            children: &[],
        };
        match sai_diagnostics::Code::from_name(diagnostic.code) {
            Some(code) => {
                let shared = shared_diagnostic(diagnostic, code);
                rendered.push_str(&sai_diagnostics::render_diagnostic(
                    &shared, &snippets, color,
                ));
            }
            None => {
                // The registry lacks some CLI-owned codes, such as WORKSPACE_LOCK_FAILED, which
                // impl/13 §13.6.7 defines but Appendix A does not list. They keep their own text
                // in the heading, and the shared layout is reused with a registered placeholder.
                let shared = shared_diagnostic(diagnostic, UNREGISTERED_CODE_PLACEHOLDER);
                let text = sai_diagnostics::render_diagnostic(&shared, &snippets, color);
                rendered.push_str(&text.replacen(
                    UNREGISTERED_CODE_PLACEHOLDER.as_str(),
                    diagnostic.code,
                    1,
                ));
            }
        }
    }
    if show_summary && total_error_count > 0 {
        let counts = sai_diagnostics::SummaryCounts {
            errors: total_error_count,
            hidden_errors: total_error_count.saturating_sub(diagnostics.len()),
            max_errors,
            ..sai_diagnostics::SummaryCounts::default()
        };
        rendered.push_str(&sai_diagnostics::render_summary(
            command_name,
            counts,
            color,
        ));
    }
    writer.write_all(rendered.as_bytes())
}

/// The registered code that stands in for a CLI code missing from the registry.
const UNREGISTERED_CODE_PLACEHOLDER: sai_diagnostics::Code =
    sai_diagnostics::Code::InternalCompilerError;

/// Converts a CLI diagnostic into the shared model, whose renderer owns the human layout.
fn shared_diagnostic(
    diagnostic: &CliDiagnostic,
    code: sai_diagnostics::Code,
) -> sai_diagnostics::Diagnostic {
    shared_diagnostic_with_severity(diagnostic, code, sai_diagnostics::Severity::Error)
}

fn shared_diagnostic_with_severity(
    diagnostic: &CliDiagnostic,
    code: sai_diagnostics::Code,
    severity: sai_diagnostics::Severity,
) -> sai_diagnostics::Diagnostic {
    let primary = diagnostic.primary.as_ref().map(|location| {
        sai_diagnostics::Location::new(
            location.path.clone(),
            cli_span(location.start.byte, location.end.byte),
        )
    });
    let mut shared =
        sai_diagnostics::Diagnostic::new(code, severity, diagnostic.message.clone(), primary);
    shared.primary_label = diagnostic.primary_label.clone();
    if let Some(help) = &diagnostic.help {
        shared
            .push_child(sai_diagnostics::Child {
                kind: sai_diagnostics::ChildKind::Help,
                message: help.clone(),
                location: None,
            })
            .expect("a diagnostic without a workspace snapshot accepts children");
    }
    shared
}

fn shared_check_diagnostic(
    diagnostic: &CheckDiagnostic,
    code: sai_diagnostics::Code,
) -> sai_diagnostics::Diagnostic {
    let mut shared =
        shared_diagnostic_with_severity(&diagnostic.diagnostic, code, diagnostic.severity);
    for related in &diagnostic.related {
        let location = sai_diagnostics::Location::new(
            related.location.path.clone(),
            cli_span(related.location.start.byte, related.location.end.byte),
        );
        shared
            .push_related(sai_diagnostics::Related::DeclaredHere {
                location,
                what: related.what.clone(),
            })
            .expect("a diagnostic without a workspace snapshot accepts related locations");
    }
    for child in &diagnostic.children {
        let location = child.location.as_ref().map(|location| {
            sai_diagnostics::Location::new(
                location.path.clone(),
                cli_span(location.start.byte, location.end.byte),
            )
        });
        shared
            .push_child(sai_diagnostics::Child {
                kind: child.kind,
                message: child.message.clone(),
                location,
            })
            .expect("a diagnostic without a workspace snapshot accepts children");
    }
    shared
}

/// A span over the CLI's source offsets. The shared renderer identifies files by path, so every
/// CLI location uses the same placeholder file identity.
fn cli_span(start: usize, end: usize) -> sai_diagnostics::Span {
    let offset = |value: usize| {
        sai_diagnostics::TextSize::new(u32::try_from(value).expect("source offsets fit in u32"))
    };
    let range =
        sai_diagnostics::TextRange::new(offset(start), offset(end)).expect("CLI spans are ordered");
    sai_diagnostics::Span::new(sai_diagnostics::FileId::new(0), range)
}

/// Supplies the positions and snippet rows of one CLI diagnostic from the source lines that the
/// CLI rendered when it built the diagnostic.
struct CliSnippets<'a> {
    diagnostic: &'a CliDiagnostic,
    related: &'a [CheckRelated],
    children: &'a [CheckChild],
}

impl sai_diagnostics::SourceSnippets for CliSnippets<'_> {
    fn position(&self, location: &sai_diagnostics::Location) -> Option<(usize, usize)> {
        let start = usize::try_from(location.span.range.start().raw()).ok()?;
        if let Some(primary) = self.diagnostic.primary.as_ref()
            && primary.path == location.file.path
            && primary.start.byte == start
        {
            return Some((primary.start.line, primary.start.col));
        }
        self.related
            .iter()
            .find_map(|related| {
                (related.location.path == location.file.path
                    && related.location.start.byte == start)
                    .then_some((related.location.start.line, related.location.start.col))
            })
            .or_else(|| {
                self.children.iter().find_map(|child| {
                    child.location.as_ref().and_then(|child_location| {
                        (child_location.path == location.file.path
                            && child_location.start.byte == start)
                            .then_some((child_location.start.line, child_location.start.col))
                    })
                })
            })
    }

    fn snippet(
        &self,
        file: &sai_diagnostics::SourceRef,
        annotations: &[sai_diagnostics::SnippetAnnotation],
    ) -> Option<sai_diagnostics::Snippet> {
        let location = self.diagnostic.primary.as_ref()?;
        // The CLI attaches no labels, so only its primary snippet has rows.
        let [only] = annotations else {
            return None;
        };
        if location.path != file.path || !only.primary {
            return None;
        }
        let source_line = location.source_line.as_ref()?;
        let label = self.diagnostic.primary_label.as_str();
        let single_line = location.end.line == location.start.line;
        let end_col = if single_line {
            location.end_display_col
        } else {
            source_line.display_width
        };
        let mut rows = vec![snippet_row(
            location.start.line,
            source_line,
            Some((location.start_display_col, end_col)),
            single_line.then_some(label),
        )];
        for row in &location.additional_source_lines {
            rows.push(match row {
                CliAdditionalSnippetRow::Gap => sai_diagnostics::SnippetRow::Gap,
                CliAdditionalSnippetRow::Source {
                    line_number,
                    source_line,
                    annotation,
                } => snippet_row(
                    *line_number,
                    source_line,
                    *annotation,
                    (*line_number == location.end.line).then_some(label),
                ),
            });
        }
        Some(sai_diagnostics::Snippet { rows })
    }
}

/// One source row with its primary mark, drawn with the CLI's display-column rules.
fn snippet_row(
    line_number: usize,
    source_line: &RenderedSourceLine,
    annotation: Option<(usize, usize)>,
    label: Option<&str>,
) -> sai_diagnostics::SnippetRow {
    let (annotation_start, annotation_end) = annotation.unwrap_or_default();
    let (row, window_start, left_clipped, right_clipped, row_width) =
        source_line.window(annotation_start, annotation_end);
    let left_marker = if left_clipped { "..." } else { "" };
    let right_marker = if right_clipped { "..." } else { "" };
    let marks = annotation
        .map(|(annotation_start, annotation_end)| {
            let start_col = annotation_start.max(window_start);
            let row_end = window_start + row_width;
            let visible_end = annotation_end.min(row_end);
            let mark_col = start_col.saturating_sub(window_start).min(row_width);
            let mark_width = if annotation_start == annotation_end {
                1
            } else {
                visible_end
                    .saturating_sub(start_col)
                    .max(1)
                    .min(row_width.saturating_sub(mark_col).max(1))
            };
            sai_diagnostics::SnippetMark {
                column: usize::from(left_clipped) * 3 + mark_col,
                width: mark_width,
                primary: true,
                message: label.unwrap_or_default().to_owned(),
            }
        })
        .into_iter()
        .collect();
    sai_diagnostics::SnippetRow::Line {
        number: line_number,
        text: format!("{left_marker}{row}{right_marker}"),
        marks,
    }
}

fn write_json_success_header(writer: &mut impl Write, command_name: &str) -> io::Result<()> {
    writeln!(
        writer,
        "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},\"command\":\"{}\",\"root\":null}}",
        env!("CARGO_PKG_VERSION"),
        escape_json(command_name)
    )
}

fn write_json_check_header(writer: &mut impl Write, root_identity: Option<&str>) -> io::Result<()> {
    let root = root_identity.map_or_else(
        || "null".to_owned(),
        |identity| format!("\"{}\"", escape_json(identity)),
    );
    writeln!(
        writer,
        "{{\"type\":\"header\",\"format\":\"sai-diagnostics\",\"version\":1,\"tool\":{{\"name\":\"sai\",\"version\":\"{}\"}},\"command\":\"check\",\"root\":{root}}}",
        env!("CARGO_PKG_VERSION")
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
    write_json_diagnostic_records(writer, diagnostics)?;
    write_json_summary(writer, total_error_count, diagnostics.len(), exit_code)
}

fn write_json_diagnostic_records(
    writer: &mut impl Write,
    diagnostics: &[CliDiagnostic],
) -> io::Result<()> {
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
        let primary = diagnostic
            .primary
            .as_ref()
            .map_or_else(|| "null".to_owned(), json_location);
        writeln!(
            writer,
            "{{\"type\":\"diagnostic\",\"code\":\"{}\",\"severity\":\"error\",\"level\":null,\"message\":\"{}\",\"primary\":{},\"primary_label\":\"{}\",\"labels\":[],\"children\":{},\"fixes\":[],\"related\":[],\"data\":{},\"node\":null,\"symbol\":null,\"package\":null}}",
            diagnostic.code,
            escape_json(&diagnostic.message),
            primary,
            escape_json(&diagnostic.primary_label),
            children,
            data
        )?;
    }
    Ok(())
}

fn write_json_summary(
    writer: &mut impl Write,
    total_error_count: usize,
    shown_error_count: usize,
    exit_code: i32,
) -> io::Result<()> {
    writeln!(
        writer,
        "{{\"type\":\"summary\",\"errors\":{},\"warnings\":0,\"infos\":0,\"shown_errors\":{},\"suppressed\":{{\"cascade\":0,\"duplicate\":0,\"dependency\":0}},\"exit_code\":{exit_code}}}",
        total_error_count, shown_error_count
    )
}

fn json_location(location: &CliLocation) -> String {
    json_location_with_package(location, None)
}

fn json_location_with_package(location: &CliLocation, package: Option<&str>) -> String {
    let package = package.map_or_else(
        || "null".to_owned(),
        |identity| format!("\"{}\"", escape_json(identity)),
    );
    format!(
        "{{\"file\":{{\"path\":\"{}\",\"package\":{package},\"snapshot\":null}},\"start\":{},\"end\":{}}}",
        escape_json(&location.path),
        json_position(location.start),
        json_position(location.end)
    )
}

fn json_position(position: CliPosition) -> String {
    format!(
        "{{\"byte\":{},\"line\":{},\"col\":{},\"utf16_col\":{}}}",
        position.byte, position.line, position.col, position.utf16_col
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

fn fmt_human_changed_path(file: &SelectedFmtFile, workspace_discovery: bool) -> String {
    if workspace_discovery {
        human_file_path(&file.source_path)
    } else {
        human_file_path(&file.path.to_string_lossy())
    }
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
    use std::io::{self, Read};
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        CancellationAwareStdin, CheckDiagnostic, CliDiagnostic, CliLocation, CliPosition,
        CliSourceIndex, EXPLAIN_CODE_SUMMARIES, RenderedSourceLine, StdinBroker, StdinReadError,
        cli_position, compare_check_diagnostics, fmt_path_selection_signature, human_file_path,
        internal_fmt_diagnostic, parse_args, relative_path, report_fmt_diagnostics, run,
        run_new_with_creator, run_with_cancellation, run_with_cancellation_and_input,
        write_file_atomically_in_directory, write_human_diagnostics,
    };
    use sai_diagnostics::{Value, ValueObject};
    use sai_driver::CancellationToken;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    struct CancellingReader {
        cancellation: CancellationToken,
        reads: usize,
    }

    impl std::io::Read for CancellingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads == 1 {
                buffer[0] = b'p';
                self.cancellation.cancel();
                Ok(1)
            } else {
                Err(io::Error::other("read after cancellation"))
            }
        }
    }

    struct BlockingReader {
        started: Option<mpsc::Sender<()>>,
        released: mpsc::Receiver<()>,
        finished: Option<mpsc::Sender<()>>,
    }

    impl std::io::Read for BlockingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            self.released
                .recv()
                .map_err(|_| io::Error::other("blocking reader release was dropped"))?;
            if let Some(finished) = self.finished.take() {
                let _ = finished.send(());
            }
            Ok(0)
        }
    }

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

    fn create_check_package(temporary: &TestDirectory) -> PathBuf {
        let directory = temporary.child("package");
        sai_package::new_package::create_new_package(&directory, "acme:check", &[])
            .expect("create package fixture");
        directory
    }

    fn check_diagnostic_lines<'a>(stdout: &'a str, code: &str) -> Vec<&'a str> {
        let marker = format!("\"code\":\"{code}\"");
        stdout
            .lines()
            .filter(|line| line.contains(&marker))
            .collect()
    }

    fn set_check_targets(package: &Path, targets: &str) {
        let manifest_path = package.join("sai.package");
        let manifest = fs::read_to_string(&manifest_path).expect("read package manifest fixture");
        let updated = manifest.replace(
            "targets { wasm32-wasi }",
            &format!("targets {{ {targets} }}"),
        );
        assert_ne!(manifest, updated, "target declaration is present");
        fs::write(manifest_path, updated).expect("write target declaration");
    }

    fn check_diagnostic_for_order(
        code: &'static str,
        severity: sai_diagnostics::Severity,
        path: Option<&str>,
        start: usize,
        end: usize,
        message: &str,
        help: Option<&str>,
        argument: &str,
    ) -> CheckDiagnostic {
        let primary = path.map(|path| CliLocation {
            path: path.to_owned(),
            start: CliPosition {
                byte: start,
                line: 1,
                col: start + 1,
                utf16_col: start + 1,
            },
            end: CliPosition {
                byte: end,
                line: 1,
                col: end + 1,
                utf16_col: end + 1,
            },
            source_line: None,
            additional_source_lines: Vec::new(),
            start_display_col: start,
            end_display_col: end,
        });
        let package = primary.as_ref().map(|_| "acme:check@0.1.0".to_owned());
        CheckDiagnostic {
            diagnostic: CliDiagnostic {
                code,
                message: message.to_owned(),
                primary,
                primary_label: String::new(),
                argument_index: None,
                argument: argument.to_owned(),
                help: help.map(str::to_owned),
                data: None,
            },
            severity,
            package,
            related: Vec::new(),
            children: Vec::new(),
        }
    }

    #[test]
    fn package_owned_locationless_diagnostic_keeps_identity_in_json() {
        let mut diagnostic = check_diagnostic_for_order(
            "SOURCE_READ_FAILED",
            sai_diagnostics::Severity::Error,
            None,
            0,
            0,
            "could not read a source file",
            None,
            "",
        );
        diagnostic.package = Some("acme:check@0.1.0".to_owned());

        let record = String::from_utf8(super::check_diagnostic_record_bytes(&diagnostic))
            .expect("JSON record is UTF-8");

        assert!(record.contains("\"primary\":null"));
        assert!(record.contains("\"package\":\"acme:check@0.1.0\"}"));
        assert!(!record.contains("\"file\":"));
    }

    #[test]
    fn check_diagnostics_sort_by_location_class_then_stably_by_record() {
        let mut diagnostics = vec![
            check_diagnostic_for_order(
                "SOURCE",
                sai_diagnostics::Severity::Error,
                Some("src/z.sai"),
                0,
                1,
                "source",
                None,
                "",
            ),
            check_diagnostic_for_order(
                "LOCK",
                sai_diagnostics::Severity::Error,
                Some("sai.lock"),
                0,
                1,
                "lock",
                None,
                "",
            ),
            check_diagnostic_for_order(
                "MANIFEST_LATE",
                sai_diagnostics::Severity::Error,
                Some("sai.package"),
                8,
                9,
                "manifest late",
                None,
                "",
            ),
            check_diagnostic_for_order(
                "MANIFEST_EARLY",
                sai_diagnostics::Severity::Error,
                Some("sai.package"),
                2,
                3,
                "manifest early",
                None,
                "",
            ),
            check_diagnostic_for_order(
                "LATE_NULL",
                sai_diagnostics::Severity::Error,
                None,
                0,
                0,
                "locationless",
                None,
                "z",
            ),
            check_diagnostic_for_order(
                "SAME",
                sai_diagnostics::Severity::Warning,
                Some("src/lib.sai"),
                4,
                5,
                "same location",
                None,
                "",
            ),
            check_diagnostic_for_order(
                "SAME",
                sai_diagnostics::Severity::Error,
                Some("src/lib.sai"),
                4,
                5,
                "same location",
                None,
                "",
            ),
            check_diagnostic_for_order(
                "TIE",
                sai_diagnostics::Severity::Error,
                Some("src/lib.sai"),
                6,
                7,
                "same record prefix",
                Some("z"),
                "",
            ),
            check_diagnostic_for_order(
                "TIE",
                sai_diagnostics::Severity::Error,
                Some("src/lib.sai"),
                6,
                7,
                "same record prefix",
                Some("a"),
                "",
            ),
        ];

        diagnostics.sort_by(compare_check_diagnostics);

        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            [
                "LATE_NULL",
                "MANIFEST_EARLY",
                "MANIFEST_LATE",
                "LOCK",
                "SAME",
                "SAME",
                "TIE",
                "TIE",
                "SOURCE",
            ]
        );
        assert_eq!(diagnostics[4].severity, sai_diagnostics::Severity::Error);
        assert_eq!(diagnostics[5].severity, sai_diagnostics::Severity::Warning);
        assert_eq!(diagnostics[6].diagnostic.help.as_deref(), Some("a"));
        assert_eq!(diagnostics[7].diagnostic.help.as_deref(), Some("z"));
    }

    fn invoke_with_stdin(arguments: &[&str], stdin: &[u8]) -> (i32, String, String) {
        let arguments = arguments
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        let mut input = io::Cursor::new(stdin);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = run_with_cancellation_and_input(
            &arguments,
            &CancellationToken::default(),
            &mut input,
            &mut stdout,
            &mut stderr,
            false,
        );
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
    fn check_syntax_parses_valid_package_but_reports_incomplete_analysis() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let source_path = package.join("src/lib.sai");
        let original_source = fs::read(&source_path).expect("read valid source fixture");
        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--tests",
            "--fix",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(stdout.contains("\"stage\":0,\"feature\":\"sai check\""));
        assert!(stdout.contains("\"root\":\"acme:check@0.1.0\""));
        assert!(stdout.contains(
            "syntax parsing, module-tree validation, target filtering, Phase 1 declarations, and core export-name checks are implemented"
        ));
        assert!(stdout.contains("module-tree validation"));
        assert!(stdout.contains("test-only analysis"));
        assert!(stdout.contains("`--fix`"));
        assert!(stdout.contains("\"command\":\"check\""));
        assert!(stdout.contains("\"exit_code\":1"));
        assert!(!stdout.contains("\"type\":\"result\""));
        assert_eq!(
            fs::read(&source_path).expect("source remains unchanged"),
            original_source
        );
    }

    #[test]
    fn check_skips_malformed_source_in_hidden_subdirectories() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let hidden_directory = package.join("src/.hidden");
        fs::create_dir_all(&hidden_directory).expect("create hidden source directory");
        fs::write(hidden_directory.join("hidden.sai"), b"fn {\n")
            .expect("write malformed hidden source");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(!stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(!stdout.contains(".hidden/hidden.sai"));
    }

    #[test]
    fn check_reports_parse_error_location_and_incomplete_analysis_in_json() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(package.join("src/lib.sai"), b"fn {\n").expect("write malformed source");
        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(stdout.contains("\"path\":\"src/lib.sai\""));
        assert!(stdout.contains("\"line\":1"));
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(stdout.contains(
            "syntax parsing, module-tree validation, target filtering, Phase 1 declarations, and core export-name checks are implemented"
        ));
        assert!(stdout.contains("\"exit_code\":1"));

        let parse_diagnostic = stdout
            .lines()
            .find(|line| line.contains("\"code\":\"EXPECTED_TOKEN\""))
            .expect("source parse diagnostic is present");
        assert!(parse_diagnostic.contains(
            "\"primary\":{\"file\":{\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\",\"snapshot\":null}"
        ));
        assert!(parse_diagnostic.contains("\"symbol\":null,\"package\":\"acme:check@0.1.0\"}"));

        let incomplete_diagnostic = stdout
            .lines()
            .find(|line| line.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""))
            .expect("incomplete-analysis diagnostic is present");
        assert!(incomplete_diagnostic.contains("\"primary\":null"));
        assert!(
            incomplete_diagnostic.contains(
                "syntax parsing, module-tree validation, target filtering, Phase 1 declarations, and core export-name checks are implemented"
            )
        );
        assert!(incomplete_diagnostic.contains("\"symbol\":null,\"package\":null}"));
    }

    #[test]
    fn check_reports_phase1_duplicate_binding_with_first_declaration_related_location() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(
            package.join("src/lib.sai"),
            b"fn repeated() {}\nfn repeated() {}\n",
        )
        .expect("write duplicate declarations");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let duplicate = stdout
            .lines()
            .find(|line| line.contains("\"code\":\"DUPLICATE_BINDING\""))
            .expect("Phase 1 duplicate binding is reported");
        assert!(duplicate.contains("\"path\":\"src/lib.sai\",\"package\":\"acme:check@0.1.0\""));
        assert!(duplicate.contains("\"start\":{\"byte\":20,\"line\":2"));
        assert!(duplicate.contains("\"related\":[{\"kind\":\"declared_here\""));
        assert!(duplicate.contains("\"what\":\"first declaration\""));
        assert!(duplicate.contains("\"start\":{\"byte\":3,\"line\":1"));
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_resolves_local_root_and_nested_module_targets_and_rejects_current_modules() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let root_source = "use acme:check\nuse acme:check/model\n";
        let model_source = "use acme:check\nuse acme:check/model\n";
        fs::write(package.join("src/lib.sai"), root_source).expect("write root uses");
        fs::write(package.join("src/model.sai"), model_source).expect("write nested uses");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let current_module = check_diagnostic_lines(&stdout, "USE_OF_CURRENT_MODULE");
        assert_eq!(current_module.len(), 2, "{stdout}");
        assert!(current_module.iter().any(|line| {
            let start = root_source.find("acme:check").expect("root self-use");
            line.contains("\"path\":\"src/lib.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{start},"))
        }));
        assert!(current_module.iter().any(|line| {
            let start = model_source
                .rfind("acme:check/model")
                .expect("nested self-use");
            line.contains("\"path\":\"src/model.sai\"")
                && line.contains(&format!("\"start\":{{\"byte\":{start},"))
        }));
        assert!(
            check_diagnostic_lines(&stdout, "UNKNOWN_MODULE").is_empty(),
            "{stdout}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_binds_local_module_aliases_and_reports_scope_conflicts() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let source = concat!(
            "use acme:check/model\n",
            "use acme:check/model\n",
            "fn clash() {}\n",
            "use acme:check/model as clash\n",
            "interface codec {}\n",
            "use acme:check/codec as api\n",
            "use acme:check/codec as api\n",
            "use acme:check/panic\n",
            "use acme:check/unreachable\n",
            "use acme:check/type\n",
            "use other:package/type\n",
            "pub use acme:check/model\n",
            "pub use acme:check/missing\n",
            "pub use other:package/remote\n",
            "pub use acme:check/Some\n",
            "pub use\n",
            "use acme:check/model.{child}\n",
        );
        fs::write(package.join("src/lib.sai"), source).expect("write module aliases");
        fs::write(package.join("src/model.sai"), "fn child() {}\n").expect("write imported module");
        fs::write(package.join("src/panic.sai"), "fn recover() {}\n")
            .expect("write module that shadows a prelude name");
        fs::write(package.join("src/unreachable.sai"), "fn fail() {}\n")
            .expect("write another module that shadows a prelude name");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let redundant = check_diagnostic_lines(&stdout, "REDUNDANT_IMPORT");
        assert_eq!(redundant.len(), 2, "{stdout}");
        assert!(
            redundant[0].contains("\"severity\":\"warning\""),
            "{stdout}"
        );
        let duplicate = check_diagnostic_lines(&stdout, "DUPLICATE_BINDING");
        assert_eq!(duplicate.len(), 1, "{stdout}");
        let clash_name_start = source.find("as clash").expect("explicit alias") + "as ".len();
        assert!(
            duplicate[0].contains("\"path\":\"src/lib.sai\"")
                && duplicate[0].contains(&format!("\"start\":{{\"byte\":{clash_name_start},")),
            "{stdout}"
        );
        assert!(
            duplicate[0].contains("\"related\":[{\"kind\":\"declared_here\""),
            "{stdout}"
        );
        let pub_use_without_items = check_diagnostic_lines(&stdout, "PUB_USE_WITHOUT_ITEMS");
        assert_eq!(pub_use_without_items.len(), 5, "{stdout}");
        let missing_path_start = source.rfind("pub use\n").expect("missing package path");
        assert!(
            pub_use_without_items.iter().any(|line| {
                line.contains(&format!("\"start\":{{\"byte\":{missing_path_start},"))
            }),
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "UNKNOWN_MODULE").len(),
            2,
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "USE_BINDING_IS_KEYWORD").len(),
            2,
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "SHADOWS_PRELUDE").len(),
            2,
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "MALFORMED_PACKAGE_PATH").len(),
            1,
            "{stdout}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "UNKNOWN_PACKAGE").is_empty(),
            "{stdout}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_reports_prelude_shadow_warnings_for_module_declarations() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let source = concat!(
            "type Option {}\n",
            "fn panic() {}\n",
            "@target(\"wasm64-core\")\n",
            "type Vec {}\n",
        );
        fs::write(package.join("src/lib.sai"), source).expect("write prelude shadows");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1, "{stdout}");
        assert!(stderr.is_empty());
        assert_eq!(
            check_diagnostic_lines(&stdout, "FEATURE_NOT_IMPLEMENTED").len(),
            1,
            "{stdout}"
        );
        let shadows = check_diagnostic_lines(&stdout, "SHADOWS_PRELUDE");
        assert_eq!(shadows.len(), 2, "{stdout}");
        for (line, name) in shadows.iter().zip(["Option", "panic"]) {
            let start = source.find(name).expect("shadow name is present");
            assert!(
                line.contains("\"severity\":\"warning\"")
                    && line.contains("\"path\":\"src/lib.sai\"")
                    && line.contains(&format!("\"start\":{{\"byte\":{start},")),
                "{line}"
            );
        }
        assert!(check_diagnostic_lines(&stdout, "RESERVED_TYPE_NAME").is_empty());
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_resolves_local_item_imports_and_reexports() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let root_source = concat!(
            "use acme:check/model.{PublicType, public_fn as local_fn, private_fn}\n",
            "use acme:check/bridge.{public_fn as chained_fn}\n",
            "use acme:check/model.{public_fn as local_fn}\n",
            "use acme:check/model.{public_fn as clash}\n",
            "fn clash() {}\n",
            "use acme:check/model.{PublicType as publicType}\n",
            "use acme:check/model.{public_fn as repeated, public_fn as repeated}\n",
            "use acme:check/model.{public_fn as same, private_fn as same}\n",
            "use acme:check/model.{Missing}\n",
            "pub use acme:check/model.{public_fn as public_api}\n",
            "pub use acme:check/model.{private_fn as private_api}\n",
            "pub use acme:check/model as public_fn.{public_fn}\n",
        );
        fs::write(package.join("src/lib.sai"), root_source).expect("write item imports");
        fs::write(
            package.join("src/model.sai"),
            concat!(
                "pub type PublicType {}\n",
                "pub fn public_fn() {}\n",
                "fn private_fn() {}\n",
            ),
        )
        .expect("write imported module");
        fs::write(
            package.join("src/facade.sai"),
            "pub use acme:check/model.{public_fn as from_model}\n",
        )
        .expect("write first re-export");
        fs::write(
            package.join("src/bridge.sai"),
            "pub use acme:check/facade.{from_model as public_fn}\n",
        )
        .expect("write re-export chain");
        fs::write(
            package.join("src/consumer.sai"),
            "use acme:check.{public_api, public_fn as forwarded_fn}\n",
        )
        .expect("write external re-export consumer");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(
            check_diagnostic_lines(&stdout, "REDUNDANT_IMPORT").len(),
            2,
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "DUPLICATE_BINDING").len(),
            3,
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "ALIAS_CASE_MISMATCH").len(),
            1,
            "{stdout}"
        );
        let unresolved = check_diagnostic_lines(&stdout, "UNRESOLVED_IMPORT");
        assert_eq!(unresolved.len(), 1, "{stdout}");
        let missing_name_start = root_source
            .find("Missing")
            .expect("missing item import is retained");
        assert!(
            unresolved[0].contains(&format!("\"start\":{{\"byte\":{missing_name_start},")),
            "{stdout}"
        );
        assert_eq!(
            check_diagnostic_lines(&stdout, "PRIVATE_REEXPORT").len(),
            1,
            "{stdout}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "PRIVATE_ITEM").is_empty(),
            "{stdout}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "UNKNOWN_MODULE").is_empty(),
            "{stdout}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_reports_local_pub_use_cycles_once() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(package.join("src/lib.sai"), "use acme:check/a.{from_b}\n")
            .expect("write cycle entry point");
        fs::write(
            package.join("src/a.sai"),
            "pub use acme:check/b.{from_a as from_b}\n",
        )
        .expect("write first cycle edge");
        fs::write(
            package.join("src/b.sai"),
            "pub use acme:check/a.{from_b as from_a}\n",
        )
        .expect("write second cycle edge");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let cycles = check_diagnostic_lines(&stdout, "REEXPORT_CYCLE");
        assert_eq!(cycles.len(), 1, "{stdout}");
        assert!(
            cycles[0].contains("\"path\":\"src/a.sai\"")
                && cycles[0].contains("\"path\":\"src/b.sai\""),
            "{stdout}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "UNRESOLVED_IMPORT").is_empty(),
            "{stdout}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "UNKNOWN_MODULE").is_empty(),
            "{stdout}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_reports_local_unknown_module_candidates_and_full_pkg_path_span() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let source = concat!(
            "use acme:check/absent\n",
            "use acme:check/area/absent\n",
            "use acme:check/model/absent\n",
        );
        fs::write(package.join("src/lib.sai"), source).expect("write unknown module use");
        // Reverse creation order so the JSON candidate order proves it is based
        // on source spelling rather than filesystem creation order.
        fs::write(package.join("src/zeta.sai"), "fn zeta() {}\n").expect("write zeta module");
        fs::write(package.join("src/alpha.sai"), "fn alpha() {}\n").expect("write alpha module");
        fs::create_dir_all(package.join("src/area")).expect("create namespace directory");
        fs::write(package.join("src/area/child.sai"), "fn child() {}\n")
            .expect("write namespace child module");
        fs::write(package.join("src/model.sai"), "fn model() {}\n")
            .expect("write module without child modules");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let unknown = check_diagnostic_lines(&stdout, "UNKNOWN_MODULE");
        assert_eq!(unknown.len(), 3, "{stdout}");
        let pkg_path = "acme:check/absent";
        let root_start = source.find(pkg_path).expect("root package path");
        let root_end = root_start + pkg_path.len();
        let root_unknown = unknown
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{root_start},")))
            .expect("root unknown module diagnostic");
        assert!(root_unknown.contains("\"path\":\"src/lib.sai\""));
        assert!(root_unknown.contains(&format!("\"end\":{{\"byte\":{root_end},")));
        assert!(
            root_unknown.contains(
                "\"data\":{\"matched_prefix\":[],\"module_candidates\":[\"alpha\",\"model\",\"zeta\"]}"
            ),
            "{root_unknown}"
        );

        let nested_path = "acme:check/area/absent";
        let nested_start = source.find(nested_path).expect("namespace package path");
        let nested_unknown = unknown
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{nested_start},")))
            .expect("namespace unknown module diagnostic");
        assert!(
            nested_unknown.contains(
                "\"data\":{\"matched_prefix\":[\"area\"],\"module_candidates\":[\"child\"]}"
            ),
            "{nested_unknown}"
        );
        let empty_prefix_path = "acme:check/model/absent";
        let empty_prefix_start = source.find(empty_prefix_path).expect("matched module path");
        let empty_prefix_unknown = unknown
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{empty_prefix_start},")))
            .expect("unknown module below a module with no children");
        assert!(
            empty_prefix_unknown
                .contains("\"data\":{\"matched_prefix\":[\"model\"],\"module_candidates\":[]}"),
            "{empty_prefix_unknown}"
        );
        assert!(
            empty_prefix_unknown.contains("no modules directly under `model`"),
            "{empty_prefix_unknown}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));

        let (human_exit_code, human_stdout, human_stderr) =
            invoke(&["--format=human", "--directory", package_path, "check"]);
        assert_eq!(human_exit_code, 1);
        let human_output = format!("{human_stdout}{human_stderr}");
        assert!(
            human_output.contains("no modules directly under `model`"),
            "{human_output}"
        );
    }

    #[test]
    fn check_adds_root_and_kebab_path_help_without_normalizing_module_segments() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let source = concat!(
            "use acme:check/lib\n",
            "use acme:check/some-module\n",
            "use acme:check/lib/some-module\n",
        );
        fs::write(package.join("src/lib.sai"), source).expect("write invalid path uses");
        fs::write(package.join("src/some_module.sai"), "fn some_module() {}\n")
            .expect("write snake-case module");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let unknown = check_diagnostic_lines(&stdout, "UNKNOWN_MODULE");
        assert_eq!(unknown.len(), 3, "{stdout}");
        let root_start = source.find("acme:check/lib").expect("root path");
        let root_help = unknown
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{root_start},")))
            .expect("`lib` path diagnostic");
        assert!(
            root_help.contains(
                "\"kind\":\"help\",\"message\":\"the root module is named by the package identity alone\""
            ),
            "{root_help}"
        );
        let kebab_start = source.find("acme:check/some-module").expect("kebab path");
        let kebab_help = unknown
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{kebab_start},")))
            .expect("kebab path diagnostic");
        assert!(
            kebab_help.contains("acme:check/some_module"),
            "{kebab_help}"
        );
        let combined_start = source
            .find("acme:check/lib/some-module")
            .expect("path with both help messages");
        let combined_help = unknown
            .iter()
            .find(|line| line.contains(&format!("\"start\":{{\"byte\":{combined_start},")))
            .expect("combined help diagnostic");
        assert!(
            combined_help.contains("the root module is named by the package identity alone"),
            "{combined_help}"
        );
        assert!(
            combined_help.contains("acme:check/lib/some_module"),
            "{combined_help}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));

        let (human_exit_code, human_stdout, human_stderr) =
            invoke(&["--format=human", "--directory", package_path, "check"]);
        assert_eq!(human_exit_code, 1);
        let human_output = format!("{human_stdout}{human_stderr}");
        assert!(
            human_output.contains("help: the root module is named by the package identity alone"),
            "{human_output}"
        );
        assert!(
            human_output.contains("help: try `acme:check/lib/some_module`"),
            "{human_output}"
        );
    }

    #[test]
    fn check_keeps_test_only_imports_unresolved_without_test_analysis() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let manifest_path = package.join("sai.package");
        let mut manifest = fs::read_to_string(&manifest_path).expect("read package manifest");
        manifest.push_str("\nrequires test { acme:test-support @ ^1 }\n");
        fs::write(manifest_path, manifest).expect("declare test dependency");
        let source = concat!(
            "use acme:test-support/missing as fixtures\n",
            "use acme:test-support/missing.{Missing as fixtures}\n",
        );
        fs::write(package.join("src/lib.sai"), source).expect("write test-only imports");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(
            check_diagnostic_lines(&stdout, "DUPLICATE_BINDING").len(),
            1,
            "{stdout}"
        );
        for code in [
            "UNKNOWN_PACKAGE",
            "UNKNOWN_MODULE",
            "UNRESOLVED_IMPORT",
            "INACTIVE_DEPENDENCY",
            "TEST_DEPENDENCY_OUTSIDE_TEST",
        ] {
            assert!(
                check_diagnostic_lines(&stdout, code).is_empty(),
                "{code}: {stdout}"
            );
        }
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_reports_inactive_declared_dependency_at_its_package_path() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        set_check_targets(&package, "wasm32-core, wasm32-wasi");
        let manifest_path = package.join("sai.package");
        let mut manifest = fs::read_to_string(&manifest_path).expect("read package manifest");
        manifest.push_str("\nrequires { acme:targeted @ ^1 only { wasm32-wasi } }\n");
        fs::write(manifest_path, manifest).expect("declare target-restricted dependency");
        let source = "use acme:targeted/module.{Missing as local}\n";
        fs::write(package.join("src/lib.sai"), source).expect("write inactive dependency use");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-core",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let inactive = check_diagnostic_lines(&stdout, "INACTIVE_DEPENDENCY");
        assert_eq!(inactive.len(), 1, "{stdout}");
        let path_start = source.find("acme:targeted/module").expect("package path");
        assert!(
            inactive[0].contains("\"path\":\"src/lib.sai\"")
                && inactive[0].contains(&format!("\"start\":{{\"byte\":{path_start},")),
            "{inactive:?}"
        );
        assert!(
            inactive[0].contains("wasm32-wasi")
                && inactive[0].contains("add a matching `@target(...)`"),
            "{inactive:?}"
        );
        for code in ["UNKNOWN_PACKAGE", "UNKNOWN_MODULE", "UNRESOLVED_IMPORT"] {
            assert!(
                check_diagnostic_lines(&stdout, code).is_empty(),
                "{code}: {stdout}"
            );
        }
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_defers_interface_targets_and_skips_foreign_filtered_and_malformed_uses() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        set_check_targets(&package, "wasm32-core, wasm32-wasi");
        fs::write(package.join("src/lib.sai"), "interface codec {}\n")
            .expect("write local interface");
        let client_source = concat!(
            "use acme:check/codec\n",
            "use other:package/missing\n",
            "use acme:check/visible_missing\n",
            "@target(\"wasm32-core\")\n",
            "use acme:check/missing\n",
            "use acme:check/Some\n",
        );
        fs::write(package.join("src/client.sai"), client_source)
            .expect("write deferred and filtered uses");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-wasi",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(
            check_diagnostic_lines(&stdout, "MALFORMED_PACKAGE_PATH").len(),
            1,
            "{stdout}"
        );
        let unknown = check_diagnostic_lines(&stdout, "UNKNOWN_MODULE");
        assert_eq!(unknown.len(), 1, "{stdout}");
        let expected_start = client_source
            .find("acme:check/visible_missing")
            .expect("unfiltered local missing module");
        assert!(
            unknown[0].contains(&format!("\"start\":{{\"byte\":{expected_start},")),
            "{unknown:?}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "UNKNOWN_PACKAGE").is_empty(),
            "{stdout}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_validates_export_name_targets_on_component_targets_without_export_uniqueness() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        set_check_targets(&package, "wasm32-core, wasm32-wasi");
        fs::write(
            package.join("src/lib.sai"),
            concat!(
                "@export_name(\"ordinary\")\n",
                "fn ordinary() {}\n",
                "@export_name(\"duplicate\")\n",
                "export fn first() {}\n",
                "@export_name(\"duplicate\")\n",
                "export fn second() {}\n",
            ),
        )
        .expect("write component export-name validation fixture");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-wasi",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(
            check_diagnostic_lines(&stdout, "INVALID_ATTRIBUTE_TARGET").len(),
            1,
            "{stdout}"
        );
        assert!(
            check_diagnostic_lines(&stdout, "DUPLICATE_EXPORT_NAME").is_empty(),
            "{stdout}"
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_filters_phase1_declarations_using_selected_target_capabilities() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        set_check_targets(&package, "wasm32-core, wasm32-wasi");
        fs::write(
            package.join("src/lib.sai"),
            b"@target(host)\nfn selected() {}\nfn selected() {}\n",
        )
        .expect("write target-filtered declarations");

        let (core_exit, core_stdout, core_stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-core",
        ]);
        let (wasi_exit, wasi_stdout, wasi_stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target",
            "wasm32-wasi",
        ]);

        assert_eq!(core_exit, 1);
        assert!(core_stderr.is_empty());
        assert!(core_stdout.contains("\"code\":\"DUPLICATE_BINDING\""));
        assert_eq!(wasi_exit, 1);
        assert!(wasi_stderr.is_empty());
        assert!(!wasi_stdout.contains("\"code\":\"DUPLICATE_BINDING\""));
        assert!(!core_stdout.contains("\"code\":\"TARGET_NOT_DECLARED\""));
        assert!(!wasi_stdout.contains("\"code\":\"TARGET_NOT_DECLARED\""));
    }

    #[test]
    fn check_reports_duplicate_core_export_names_only_for_core_targets() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        set_check_targets(&package, "wasm32-core, wasm32-wasi");
        let source = concat!(
            "@export_name(\"wire\")\n",
            "export fn first() {}\n",
            "@export_name(\"wire\")\n",
            "export fn second() {}\n",
            "export fn memory() {}\n",
            "@export_name(\"invalid-target\")\n",
            "fn ordinary() {}\n",
        );
        fs::write(package.join("src/lib.sai"), source).expect("write exported functions");

        let (core_exit, core_stdout, core_stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-core",
        ]);
        let (component_exit, component_stdout, component_stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-wasi",
        ]);
        let (test_exit, test_stdout, test_stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--tests",
            "--target=wasm32-core",
        ]);
        let (test_wasi_exit, test_wasi_stdout, test_wasi_stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--tests",
            "--target=wasm32-wasi",
        ]);
        let (human_exit, _, human_stderr) =
            invoke(&["--directory", package_path, "check", "--target=wasm32-core"]);

        assert_eq!(core_exit, 1);
        assert!(core_stderr.is_empty());
        let export_diagnostics = core_stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"DUPLICATE_EXPORT_NAME\""))
            .collect::<Vec<_>>();
        assert_eq!(export_diagnostics.len(), 2, "{core_stdout}");
        let duplicate = export_diagnostics
            .iter()
            .find(|line| line.contains("export name `wire`"))
            .expect("duplicate explicit export is reported");
        let duplicate_start = source.find("\"wire\"").unwrap();
        assert!(duplicate.contains(&format!("\"start\":{{\"byte\":{duplicate_start},")));
        assert!(duplicate.contains("\"what\":\"first export with this name\""));
        let memory = export_diagnostics
            .iter()
            .find(|line| line.contains("export name `memory`"))
            .expect("reserved memory export is reported");
        assert!(
            memory.contains("linear memory is exported under the name `memory`"),
            "{memory}"
        );
        assert_eq!(human_exit, 1);
        assert!(human_stderr.contains("linear memory is exported under the name `memory`"));

        assert_eq!(component_exit, 1);
        assert!(component_stderr.is_empty());
        assert!(!component_stdout.contains("\"code\":\"DUPLICATE_EXPORT_NAME\""));
        assert_eq!(test_exit, 1);
        assert!(test_stderr.is_empty());
        assert!(!test_stdout.contains("\"code\":\"DUPLICATE_EXPORT_NAME\""));
        assert!(test_stdout.contains("\"code\":\"INVALID_ATTRIBUTE_TARGET\""));
        assert_eq!(test_wasi_exit, 1);
        assert!(test_wasi_stderr.is_empty());
        assert!(!test_wasi_stdout.contains("\"code\":\"DUPLICATE_EXPORT_NAME\""));
        assert!(test_wasi_stdout.contains("\"code\":\"INVALID_ATTRIBUTE_TARGET\""));
        assert!(!core_stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
        assert!(!component_stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_reports_a_known_target_missing_from_the_manifest() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        set_check_targets(&package, "wasm64-core");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            package_path,
            "check",
            "--target=wasm32-core",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let diagnostic = stdout
            .lines()
            .find(|line| line.contains("\"code\":\"TARGET_NOT_DECLARED\""))
            .expect("undeclared target is a package diagnostic");
        assert!(diagnostic.contains("\"message\":\"target `wasm32-core` is not declared"));
        assert!(diagnostic.contains("\"target\":\"wasm32-core\""));
        assert!(diagnostic.contains("\"package\":\"acme:check@0.1.0\""));
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_reports_root_and_nested_interface_module_file_conflicts() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(package.join("src/lib.sai"), b"interface codec {}\n")
            .expect("write root interface");
        fs::write(package.join("src/codec.sai"), b"fn root_child() {}\n")
            .expect("write root module file");
        fs::create_dir_all(package.join("src/parent")).expect("create nested module directory");
        fs::write(package.join("src/parent.sai"), b"interface child {}\n")
            .expect("write nested interface");
        fs::write(
            package.join("src/parent/child.sai"),
            b"fn nested_child() {}\n",
        )
        .expect("write nested module file");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let conflicts = stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"MODULE_INTERFACE_CONFLICT\""))
            .collect::<Vec<_>>();
        assert_eq!(conflicts.len(), 2, "{stdout}");
        assert!(
            conflicts
                .iter()
                .any(|line| line.contains("\"path\":\"src/lib.sai\""))
        );
        assert!(
            conflicts
                .iter()
                .any(|line| line.contains("\"path\":\"src/parent.sai\""))
        );
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_allows_an_interface_matching_only_a_namespace_directory() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(package.join("src/lib.sai"), b"interface codec {}\n").expect("write interface");
        fs::create_dir_all(package.join("src/codec")).expect("create namespace directory");
        fs::write(package.join("src/codec/nested.sai"), b"fn nested() {}\n")
            .expect("write nested module");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(!stdout.contains("\"code\":\"MODULE_INTERFACE_CONFLICT\""));
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
    }

    #[test]
    fn check_reports_phase1_reserved_types_and_test_name_errors() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(
            package.join("src/lib.sai"),
            concat!(
                "type I32 {}\n",
                "test \"\" {}\n",
                "test \"\\n\" {}\n",
                "test \"shared\" {}\n",
                "property \"shared\"() {}\n",
            ),
        )
        .expect("write declarations with invalid test names");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"RESERVED_TYPE_NAME\""));
        assert_eq!(stdout.matches("\"code\":\"INVALID_TEST_NAME\"").count(), 2);
        assert_eq!(
            stdout.matches("\"code\":\"DUPLICATE_TEST_NAME\"").count(),
            1
        );
        let duplicate = stdout
            .lines()
            .find(|line| line.contains("\"code\":\"DUPLICATE_TEST_NAME\""))
            .expect("cross-kind duplicate test name is reported");
        assert!(duplicate.contains("\"what\":\"first test or property with this name\""));
    }

    #[test]
    fn check_reports_module_tree_errors_and_keeps_parser_diagnostics_in_json() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(package.join("src/Bad.sai"), b"fn {\n").expect("write malformed module");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_MODULE_NAME\""));
        assert!(stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(stdout.contains("\"path\":\"Bad.sai\",\"segment\":\"Bad\""));
        assert!(stdout.contains("\"path\":\"src/Bad.sai\""));
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[cfg(unix)]
    #[test]
    fn check_reports_source_symlinks_as_locationless_module_tree_diagnostics() {
        use std::os::unix::fs::symlink;

        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        symlink(package.join("src/lib.sai"), package.join("src/linked.sai"))
            .expect("create source symlink");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let diagnostic = stdout
            .lines()
            .find(|line| line.contains("\"code\":\"SYMLINK_IN_SOURCE\""))
            .expect("source symlink diagnostic is present");
        assert!(diagnostic.contains("\"primary\":null"));
        assert!(diagnostic.contains("\"path\":\"linked.sai\""));
    }

    #[cfg(unix)]
    #[test]
    fn check_rejects_a_symlinked_source_root_without_following_it() {
        use std::os::unix::fs::symlink;

        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let source_target = package.join("source-target");
        fs::rename(package.join("src"), &source_target).expect("move source directory");
        symlink(&source_target, package.join("src")).expect("link declared source root");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let diagnostics = stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"SYMLINK_IN_SOURCE\""))
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("\"primary\":null"));
        assert!(diagnostics[0].contains("\"path\":\"src\""));
        assert!(!stdout.contains("\"code\":\"MISSING_ROOT_MODULE\""));
        assert!(!stdout.contains("\"path\":\"source-target/lib.sai\""));
    }

    #[cfg(unix)]
    #[test]
    fn check_rejects_symlink_ancestors_of_the_declared_source_root() {
        use std::os::unix::fs::symlink;

        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        let actual_directory = package.join("actual");
        fs::create_dir(&actual_directory).expect("create source target directory");
        fs::rename(package.join("src"), actual_directory.join("src"))
            .expect("move source below target directory");
        fs::write(actual_directory.join("src/lib.sai"), b"fn {\n")
            .expect("make the linked target syntactically invalid");
        symlink(&actual_directory, package.join("alias")).expect("link source ancestor");
        let manifest_path = package.join("sai.package");
        let manifest = fs::read_to_string(&manifest_path)
            .expect("read package manifest")
            .replace("source \"src\"", "source \"alias/src\"");
        fs::write(&manifest_path, manifest).expect("update source directory declaration");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let diagnostics = stdout
            .lines()
            .filter(|line| line.contains("\"code\":\"SYMLINK_IN_SOURCE\""))
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("\"primary\":null"));
        assert!(diagnostics[0].contains("\"path\":\"alias/src\""));
        assert!(!stdout.contains("\"path\":\"actual/src/lib.sai\""));
        assert!(!stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
    }

    #[cfg(unix)]
    #[test]
    fn check_reports_module_tree_inventory_paths_it_cannot_represent() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::write(package.join("src/bad\\name.sai"), b"fn valid() {}\n")
            .expect("write source with an unrepresentable package path");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"SOURCE_READ_FAILED\""));
        assert!(stdout.contains("canonical package-relative path"));
        assert!(!stdout.contains("\"code\":\"INTERNAL_COMPILER_ERROR\""));
    }

    #[test]
    fn check_without_a_package_exits_as_usage_error_without_incomplete_notice() {
        let temporary = TestDirectory::new();
        let directory = temporary.0.to_str().expect("temporary path is UTF-8");
        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", directory, "check"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"NO_PACKAGE_FOUND\""));
        assert!(!stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(stdout.contains("\"root\":null"));
        assert!(stdout.contains("\"exit_code\":2"));
    }

    #[test]
    fn check_reports_missing_root_module_at_package_header_with_expected_path() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::remove_file(package.join("src/lib.sai")).expect("remove root module");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"MISSING_ROOT_MODULE\""));
        assert_eq!(
            stdout.matches("\"code\":\"MISSING_ROOT_MODULE\"").count(),
            1
        );
        assert!(
            check_diagnostic_lines(&stdout, "UNKNOWN_MODULE").is_empty(),
            "{stdout}"
        );
        assert!(stdout.contains("\"path\":\"sai.package\""));
        assert!(stdout.contains("\"line\":1"));
        assert!(stdout.contains("\"data\":{\"path\":\"src/lib.sai\"}"));
    }

    #[test]
    fn check_traverses_dot_prefixed_declared_root_and_canonicalizes_dot_source_path() {
        let dot_root_temporary = TestDirectory::new();
        let dot_root_package = create_check_package(&dot_root_temporary);
        let source_directory = dot_root_package.join(".src");
        fs::rename(dot_root_package.join("src"), &source_directory)
            .expect("rename source directory to a dot-prefixed name");
        let manifest_path = dot_root_package.join("sai.package");
        let manifest = fs::read_to_string(&manifest_path)
            .expect("read package manifest")
            .replace("source \"src\"", "source \".src\"");
        fs::write(&manifest_path, manifest).expect("update source directory declaration");
        fs::create_dir(source_directory.join(".hidden")).expect("create hidden subdirectory");
        fs::write(source_directory.join(".hidden/bad.sai"), b"fn {\n")
            .expect("write malformed hidden module");
        let dot_root_package_path = dot_root_package.to_str().expect("package path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            dot_root_package_path,
            "check",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(!stdout.contains("\"code\":\"MISSING_ROOT_MODULE\""));
        assert!(!stdout.contains("\"code\":\"EXPECTED_TOKEN\""));

        let dot_source_temporary = TestDirectory::new();
        let dot_source_package = create_check_package(&dot_source_temporary);
        fs::remove_dir_all(dot_source_package.join("src")).expect("remove declared source");
        let manifest_path = dot_source_package.join("sai.package");
        let manifest = fs::read_to_string(&manifest_path)
            .expect("read package manifest")
            .replace("source \"src\"", "source \".\"");
        fs::write(&manifest_path, manifest).expect("update source declaration");
        let dot_source_package_path = dot_source_package.to_str().expect("package path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            dot_source_package_path,
            "check",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"MISSING_ROOT_MODULE\""));
        assert!(stdout.contains("\"data\":{\"path\":\"lib.sai\"}"));
        assert!(!stdout.contains("\"data\":{\"path\":\"./lib.sai\"}"));
    }

    #[test]
    fn check_reports_missing_and_non_directory_source_roots() {
        for use_file_as_source_directory in [false, true] {
            let temporary = TestDirectory::new();
            let package = create_check_package(&temporary);
            let package_path = package.to_str().expect("package path is UTF-8");
            let source_directory = package.join("src");
            fs::remove_dir_all(&source_directory).expect("remove source directory");
            if use_file_as_source_directory {
                fs::write(&source_directory, b"not a directory")
                    .expect("replace source directory with a file");
            }

            let (exit_code, stdout, stderr) =
                invoke(&["--format=json", "--directory", package_path, "check"]);

            assert_eq!(exit_code, 1);
            assert!(stderr.is_empty());
            assert!(stdout.contains("\"code\":\"MISSING_ROOT_MODULE\""));
            assert_eq!(
                stdout.matches("\"code\":\"MISSING_ROOT_MODULE\"").count(),
                1
            );
            assert!(stdout.contains("\"path\":\"sai.package\""));
            assert!(stdout.contains("\"line\":1"));
            assert!(stdout.contains("\"data\":{\"path\":\"src/lib.sai\"}"));
        }
    }

    #[test]
    fn check_reports_missing_root_when_source_path_crosses_a_file() {
        let temporary = TestDirectory::new();
        let package = create_check_package(&temporary);
        let package_path = package.to_str().expect("package path is UTF-8");
        fs::remove_dir_all(package.join("src")).expect("remove source directory");
        fs::write(package.join("src"), b"not a directory").expect("replace source with a file");
        let manifest_path = package.join("sai.package");
        let manifest = fs::read_to_string(&manifest_path)
            .expect("read package manifest")
            .replace("source \"src\"", "source \"src/nested\"");
        fs::write(&manifest_path, manifest).expect("update source directory declaration");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "check"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(
            stdout.matches("\"code\":\"MISSING_ROOT_MODULE\"").count(),
            1
        );
        assert!(stdout.contains("\"data\":{\"path\":\"src/nested/lib.sai\"}"));
        assert!(!stdout.contains("\"code\":\"SOURCE_READ_FAILED\""));
    }

    #[test]
    fn check_rejects_unknown_options_before_incomplete_notice() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--format=json", "--bogus"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNKNOWN_OPTION\""));
        assert!(stdout.contains("\"command\":\"check\""));
        assert!(!stdout.contains("FEATURE_NOT_IMPLEMENTED"));
    }

    #[test]
    fn check_reports_missing_option_values() {
        let (exit_code, stdout, stderr) = invoke(&["check", "--format=json", "--target"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"MISSING_ARGUMENT\""));
        assert!(stdout.contains("\"argument_index\":3"));
    }

    #[test]
    fn check_validates_common_enumerated_values() {
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

    #[cfg(unix)]
    #[test]
    fn fmt_selection_signature_detects_same_path_directory_and_file_replacements() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        let moved = directory.child("moved");
        fs::create_dir(&package).unwrap();
        fs::write(package.join("sai.package"), b"package acme:app@1.0.0\n").unwrap();

        let initial = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&package),
            &directory.0,
            &CancellationToken::default(),
        );
        let initial_signature = fmt_path_selection_signature(&initial);

        fs::rename(&package, &moved).unwrap();
        fs::create_dir(&package).unwrap();
        fs::write(package.join("sai.package"), b"package acme:app@1.0.0\n").unwrap();
        let directory_replacement = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&package),
            &directory.0,
            &CancellationToken::default(),
        );
        assert_ne!(
            fmt_path_selection_signature(&directory_replacement),
            initial_signature
        );

        let file_signature = fmt_path_selection_signature(&directory_replacement);
        let manifest = package.join("sai.package");
        let moved_manifest = package.join("old.sai.package");
        fs::rename(&manifest, &moved_manifest).unwrap();
        fs::write(&manifest, b"package acme:app@1.0.0\n").unwrap();
        let file_replacement = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&package),
            &directory.0,
            &CancellationToken::default(),
        );
        assert_ne!(
            fmt_path_selection_signature(&file_replacement),
            file_signature
        );
    }

    #[test]
    fn formatter_package_roots_deduplicate_physical_aliases_deterministically() {
        let directory = TestDirectory::new();
        let physical = super::open_fmt_package_root(&directory.0, None).unwrap();
        let mut first_alias = physical.clone();
        first_alias.path = directory.0.join("z-alias");
        let mut second_alias = physical;
        second_alias.path = directory.0.join("a-alias");

        let roots = super::normalize_fmt_package_roots(vec![first_alias, second_alias]);

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, directory.0.join("a-alias"));
    }

    #[test]
    fn fmt_formats_package_stdin_and_emits_the_json_result_contract() {
        let source = b"package acme:app@1.0.0\nsource\"src\"\n";
        let expected = "package acme:app @ 1.0.0\nsource \"src\"\n";
        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["fmt", "--stdin-kind", "package", "-"], source);

        assert_eq!(exit_code, 0);
        assert_eq!(stdout, expected);
        assert!(stderr.is_empty());

        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &["--format=json", "fmt", "--stdin-kind=package", "-"],
            source,
        );
        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"kind\":\"fmt\""));
        assert!(stdout.contains("\"check\":false,\"files\":1,\"changed\":[\"<stdin>\"]"));
        assert!(stdout.contains("\"output\":\"package acme:app @ 1.0.0\\nsource \\\"src\\\"\\n\""));
        assert!(stdout.contains("\"exit_code\":0"));
    }

    #[test]
    fn fmt_formats_source_stdin_and_emits_the_json_result_contract() {
        let source = b"#!/usr/bin/env sai  \nfn greet(name:Str){} // signature";
        let expected = "#!/usr/bin/env sai\nfn greet(name: Str) {} // signature\n";
        let (exit_code, stdout, stderr) = invoke_with_stdin(&["fmt", "-"], source);

        assert_eq!(exit_code, 0);
        assert_eq!(stdout, expected);
        assert!(stderr.is_empty());

        let (exit_code, stdout, stderr) = invoke_with_stdin(&["--format=json", "fmt", "-"], source);
        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"kind\":\"fmt\""));
        assert!(stdout.contains("\"check\":false,\"files\":1,\"changed\":[\"<stdin>\"]"));
        assert!(stdout.contains(
            "\"output\":\"#!/usr/bin/env sai\\nfn greet(name: Str) {} // signature\\n\""
        ));
        assert!(stdout.contains("\"exit_code\":0"));
    }

    #[test]
    fn fmt_check_reports_the_first_differing_source_stdin_line() {
        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["--format=json", "fmt", "--check", "-"], b"fn f(){}");

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(
            stdout.contains("\"code\":\"UNFORMATTED_FILE\""),
            "unexpected --check output: {stdout}"
        );
        assert!(stdout.contains("\"path\":\"<stdin>\""));
        assert!(stdout.contains("\"line\":1,\"expected\":\"fn f() {}\""));
        assert!(stdout.contains(
            "\"kind\":\"fmt\",\"data\":{\"check\":true,\"files\":1,\"changed\":[\"<stdin>\"],\"refused\":[],\"output\":null}"
        ));
        assert!(stdout.contains("\"exit_code\":1"));

        let canonical = b"fn f() {}\n";
        let (exit_code, stdout, stderr) = invoke_with_stdin(&["fmt", "--check", "-"], canonical);
        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[test]
    fn fmt_reports_source_stdin_refusals_and_unsupported_shapes() {
        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["--format=json", "fmt", "-"], b"fn broken(");

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(stdout.contains("\"path\":\"<stdin>\""));
        assert!(stdout.contains(
            "\"kind\":\"fmt\",\"data\":{\"check\":false,\"files\":1,\"changed\":[],\"refused\":[\"<stdin>\"],\"output\":null}"
        ));

        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["--format=json", "fmt", "-"], b"fn f(){fn(var _){}}");
        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""));
        assert!(stdout.contains("\"feature\":\"sai fmt source syntax\""));
        assert!(stdout.contains(
            "\"kind\":\"fmt\",\"data\":{\"check\":false,\"files\":1,\"changed\":[],\"refused\":[],\"output\":null}"
        ));
    }

    #[test]
    fn fmt_source_max_errors_bounds_retained_diagnostics_and_keeps_the_total() {
        let source = format!("// {}\nfn f() {{}}\n", "\u{202e}".repeat(128));
        let expected_errors = match sai_fmt::parse_source(source.as_bytes()).unwrap_err() {
            sai_fmt::SourceInputError::Refused(refusal) => refusal
                .input_errors
                .len()
                .saturating_add(refusal.lexical_errors.len())
                .saturating_add(refusal.syntax_errors.len()),
            sai_fmt::SourceInputError::InternalError => panic!("source refusal was expected"),
        };
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &["--format=json", "--max-errors=1", "fmt", "-"],
            source.as_bytes(),
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains(&format!("\"errors\":{expected_errors}")));
        assert!(stdout.contains("\"shown_errors\":1"));
        assert_eq!(stdout.matches("\"type\":\"diagnostic\"").count(), 1);
    }

    #[test]
    fn fmt_maps_source_stdin_diagnostics_back_to_original_bytes() {
        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["--format=json", "fmt", "-"], b"\xef\xbb\xbffn broken(");

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(stdout.contains("\"start\":{\"byte\":13"));
        assert!(stdout.contains("\"end\":{\"byte\":13"));

        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["--format=json", "fmt", "-"], b"\xef\xbb\xbf\xff");

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_UTF8\""));
        assert!(stdout.contains("\"start\":{\"byte\":3"));
        assert!(stdout.contains("\"end\":{\"byte\":4"));
    }

    #[test]
    fn fmt_source_stdin_reports_the_actual_oversized_input_length() {
        let byte_len = sai_vfs::MAX_SOURCE_BYTES + 37;
        let source = vec![b'x'; byte_len];
        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["--format=json", "fmt", "-"], &source);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"IMPLEMENTATION_LIMIT_EXCEEDED\""));
        assert!(stdout.contains(&format!("\"value\":{byte_len}")));
        assert!(stdout.contains("\"refused\":[\"<stdin>\"]"));
    }

    #[test]
    fn source_diagnostic_data_serializes_nested_json_values() {
        let data = ValueObject::from([
            (
                "feature".to_owned(),
                Value::String("quoted \"text\"".to_owned()),
            ),
            (
                "items".to_owned(),
                Value::Array(vec![
                    Value::Bool(true),
                    Value::Null,
                    Value::Object(ValueObject::from([(
                        "nested".to_owned(),
                        Value::Integer(2),
                    )])),
                ]),
            ),
            ("stage".to_owned(), Value::Integer(1)),
        ]);

        assert_eq!(
            super::source_value_object_json(&data),
            r#"{"feature":"quoted \"text\"","items":[true,null,{"nested":2}],"stage":1}"#
        );
    }

    #[test]
    fn fmt_without_paths_formats_transitive_workspace_members_only() {
        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let root_source = workspace.join("src");
        let member = workspace.join("members/member");
        let member_source = member.join("code");
        let outside = directory.child("outside");
        let outside_source = outside.join("src");
        fs::create_dir_all(root_source.join(".hidden")).unwrap();
        fs::create_dir_all(root_source.join("out")).unwrap();
        fs::create_dir_all(&member_source).unwrap();
        fs::create_dir_all(&outside_source).unwrap();
        fs::write(
            workspace.join("sai.package"),
            b"package acme:root @ 1.0.0\nsource \"src\"\nrequires { acme:bridge @ ^1 from path \"../outside\" }\n",
        )
        .unwrap();
        fs::write(
            member.join("sai.package"),
            b"package acme:member @ 1.0.0\nsource \"code\"\n",
        )
        .unwrap();
        fs::write(
            outside.join("sai.package"),
            b"package acme:bridge @ 1.0.0\nsource \"src\"\nrequires { acme:member @ ^1 from path \"../workspace/members/member\" }\n",
        )
        .unwrap();
        let root_source_file = root_source.join("lib.sai");
        let hidden_source_file = root_source.join(".hidden/hidden.sai");
        let generated_source_file = root_source.join("out/generated.sai");
        let member_source_file = member_source.join("lib.sai");
        let outside_source_file = outside_source.join("lib.sai");
        for (path, contents) in [
            (&root_source_file, b"fn root(){}".as_slice()),
            (&hidden_source_file, b"fn hidden(){}".as_slice()),
            (&generated_source_file, b"fn generated(){}".as_slice()),
            (&member_source_file, b"fn member(){}".as_slice()),
            (&outside_source_file, b"fn outside(){}".as_slice()),
        ] {
            fs::write(path, contents).unwrap();
        }
        let start_directory = workspace.join("src");
        let start_directory = start_directory.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--directory",
            start_directory,
            "fmt",
            "--check",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"files\":6"));
        assert!(stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
        assert_eq!(fs::read(&root_source_file).unwrap(), b"fn root(){}");
        assert_eq!(fs::read(&hidden_source_file).unwrap(), b"fn hidden(){}");
        assert_eq!(
            fs::read(&generated_source_file).unwrap(),
            b"fn generated(){}"
        );
        assert_eq!(fs::read(&member_source_file).unwrap(), b"fn member(){}");
        assert_eq!(fs::read(&outside_source_file).unwrap(), b"fn outside(){}");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", start_directory, "fmt"]);

        assert_eq!(exit_code, 0, "unexpected workspace fmt output: {stdout}");
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"files\":6"));
        assert!(stdout.contains("\"refused\":[]"));
        assert_eq!(fs::read(&root_source_file).unwrap(), b"fn root() {}\n");
        assert_eq!(fs::read(&hidden_source_file).unwrap(), b"fn hidden() {}\n");
        assert_eq!(
            fs::read(&generated_source_file).unwrap(),
            b"fn generated() {}\n"
        );
        assert_eq!(fs::read(&member_source_file).unwrap(), b"fn member() {}\n");
        assert_eq!(fs::read(&outside_source_file).unwrap(), b"fn outside(){}");
        assert!(!outside.join(".sai/lock").exists());
    }

    #[test]
    fn fmt_without_a_package_reports_the_documented_exit_code() {
        let directory = TestDirectory::new();
        let start_directory = directory.0.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", start_directory, "fmt"]);

        assert_eq!(exit_code, 2);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"NO_PACKAGE_FOUND\""));
        assert!(stdout.contains("\"exit_code\":2"));
    }

    #[test]
    fn fmt_workspace_reports_a_missing_path_package_and_formats_known_files() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        let source = package.join("src");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            package.join("sai.package"),
            b"package acme:root @ 1.0.0\nsource \"src\"\nrequires { acme:missing @ ^1 from path \"missing\" }\n",
        )
        .unwrap();
        let source_file = source.join("lib.sai");
        fs::write(&source_file, b"fn root(){}").unwrap();
        let package_path = package.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "fmt"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(
            stdout.contains("\"code\":\"SOURCE_READ_FAILED\""),
            "{stdout}"
        );
        assert!(stdout.contains("\"path\":\"missing\""), "{stdout}");
        assert!(!stdout.contains("\"path\":\"../"), "{stdout}");
        assert!(stdout.contains("\"files\":2"));
        assert_eq!(fs::read(source_file).unwrap(), b"fn root() {}\n");
    }

    #[test]
    fn fmt_workspace_max_errors_keeps_the_first_diagnostic_in_output_order() {
        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let later_member = workspace.join("z-member");
        let earlier_member = workspace.join("a-member");
        fs::create_dir_all(&later_member).unwrap();
        fs::create_dir_all(&earlier_member).unwrap();
        fs::write(
            workspace.join("sai.package"),
            b"package acme:root @ 1.0.0\n\
              requires {\n\
                  acme:z @ ^1 from path \"z-member\"\n\
                  acme:a @ ^1 from path \"a-member\"\n\
              }\n",
        )
        .unwrap();
        fs::write(
            later_member.join("sai.package"),
            b"package acme:z @ 1.0.0\nsource \"missing\"\n",
        )
        .unwrap();
        fs::write(
            earlier_member.join("sai.package"),
            b"package acme:a @ 1.0.0\nsource \"missing\"\n",
        )
        .unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--max-errors=1",
            "--directory",
            workspace_path,
            "fmt",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"errors\":2"));
        assert!(stdout.contains("\"shown_errors\":1"));
        assert!(stdout.contains("\"path\":\"a-member/missing\""));
        assert!(!stdout.contains("\"path\":\"z-member/missing\""));
    }

    #[test]
    fn fmt_workspace_reports_external_invalid_paths_without_exposing_the_external_path() {
        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let external = directory.child("external");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(
            workspace.join("sai.package"),
            b"package acme:root @ 1.0.0\nrequires { acme:external @ ^1 from path \"../external\" }\n",
        )
        .unwrap();
        fs::write(
            external.join("sai.package"),
            b"package acme:external @ 1.0.0\nsource \"../outside\"\n",
        )
        .unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");
        let external_path = external.to_string_lossy();

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", workspace_path, "fmt"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_PACKAGE_PATH\""));
        assert!(stdout.contains("\"path\":\"sai.package\""));
        assert!(!stdout.contains(external_path.as_ref()));
        assert!(!stdout.contains("../"));
    }

    #[test]
    fn fmt_workspace_diagnostics_do_not_render_spans_against_changed_manifest_bytes() {
        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let manifest_path = workspace.join("sai.package");
        fs::write(
            &manifest_path,
            b"package acme:root @ 1.0.0\nsource \"changed\"\n",
        )
        .unwrap();
        let canonical_workspace = fs::canonicalize(&workspace).unwrap();
        let canonical_manifest = fs::canonicalize(&manifest_path).unwrap();
        let discovery = crate::fmt_workspace::FmtWorkspaceDiscovery {
            workspace_root: Some(canonical_workspace.clone()),
            input_paths: vec![manifest_path.clone()],
            manifest_snapshots: vec![crate::fmt_workspace::FmtManifestSnapshot {
                logical_path: manifest_path,
                path: canonical_manifest.clone(),
                package_directory: Some(canonical_workspace.clone()),
                digest: Some([0; 32]),
                oversized: false,
            }],
            diagnostics: vec![crate::fmt_workspace::FmtWorkspaceDiagnostic {
                manifest_path: Some(canonical_manifest),
                display_path: canonical_workspace.join("sai.package"),
                code: "INVALID_PACKAGE_PATH",
                message: "invalid package path".to_owned(),
                span: Some(30..37),
            }],
            total_error_count: 1,
            ..crate::fmt_workspace::FmtWorkspaceDiscovery::default()
        };

        let diagnostics = super::fmt_workspace_diagnostics(
            &discovery,
            &canonical_workspace,
            &canonical_workspace,
            false,
        );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "INVALID_PACKAGE_PATH");
        assert!(diagnostics[0].primary.is_none());
    }

    #[test]
    fn fmt_reports_invalid_source_paths_and_still_discovers_valid_members() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        let dependency = package.join("dep");
        fs::create_dir_all(&dependency).unwrap();
        fs::write(
            package.join("sai.package"),
            b"package acme:root@1.0.0\nsource \"../outside\"\nrequires { acme:dep @ ^1 from path \"dep\" }\n",
        )
        .unwrap();
        fs::write(dependency.join("sai.package"), b"package acme:dep@1.0.0\n").unwrap();
        let package_path = package.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", package_path, "fmt"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"INVALID_PACKAGE_PATH\""));
        assert!(stdout.contains("\"path\":\"sai.package\""));
        assert!(stdout.contains("\"files\":2"));
        assert!(dependency.join(".sai/lock").is_file());
    }

    #[test]
    fn fmt_bounds_external_discovery_diagnostics_and_hides_external_paths() {
        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let first = directory.child("outside-first");
        let second = directory.child("outside-second");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(
            workspace.join("sai.package"),
            b"package acme:root @ 1.0.0\nrequires {\n  acme:first @ ^1 from path \"../outside-first\"\n  acme:second @ ^1 from path \"../outside-second\"\n}\n",
        )
        .unwrap();
        fs::write(
            first.join("sai.package"),
            b"package acme:first @ 1.0.0\nunknown-first {}\n",
        )
        .unwrap();
        fs::write(
            second.join("sai.package"),
            b"package acme:second @ 1.0.0\nunknown-second {}\n",
        )
        .unwrap();
        let expected_errors = [&first, &second]
            .into_iter()
            .map(|package| {
                let source = fs::read(package.join("sai.package")).unwrap();
                let parsed = sai_package::manifest::parse_for_role(
                    &source,
                    sai_package::manifest::PackageRole::Dependency,
                );
                parsed
                    .lexical_error_count()
                    .saturating_add(parsed.error_count())
            })
            .sum::<usize>();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&[
            "--format=json",
            "--max-errors=1",
            "--directory",
            workspace_path,
            "fmt",
        ]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains(&format!("\"errors\":{expected_errors}")));
        assert!(stdout.contains("\"shown_errors\":1"));
        assert!(stdout.contains("\"path\":\"sai.package\""));
        assert!(!stdout.contains("outside-first"));
        assert!(!stdout.contains("outside-second"));
    }

    #[cfg(unix)]
    #[test]
    fn fmt_without_paths_locks_the_package_that_owns_a_symlinked_manifest() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let outside = directory.child("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let manifest_target = outside.join("manifest-data");
        fs::write(&manifest_target, b"package acme:root@1.0.0\n").unwrap();
        symlink(&manifest_target, workspace.join("sai.package")).unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", workspace_path, "fmt"]);

        assert_eq!(exit_code, 0, "unexpected workspace fmt output: {stdout}");
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"files\":1"));
        assert!(stdout.contains("\"changed\":[\"sai.package\"]"));
        assert_eq!(
            fs::read(&manifest_target).unwrap(),
            b"package acme:root @ 1.0.0\n"
        );
        assert!(workspace.join(".sai/lock").is_file());
        assert!(!outside.join(".sai/lock").exists());
        assert!(
            fs::symlink_metadata(workspace.join("sai.package"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn fmt_workspace_manifest_symlink_locks_a_package_at_its_physical_target() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let target_package = directory.child("target-package");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&target_package).unwrap();
        let target_manifest = target_package.join("sai.package");
        fs::write(&target_manifest, b"package acme:target@1.0.0\n").unwrap();
        symlink(&target_manifest, workspace.join("sai.package")).unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", workspace_path, "fmt"]);

        assert_eq!(exit_code, 0, "unexpected workspace fmt output: {stdout}");
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"changed\":[\"sai.package\"]"));
        assert!(!stdout.contains(target_package.to_str().unwrap()));
        assert_eq!(
            fs::read(&target_manifest).unwrap(),
            b"package acme:target @ 1.0.0\n"
        );
        assert!(workspace.join(".sai/lock").is_file());
        assert!(target_package.join(".sai/lock").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn fmt_workspace_manifest_target_lock_errors_use_the_logical_path() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let target_package = directory.child("target-package");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(target_package.join(".sai")).unwrap();
        let target_manifest = target_package.join("sai.package");
        let original = b"package acme:target@1.0.0\n";
        fs::write(&target_manifest, original).unwrap();
        fs::write(target_package.join(".sai/lock"), b"occupied").unwrap();
        symlink(&target_manifest, workspace.join("sai.package")).unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", workspace_path, "fmt"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"WORKSPACE_LOCK_FAILED\""));
        assert!(stdout.contains("\"path\":\"sai.package\""));
        assert!(!stdout.contains(target_package.to_str().unwrap()));
        assert!(!stdout.contains("../"));
        assert_eq!(fs::read(&target_manifest).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn fmt_human_progress_hides_external_manifest_symlink_targets() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let outside = directory.child("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let manifest_target = outside.join("manifest-data");
        fs::write(&manifest_target, b"package acme:root@1.0.0\n").unwrap();
        symlink(&manifest_target, workspace.join("sai.package")).unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&["--directory", workspace_path, "fmt"]);

        assert_eq!(
            exit_code, 0,
            "unexpected workspace fmt output: {stdout}{stderr}"
        );
        assert!(stdout.is_empty());
        assert!(stderr.contains("formatted sai.package\n"));
        assert!(!stderr.contains(outside.to_str().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn fmt_workspace_manifest_error_paths_use_the_logical_path_for_external_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let outside = directory.child("outside");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let manifest_target = outside.join("manifest-data");
        fs::write(&manifest_target, b"package acme:root @ 1.0.0\n").unwrap();
        let moved_manifest_package = outside.join("moved-package");
        fs::create_dir_all(&moved_manifest_package).unwrap();
        fs::write(
            moved_manifest_package.join("sai.package"),
            b"package acme:moved @ 1.0.0\n",
        )
        .unwrap();
        symlink(&manifest_target, workspace.join("sai.package")).unwrap();
        let canonical_workspace = fs::canonicalize(&workspace).unwrap();
        let canonical_manifest = fs::canonicalize(workspace.join("sai.package")).unwrap();
        let moved_manifest = fs::canonicalize(moved_manifest_package.join("sai.package")).unwrap();
        let signature = super::FmtWorkspaceSignature {
            root: Some(canonical_workspace.clone()),
            input_paths: vec![canonical_workspace.join("sai.package")],
            manifests: vec![crate::fmt_workspace::FmtManifestSnapshot {
                logical_path: canonical_workspace.join("sai.package"),
                path: canonical_manifest.clone(),
                package_directory: Some(canonical_workspace.clone()),
                digest: None,
                oversized: false,
            }],
            source_directories: Vec::new(),
            errors: Vec::new(),
            total_error_count: 0,
        };

        let display_path =
            super::fmt_workspace_display_path(&canonical_manifest, true, Some(&signature));
        let selection_error_path =
            super::fmt_workspace_selection_error_path(&canonical_manifest, &signature);
        let package_roots = super::fmt_package_roots_for_input(
            &canonical_manifest,
            true,
            Some(&signature),
            &canonical_workspace,
        )
        .unwrap();
        let retargeted_manifest_roots = super::fmt_package_roots_for_input(
            &moved_manifest,
            true,
            Some(&signature),
            &canonical_workspace,
        );
        let retargeted_display_path =
            super::fmt_workspace_display_path(&moved_manifest, true, Some(&signature));

        assert_eq!(display_path, canonical_workspace.join("sai.package"));
        assert_eq!(
            selection_error_path,
            canonical_workspace.join("sai.package")
        );
        assert_eq!(package_roots, vec![canonical_workspace.clone()]);
        assert!(retargeted_manifest_roots.is_err());
        assert_eq!(
            retargeted_display_path,
            canonical_workspace.join("sai.package")
        );
        assert!(!display_path.starts_with(outside));
    }

    #[cfg(unix)]
    #[test]
    fn fmt_workspace_detects_a_retargeted_manifest_even_when_it_deduplicates_with_a_member() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let member = workspace.join("member");
        let original_target = directory.child("root-manifest-data");
        fs::create_dir_all(&member).unwrap();
        fs::write(&original_target, b"package acme:root @ 1.0.0\n").unwrap();
        fs::write(member.join("sai.package"), b"package acme:member @ 1.0.0\n").unwrap();
        let root_manifest = workspace.join("sai.package");
        symlink(&original_target, &root_manifest).unwrap();
        let canonical_workspace = fs::canonicalize(&workspace).unwrap();
        let canonical_member = fs::canonicalize(&member).unwrap();
        let original_target = fs::canonicalize(&root_manifest).unwrap();
        let member_manifest = canonical_member.join("sai.package");
        let canonical_member_manifest = fs::canonicalize(&member_manifest).unwrap();
        let canonical_root_manifest = canonical_workspace.join("sai.package");
        let signature = super::FmtWorkspaceSignature {
            root: Some(canonical_workspace.clone()),
            input_paths: vec![canonical_root_manifest.clone(), member_manifest.clone()],
            manifests: vec![
                crate::fmt_workspace::FmtManifestSnapshot {
                    logical_path: canonical_root_manifest.clone(),
                    path: original_target,
                    package_directory: Some(canonical_workspace.clone()),
                    digest: None,
                    oversized: false,
                },
                crate::fmt_workspace::FmtManifestSnapshot {
                    logical_path: member_manifest.clone(),
                    path: canonical_member_manifest.clone(),
                    package_directory: Some(canonical_member),
                    digest: None,
                    oversized: false,
                },
            ],
            source_directories: Vec::new(),
            errors: Vec::new(),
            total_error_count: 0,
        };

        fs::remove_file(&root_manifest).unwrap();
        symlink(&member_manifest, &root_manifest).unwrap();
        let selection = crate::fmt_paths::select_fmt_workspace_paths(
            &signature.input_paths,
            &canonical_workspace,
            &CancellationToken::default(),
        );

        assert_eq!(selection.files.len(), 1);
        assert_eq!(selection.files[0].path, canonical_member_manifest);
        assert_eq!(selection.resolved_manifest_paths.len(), 2);
        assert!(!super::fmt_workspace_manifest_resolutions_match(
            &selection, &signature
        ));
    }

    #[test]
    fn fmt_workspace_signature_tracks_discovery_errors_beyond_retained_diagnostics() {
        let mut discovery = crate::fmt_workspace::FmtWorkspaceDiscovery {
            total_error_count: 3,
            ..crate::fmt_workspace::FmtWorkspaceDiscovery::default()
        };
        let failed_read_signature = super::fmt_workspace_signature(&discovery);

        discovery.total_error_count = 0;

        assert_ne!(
            failed_read_signature,
            super::fmt_workspace_signature(&discovery)
        );
    }

    #[test]
    fn fmt_workspace_manifest_source_must_match_its_snapshot_digest() {
        use sha2::{Digest, Sha256};

        let source = b"package acme:root @ 1.0.0\n";
        let manifest_path = PathBuf::from("/workspace/sai.package");
        let package_directory = PathBuf::from("/workspace");
        let digest: [u8; 32] = Sha256::digest(source).into();
        let mut signature = super::FmtWorkspaceSignature {
            root: Some(package_directory.clone()),
            input_paths: vec![manifest_path.clone()],
            manifests: vec![crate::fmt_workspace::FmtManifestSnapshot {
                logical_path: manifest_path.clone(),
                path: manifest_path.clone(),
                package_directory: Some(package_directory),
                digest: Some(digest),
                oversized: false,
            }],
            source_directories: Vec::new(),
            errors: Vec::new(),
            total_error_count: 0,
        };

        assert!(super::fmt_workspace_manifest_source_matches_discovery(
            &manifest_path,
            source,
            &signature
        ));
        assert!(!super::fmt_workspace_manifest_source_matches_discovery(
            &manifest_path,
            b"package acme:changed @ 1.0.0\n",
            &signature
        ));
        signature.manifests[0].digest = None;
        assert!(!super::fmt_workspace_manifest_source_matches_discovery(
            &manifest_path,
            source,
            &signature
        ));
    }

    #[test]
    fn fmt_does_not_write_nested_package_sources_when_the_declaring_package_lock_fails() {
        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let nested = workspace.join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            workspace.join("sai.package"),
            b"package acme:root @ 1.0.0\nsource \"src\"\n",
        )
        .unwrap();
        fs::write(nested.join("sai.package"), b"package acme:nested @ 1.0.0\n").unwrap();
        let source_file = nested.join("lib.sai");
        fs::write(&source_file, b"fn nested(){}").unwrap();
        fs::create_dir_all(workspace.join(".sai")).unwrap();
        fs::write(workspace.join(".sai/lock"), b"not-empty").unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", workspace_path, "fmt"]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"WORKSPACE_LOCK_FAILED\""));
        assert_eq!(fs::read(source_file).unwrap(), b"fn nested(){}");
        assert!(nested.join(".sai/lock").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn fmt_locks_every_package_aliasing_one_manifest_target() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let workspace = directory.child("workspace");
        let outside = directory.child("outside");
        let first_member = workspace.join("members/a");
        let second_member = workspace.join("members/b");
        fs::create_dir_all(&first_member).unwrap();
        fs::create_dir_all(&second_member).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            workspace.join("sai.package"),
            b"package acme:root @ 1.0.0\nrequires {\n  acme:a @ ^1 from path \"members/a\"\n  acme:b @ ^1 from path \"members/b\"\n}\n",
        )
        .unwrap();
        let shared_manifest = outside.join("manifest-data");
        fs::write(&shared_manifest, b"package acme:shared@1.0.0\n").unwrap();
        symlink(&shared_manifest, first_member.join("sai.package")).unwrap();
        symlink(&shared_manifest, second_member.join("sai.package")).unwrap();
        let workspace_path = workspace.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "--directory", workspace_path, "fmt"]);

        assert_eq!(exit_code, 0, "unexpected workspace fmt output: {stdout}");
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"files\":2"));
        assert!(workspace.join(".sai/lock").is_file());
        assert!(first_member.join(".sai/lock").is_file());
        assert!(second_member.join(".sai/lock").is_file());
        assert!(!outside.join(".sai/lock").exists());
        assert_eq!(
            fs::read(&shared_manifest).unwrap(),
            b"package acme:shared @ 1.0.0\n"
        );
    }

    #[test]
    fn fmt_atomically_formats_an_explicit_package_path() {
        let directory = TestDirectory::new();
        let manifest = directory.child("sai.package");
        fs::write(&manifest, b"package acme:app@1.0.0\nsource\"src\"\n").unwrap();
        let manifest = manifest.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", manifest]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            fs::read(directory.child("sai.package")).unwrap(),
            b"package acme:app @ 1.0.0\nsource \"src\"\n"
        );
        assert!(stdout.contains("\"kind\":\"fmt\""));
        assert!(stdout.contains("\"files\":1,\"changed\":[\"sai.package\"]"));
        assert!(stdout.contains("\"refused\":[]"));
        assert!(stdout.contains("\"exit_code\":0"));
        assert!(directory.child(".sai/lock").is_file());
    }

    #[test]
    fn fmt_check_and_atomic_write_support_an_explicit_source_path() {
        let directory = TestDirectory::new();
        let source = directory.child("source.sai");
        let original = b"fn greet(name:Str){}";
        fs::write(&source, original).unwrap();
        let source_path = source.to_str().expect("temporary path is UTF-8");
        let canonical_source = fs::canonicalize(&source).unwrap();
        let source_reference =
            super::fmt_path_reference(&canonical_source, &std::env::current_dir().unwrap(), None);
        let source_reference_json = super::escape_json(&source_reference);

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", "--check", source_path]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(
            stdout.contains("\"code\":\"UNFORMATTED_FILE\""),
            "unexpected source --check output: {stdout}"
        );
        assert!(stdout.contains(&format!("\"file\":{{\"path\":\"{source_reference_json}\"")));
        assert!(stdout.contains(&format!(
            "\"files\":1,\"changed\":[\"{source_reference_json}\"]"
        )));

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", source_path]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert_eq!(fs::read(&source).unwrap(), b"fn greet(name: Str) {}\n");
        assert!(stdout.contains(&format!("\"changed\":[\"{source_reference_json}\"]")));
        assert!(stdout.contains("\"refused\":[]"));
    }

    #[test]
    fn fmt_source_paths_distinguish_refusals_from_unsupported_shapes() {
        let directory = TestDirectory::new();
        let unsupported = directory.child("unsupported.sai");
        let broken = directory.child("source.sai");
        let unsupported_source = b"fn f(){fn(var _){}}";
        let broken_source = b"\xef\xbb\xbffn broken(";
        fs::write(&unsupported, unsupported_source).unwrap();
        fs::write(&broken, broken_source).unwrap();
        let unsupported_path = unsupported.to_str().expect("temporary path is UTF-8");
        let broken_path = broken.to_str().expect("temporary path is UTF-8");
        let working_directory = std::env::current_dir().unwrap();
        let canonical_broken = fs::canonicalize(&broken).unwrap();
        let broken_reference =
            super::fmt_path_reference(&canonical_broken, &working_directory, None);
        let broken_reference_json = super::escape_json(&broken_reference);

        let (exit_code, stdout, stderr) =
            invoke(&["--format=json", "fmt", unsupported_path, broken_path]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(fs::read(unsupported).unwrap(), unsupported_source);
        assert_eq!(fs::read(broken).unwrap(), broken_source);
        assert!(
            stdout.contains("\"code\":\"FEATURE_NOT_IMPLEMENTED\""),
            "unexpected explicit source-path output: {stdout}"
        );
        assert!(stdout.contains("\"feature\":\"sai fmt source syntax\""));
        assert!(stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(stdout.contains(&format!("\"file\":{{\"path\":\"{broken_reference_json}\"")));
        assert!(stdout.contains("\"start\":{\"byte\":13"));
        assert!(stdout.contains(&format!("\"refused\":[\"{broken_reference_json}\"]")));
        assert!(stdout.contains("\"exit_code\":1"));
    }

    #[test]
    fn fmt_source_path_reports_the_actual_oversized_input_length() {
        let directory = TestDirectory::new();
        let source = directory.child("large.sai");
        let byte_len = sai_vfs::MAX_SOURCE_BYTES + 37;
        fs::write(&source, vec![b'x'; byte_len]).unwrap();
        let source_path = source.to_str().expect("temporary path is UTF-8");
        let source_reference = super::fmt_path_reference(
            &fs::canonicalize(&source).unwrap(),
            &std::env::current_dir().unwrap(),
            None,
        );
        let source_reference_json = super::escape_json(&source_reference);

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", source_path]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(fs::metadata(source).unwrap().len(), byte_len as u64);
        assert!(stdout.contains("\"code\":\"IMPLEMENTATION_LIMIT_EXCEEDED\""));
        assert!(stdout.contains(&format!("\"value\":{byte_len}")));
        assert!(stdout.contains(&format!("\"refused\":[\"{source_reference_json}\"]")));
    }

    #[test]
    fn fmt_reports_human_progress_to_stderr_and_quiet_suppresses_it() {
        let directory = TestDirectory::new();
        let manifest = directory.child("sai.package");
        fs::write(&manifest, b"package acme:app@1.0.0\nsource\"src\"\n").unwrap();
        let manifest = manifest.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&["fmt", manifest]);
        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.starts_with("formatted "));
        assert!(stderr.ends_with("/sai.package\n"));

        fs::write(
            directory.child("sai.package"),
            b"package acme:app@1.0.0\nsource\"src\"\n",
        )
        .unwrap();
        let (exit_code, stdout, stderr) = invoke(&["fmt", "--quiet", manifest]);
        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn fmt_atomic_replacement_stays_in_the_selected_directory_after_path_swap() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        let moved = directory.child("moved");
        let outside = directory.child("outside");
        fs::create_dir(&package).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(package.join("sai.package"), b"old").unwrap();
        fs::write(outside.join("sai.package"), b"outside").unwrap();

        let selection = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&package),
            &directory.0,
            &CancellationToken::default(),
        );
        assert_eq!(selection.files.len(), 1);
        let selected = &selection.files[0];
        let parent = selected.parent.as_ref().unwrap();
        let parent_handle = parent.try_clone().unwrap().into_std_file();
        let source = sai_windows_fs::open_file_without_following(
            &parent_handle,
            selected.path.parent().unwrap(),
            &selected.name,
        )
        .unwrap();
        let permissions = cap_std::fs::File::from_std(source)
            .metadata()
            .unwrap()
            .permissions();

        fs::rename(&package, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, &package).unwrap();

        write_file_atomically_in_directory(
            parent,
            &selected.name,
            selected.identity,
            b"old",
            permissions,
            b"formatted",
        )
        .unwrap();

        assert_eq!(fs::read(moved.join("sai.package")).unwrap(), b"formatted");
        assert_eq!(fs::read(outside.join("sai.package")).unwrap(), b"outside");
    }

    #[test]
    fn fmt_input_read_uses_the_selected_handle_after_path_replacement() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        fs::create_dir(&package).unwrap();
        let manifest = package.join("sai.package");
        fs::write(&manifest, b"selected bytes").unwrap();
        let selected = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&manifest),
            &directory.0,
            &CancellationToken::default(),
        )
        .files
        .into_iter()
        .next()
        .unwrap();
        let mut file = super::SelectedFmtFile {
            path: selected.path,
            is_package_manifest: selected.is_package_manifest,
            source_path: "sai.package".to_owned(),
            package_root: None,
            package_root_identities: Vec::new(),
            parent: selected.parent,
            name: selected.name,
            identity: selected.identity,
            identity_guard: selected.identity_guard,
        };

        fs::rename(&manifest, package.join("previous")).unwrap();
        fs::write(&manifest, b"replacement bytes").unwrap();

        let mut selected_contents = Vec::new();
        super::open_selected_fmt_file(&mut file)
            .unwrap()
            .read_to_end(&mut selected_contents)
            .unwrap();

        assert_eq!(selected_contents, b"selected bytes");
        assert_eq!(fs::read(manifest).unwrap(), b"replacement bytes");
        assert!(file.identity_guard.is_some());
    }

    #[test]
    fn fmt_atomic_write_refuses_a_replaced_selected_file() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        fs::create_dir(&package).unwrap();
        let manifest = package.join("sai.package");
        fs::write(&manifest, b"selected").unwrap();
        let selection = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&manifest),
            &directory.0,
            &CancellationToken::default(),
        );
        assert_eq!(selection.files.len(), 1);
        let selected = &selection.files[0];
        let parent = selected.parent.as_ref().unwrap();
        let expected_identity = selected.identity;
        let name = selected.name.clone();
        let parent_handle = parent.try_clone().unwrap().into_std_file();
        let source = sai_windows_fs::open_file_without_following(
            &parent_handle,
            selected.path.parent().unwrap(),
            &selected.name,
        )
        .unwrap();
        let permissions = cap_std::fs::File::from_std(source)
            .metadata()
            .unwrap()
            .permissions();

        fs::rename(&manifest, package.join("previous")).unwrap();
        fs::write(&manifest, b"editor change").unwrap();

        let error = write_file_atomically_in_directory(
            parent,
            &name,
            expected_identity,
            b"selected",
            permissions,
            b"formatted stale input",
        )
        .unwrap_err();

        assert!(!error.installed);
        assert_eq!(error.source.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::read(manifest).unwrap(), b"editor change");
    }

    #[test]
    fn fmt_atomic_write_refuses_in_place_edits_to_the_selected_file() {
        let directory = TestDirectory::new();
        let package = directory.child("package");
        fs::create_dir(&package).unwrap();
        let manifest = package.join("sai.package");
        fs::write(&manifest, b"selected").unwrap();
        let selection = crate::fmt_paths::select_fmt_paths(
            std::slice::from_ref(&manifest),
            &directory.0,
            &CancellationToken::default(),
        );
        assert_eq!(selection.files.len(), 1);
        let selected = &selection.files[0];
        let parent = selected.parent.as_ref().unwrap();
        let expected_identity = selected.identity;
        let name = selected.name.clone();
        let parent_handle = parent.try_clone().unwrap().into_std_file();
        let source = sai_windows_fs::open_file_without_following(
            &parent_handle,
            selected.path.parent().unwrap(),
            &selected.name,
        )
        .unwrap();
        let permissions = cap_std::fs::File::from_std(source)
            .metadata()
            .unwrap()
            .permissions();

        fs::write(&manifest, b"editor changed same file").unwrap();

        let error = write_file_atomically_in_directory(
            parent,
            &name,
            expected_identity,
            b"selected",
            permissions,
            b"formatted stale input",
        )
        .unwrap_err();

        assert!(!error.installed);
        assert_eq!(error.source.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::read(manifest).unwrap(), b"editor changed same file");
    }

    #[test]
    fn fmt_check_reports_the_file_location_without_changing_the_file() {
        let directory = TestDirectory::new();
        let manifest = directory.child("sai.package");
        let original = b"package acme:app@1.0.0\nsource\"src\"\n";
        fs::write(&manifest, original).unwrap();
        let manifest = manifest.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", "--check", manifest]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(fs::read(directory.child("sai.package")).unwrap(), original);
        assert!(stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
        assert!(stdout.contains("\"path\":\"sai.package\""));
        assert!(stdout.contains("\"line\":1"));
        assert!(stdout.contains("\"files\":1,\"changed\":[\"sai.package\"]"));
        assert!(stdout.contains("\"exit_code\":1"));
    }

    #[test]
    fn fmt_directory_processes_package_and_source_files() {
        let directory = TestDirectory::new();
        fs::create_dir_all(directory.child("src")).unwrap();
        fs::create_dir_all(directory.child("out")).unwrap();
        fs::write(
            directory.child("sai.package"),
            b"package acme:app@1.0.0\nsource\"src\"\n",
        )
        .unwrap();
        fs::write(directory.child("src/lib.sai"), b"pub fn main(){}").unwrap();
        fs::write(directory.child("out/generated.sai"), b"ignored").unwrap();
        let root = directory.0.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", root]);

        assert_eq!(
            exit_code, 0,
            "unexpected directory result: {stdout}{stderr}"
        );
        assert!(stderr.is_empty());
        assert_eq!(
            fs::read(directory.child("sai.package")).unwrap(),
            b"package acme:app @ 1.0.0\nsource \"src\"\n"
        );
        assert_eq!(
            fs::read(directory.child("src/lib.sai")).unwrap(),
            b"pub fn main() {}\n"
        );
        assert!(stdout.contains("\"kind\":\"fmt\""));
        assert!(stdout.contains("\"files\":2"));
        assert!(stdout.contains("\"changed\":[\"sai.package\",\"src/lib.sai\"]"));
        assert!(stdout.contains("\"refused\":[]"));
        assert!(!stdout.contains("generated.sai"));
        assert!(stdout.contains("\"exit_code\":0"));
    }

    #[test]
    fn fmt_multiple_package_roots_keep_distinct_json_paths() {
        let directory = TestDirectory::new();
        for package in ["a", "b"] {
            let package_directory = directory.child(&format!("packages/{package}"));
            fs::create_dir_all(&package_directory).unwrap();
            fs::write(
                package_directory.join("sai.package"),
                b"package acme:app@1.0.0\nsource\"src\"\n",
            )
            .unwrap();
        }
        let cwd = std::env::current_dir().unwrap();
        let expected = ["a", "b"]
            .map(|package| {
                let manifest =
                    fs::canonicalize(directory.child(&format!("packages/{package}/sai.package")))
                        .unwrap();
                super::fmt_path_reference(&manifest, &cwd, None)
            })
            .map(|path| format!("\"{}\"", super::escape_json(&path)))
            .join(",");
        let root = directory.0.to_str().expect("temporary path is UTF-8");

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", root]);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains(&format!("\"changed\":[{expected}]")));
        assert!(stdout.contains("\"files\":2"));
        assert!(stdout.contains("\"exit_code\":0"));
    }

    #[test]
    fn fmt_missing_paths_are_reported_and_other_paths_continue() {
        let directory = TestDirectory::new();
        let manifest = directory.child("sai.package");
        fs::write(&manifest, b"package acme:app@1.0.0\nsource\"src\"\n").unwrap();
        let manifest = manifest.to_str().expect("temporary path is UTF-8");
        let missing = directory
            .child("missing.sai")
            .to_str()
            .expect("temporary path is UTF-8")
            .to_owned();

        let (exit_code, stdout, stderr) = invoke(&["--format=json", "fmt", &missing, manifest]);

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"PATH_NOT_FOUND\""));
        assert!(stdout.contains("\"argument\":\""));
        assert!(stdout.contains("\"files\":1,\"changed\":[\"sai.package\"]"));
        assert_eq!(
            fs::read(directory.child("sai.package")).unwrap(),
            b"package acme:app @ 1.0.0\nsource \"src\"\n"
        );
    }

    #[test]
    fn fmt_escapes_terminal_controls_only_for_terminal_output() {
        let source = b"// \x1b]0;unsafe\x07\npackage acme:app @ 1.0.0\n";
        let arguments = ["fmt", "--stdin-kind=package", "-"].map(std::ffi::OsString::from);
        let mut input = io::Cursor::new(source);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_cancellation_and_input(
            &arguments,
            &CancellationToken::default(),
            &mut input,
            &mut stdout,
            &mut stderr,
            true,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(!stdout.contains(&0x1b));
        assert!(!stdout.contains(&0x07));
        let stdout = String::from_utf8(stdout).expect("formatted output is UTF-8");
        assert!(stdout.contains("// <U+001B>]0;unsafe<U+0007>"));
    }

    #[test]
    fn fmt_check_reports_the_first_differing_stdin_line() {
        let source = b"package acme:app@1.0.0\nsource\"src\"\n";
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &[
                "--format=json",
                "fmt",
                "--check",
                "--stdin-kind=package",
                "-",
            ],
            source,
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"UNFORMATTED_FILE\""));
        assert!(stdout.contains("\"path\":\"<stdin>\""));
        assert!(stdout.contains("\"line\":1,\"expected\":\"package acme:app @ 1.0.0\""));
        assert!(stdout.contains("\"start\":{\"byte\":0,\"line\":1,\"col\":1,\"utf16_col\":1}"));
        assert!(stdout.contains(
            "\"kind\":\"fmt\",\"data\":{\"check\":true,\"files\":1,\"changed\":[\"<stdin>\"],\"refused\":[],\"output\":null}"
        ));
        assert!(stdout.contains("\"exit_code\":1"));

        let canonical = b"package acme:app @ 1.0.0\nsource \"src\"\n";
        let (exit_code, stdout, stderr) =
            invoke_with_stdin(&["fmt", "--check", "--stdin-kind=package", "-"], canonical);
        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &[
                "--format=json",
                "fmt",
                "--check",
                "--stdin-kind=package",
                "-",
            ],
            canonical,
        );
        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(stdout.contains(
            "\"kind\":\"fmt\",\"data\":{\"check\":true,\"files\":1,\"changed\":[],\"refused\":[],\"output\":null}"
        ));
    }

    #[test]
    fn fmt_reports_package_refusals_with_source_locations() {
        let source = b"package acme:app@1.0.0\ntargets { wasm32-core\n";
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &["--format=json", "fmt", "--stdin-kind=package", "-"],
            source,
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains("\"code\":\"EXPECTED_TOKEN\""));
        assert!(stdout.contains("\"path\":\"<stdin>\""));
        assert!(stdout.contains("\"expected_tokens\":[\"`}`\"]"));
        assert!(stdout.contains(
            "\"kind\":\"fmt\",\"data\":{\"check\":false,\"files\":1,\"changed\":[],\"refused\":[\"<stdin>\"],\"output\":null}"
        ));
        assert_eq!(stdout.lines().count(), 4);
    }

    #[test]
    fn fmt_cancellation_during_stdin_read_returns_interrupted_status() {
        let arguments =
            ["--format=json", "fmt", "--stdin-kind=package", "-"].map(std::ffi::OsString::from);
        let cancellation = CancellationToken::default();
        let mut stdin = CancellingReader {
            cancellation: cancellation.clone(),
            reads: 0,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_cancellation_and_input(
            &arguments,
            &cancellation,
            &mut stdin,
            &mut stdout,
            &mut stderr,
            false,
        );

        assert_eq!(exit_code, 130);
        assert!(stderr.is_empty());
        let stdout = String::from_utf8(stdout).expect("JSON stdout is UTF-8");
        assert!(stdout.contains("\"exit_code\":130"));
        assert!(!stdout.contains("\"kind\":\"fmt\""));
        assert_eq!(stdin.reads, 1);
    }

    #[test]
    fn cancellation_aware_stdin_returns_while_the_underlying_reader_is_blocked() {
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let cancellation = CancellationToken::default();
        let reader_cancellation = cancellation.clone();
        let broker = Arc::new(Mutex::new(StdinBroker::spawn(Box::new(BlockingReader {
            started: Some(started_sender),
            released: release_receiver,
            finished: Some(finished_sender),
        }))));
        let reader_thread = thread::spawn(move || {
            let mut stdin = CancellationAwareStdin {
                broker: Some(broker),
                cancellation: reader_cancellation,
            };
            stdin.read(&mut [0_u8; 1])
        });

        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("reader started and is blocked");
        cancellation.cancel();
        let error = reader_thread
            .join()
            .expect("stdin reader call completes")
            .expect_err("cancellation interrupts the CLI read");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        release_sender.send(()).expect("release background reader");
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("background reader exits after release");
    }

    #[test]
    fn fmt_internal_failures_use_the_ice_exit_and_omit_the_result_record() {
        let arguments =
            ["--format=json", "fmt", "--stdin-kind=package", "-"].map(std::ffi::OsString::from);
        let parsed = parse_args(&arguments);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = report_fmt_diagnostics(
            &parsed,
            vec![internal_fmt_diagnostic(
                "formatter verification failed: token sequence changed".to_owned(),
                None,
            )],
            1,
            None,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 101);
        assert!(stderr.is_empty());
        let stdout = String::from_utf8(stdout).expect("JSON stdout is UTF-8");
        assert!(stdout.contains(
            "\"message\":\"internal compiler error: formatter verification failed: token sequence changed\""
        ));
        assert!(stdout.contains("\"query_stack\":[]"));
        assert!(stdout.contains("\"compiler_version\":\""));
        assert!(!stdout.contains("\"kind\":\"fmt\""));
        assert!(stdout.contains("\"exit_code\":101"));
        assert_eq!(stdout.lines().count(), 3);
    }

    #[test]
    fn fmt_failures_before_a_result_omit_the_json_result_record() {
        let arguments =
            ["--format=json", "fmt", "--stdin-kind=package", "-"].map(std::ffi::OsString::from);
        let parsed = parse_args(&arguments);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = report_fmt_diagnostics(
            &parsed,
            vec![super::CliDiagnostic {
                code: "SOURCE_READ_FAILED",
                message: "could not read standard input".to_owned(),
                primary: None,
                primary_label: String::new(),
                argument_index: None,
                argument: String::new(),
                help: None,
                data: None,
            }],
            1,
            None,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        let stdout = String::from_utf8(stdout).expect("JSON stdout is UTF-8");
        assert!(stdout.contains("\"type\":\"header\""));
        assert!(stdout.contains("\"code\":\"SOURCE_READ_FAILED\""));
        assert!(!stdout.contains("\"kind\":\"fmt\""));
        assert!(stdout.contains("\"exit_code\":1"));
        assert_eq!(stdout.lines().count(), 3);
    }

    #[test]
    fn cli_positions_follow_raw_bytes_boms_line_endings_and_invalid_utf8() {
        let source = b"\xef\xbb\xbfa\xc3\xa9\xf0\x9f\x98\x80\r\nz\ry";
        assert_eq!(cli_position(source, 0).line, 1);
        assert_eq!(cli_position(source, 0).col, 1);
        assert_eq!(cli_position(source, 0).utf16_col, 1);
        assert_eq!(
            (
                cli_position(source, 10).line,
                cli_position(source, 10).col,
                cli_position(source, 10).utf16_col,
            ),
            (1, 8, 5)
        );
        assert_eq!(
            (
                cli_position(source, 12).line,
                cli_position(source, 12).col,
                cli_position(source, 12).utf16_col,
            ),
            (2, 1, 1)
        );
        assert_eq!(
            (
                cli_position(source, 14).line,
                cli_position(source, 14).col,
                cli_position(source, 14).utf16_col,
            ),
            (3, 1, 1)
        );

        let invalid = [b'a', 0xff, 0xfe, b'b'];
        assert_eq!(
            (
                cli_position(&invalid, 3).line,
                cli_position(&invalid, 3).col,
                cli_position(&invalid, 3).utf16_col,
            ),
            (1, 4, 4)
        );
    }

    #[test]
    fn cli_positions_use_checkpoints_after_long_utf8_prefixes() {
        let source = format!("{}\u{1f600}z", "a".repeat(300));
        let emoji_end = 300 + "\u{1f600}".len();

        assert_eq!(
            (
                cli_position(source.as_bytes(), emoji_end).col,
                cli_position(source.as_bytes(), emoji_end).utf16_col,
            ),
            (305, 303)
        );
        assert_eq!(
            (
                cli_position(source.as_bytes(), emoji_end + 1).col,
                cli_position(source.as_bytes(), emoji_end + 1).utf16_col,
            ),
            (306, 304)
        );
    }

    #[test]
    fn human_source_snippets_escape_controls_and_align_tabs_and_wide_characters() {
        let source = Arc::<[u8]>::from("// 日本\t\u{200b}".as_bytes());
        let forbidden_start = source.len() - "\u{200b}".len();
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", forbidden_start..source.len(), true);

        assert_eq!(
            location
                .source_line
                .as_ref()
                .expect("human location has a source line")
                .window(location.start_display_col, location.end_display_col)
                .0,
            "// 日本 <U+200B>"
        );
        assert_eq!(location.start_display_col, 8);
        assert_eq!(location.end_display_col, 16);

        let diagnostic = CliDiagnostic {
            code: "FORBIDDEN_CHARACTER",
            message: "forbidden character in manifest".to_owned(),
            primary: Some(location),
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(&mut rendered, "sai fmt", &[diagnostic], true, false, 1, 50)
            .expect("render human diagnostic");
        let rendered = String::from_utf8(rendered).expect("diagnostic is UTF-8");

        assert!(rendered.contains("1 | // 日本 <U+200B>"));
        assert!(rendered.contains("  |         ^^^^^^^^\n"));

        let source = Arc::<[u8]>::from(&b"\x1b"[..]);
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", 0..1, true);
        let diagnostic = CliDiagnostic {
            code: "FORBIDDEN_CHARACTER",
            message: "forbidden character in manifest".to_owned(),
            primary: Some(location),
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(&mut rendered, "sai fmt", &[diagnostic], true, false, 1, 50)
            .expect("render escaped control diagnostic");

        assert!(!rendered.contains(&0x1b));
        assert!(
            String::from_utf8(rendered)
                .expect("diagnostic is UTF-8")
                .contains("<U+001B>")
        );

        let rendered =
            RenderedSourceLine::new("\u{061c}\u{200e}\u{200f}\u{202e}\u{2066}\u{206a}".as_bytes());
        assert_eq!(
            rendered.window(0, rendered.display_width).0,
            "<U+061C><U+200E><U+200F><U+202E><U+2066><U+206A>"
        );
    }

    #[test]
    fn human_source_snippets_show_a_bom_without_counting_it_as_a_column() {
        let source = Arc::<[u8]>::from(&b"\xef\xbb\xbfx"[..]);
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", 3..4, true);

        assert_eq!(location.start.col, 1);
        assert_eq!(location.start.utf16_col, 1);
        assert_eq!(location.start_display_col, 8);
        assert_eq!(
            location
                .source_line
                .as_ref()
                .expect("human location has a source line")
                .window(0, 8)
                .0,
            "<U+FEFF>x"
        );

        let bom_location = source_index.location("<stdin>", 0..3, true);
        assert_eq!(bom_location.start.col, 1);
        assert_eq!(bom_location.end.col, 1);
        assert_eq!(bom_location.start_display_col, 0);
        assert_eq!(bom_location.end_display_col, 8);
    }

    #[test]
    fn json_locations_do_not_render_or_copy_source_lines() {
        let source = Arc::<[u8]>::from(vec![b'a'; 128 * 1024]);
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", 120_000..120_001, false);

        assert!(location.source_line.is_none());
        assert_eq!(location.start.col, 120_001);
        assert_eq!(location.start_display_col, 0);
    }

    #[test]
    fn human_source_snippets_clip_long_lines_around_the_diagnostic() {
        let source = Arc::<[u8]>::from(vec![b'a'; 300]);
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", 140..145, true);
        let (row, window_start, left_clipped, right_clipped, row_width) = location
            .source_line
            .as_ref()
            .expect("human location has a source line")
            .window(location.start_display_col, location.end_display_col);

        assert!(left_clipped);
        assert!(right_clipped);
        assert_eq!(window_start, 120);
        assert_eq!(row_width, 140);
        assert_eq!(row.len(), 140);
    }

    #[test]
    fn human_source_snippet_draws_an_empty_eof_range_after_the_last_character() {
        let source = Arc::<[u8]>::from(vec![b'a'; 300].as_slice());
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", source.len()..source.len(), true);
        let diagnostic = CliDiagnostic {
            code: "FORBIDDEN_CHARACTER",
            message: "forbidden character in manifest".to_owned(),
            primary: Some(location),
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(&mut rendered, "sai fmt", &[diagnostic], false, false, 1, 50)
            .expect("render empty end-of-line annotation");
        let rendered = String::from_utf8(rendered).expect("diagnostic is UTF-8");
        // Skip the heading, the location line, and the first gutter line.
        let mut rows = rendered.lines().skip(3);
        let source_row = rows.next().expect("source row");
        let annotation_row = rows.next().expect("annotation row");

        assert_eq!(
            annotation_row.rfind('^'),
            source_row.rfind('a').map(|column| column + 1)
        );
    }

    #[test]
    fn human_diagnostics_follow_the_shared_layout_and_end_each_block_with_a_blank_line() {
        let source = Arc::<[u8]>::from(&b"fn main() {\n    let x = 1\n}\n"[..]);
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("src/lib.sai", 20..21, true);
        let first = CliDiagnostic {
            code: "UNKNOWN_FIELD",
            message: "`User` has no field `x`".to_owned(),
            primary: Some(location),
            primary_label: "unknown field".to_owned(),
            argument_index: None,
            argument: String::new(),
            help: Some("replace with `name`".to_owned()),
            data: None,
        };
        let second = CliDiagnostic {
            code: "UNKNOWN_OPTION",
            message: "unknown option `--tagret`".to_owned(),
            primary: None,
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: Some("a similar option exists: `--target`".to_owned()),
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(
            &mut rendered,
            "sai fmt",
            &[first, second],
            true,
            false,
            2,
            50,
        )
        .expect("render human diagnostics");

        assert_eq!(
            String::from_utf8(rendered).expect("diagnostics are UTF-8"),
            concat!(
                "error[UNKNOWN_FIELD]: `User` has no field `x`\n",
                " --> src/lib.sai:2:9\n",
                "  |\n",
                "2 |     let x = 1\n",
                "  |         ^ unknown field\n",
                "  |\n",
                "  = help: replace with `name`\n",
                "\n",
                "error[UNKNOWN_OPTION]: unknown option `--tagret`\n",
                "  = help: a similar option exists: `--target`\n",
                "\n",
                "sai fmt failed: 2 errors\n",
            )
        );
    }

    #[test]
    fn human_diagnostics_keep_codes_missing_from_the_registry_in_the_heading() {
        let source = Arc::<[u8]>::from(&b"fn main() {\n    let x = 1\n}\n"[..]);
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let diagnostic = CliDiagnostic {
            code: "WORKSPACE_LOCK_FAILED",
            message: "could not lock the package".to_owned(),
            primary: Some(source_index.location("src/lib.sai", 20..21, true)),
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(&mut rendered, "fmt", &[diagnostic], true, false, 1, 50)
            .expect("render unregistered diagnostic");

        assert_eq!(
            String::from_utf8(rendered).expect("diagnostics are UTF-8"),
            concat!(
                "error[WORKSPACE_LOCK_FAILED]: could not lock the package\n",
                " --> src/lib.sai:2:9\n",
                "  |\n",
                "2 |     let x = 1\n",
                "  |         ^\n",
                "  |\n",
                "\n",
                "fmt failed: 1 error\n",
            )
        );
    }

    #[test]
    fn human_source_snippet_keeps_a_clipped_eof_annotation_visible_after_a_wide_character() {
        let line =
            RenderedSourceLine::new(format!("{}界{}", "a".repeat(19), "b".repeat(139)).as_bytes());
        let (row, window_start, left_clipped, right_clipped, row_width) =
            line.window(line.display_width, line.display_width);

        assert_eq!(window_start, 21);
        assert!(left_clipped);
        assert!(!right_clipped);
        assert_eq!(row_width, 139);
        assert!(row.ends_with(&"b".repeat(139)));
    }

    #[test]
    fn human_source_snippet_windows_never_split_a_wide_character() {
        let line =
            RenderedSourceLine::new(format!("{}界{}", "a".repeat(139), "b".repeat(10)).as_bytes());
        let (row, window_start, left_clipped, right_clipped, row_width) = line.window(20, 21);

        assert_eq!(window_start, 0);
        assert!(!left_clipped);
        assert!(right_clipped);
        assert_eq!(row_width, 139);
        assert!(!row.contains('界'));
    }

    #[test]
    fn human_source_snippet_window_keeps_a_wide_diagnostic_character_visible() {
        let line =
            RenderedSourceLine::new(format!("{}界{}", "a".repeat(139), "b".repeat(20)).as_bytes());
        let (row, window_start, left_clipped, right_clipped, row_width) = line.window(0, 141);

        assert_eq!(window_start, 1);
        assert!(left_clipped);
        assert!(right_clipped);
        assert_eq!(row_width, 140);
        assert!(row.starts_with(&"a".repeat(138)));
        assert!(row.contains('界'));
    }

    #[test]
    fn human_source_snippet_stores_controls_without_expanding_the_full_line() {
        let source = vec![0; 1024 * 1024];
        let line = RenderedSourceLine::new(&source);
        let (row, _, _, _, row_width) = line.window(0, 1);

        assert_eq!(line.text.len(), source.len());
        assert_eq!(row_width, 136);
        assert_eq!(row.matches("<U+0000>").count(), 17);
    }

    #[test]
    fn human_source_snippet_keeps_trailing_zero_width_marks_on_the_line() {
        let line = RenderedSourceLine::new("a\u{0301}".as_bytes());
        let (row, _, _, _, row_width) = line.window(0, line.display_width);

        assert_eq!(row, "a\u{0301}");
        assert_eq!(row_width, 1);
    }

    #[test]
    fn fmt_reports_utf16_columns_for_diagnostics_at_the_end_of_a_long_line() {
        let source = format!(
            "// {}\u{202e}\npackage acme:app @ 1.0.0\n",
            "a".repeat(1024 * 1024)
        );
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &[
                "--format=json",
                "--max-errors=1",
                "fmt",
                "--stdin-kind=package",
                "-",
            ],
            source.as_bytes(),
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains(
            "\"start\":{\"byte\":1048579,\"line\":1,\"col\":1048580,\"utf16_col\":1048580}"
        ));
    }

    #[test]
    fn human_source_snippets_show_both_ends_of_multiline_diagnostics() {
        let source = Arc::<[u8]>::from("begin\n  middle\n    end".as_bytes());
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", 0..source.len(), true);
        let diagnostic = CliDiagnostic {
            code: "UNTERMINATED_COMMENT",
            message: "unterminated block comment".to_owned(),
            primary: Some(location),
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(&mut rendered, "sai fmt", &[diagnostic], true, false, 1, 50)
            .expect("render multiline diagnostic");
        let rendered = String::from_utf8(rendered).expect("diagnostic is UTF-8");

        assert!(rendered.contains("1 | begin"));
        assert!(rendered.contains("  | ^^^^^\n"));
        assert!(rendered.contains("2 |   middle"));
        assert!(rendered.contains("3 |     end"));
        assert!(rendered.contains("  |     ^^^\n"));
    }

    #[test]
    fn human_source_snippets_show_a_lone_cr_as_the_diagnosed_character() {
        let source = Arc::<[u8]>::from(b"before\rafter".as_slice());
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let location = source_index.location("<stdin>", 6..7, true);
        let diagnostic = CliDiagnostic {
            code: "INVALID_LINE_ENDING",
            message: "use LF line endings".to_owned(),
            primary: Some(location),
            primary_label: String::new(),
            argument_index: None,
            argument: String::new(),
            help: None,
            data: None,
        };
        let mut rendered = Vec::new();
        write_human_diagnostics(&mut rendered, "sai fmt", &[diagnostic], true, false, 1, 50)
            .expect("render lone-CR diagnostic");
        let rendered = String::from_utf8(rendered).expect("diagnostic is UTF-8");

        assert!(rendered.contains("1 | before<U+000D>"));
        assert!(rendered.contains("  |       ^^^^^^^^\n"));
        assert!(!rendered.contains("2 | after"));
    }

    #[test]
    fn cached_source_snippets_keep_a_lone_cr_visible_for_later_diagnostics() {
        let source = Arc::<[u8]>::from("before\u{202e}\rafter".as_bytes());
        let mut source_index = CliSourceIndex::new(Arc::clone(&source));
        let _earlier_location = source_index.location("<stdin>", 6..9, true);
        let cr_location = source_index.location("<stdin>", 9..10, true);
        let (row, _, _, _, _) = cr_location
            .source_line
            .as_ref()
            .expect("human location has a source line")
            .window(cr_location.start_display_col, cr_location.end_display_col);

        assert!(row.contains("<U+000D>"));
    }

    #[test]
    fn fmt_max_errors_limits_expensive_diagnostic_rendering_and_keeps_the_total() {
        let source = format!("package acme:app @ 1.0.0\n// {}\n", "\u{202e}".repeat(1000));
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &[
                "--format=json",
                "--max-errors=1",
                "fmt",
                "--stdin-kind=package",
                "-",
            ],
            source.as_bytes(),
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(stdout.lines().count(), 4);
        assert!(stdout.contains("\"errors\":1000"));
        assert!(stdout.contains("\"shown_errors\":1"));
        assert!(stdout.contains("\"code\":\"FORBIDDEN_CHARACTER\""));
    }

    #[test]
    fn fmt_max_errors_bounds_retained_comment_diagnostics_and_keeps_exact_total() {
        let source = format!("// {}\npackage acme:app @ 1.0.0\n", "\u{00a0}".repeat(8192));
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &[
                "--format=json",
                "--max-errors=1",
                "fmt",
                "--stdin-kind=package",
                "-",
            ],
            source.as_bytes(),
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert_eq!(stdout.lines().count(), 4);
        assert!(stdout.contains("\"errors\":8192"));
        assert!(stdout.contains("\"shown_errors\":1"));
        assert_eq!(
            stdout.matches("\"code\":\"FORBIDDEN_CHARACTER\"").count(),
            1
        );
    }

    #[test]
    fn fmt_max_errors_bounds_parser_diagnostics_and_keeps_exact_total() {
        let mut source = String::from("package acme:app @ 1.0.0\n");
        for _ in 0..32 {
            source.push_str("compiler > 1.0.0\n");
        }
        source.push_str("future {}\n");
        let parsed = sai_package::manifest::parse_for_role_with_error_limit(
            source.as_bytes(),
            sai_package::manifest::PackageRole::Root,
            1,
        );
        let expected_errors = parsed.refusing_error_count();
        let (exit_code, stdout, stderr) = invoke_with_stdin(
            &[
                "--format=json",
                "--max-errors=1",
                "fmt",
                "--stdin-kind=package",
                "-",
            ],
            source.as_bytes(),
        );

        assert_eq!(exit_code, 1);
        assert!(stderr.is_empty());
        assert!(stdout.contains(&format!("\"errors\":{expected_errors}")));
        assert!(stdout.contains("\"shown_errors\":1"));
        assert_eq!(stdout.matches("\"type\":\"diagnostic\"").count(), 1);
        assert!(stdout.contains("\"code\":\"UNKNOWN_DECLARATION\""));
    }

    #[test]
    fn stdin_size_limit_reads_only_one_byte_past_the_manifest_limit() {
        let cancellation = CancellationToken::default();
        let mut input = io::Cursor::new(vec![
            b'x';
            sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES
                + 100
        ]);

        let source = super::read_stdin_limited(
            &mut input,
            sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES,
            &cancellation,
        )
        .expect("stdin read succeeds");

        assert_eq!(
            source.len(),
            sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES + 1
        );
        assert_eq!(
            input.position(),
            (sai_package::manifest::MAX_PACKAGE_MANIFEST_BYTES + 1) as u64
        );
        assert!(matches!(
            super::read_stdin_limited(&mut io::empty(), 1, &{
                let cancelled = CancellationToken::default();
                cancelled.cancel();
                cancelled
            }),
            Err(StdinReadError::Interrupted)
        ));
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
