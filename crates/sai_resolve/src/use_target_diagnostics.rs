//! Source diagnostics for the package-local module part of `use` resolution.
//!
//! Foreign package selection and interface binding are left to later
//! resolver stages. This bridge is deliberately one-shot and consumes the
//! target-filtered module inputs produced for Phase 1.

use crate::module_target::ModuleTargetError;
use crate::package_scopes::{
    DefKind, FilteredModule, PackageScopesError, Phase1PackageScopes, Res,
};
use crate::use_path::{ParsedUsePath, parse_use_path};
use crate::use_target::{UseTarget, UseTargetError, resolve_parsed_use_target};
use crate::{ModuleId, ModuleTree};
use sai_diagnostics::{Child, ChildKind, Code, Diagnostic, Location, Severity, Value};
use sai_hir::item_tree::UseEntry;
use sai_intern::{Interner, PackageIdError, PackageInterner};
use sai_syntax::ast::{self, AstNode, AstToken};
use sai_vfs::{PackageKey, PackageKeyError, Span, TextRange};
use std::collections::BTreeMap;

/// Failure while assembling source diagnostics for local `use` targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UseTargetDiagnosticsError {
    Package(PackageIdError),
    PackageKey(PackageKeyError),
    PackageScopes(PackageScopesError),
    Resolution(UseTargetError),
    ModuleScopeCountMismatch,
}

impl From<PackageIdError> for UseTargetDiagnosticsError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<PackageKeyError> for UseTargetDiagnosticsError {
    fn from(error: PackageKeyError) -> Self {
        Self::PackageKey(error)
    }
}

impl From<PackageScopesError> for UseTargetDiagnosticsError {
    fn from(error: PackageScopesError) -> Self {
        Self::PackageScopes(error)
    }
}

impl From<UseTargetError> for UseTargetDiagnosticsError {
    fn from(error: UseTargetError) -> Self {
        Self::Resolution(error)
    }
}

/// Reports local module-target errors for surviving, target-filtered uses.
///
/// Invalid package-path syntax remains owned by the parser. Foreign package
/// paths are preserved for package selection, and an unresolved final segment
/// that names an interface in its parent module is deferred to interface
/// resolution.
pub fn collect_local_use_target_diagnostics(
    module_tree: &ModuleTree,
    module_inputs: &BTreeMap<ModuleId, FilteredModule<'_>>,
    scopes: &Phase1PackageScopes,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<Vec<Diagnostic>, UseTargetDiagnosticsError> {
    if module_tree.modules.len() != scopes.modules.len() {
        return Err(UseTargetDiagnosticsError::ModuleScopeCountMismatch);
    }
    if module_tree.modules.is_empty() {
        return Ok(Vec::new());
    }

    let package_key_spelling = packages.resolve(module_tree.package)?;
    let package_key = PackageKey::new(package_key_spelling.to_string())?;
    let mut diagnostics = Vec::new();

    for raw_module_id in 0..module_tree.modules.len() {
        let module_id = ModuleId::from_raw(
            u32::try_from(raw_module_id).map_err(|_| PackageScopesError::TooManyModules)?,
        );
        let input = module_inputs
            .get(&module_id)
            .ok_or(PackageScopesError::MissingModuleInput(module_id))?;

        for entry in &input.filtered.item_tree.uses {
            let Some((path_range, has_error)) = use_package_path_range(entry, input)? else {
                continue;
            };
            if has_error {
                continue;
            }

            let parsed = match parse_use_path(entry, symbols) {
                Ok(parsed) => parsed,
                Err(_) => continue,
            };
            match resolve_parsed_use_target(
                &parsed,
                &package_key,
                packages,
                module_id,
                module_tree,
                symbols,
            ) {
                Ok(UseTarget::LocalModule(_)) | Ok(UseTarget::ForeignPackage(_)) => {}
                Err(UseTargetError::Module(ModuleTargetError::UseOfCurrentModule(_))) => {
                    let location = source_location(input, path_range)?;
                    diagnostics.push(current_module_diagnostic(
                        entry,
                        location,
                        package_key.as_str(),
                    ));
                }
                Err(UseTargetError::Module(ModuleTargetError::UnknownModule {
                    matched_prefix,
                    module_candidates,
                })) => {
                    if interface_is_target(module_tree, scopes, &parsed) {
                        continue;
                    }
                    let location = source_location(input, path_range)?;
                    diagnostics.push(unknown_module_diagnostic(
                        entry,
                        &parsed,
                        matched_prefix,
                        module_candidates,
                        location,
                        package_key.as_str(),
                        symbols,
                    )?);
                }
                // The package tree owns the missing-root diagnostic. A path
                // parse error is already owned by the lexer/parser.
                Err(UseTargetError::Module(ModuleTargetError::MissingRootModule))
                | Err(UseTargetError::Path(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    Ok(diagnostics)
}

fn use_package_path_range(
    entry: &UseEntry,
    input: &FilteredModule<'_>,
) -> Result<Option<(TextRange, bool)>, PackageScopesError> {
    let file = input.filtered.item_tree.file;
    let syntax = input
        .ast_ids
        .pointer(entry.ast_id)
        .and_then(|pointer| pointer.resolve(file, input.root))
        .ok_or(PackageScopesError::MissingAstNode)?;
    let use_decl = ast::UseDecl::cast(syntax).ok_or(PackageScopesError::MissingAstNode)?;
    let Some(package_path) = use_decl.package() else {
        return Ok(None);
    };
    Ok(Some((
        package_path.syntax().text_range(),
        package_path.syntax().has_error(),
    )))
}

fn source_location(
    input: &FilteredModule<'_>,
    range: TextRange,
) -> Result<Location, PackageScopesError> {
    Location::from_decoded_source(
        input.source_ref.clone(),
        Span::new(input.filtered.item_tree.file, range),
        input.source,
    )
    .ok_or(PackageScopesError::InvalidSourceRange)
}

fn interface_is_target(
    module_tree: &ModuleTree,
    scopes: &Phase1PackageScopes,
    path: &ParsedUsePath,
) -> bool {
    let Some(interface_name) = path.segments.last().copied() else {
        return false;
    };
    let parent_path = &path.segments[..path.segments.len() - 1];
    let Some(module_index) = module_tree
        .modules
        .iter()
        .position(|module| module.path.as_slice() == parent_path)
    else {
        return false;
    };
    scopes.modules[module_index]
        .declared
        .get(&interface_name)
        .is_some_and(|binding| matches!(binding.res, Res::Def(DefKind::Interface, _)))
}

fn current_module_diagnostic(
    entry: &UseEntry,
    location: Location,
    package_key: &str,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        Code::UseOfCurrentModule,
        Severity::Error,
        format!(
            "`{}` names the module that contains this use",
            entry.pkg_path
        ),
        Some(location),
    );
    diagnostic.primary_label = "current module".to_owned();
    diagnostic.package = Some(package_key.to_owned());
    diagnostic
}

fn unknown_module_diagnostic(
    entry: &UseEntry,
    path: &ParsedUsePath,
    matched_prefix: Vec<String>,
    module_candidates: Vec<String>,
    location: Location,
    package_key: &str,
    symbols: &Interner,
) -> Result<Diagnostic, PackageScopesError> {
    let mut diagnostic = Diagnostic::new(
        Code::UnknownModule,
        Severity::Error,
        format!("no module or interface has the path `{}`", entry.pkg_path),
        Some(location),
    );
    diagnostic.primary_label = "unknown module path".to_owned();
    diagnostic.package = Some(package_key.to_owned());
    diagnostic.data.insert(
        "matched_prefix".to_owned(),
        Value::Array(matched_prefix.iter().cloned().map(Value::String).collect()),
    );
    diagnostic.data.insert(
        "module_candidates".to_owned(),
        Value::Array(
            module_candidates
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );

    let prefix = if matched_prefix.is_empty() {
        "the package root".to_owned()
    } else {
        format!("`{}`", matched_prefix.join("/"))
    };
    let child_message = if module_candidates.is_empty() {
        format!("no modules directly under {prefix}")
    } else {
        format!(
            "modules directly under {prefix}: {}",
            module_candidates
                .iter()
                .map(|candidate| format!("`{candidate}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    diagnostic
        .push_child(Child {
            kind: ChildKind::Note,
            message: child_message,
            location: None,
        })
        .expect("a diagnostic without a workspace snapshot accepts children");

    let spellings = path
        .segments
        .iter()
        .map(|segment| {
            symbols
                .resolve(*segment)
                .map(|spelling| spelling.to_string())
                .map_err(PackageScopesError::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if spellings.iter().any(|segment| segment == "lib") {
        push_help(
            &mut diagnostic,
            "the root module is named by the package identity alone",
        );
    }
    if spellings.iter().any(|segment| segment.contains('-')) {
        let suggested_segments = spellings
            .iter()
            .map(|segment| segment.replace('-', "_"))
            .collect::<Vec<_>>();
        let suggested_path = if suggested_segments.is_empty() {
            path.package_identity.clone()
        } else {
            format!("{}/{}", path.package_identity, suggested_segments.join("/"))
        };
        push_help(
            &mut diagnostic,
            &format!("try `{suggested_path}`; hyphens do not normalize in SAI module paths"),
        );
    }

    Ok(diagnostic)
}

fn push_help(diagnostic: &mut Diagnostic, message: &str) {
    diagnostic
        .push_child(Child {
            kind: ChildKind::Help,
            message: message.to_owned(),
            location: None,
        })
        .expect("a diagnostic without a workspace snapshot accepts children");
}
