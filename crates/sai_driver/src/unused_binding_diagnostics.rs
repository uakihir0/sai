//! Conservative, standalone conversion of owner-local unused binding candidates.
//!
//! This is not part of the driver pipeline. `Complete` inputs are caller
//! assertions about the exact owner, build, snapshot, and symbol interner;
//! incomplete scope or lint collection must never be passed as complete.
//!
//! Fix verification currently accepts only one edit entirely within the name.
//! A conforming planner can also reformat the enclosing format unit, even
//! when the rename itself is safe. Those otherwise valid fixes return
//! `Incomplete`, not `Complete`, until format-unit changes can be verified.
//!
//! Conversion rejects more than 64 candidates or a candidate count times
//! original source size above 8 MiB, before calling the planner. This bounds
//! the input volume passed through per-fix validation, not the cost of the
//! supplied planner or of candidate analysis. It counts suppressed candidates
//! too and applies independently to each invocation, not to a whole build.

use std::collections::{BTreeMap, HashMap, HashSet};

use sai_diagnostics::{
    Applicability, Code, Diagnostic, Fix, FixOperation, FixOperationName, FixOperationPlanner,
    LintResolution, LintResolutionContext, Location, NodeGroup, NodeId, OperationPlanError,
    SourceRef, TextEdit, Value, ValueObject, resolve_lint_occurrence,
};
use sai_hir::SpanCtxt;
use sai_hir::body::Body;
use sai_hir::source_map::HirSourceMap;
use sai_intern::Interner;
use sai_resolve::unused_binding::{ModuleAliases, UnusedBindingAnalysis, analyze_unused_bindings};
use sai_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use sai_vfs::{FileId, Span, TextRange, WorkspaceSnapshot};

const MAX_CANDIDATES: usize = 64;
const MAX_VALIDATION_INPUT_BYTES: usize = 8 * 1024 * 1024;

fn within_validation_budget(candidate_count: usize, source_bytes: usize) -> bool {
    candidate_count <= MAX_CANDIDATES
        && candidate_count
            .checked_mul(source_bytes)
            .is_some_and(|bytes| bytes <= MAX_VALIDATION_INPUT_BYTES)
}

struct NameOnlyPlanner<'a, P> {
    inner: &'a P,
    primary: &'a Location,
    raw_source: &'a [u8],
    replacement: &'a str,
}

impl<P: FixOperationPlanner> FixOperationPlanner for NameOnlyPlanner<'_, P> {
    fn plan_edits(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<TextEdit>, OperationPlanError> {
        if primary != self.primary {
            return Err(OperationPlanError::NotApplicable);
        }
        let edits = self.inner.plan_edits(operation, primary, snapshot)?;
        let [edit] = edits.as_slice() else {
            return Err(OperationPlanError::NotApplicable);
        };
        if edit.file != primary.file
            || edit.range.file != primary.span.file
            || edit.range.range.start() < primary.span.range.start()
            || edit.range.range.end() > primary.span.range.end()
        {
            return Err(OperationPlanError::NotApplicable);
        }
        let prefix = self
            .raw_source
            .get(primary.span.range.start().to_usize()..edit.range.range.start().to_usize())
            .ok_or(OperationPlanError::NotApplicable)?;
        let suffix = self
            .raw_source
            .get(edit.range.range.end().to_usize()..primary.span.range.end().to_usize())
            .ok_or(OperationPlanError::NotApplicable)?;
        let expected = self.replacement.as_bytes();
        if prefix
            .len()
            .checked_add(edit.new_text.len())
            .and_then(|len| len.checked_add(suffix.len()))
            != Some(expected.len())
            || !expected.starts_with(prefix)
            || expected.get(prefix.len()..prefix.len() + edit.new_text.len())
                != Some(edit.new_text.as_bytes())
            || !expected.ends_with(suffix)
        {
            return Err(OperationPlanError::NotApplicable);
        }
        Ok(edits)
    }

    fn apply_operation(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
        if primary != self.primary {
            return Err(OperationPlanError::NotApplicable);
        }
        let result = self.inner.apply_operation(operation, primary, snapshot)?;
        let expected_len = self
            .raw_source
            .len()
            .checked_sub(primary.span.range.len() as usize)
            .and_then(|len| len.checked_add(self.replacement.len()))
            .ok_or(OperationPlanError::NotApplicable)?;
        if result.len() != 1
            || result
                .get(&primary.span.file)
                .is_none_or(|bytes| bytes.len() != expected_len)
        {
            return Err(OperationPlanError::NotApplicable);
        }
        Ok(result)
    }
}

struct NameIndex<'a> {
    nodes: HashMap<TextRange, Option<NodeId>>,
    identifiers: HashSet<&'a str>,
}

impl<'a> NameIndex<'a> {
    fn build(root: &SyntaxNode, source: &'a str) -> Option<Self> {
        let mut result = Self {
            nodes: HashMap::new(),
            identifiers: HashSet::new(),
        };
        let mut index = 0_u64;
        let mut stack = vec![SyntaxElement::Node(root.clone())];
        while let Some(element) = stack.pop() {
            let current = index;
            index = index.checked_add(1)?;
            match element {
                SyntaxElement::Node(node) => {
                    if matches!(node.kind(), SyntaxKind::NAME | SyntaxKind::NAME_REF) {
                        let node_id = NodeId::new(NodeGroup::Name, current);
                        result
                            .nodes
                            .entry(node.text_range())
                            .and_modify(|value| *value = None)
                            .or_insert(Some(node_id));
                    }
                    stack.extend(
                        node.children_with_tokens()
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev(),
                    );
                }
                SyntaxElement::Token(token) => {
                    if matches!(token.kind(), SyntaxKind::LIDENT | SyntaxKind::UIDENT) {
                        let range = token.text_range();
                        let text = source.get(range.start().to_usize()..range.end().to_usize())?;
                        result
                            .identifiers
                            .insert(text.strip_prefix("r#").unwrap_or(text));
                    }
                }
            }
        }
        Some(result)
    }
}

/// Complete context for this owner and build, with all attribute ranges and
/// locations expressed in original-file byte offsets. The caller must collect
/// every enclosing lint attribute, both policies, manifest and CLI settings,
/// package membership and deny-warnings state. `primary` is ignored here.
pub enum LintContext<'a> {
    Complete(&'a LintResolutionContext<'a>),
    Unavailable,
}

/// Whether the analyzed body is real, active for this build, and its
/// complete lowering diagnostics are available for the exact owner.
pub enum OwnerScope<'a> {
    Complete {
        lowering_diagnostics: &'a [Diagnostic],
    },
    Unavailable,
}

/// The inputs must describe a real, active body, its complete lowering
/// diagnostics, and the complete module/interface alias scope for this owner.
/// The planner must implement the CST operation and the format-unit guarantee
/// of impl/11 §11.7.4. It is not sufficient to supply arbitrary text edits.
pub struct UnusedBindingInputs<'a, 'hir, P> {
    pub body: &'a Body<'hir>,
    pub source_map: &'a HirSourceMap,
    pub snapshot: &'a WorkspaceSnapshot,
    pub symbols: &'a Interner,
    pub module_aliases: ModuleAliases<'a>,
    pub owner_scope: OwnerScope<'a>,
    pub lint: LintContext<'a>,
    pub planner: &'a P,
}

/// No diagnostics are returned when any part of the conversion is unknown.
#[derive(Debug)]
pub enum UnusedBindingDiagnostics {
    Complete(Vec<Diagnostic>),
    Incomplete,
}

pub fn unused_binding_diagnostics<P: FixOperationPlanner>(
    inputs: UnusedBindingInputs<'_, '_, P>,
) -> UnusedBindingDiagnostics {
    convert(inputs).unwrap_or(UnusedBindingDiagnostics::Incomplete)
}

fn convert<P: FixOperationPlanner>(
    inputs: UnusedBindingInputs<'_, '_, P>,
) -> Option<UnusedBindingDiagnostics> {
    let LintContext::Complete(lint) = inputs.lint else {
        return None;
    };
    let ModuleAliases::Complete(aliases) = inputs.module_aliases else {
        return None;
    };
    let OwnerScope::Complete {
        lowering_diagnostics,
    } = inputs.owner_scope
    else {
        return None;
    };
    let UnusedBindingAnalysis::Complete { owner, unused } = analyze_unused_bindings(
        inputs.body,
        inputs.source_map,
        inputs.snapshot,
        inputs.symbols,
        ModuleAliases::Complete(aliases),
        lowering_diagnostics,
    ) else {
        return None;
    };
    if owner != inputs.body.owner {
        return None;
    }
    let file = inputs.snapshot.file(inputs.source_map.file())?;
    let source = file.source();
    if !within_validation_budget(unused.len(), source.raw_bytes().len()) {
        return None;
    }
    for attribute in lint.attributes {
        let range = attribute.syntax_range();
        if range.file != source.id()
            || !source.is_original_char_boundary(range.range.start())
            || !source.is_original_char_boundary(range.range.end())
            || !source.is_original_crlf_boundary(range.range.start())
            || !source.is_original_crlf_boundary(range.range.end())
            || range.range.end().to_usize() > source.raw_bytes().len()
        {
            return None;
        }
        Diagnostic::new(
            Code::UnusedVariable,
            sai_diagnostics::Severity::Warning,
            "",
            Some(attribute.location().clone()),
        )
        .with_workspace_snapshot(inputs.snapshot)
        .ok()?;
    }
    let source_ref = SourceRef {
        path: file.path().to_owned(),
        package: file.package().map(str::to_owned),
        snapshot: file.document_version(),
    };
    let parsed = sai_syntax::parse(inputs.snapshot, inputs.source_map.file()).ok()?;
    if !parsed.errors.is_empty() {
        return None;
    }
    let names = NameIndex::build(&parsed.root, source.text())?;
    let alias_names = aliases
        .iter()
        .map(|alias| inputs.symbols.resolve(*alias).ok())
        .collect::<Option<HashSet<_>>>()?;
    let mut diagnostics = Vec::new();
    for candidate in unused {
        if candidate.span.ctxt != SpanCtxt::Root || candidate.span.file != inputs.source_map.file()
        {
            return None;
        }
        let name = inputs.symbols.resolve(candidate.name).ok()?;
        let spelling = source
            .text()
            .get(candidate.span.range.start().to_usize()..candidate.span.range.end().to_usize())?;
        if spelling.strip_prefix("r#").unwrap_or(spelling) != name.as_ref() || name.starts_with('_')
        {
            return None;
        }
        let primary = Location::from_decoded_source(
            source_ref.clone(),
            Span::new(candidate.span.file, candidate.span.range),
            source,
        )?;
        let occurrence = LintResolutionContext {
            primary: Some(&primary),
            ..*lint
        };
        let LintResolution::Emitted { severity, level } =
            resolve_lint_occurrence("UNUSED_VARIABLE", &occurrence).ok()?
        else {
            continue;
        };

        let replacement = format!("_{name}");
        if names.identifiers.contains(replacement.as_str())
            || alias_names.contains(replacement.as_str())
        {
            return None;
        }
        let node = names.nodes.get(&candidate.span.range).copied().flatten()?;
        let operation = FixOperation::new(
            FixOperationName::ReplaceNode,
            node,
            ValueObject::from([("text".to_owned(), Value::String(replacement.clone()))]),
        )
        .ok()?;
        let planner = NameOnlyPlanner {
            inner: inputs.planner,
            primary: &primary,
            raw_source: source.raw_bytes(),
            replacement: &replacement,
        };
        let fix = Fix::new(
            format!("rename `{spelling}` to `{replacement}`"),
            Applicability::MachineApplicable,
            operation,
            Vec::new(),
            &primary,
            inputs.snapshot,
            &planner,
        )
        .ok()?;
        let mut diagnostic = Diagnostic::new(
            Code::UnusedVariable,
            severity,
            format!("unused variable `{spelling}`"),
            Some(primary),
        )
        .with_workspace_snapshot(inputs.snapshot)
        .ok()?;
        diagnostic.set_level(Some(level)).ok()?;
        diagnostic.push_fix(fix).ok()?;
        diagnostics.push(diagnostic);
    }
    Some(UnusedBindingDiagnostics::Complete(diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::{BTreeMap, HashSet};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Instant;

    use sai_diagnostics::{
        LevelSource, LintLevel, LintPolicies, ManifestLintLevel, OperationPlanError,
        OverrideLintLevel, ScopedLintLevel, Severity, TextEdit,
    };
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId};
    use sai_intern::PackageInterner;
    use sai_syntax::ast::AstIdMap;
    use sai_vfs::{FileId, SnapshotFile, SourceFile, TextRange, TextSize};

    #[derive(Default)]
    struct Planner {
        invalid: bool,
        foreign_edit: bool,
        format_source: bool,
        plan_calls: Rc<Cell<usize>>,
        apply_calls: Rc<Cell<usize>>,
    }

    impl FixOperationPlanner for Planner {
        fn plan_edits(
            &self,
            operation: &FixOperation,
            primary: &Location,
            snapshot: &WorkspaceSnapshot,
        ) -> Result<Vec<TextEdit>, OperationPlanError> {
            self.plan_calls.set(self.plan_calls.get() + 1);
            let Some(Value::String(text)) = operation.args().get("text") else {
                return Err(OperationPlanError::NotApplicable);
            };
            if self.format_source {
                let source = snapshot
                    .file(primary.span.file)
                    .ok_or(OperationPlanError::TargetNotFound)?
                    .source();
                let original = source.text();
                if source.had_bom() || !original.is_ascii() {
                    return Err(OperationPlanError::NotApplicable);
                }
                let parsed = sai_syntax::parse(snapshot, primary.span.file)
                    .map_err(|_| OperationPlanError::TargetNotFound)?;
                let names = NameIndex::build(&parsed.root, original)
                    .ok_or(OperationPlanError::TargetNotFound)?;
                if names.nodes.get(&primary.span.range).copied().flatten() != Some(operation.node())
                {
                    return Err(OperationPlanError::TargetNotFound);
                }
                let statement = parsed
                    .root
                    .descendants()
                    .find(|node| {
                        node.kind() == SyntaxKind::LET_STMT
                            && node.text_range().start() <= primary.span.range.start()
                            && primary.span.range.end() <= node.text_range().end()
                    })
                    .ok_or(OperationPlanError::TargetNotFound)?;
                let statement_range = statement.text_range();
                let statement_text = original
                    .get(statement_range.start().to_usize()..statement_range.end().to_usize())
                    .ok_or(OperationPlanError::TargetNotFound)?;
                if statement_text != "let x=1" || text != "_x" {
                    return Err(OperationPlanError::NotApplicable);
                }
                let mut formatted = original.to_owned();
                formatted.replace_range(
                    statement_range.start().to_usize()..statement_range.end().to_usize(),
                    "let _x = 1",
                );
                let prefix = original
                    .bytes()
                    .zip(formatted.bytes())
                    .take_while(|(left, right)| left == right)
                    .count();
                let max_suffix = original.len().min(formatted.len()) - prefix;
                let suffix = original
                    .bytes()
                    .rev()
                    .zip(formatted.bytes().rev())
                    .take(max_suffix)
                    .take_while(|(left, right)| left == right)
                    .count();
                return Ok(vec![TextEdit {
                    file: primary.file.clone(),
                    range: Span::new(
                        primary.span.file,
                        TextRange::new(
                            TextSize::from_usize(prefix)
                                .ok_or(OperationPlanError::NotApplicable)?,
                            TextSize::from_usize(original.len() - suffix)
                                .ok_or(OperationPlanError::NotApplicable)?,
                        )
                        .ok_or(OperationPlanError::NotApplicable)?,
                    ),
                    new_text: formatted[prefix..formatted.len() - suffix].to_owned(),
                }]);
            }
            Ok(vec![TextEdit {
                file: primary.file.clone(),
                range: if self.foreign_edit {
                    Span::new(FileId::new(2), primary.span.range)
                } else {
                    primary.span
                },
                new_text: if self.invalid {
                    "wrong".to_owned()
                } else {
                    text.clone()
                },
            }])
        }

        fn apply_operation(
            &self,
            operation: &FixOperation,
            primary: &Location,
            snapshot: &WorkspaceSnapshot,
        ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
            self.apply_calls.set(self.apply_calls.get() + 1);
            let edit = self.plan_edits(operation, primary, snapshot)?.remove(0);
            let mut bytes = snapshot
                .file(primary.span.file)
                .unwrap()
                .source()
                .raw_bytes()
                .to_vec();
            bytes.splice(
                edit.range.range.start().to_usize()..edit.range.range.end().to_usize(),
                edit.new_text.bytes(),
            );
            Ok(BTreeMap::from([(primary.span.file, bytes)]))
        }
    }

    #[derive(Default)]
    struct Options {
        bom: bool,
        alias: Option<&'static str>,
        cli: Option<OverrideLintLevel>,
        manifest: Option<OverrideLintLevel>,
        attribute: Option<OverrideLintLevel>,
        dependency: bool,
        deny_warnings: bool,
        no_aliases: bool,
        no_scope: bool,
        no_lint: bool,
        invalid_planner: bool,
        foreign_edit: bool,
        format_source: bool,
        stale_snapshot: bool,
        lowering_error: bool,
    }

    fn run(text: &str, options: Options) -> UnusedBindingDiagnostics {
        run_counted(text, options).0
    }

    fn run_counted(text: &str, options: Options) -> (UnusedBindingDiagnostics, usize, usize) {
        let file_id = FileId::new(1);
        let source = Arc::new(SourceFile::new(file_id, text, options.bom).unwrap());
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source.clone(), "src/lib.sai", None, None)])
                .unwrap();
        let parsed = sai_syntax::parse(&snapshot, file_id).unwrap();
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let ast_ids = AstIdMap::new(file_id, &parsed.root).unwrap();
        let symbols = Interner::new();
        let tree =
            sai_hir::item_tree::ItemTree::collect(file_id, text, &parsed.root, &ast_ids, &symbols)
                .unwrap();
        let entry = &tree.items[0];
        let packages = PackageInterner::new();
        let package = packages.intern("acme:app#1.0").unwrap();
        let paths = DefPathInterner::new(package, &packages, &symbols).unwrap();
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::ValueNs(entry.name),
                disambiguator: 0,
            }],
            &packages,
            &symbols,
        )
        .unwrap();
        let owner = OwnerId {
            def_id: paths.intern(path.clone()).unwrap(),
        };
        let arena = sai_hir::arena::HirArena::new();
        let lowering = sai_hir::lower::HirLoweringContext::new(
            &ast_ids,
            &snapshot,
            &parsed.root,
            &arena,
            &symbols,
            &paths,
        );
        let (lowered, source_map) =
            sai_hir::lower::lower_function_owner(owner, &path, &tree, entry.ast_id, &lowering)
                .unwrap();
        assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);
        let changed_source =
            Arc::new(SourceFile::new(file_id, format!("{text} "), options.bom).unwrap());
        let other_snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(changed_source, "src/lib.sai", None, None)])
                .unwrap();
        let selected_snapshot = if options.stale_snapshot {
            &other_snapshot
        } else {
            &snapshot
        };
        let errors = if options.lowering_error {
            vec![Diagnostic::new(
                Code::UnusedVariable,
                Severity::Error,
                "error",
                None,
            )]
        } else {
            Vec::new()
        };
        let aliases: HashSet<_> = options
            .alias
            .into_iter()
            .map(|alias| symbols.intern(alias).unwrap())
            .collect();
        let point = if options.bom { 3 } else { 0 };
        let full_range = Span::new(
            file_id,
            TextRange::new(
                TextSize::new(point),
                TextSize::new(point + text.len() as u32),
            )
            .unwrap(),
        );
        let location = Location::new(
            "src/lib.sai",
            Span::new(
                file_id,
                TextRange::new(TextSize::new(point), TextSize::new(point)).unwrap(),
            ),
        );
        let attributes = options
            .attribute
            .into_iter()
            .map(|level| {
                ScopedLintLevel::new("UNUSED_VARIABLE", level, full_range, location.clone())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let manifest = options
            .manifest
            .map(|level| ManifestLintLevel::new(level, location));
        let policies = LintPolicies::default();
        let lint = LintResolutionContext {
            primary: None,
            attributes: &attributes,
            cli: options.cli,
            manifest: manifest.as_ref(),
            package_policy: &policies,
            root_policy: &policies,
            is_root_or_workspace_member: !options.dependency,
            deny_warnings: options.deny_warnings,
        };
        let planner = Planner {
            invalid: options.invalid_planner,
            foreign_edit: options.foreign_edit,
            format_source: options.format_source,
            ..Planner::default()
        };
        let result = unused_binding_diagnostics(UnusedBindingInputs {
            body: lowered.body.unwrap(),
            source_map: &source_map,
            snapshot: selected_snapshot,
            symbols: &symbols,
            module_aliases: if options.no_aliases {
                ModuleAliases::Unavailable
            } else {
                ModuleAliases::Complete(&aliases)
            },
            owner_scope: if options.no_scope {
                OwnerScope::Unavailable
            } else {
                OwnerScope::Complete {
                    lowering_diagnostics: &errors,
                }
            },
            lint: if options.no_lint {
                LintContext::Unavailable
            } else {
                LintContext::Complete(&lint)
            },
            planner: &planner,
        });
        (result, planner.plan_calls.get(), planner.apply_calls.get())
    }

    fn one(result: UnusedBindingDiagnostics) -> Diagnostic {
        let UnusedBindingDiagnostics::Complete(mut diagnostics) = result else {
            panic!("incomplete")
        };
        assert_eq!(diagnostics.len(), 1);
        diagnostics.remove(0)
    }

    #[test]
    fn produces_original_byte_locations_and_safe_fixes() {
        for (text, name, original, replacement) in [
            ("fn sample(x: I32) -> I32 { 0 }\n", "x", "x", "_x"),
            (
                "fn sample(r#type: I32) -> I32 { 0 }\n",
                "r#type",
                "r#type",
                "_type",
            ),
        ] {
            let diagnostic = one(run(
                text,
                Options {
                    bom: true,
                    ..Options::default()
                },
            ));
            let range = diagnostic.primary().unwrap().span.range;
            assert_eq!(range.start().to_usize(), text.find(name).unwrap() + 3);
            assert_eq!(
                range.end().to_usize() - range.start().to_usize(),
                original.len()
            );
            assert_eq!(diagnostic.severity, Severity::Warning);
            assert_eq!(diagnostic.level().unwrap().source, LevelSource::Default);
            assert_eq!(
                diagnostic.fixes()[0].applicability(),
                Applicability::MachineApplicable
            );
            assert_eq!(diagnostic.fixes()[0].edits()[0].new_text, replacement);
        }
    }

    #[test]
    fn resolves_levels_and_preserves_provenance() {
        let text = "fn sample(x: I32) -> I32 { 0 }\n";
        for (options, severity, source, value) in [
            (
                Options {
                    cli: Some(OverrideLintLevel::Deny),
                    ..Options::default()
                },
                Severity::Error,
                LevelSource::Cli,
                LintLevel::Deny,
            ),
            (
                Options {
                    manifest: Some(OverrideLintLevel::Deny),
                    ..Options::default()
                },
                Severity::Error,
                LevelSource::Manifest,
                LintLevel::Deny,
            ),
            (
                Options {
                    attribute: Some(OverrideLintLevel::Warn),
                    cli: Some(OverrideLintLevel::Deny),
                    ..Options::default()
                },
                Severity::Warning,
                LevelSource::Attribute,
                LintLevel::Warn,
            ),
            (
                Options {
                    deny_warnings: true,
                    ..Options::default()
                },
                Severity::Error,
                LevelSource::DenyWarnings,
                LintLevel::Warn,
            ),
        ] {
            let diagnostic = one(run(text, options));
            assert_eq!(diagnostic.severity, severity);
            let level = diagnostic.level().unwrap();
            assert_eq!((level.source, level.value), (source, value));
            assert_eq!(
                level.location.is_some(),
                matches!(source, LevelSource::Attribute | LevelSource::Manifest)
            );
        }
        for options in [
            Options {
                cli: Some(OverrideLintLevel::Allow),
                ..Options::default()
            },
            Options {
                dependency: true,
                cli: Some(OverrideLintLevel::Deny),
                deny_warnings: true,
                ..Options::default()
            },
        ] {
            assert!(
                matches!(run(text, options), UnusedBindingDiagnostics::Complete(diagnostics) if diagnostics.is_empty())
            );
        }
    }

    #[test]
    fn refuses_unknown_or_unsafe_inputs() {
        let text = "fn sample(x: I32) -> I32 { 0 }\n";
        for options in [
            Options {
                no_aliases: true,
                ..Options::default()
            },
            Options {
                no_scope: true,
                ..Options::default()
            },
            Options {
                no_lint: true,
                ..Options::default()
            },
            Options {
                alias: Some("_x"),
                ..Options::default()
            },
            Options {
                invalid_planner: true,
                ..Options::default()
            },
            Options {
                stale_snapshot: true,
                ..Options::default()
            },
            Options {
                lowering_error: true,
                ..Options::default()
            },
            Options {
                alias: Some("x"),
                ..Options::default()
            },
        ] {
            assert!(matches!(
                run(text, options),
                UnusedBindingDiagnostics::Incomplete
            ));
        }
        assert!(matches!(
            run(
                "fn sample(x: I32) -> I32 { let _x = 1\n 0 }\n",
                Options::default()
            ),
            UnusedBindingDiagnostics::Incomplete
        ));
        assert!(matches!(
            run(
                "fn sample(x: I32) -> I32 { let y: I32 = x\n y }\n",
                Options::default()
            ),
            UnusedBindingDiagnostics::Incomplete
        ));
        assert!(matches!(
            run(
                "fn sample(x: I32, y: I32) -> I32 { let _y = 1\n 0 }\n",
                Options::default()
            ),
            UnusedBindingDiagnostics::Incomplete
        ));
    }

    #[test]
    fn attribute_location_and_offsets_account_for_bom_and_unicode() {
        let text = "// 日本語\nfn sample(x: I32) -> I32 { 0 }\n";
        let diagnostic = one(run(
            text,
            Options {
                bom: true,
                attribute: Some(OverrideLintLevel::Deny),
                ..Options::default()
            },
        ));
        assert_eq!(
            diagnostic.primary().unwrap().span.range.start().to_usize(),
            text.find("x: I32").unwrap() + 3
        );
        assert_eq!(
            diagnostic
                .level()
                .unwrap()
                .location
                .as_ref()
                .unwrap()
                .span
                .range
                .start()
                .to_usize(),
            3
        );
        assert_eq!(diagnostic.severity, Severity::Error);
    }

    #[test]
    fn used_and_exempt_names_are_not_reported() {
        assert!(
            matches!(run("fn sample(x: I32, _y: I32) -> I32 { x }\n", Options::default()), UnusedBindingDiagnostics::Complete(diagnostics) if diagnostics.is_empty())
        );
    }

    #[test]
    fn unit_formatting_edits_outside_name_are_incomplete() {
        let text = "fn sample() -> I32 {\n    let x=1\n    0\n}\n";
        let file_id = FileId::new(1);
        let source = Arc::new(SourceFile::new(file_id, text, false).unwrap());
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, None)]).unwrap();
        let parsed = sai_syntax::parse(&snapshot, file_id).unwrap();
        let start = text.find("x=1").unwrap();
        let range = TextRange::new(
            TextSize::from_usize(start).unwrap(),
            TextSize::from_usize(start + 1).unwrap(),
        )
        .unwrap();
        let primary = Location::new("src/lib.sai", Span::new(file_id, range));
        let names = NameIndex::build(&parsed.root, text).unwrap();
        let operation = FixOperation::new(
            FixOperationName::ReplaceNode,
            names.nodes[&range].unwrap(),
            ValueObject::from([("text".to_owned(), Value::String("_x".to_owned()))]),
        )
        .unwrap();
        let planner = Planner {
            invalid: false,
            format_source: true,
            ..Planner::default()
        };
        let fix = Fix::new(
            "rename",
            Applicability::MachineApplicable,
            operation,
            Vec::new(),
            &primary,
            &snapshot,
            &planner,
        )
        .unwrap();
        assert!(fix.edits()[0].range.range.end() > primary.span.range.end());
        assert!(matches!(
            run(
                text,
                Options {
                    format_source: true,
                    ..Options::default()
                }
            ),
            UnusedBindingDiagnostics::Incomplete
        ));
    }

    #[test]
    fn indexes_names_and_node_ids_for_large_sources() {
        let mut text = String::from("fn sample() -> I32 {\n");
        for number in 0..1200 {
            text.push_str(&format!("    let value{number} = 0\n"));
        }
        text.push_str("    0\n}\n");
        let file_id = FileId::new(1);
        let source = Arc::new(SourceFile::new(file_id, text.as_str(), false).unwrap());
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, None)]).unwrap();
        let parsed = sai_syntax::parse(&snapshot, file_id).unwrap();
        assert!(parsed.errors.is_empty());
        let index = NameIndex::build(&parsed.root, &text).unwrap();
        assert!(index.identifiers.len() >= 1201);
        for name in ["value0", "value600", "value1199"] {
            let start = text.find(&format!("{name} =")).unwrap();
            let range = TextRange::new(
                TextSize::from_usize(start).unwrap(),
                TextSize::from_usize(start + name.len()).unwrap(),
            )
            .unwrap();
            let span = sai_hir::Span {
                file: file_id,
                range,
                ctxt: SpanCtxt::Root,
            };
            assert_eq!(
                index.nodes[&range].unwrap().to_string(),
                super::super::name_node_id_for_span(&parsed.root, span).unwrap()
            );
            assert!(index.identifiers.contains(name));
        }
    }

    #[test]
    fn comments_and_longer_identifiers_do_not_create_false_collisions() {
        let text = "// _x is mentioned here\nfn sample(x: I32) -> I32 { let _xyz = 1\n 0 }\n";
        assert_eq!(
            one(run(text, Options::default())).fixes()[0].edits()[0].new_text,
            "_x"
        );
    }

    #[test]
    fn validation_budget_boundaries_and_overflow() {
        assert!(within_validation_budget(0, usize::MAX));
        assert!(within_validation_budget(1, MAX_VALIDATION_INPUT_BYTES));
        assert!(!within_validation_budget(1, MAX_VALIDATION_INPUT_BYTES + 1));
        assert!(within_validation_budget(
            MAX_CANDIDATES,
            MAX_VALIDATION_INPUT_BYTES / MAX_CANDIDATES
        ));
        assert!(!within_validation_budget(
            MAX_CANDIDATES,
            MAX_VALIDATION_INPUT_BYTES / MAX_CANDIDATES + 1
        ));
        assert!(!within_validation_budget(MAX_CANDIDATES + 1, 0));
        assert!(!within_validation_budget(2, usize::MAX));
    }

    fn budget_case(count: usize, min_bytes: usize) -> String {
        let params = (0..count)
            .map(|number| format!("value{number}: I32"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut text = format!("fn sample({params}) -> I32 {{ 0 }}\n// ");
        text.push_str(&"a".repeat(min_bytes.saturating_sub(text.len() + 1)));
        text.push('\n');
        text
    }

    #[test]
    fn budget_refuses_before_any_planner_call_and_reports_timings() {
        for (count, size, expected) in [
            (4, 0, true),
            (MAX_CANDIDATES, 0, true),
            (
                MAX_CANDIDATES,
                MAX_VALIDATION_INPUT_BYTES / MAX_CANDIDATES,
                true,
            ),
            (MAX_CANDIDATES + 1, 0, false),
            (
                MAX_CANDIDATES,
                MAX_VALIDATION_INPUT_BYTES / MAX_CANDIDATES + 1,
                false,
            ),
        ] {
            let text = budget_case(count, size);
            let now = Instant::now();
            let (result, plans, applications) = run_counted(&text, Options::default());
            let elapsed = now.elapsed();
            println!(
                "candidates={count} source_bytes={} estimated_input_bytes={} planner_calls={plans} apply_calls={applications} elapsed_us={}",
                text.len(),
                count * text.len(),
                elapsed.as_micros()
            );
            if expected {
                assert!(
                    matches!(result, UnusedBindingDiagnostics::Complete(diagnostics) if diagnostics.len() == count)
                );
                assert_eq!(applications, count);
                assert_eq!(plans, count * 2);
            } else {
                assert!(matches!(result, UnusedBindingDiagnostics::Incomplete));
                assert_eq!((plans, applications), (0, 0));
            }
        }
        let text = budget_case(MAX_CANDIDATES + 1, 0);
        let (result, plans, applications) = run_counted(
            &text,
            Options {
                cli: Some(OverrideLintLevel::Allow),
                ..Options::default()
            },
        );
        assert!(matches!(result, UnusedBindingDiagnostics::Incomplete));
        assert_eq!((plans, applications), (0, 0));
    }

    #[test]
    fn unsafe_edits_are_rejected_before_operation_application() {
        let text = "fn sample(x: I32) -> I32 { 0 }\n";
        let (result, plans, applications) = run_counted(
            text,
            Options {
                invalid_planner: true,
                ..Options::default()
            },
        );
        assert!(matches!(result, UnusedBindingDiagnostics::Incomplete));
        assert_eq!((plans, applications), (1, 0));
        let (result, plans, applications) = run_counted(
            text,
            Options {
                foreign_edit: true,
                ..Options::default()
            },
        );
        assert!(matches!(result, UnusedBindingDiagnostics::Incomplete));
        assert_eq!((plans, applications), (1, 0));
        let text = "fn sample() -> I32 {\n    let x=1\n    0\n}\n";
        let (result, plans, applications) = run_counted(
            text,
            Options {
                format_source: true,
                ..Options::default()
            },
        );
        assert!(matches!(result, UnusedBindingDiagnostics::Incomplete));
        assert_eq!((plans, applications), (1, 0));
    }
}
