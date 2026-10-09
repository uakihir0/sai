//! Validation of graph relationships recorded in `sai.lock`.
//!
//! These checks determine whether the lock's recorded graph is internally
//! consistent. Root-contract comparison, source selection, pinned-path
//! contents, and fetched-content verification require resolver inputs and are
//! deliberately outside this module.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use crate::lock::{
    LockPackage, LockPackageField, LockParseOutput, LockRequirement, LockRoot, LockRootField,
};
use crate::lock_validation::{LockValidationOutput, validate as validate_lock};
use crate::manifest::Spanned;
use crate::version::{CompatibilityLine, Version};

/// One node in the lock graph, identified by package identity and
/// compatibility line.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LockGraphNode {
    pub package: Box<str>,
    pub line: CompatibilityLine,
}

/// A graph inconsistency that makes the lock unusable in Mode A.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockGraphError {
    pub kind: LockGraphErrorKind,
    /// Byte range of the edge or entry that demonstrates the inconsistency.
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockGraphErrorKind {
    MissingEntry {
        package: Box<str>,
        line: CompatibilityLine,
    },
    SelectedVersionMismatch {
        package: Box<str>,
        line: CompatibilityLine,
        selected: Version,
        entry_version: Version,
    },
    RequirementNotSatisfied {
        package: Box<str>,
        line: CompatibilityLine,
        entry_version: Version,
    },
    CycleToRoot {
        package: Box<str>,
        line: CompatibilityLine,
    },
    DependencyCycle {
        /// The cycle's nodes, with its first node repeated at the end.
        nodes: Vec<LockGraphNode>,
    },
    UnreachableEntry {
        package: Box<str>,
        line: CompatibilityLine,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockGraphValidationOutput {
    pub errors: Vec<LockGraphError>,
}

impl LockGraphValidationOutput {
    pub fn is_consistent(&self) -> bool {
        self.errors.is_empty()
    }
}

/// The graph checker requires a syntactically and lock-locally valid file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockGraphValidationInputError {
    InvalidSyntax,
    InvalidLock(LockValidationOutput),
}

#[derive(Clone, Debug)]
struct GraphEdge {
    target: LockGraphNode,
    span: Range<usize>,
}

#[derive(Clone, Debug)]
struct DfsFrame {
    node: LockGraphNode,
    next_edge: usize,
}

/// Checks the recorded dependency graph after syntax and lock-local validation.
///
/// Every edge must select the entry for the line of its requirement minimum;
/// that entry must satisfy the requirement and match the edge's recorded
/// selection. All entries must be reachable from the root, and the recorded
/// graph must contain no cycles. An edge to the root's own node is reported as
/// a cycle even though the root has no package entry.
///
/// Callers must run graph-dependent checks that use the original parsed values
/// (such as capability-name validation) before normalizing or serializing the
/// lock.
pub fn validate(
    parsed: &LockParseOutput,
) -> Result<LockGraphValidationOutput, LockGraphValidationInputError> {
    let local_validation = validate_lock(parsed);
    if !local_validation.is_valid() {
        return Err(LockGraphValidationInputError::InvalidLock(local_validation));
    }
    if !parsed.is_syntax_valid() {
        return Err(LockGraphValidationInputError::InvalidSyntax);
    }

    let file = &parsed.file;
    let root = file.root.as_ref().expect("valid lock has a root block");
    let root_node = node_for_root(&root.value);
    let entries = file
        .packages
        .iter()
        .map(|entry| (node_for_package(&entry.value), entry))
        .collect::<HashMap<_, _>>();
    let ordered_nodes = file
        .packages
        .iter()
        .map(|entry| node_for_package(&entry.value))
        .collect::<Vec<_>>();

    let mut output = LockGraphValidationOutput::default();
    let mut root_edges = Vec::new();
    let mut edges_by_node = HashMap::<LockGraphNode, Vec<GraphEdge>>::new();

    for field in &root.value.fields {
        if let LockRootField::Requires(requirement) = &field.value {
            let edge = collect_edge(
                requirement,
                field.span.clone(),
                &root_node,
                &entries,
                &mut output.errors,
            );
            if let Some(edge) = edge {
                root_edges.push(edge);
            }
        }
    }

    for entry in &file.packages {
        let source_node = node_for_package(&entry.value);
        let mut edges = Vec::new();
        for field in &entry.value.fields {
            if let LockPackageField::Requires(requirement) = &field.value {
                if let Some(edge) = collect_edge(
                    requirement,
                    field.span.clone(),
                    &root_node,
                    &entries,
                    &mut output.errors,
                ) {
                    edges.push(edge);
                }
            }
        }
        edges_by_node.insert(source_node, edges);
    }

    let reachable = reachable_nodes(&root_edges, &entries, &edges_by_node);
    for node in &ordered_nodes {
        if !reachable.contains(node) {
            let entry = entries[node];
            output.errors.push(LockGraphError {
                kind: LockGraphErrorKind::UnreachableEntry {
                    package: node.package.clone(),
                    line: node.line,
                },
                span: entry.span.clone(),
            });
        }
    }

    if let Some(cycle) = find_cycle(&ordered_nodes, &entries, &edges_by_node, &root_node) {
        output.errors.push(cycle);
    }

    Ok(output)
}

fn collect_edge(
    requirement: &LockRequirement,
    span: Range<usize>,
    root_node: &LockGraphNode,
    entries: &HashMap<LockGraphNode, &Spanned<LockPackage>>,
    errors: &mut Vec<LockGraphError>,
) -> Option<GraphEdge> {
    let target = LockGraphNode {
        package: requirement.package.clone(),
        line: requirement.requirement.compatibility_line(),
    };

    if &target == root_node {
        errors.push(LockGraphError {
            kind: LockGraphErrorKind::CycleToRoot {
                package: target.package.clone(),
                line: target.line,
            },
            span,
        });
        return None;
    }

    let Some(entry) = entries.get(&target) else {
        errors.push(LockGraphError {
            kind: LockGraphErrorKind::MissingEntry {
                package: target.package.clone(),
                line: target.line,
            },
            span,
        });
        return None;
    };

    if entry.value.version != requirement.selected {
        errors.push(LockGraphError {
            kind: LockGraphErrorKind::SelectedVersionMismatch {
                package: target.package.clone(),
                line: target.line,
                selected: requirement.selected.clone(),
                entry_version: entry.value.version.clone(),
            },
            span: span.clone(),
        });
    }
    if !requirement
        .requirement
        .matches_for_resolution(&entry.value.version)
    {
        errors.push(LockGraphError {
            kind: LockGraphErrorKind::RequirementNotSatisfied {
                package: target.package.clone(),
                line: target.line,
                entry_version: entry.value.version.clone(),
            },
            span: span.clone(),
        });
    }

    Some(GraphEdge { target, span })
}

fn reachable_nodes(
    root_edges: &[GraphEdge],
    entries: &HashMap<LockGraphNode, &Spanned<LockPackage>>,
    edges_by_node: &HashMap<LockGraphNode, Vec<GraphEdge>>,
) -> HashSet<LockGraphNode> {
    let mut reachable = HashSet::new();
    let mut pending = VecDeque::new();
    for edge in root_edges {
        if entries.contains_key(&edge.target) && reachable.insert(edge.target.clone()) {
            pending.push_back(edge.target.clone());
        }
    }

    while let Some(node) = pending.pop_front() {
        if let Some(edges) = edges_by_node.get(&node) {
            for edge in edges {
                if entries.contains_key(&edge.target) && reachable.insert(edge.target.clone()) {
                    pending.push_back(edge.target.clone());
                }
            }
        }
    }
    reachable
}

fn find_cycle(
    ordered_nodes: &[LockGraphNode],
    entries: &HashMap<LockGraphNode, &Spanned<LockPackage>>,
    edges_by_node: &HashMap<LockGraphNode, Vec<GraphEdge>>,
    root_node: &LockGraphNode,
) -> Option<LockGraphError> {
    let mut states = HashMap::<LockGraphNode, u8>::new();
    let mut active_positions = HashMap::<LockGraphNode, usize>::new();

    for start in ordered_nodes {
        if states.get(start).copied().unwrap_or(0) != 0 {
            continue;
        }
        states.insert(start.clone(), 1);
        active_positions.insert(start.clone(), 0);
        let mut stack = vec![DfsFrame {
            node: start.clone(),
            next_edge: 0,
        }];

        while !stack.is_empty() {
            let frame = stack.last_mut().expect("stack is non-empty");
            let Some(edges) = edges_by_node.get(&frame.node) else {
                finish_dfs_frame(&mut stack, &mut states, &mut active_positions);
                continue;
            };
            if frame.next_edge == edges.len() {
                finish_dfs_frame(&mut stack, &mut states, &mut active_positions);
                continue;
            }

            let edge = &edges[frame.next_edge];
            frame.next_edge += 1;
            if &edge.target == root_node || !entries.contains_key(&edge.target) {
                continue;
            }

            match states.get(&edge.target).copied().unwrap_or(0) {
                0 => {
                    states.insert(edge.target.clone(), 1);
                    active_positions.insert(edge.target.clone(), stack.len());
                    stack.push(DfsFrame {
                        node: edge.target.clone(),
                        next_edge: 0,
                    });
                }
                1 => {
                    let cycle_start = active_positions[&edge.target];
                    let mut nodes = stack[cycle_start..]
                        .iter()
                        .map(|item| item.node.clone())
                        .collect::<Vec<_>>();
                    nodes.push(edge.target.clone());
                    rotate_cycle_to_least_node(&mut nodes);
                    return Some(LockGraphError {
                        kind: LockGraphErrorKind::DependencyCycle { nodes },
                        span: edge.span.clone(),
                    });
                }
                2 => {}
                _ => unreachable!(),
            }
        }
    }
    None
}

fn rotate_cycle_to_least_node(nodes: &mut [LockGraphNode]) {
    let cycle_length = nodes.len() - 1;
    let first = nodes[..cycle_length]
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| left.cmp(right))
        .map(|(index, _)| index)
        .expect("a cycle contains at least one node");
    nodes[..cycle_length].rotate_left(first);
    nodes[cycle_length] = nodes[0].clone();
}

fn finish_dfs_frame(
    stack: &mut Vec<DfsFrame>,
    states: &mut HashMap<LockGraphNode, u8>,
    active_positions: &mut HashMap<LockGraphNode, usize>,
) {
    let frame = stack.pop().expect("stack is non-empty");
    states.insert(frame.node.clone(), 2);
    active_positions.remove(&frame.node);
}

fn node_for_root(root: &LockRoot) -> LockGraphNode {
    LockGraphNode {
        package: root.package.clone(),
        line: root.version.compatibility_line(),
    }
}

fn node_for_package(package: &LockPackage) -> LockGraphNode {
    LockGraphNode {
        package: package.package.clone(),
        line: package.version.compatibility_line(),
    }
}

#[cfg(test)]
mod tests {
    use super::{LockGraphErrorKind, LockGraphValidationInputError, validate};
    use crate::lock::parse;

    fn lock(root_edges: &[&str], entries: &[(&str, &str, &[&str])]) -> String {
        let mut source = String::from(
            "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n",
        );
        for edge in root_edges {
            source.push_str("    ");
            source.push_str(edge);
            source.push('\n');
        }
        for (package, version, edges) in entries {
            source.push_str(&format!(
                "package {package} @ {version}\n    source path \"../deps/{}\"\n",
                package.replace(':', "-")
            ));
            for edge in *edges {
                source.push_str("    ");
                source.push_str(edge);
                source.push('\n');
            }
        }
        source
    }

    fn error_kinds(source: &str) -> Vec<LockGraphErrorKind> {
        validate(&parse(source.as_bytes()))
            .expect("fixture has valid syntax and lock-local semantics")
            .errors
            .into_iter()
            .map(|error| error.kind)
            .collect()
    }

    #[test]
    fn accepts_a_reachable_acyclic_recorded_graph() {
        let source = lock(
            &["requires acme:a @ ^1 -> 1.2.0"],
            &[
                ("acme:a", "1.2.0", &["requires acme:b @ ^2 -> 2.0.0"]),
                ("acme:b", "2.0.0", &[]),
            ],
        );
        let output = validate(&parse(source.as_bytes())).expect("valid lock input");
        assert!(output.is_consistent(), "{:?}", output.errors);
    }

    #[test]
    fn reports_edges_without_entries() {
        let source = lock(&["requires acme:missing @ ^1 -> 1.0.0"], &[]);
        assert_eq!(
            error_kinds(&source),
            vec![LockGraphErrorKind::MissingEntry {
                package: "acme:missing".into(),
                line: crate::version::Version::parse("1.0.0")
                    .expect("valid version")
                    .compatibility_line(),
            }]
        );
    }

    #[test]
    fn reports_a_selected_version_that_differs_from_its_entry() {
        let source = lock(
            &["requires acme:a @ ^1 -> 1.3.0"],
            &[("acme:a", "1.2.0", &[])],
        );
        assert!(
            error_kinds(&source).contains(&LockGraphErrorKind::SelectedVersionMismatch {
                package: "acme:a".into(),
                line: crate::version::Version::parse("1.0.0")
                    .expect("valid version")
                    .compatibility_line(),
                selected: crate::version::Version::parse("1.3.0").expect("valid version"),
                entry_version: crate::version::Version::parse("1.2.0").expect("valid version"),
            })
        );
    }

    #[test]
    fn reports_entry_versions_outside_the_edge_requirement() {
        let source = lock(
            &["requires acme:a @ >=1, <1.2 -> 1.2.0"],
            &[("acme:a", "1.2.0", &[])],
        );
        assert!(
            error_kinds(&source).contains(&LockGraphErrorKind::RequirementNotSatisfied {
                package: "acme:a".into(),
                line: crate::version::Version::parse("1.0.0")
                    .expect("valid version")
                    .compatibility_line(),
                entry_version: crate::version::Version::parse("1.2.0").expect("valid version"),
            })
        );
    }

    #[test]
    fn reports_entries_that_cannot_be_reached_from_the_root() {
        let source = lock(
            &["requires acme:a @ ^1 -> 1.0.0"],
            &[("acme:a", "1.0.0", &[]), ("acme:orphan", "2.0.0", &[])],
        );
        assert!(
            error_kinds(&source).contains(&LockGraphErrorKind::UnreachableEntry {
                package: "acme:orphan".into(),
                line: crate::version::Version::parse("2.0.0")
                    .expect("valid version")
                    .compatibility_line(),
            })
        );
    }

    #[test]
    fn reports_cycles_between_dependency_entries() {
        let source = lock(
            &["requires acme:z @ ^1 -> 1.0.0"],
            &[
                ("acme:z", "1.0.0", &["requires acme:a @ ^1 -> 1.0.0"]),
                ("acme:a", "1.0.0", &["requires acme:z @ ^1 -> 1.0.0"]),
            ],
        );
        let kinds = error_kinds(&source);
        let cycle = kinds
            .iter()
            .find_map(|kind| match kind {
                LockGraphErrorKind::DependencyCycle { nodes } => Some(nodes),
                _ => None,
            })
            .expect("cycle reported");
        assert_eq!(
            cycle
                .iter()
                .map(|node| node.package.as_ref())
                .collect::<Vec<_>>(),
            ["acme:a", "acme:z", "acme:a"]
        );
    }

    #[test]
    fn canonical_cycle_rotation_preserves_every_node() {
        let source = lock(
            &["requires acme:b @ ^1 -> 1.0.0"],
            &[
                ("acme:b", "1.0.0", &["requires acme:z @ ^1 -> 1.0.0"]),
                ("acme:z", "1.0.0", &["requires acme:a @ ^1 -> 1.0.0"]),
                ("acme:a", "1.0.0", &["requires acme:b @ ^1 -> 1.0.0"]),
            ],
        );
        let cycle = error_kinds(&source)
            .into_iter()
            .find_map(|kind| match kind {
                LockGraphErrorKind::DependencyCycle { nodes } => Some(nodes),
                _ => None,
            })
            .expect("cycle reported");
        assert_eq!(
            cycle
                .iter()
                .map(|node| node.package.as_ref())
                .collect::<Vec<_>>(),
            ["acme:a", "acme:b", "acme:z", "acme:a"]
        );
    }

    #[test]
    fn reports_a_dependency_edge_back_to_the_root_node() {
        let source = lock(
            &["requires acme:a @ ^1 -> 1.0.0"],
            &[("acme:a", "1.0.0", &["requires acme:app @ ^1 -> 1.0.0"])],
        );
        assert!(
            error_kinds(&source).contains(&LockGraphErrorKind::CycleToRoot {
                package: "acme:app".into(),
                line: crate::version::Version::parse("1.0.0")
                    .expect("valid version")
                    .compatibility_line(),
            })
        );
    }

    #[test]
    fn root_identity_on_another_line_is_a_distinct_node() {
        let source = lock(
            &["requires acme:child @ ^1 -> 1.0.0"],
            &[
                (
                    "acme:child",
                    "1.0.0",
                    &["requires acme:app @ ^0.4 -> 0.4.2"],
                ),
                ("acme:app", "0.4.2", &[]),
            ],
        );
        let output = validate(&parse(source.as_bytes())).expect("valid lock input");
        assert!(output.is_consistent(), "{:?}", output.errors);
    }

    #[test]
    fn rejects_invalid_input_before_graph_analysis() {
        let future_format = parse(b"lock 2\n");
        assert!(matches!(
            validate(&future_format),
            Err(LockGraphValidationInputError::InvalidLock(_))
        ));

        let invalid_syntax = parse(
            b"lock 1\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n    unknown-field value\n",
        );
        assert_eq!(
            validate(&invalid_syntax),
            Err(LockGraphValidationInputError::InvalidSyntax)
        );
    }
}
