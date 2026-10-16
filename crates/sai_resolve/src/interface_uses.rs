//! Local interface-use graph analysis.
//!
//! The caller supplies interface IDs in generation order and already-resolved
//! local interface references in source order. References to interfaces
//! outside the supplied package-local interface list are ignored.

use sai_diagnostics::{Child, ChildKind, Code, Diagnostic, Location, Severity};
use sai_hir::LocalDefId;
use sai_vfs::Span;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// A resolved type reference from one local interface to another.
///
/// References must be supplied in package generation order, and in source
/// order within each interface. The location's span is retained in
/// [`InterfaceUses::uses`] and is also used for cycle diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceUseReference {
    pub from: LocalDefId,
    pub to: LocalDefId,
    pub location: Location,
}

/// Input to [`interface_uses`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceUsesInput {
    /// Canonical package identity used to attribute diagnostics.
    pub package: String,
    /// Local interfaces in the compiler's generation order.
    pub interfaces: Vec<LocalDefId>,
    /// Resolved local references in generation and source order.
    pub references: Vec<InterfaceUseReference>,
}

/// Local interface-use edges and cycle diagnostics for one package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceUses {
    /// Interface → distinct local interfaces it uses, in first-reference
    /// order. Self-use references are omitted.
    pub uses: BTreeMap<LocalDefId, Vec<(LocalDefId, Span)>>,
    /// Every member of each nontrivial strongly connected component.
    pub cyclic: BTreeSet<LocalDefId>,
    /// One diagnostic per nontrivial strongly connected component, ordered by
    /// the generation position of its first interface.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug)]
struct Edge {
    target: usize,
    location: Location,
}

#[derive(Clone, Copy, Debug)]
struct TarjanFrame {
    node: usize,
    next_edge: usize,
    parent: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct CycleEdge {
    from: usize,
    edge_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct CycleSearchFrame {
    node: usize,
    next_edge: usize,
}

/// Builds the local interface-use graph and reports nontrivial SCCs.
///
/// The edge list is deduplicated by ordered `(from, to)` pair, retaining the
/// first source reference. Interface and reference order are input contracts;
/// neither session-local definition-ID ordering nor hash-table iteration
/// affects SCC traversal or diagnostics.
pub fn interface_uses(input: InterfaceUsesInput) -> InterfaceUses {
    let mut interface_index = HashMap::with_capacity(input.interfaces.len());
    for (index, interface) in input.interfaces.iter().copied().enumerate() {
        interface_index.entry(interface).or_insert(index);
    }

    let mut uses = BTreeMap::new();
    let mut adjacency = vec![Vec::<Edge>::new(); input.interfaces.len()];
    let mut seen_edges = (0..input.interfaces.len())
        .map(|_| HashSet::<usize>::new())
        .collect::<Vec<_>>();
    for interface in &input.interfaces {
        uses.entry(*interface).or_insert_with(Vec::new);
    }

    for reference in input.references {
        let (Some(&from), Some(&to)) = (
            interface_index.get(&reference.from),
            interface_index.get(&reference.to),
        ) else {
            continue;
        };
        if from == to || !seen_edges[from].insert(to) {
            continue;
        }
        uses.entry(reference.from)
            .or_insert_with(Vec::new)
            .push((reference.to, reference.location.span));
        adjacency[from].push(Edge {
            target: to,
            location: reference.location,
        });
    }

    let mut components = strongly_connected_components(&adjacency)
        .into_iter()
        .filter(|component| component.len() > 1)
        .collect::<Vec<_>>();
    for component in &mut components {
        component.sort_unstable();
    }
    components.sort_by_key(|component| component[0]);

    let mut cyclic = BTreeSet::new();
    let mut diagnostics = Vec::with_capacity(components.len());
    let mut in_component = vec![false; input.interfaces.len()];
    let mut visit_marks = vec![0usize; input.interfaces.len()];
    let mut search_generation = 0usize;
    for component in components {
        search_generation = search_generation.checked_add(1).unwrap_or_else(|| {
            visit_marks.fill(0);
            1
        });
        for &node in &component {
            in_component[node] = true;
            cyclic.insert(input.interfaces[node]);
        }

        let start = component[0];
        let cycle = find_cycle(
            start,
            &in_component,
            &adjacency,
            &mut visit_marks,
            search_generation,
        )
        .expect("every nontrivial strongly connected component has a cycle");
        diagnostics.push(cycle_diagnostic(&input.package, &cycle, &adjacency));

        for &node in &component {
            in_component[node] = false;
        }
    }

    InterfaceUses {
        uses,
        cyclic,
        diagnostics,
    }
}

fn strongly_connected_components(adjacency: &[Vec<Edge>]) -> Vec<Vec<usize>> {
    let mut next_index = 0usize;
    let mut indices = vec![None; adjacency.len()];
    let mut lowlinks = vec![0usize; adjacency.len()];
    let mut active = Vec::new();
    let mut on_active_stack = vec![false; adjacency.len()];
    let mut frames = Vec::<TarjanFrame>::new();
    let mut components = Vec::new();

    for root in 0..adjacency.len() {
        if indices[root].is_some() {
            continue;
        }

        indices[root] = Some(next_index);
        lowlinks[root] = next_index;
        next_index += 1;
        active.push(root);
        on_active_stack[root] = true;
        frames.push(TarjanFrame {
            node: root,
            next_edge: 0,
            parent: None,
        });

        while !frames.is_empty() {
            let (node, next_edge) = {
                let frame = frames.last_mut().expect("current Tarjan frame exists");
                let edge = frame.next_edge;
                frame.next_edge += usize::from(edge < adjacency[frame.node].len());
                (frame.node, edge)
            };
            if next_edge < adjacency[node].len() {
                let target = adjacency[node][next_edge].target;

                if indices[target].is_none() {
                    indices[target] = Some(next_index);
                    lowlinks[target] = next_index;
                    next_index += 1;
                    active.push(target);
                    on_active_stack[target] = true;
                    frames.push(TarjanFrame {
                        node: target,
                        next_edge: 0,
                        parent: Some(node),
                    });
                } else if on_active_stack[target] {
                    lowlinks[node] = lowlinks[node].min(indices[target].expect("visited target"));
                }
                continue;
            }

            let finished = frames.pop().expect("current Tarjan frame exists");
            if lowlinks[finished.node] == indices[finished.node].expect("visited node") {
                let mut component = Vec::new();
                loop {
                    let node = active.pop().expect("Tarjan root is active");
                    on_active_stack[node] = false;
                    component.push(node);
                    if node == finished.node {
                        break;
                    }
                }
                components.push(component);
            }
            if let Some(parent) = finished.parent {
                lowlinks[parent] = lowlinks[parent].min(lowlinks[finished.node]);
            }
        }
    }

    components
}

fn find_cycle(
    start: usize,
    in_component: &[bool],
    adjacency: &[Vec<Edge>],
    visit_marks: &mut [usize],
    search_generation: usize,
) -> Option<Vec<CycleEdge>> {
    let (first_edge_index, first_edge) = adjacency[start]
        .iter()
        .enumerate()
        .find(|(_, edge)| in_component[edge.target] && edge.target != start)?;
    let mut cycle = vec![CycleEdge {
        from: start,
        edge_index: first_edge_index,
    }];
    visit_marks[start] = search_generation;
    visit_marks[first_edge.target] = search_generation;
    let mut frames = vec![CycleSearchFrame {
        node: first_edge.target,
        next_edge: 0,
    }];

    while let Some(frame) = frames.last_mut() {
        if frame.next_edge >= adjacency[frame.node].len() {
            frames.pop();
            if !frames.is_empty() {
                cycle.pop();
            }
            continue;
        }

        let from = frame.node;
        let edge_index = frame.next_edge;
        let target = adjacency[from][edge_index].target;
        frame.next_edge += 1;
        if !in_component[target] {
            continue;
        }
        if target == start {
            cycle.push(CycleEdge { from, edge_index });
            return Some(cycle);
        }
        if visit_marks[target] == search_generation {
            continue;
        }

        visit_marks[target] = search_generation;
        cycle.push(CycleEdge { from, edge_index });
        frames.push(CycleSearchFrame {
            node: target,
            next_edge: 0,
        });
    }

    None
}

fn cycle_diagnostic(package: &str, cycle: &[CycleEdge], adjacency: &[Vec<Edge>]) -> Diagnostic {
    let first = cycle.first().expect("a cycle has a first edge");
    let first_location = &adjacency[first.from][first.edge_index].location;
    let mut diagnostic = Diagnostic::new(
        Code::InterfaceUseCycle,
        Severity::Error,
        "local interfaces use each other's types cyclically",
        Some(first_location.clone()),
    );
    diagnostic.primary_label = "first reference in this interface-use cycle".to_owned();
    diagnostic.package = Some(package.to_owned());
    for cycle_edge in cycle.iter().skip(1) {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "this reference continues the interface-use cycle".to_owned(),
                location: Some(
                    adjacency[cycle_edge.from][cycle_edge.edge_index]
                        .location
                        .clone(),
                ),
            })
            .expect("unbound diagnostics accept cycle notes");
    }
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData};
    use sai_intern::{Interner, PackageInterner};
    use sai_vfs::{FileId, TextRange, TextSize};

    struct Fixture {
        _packages: PackageInterner,
        symbols: Interner,
        paths: DefPathInterner,
        package: sai_hir::PackageId,
    }

    impl Fixture {
        fn new() -> Self {
            let packages = PackageInterner::new();
            let package = packages
                .intern("acme:app#1.0")
                .expect("package key interns");
            let symbols = Interner::new();
            let paths = DefPathInterner::new(package, &packages, &symbols).expect("paths build");
            Self {
                _packages: packages,
                symbols,
                paths,
                package,
            }
        }

        fn interface(&self, name: &str) -> LocalDefId {
            let name = self.symbols.intern(name).expect("interface name interns");
            self.paths
                .intern(
                    DefPath::new(
                        self.package,
                        vec![DisambiguatedDefPathData {
                            data: DefPathData::Interface(name),
                            disambiguator: u32::try_from(self.paths.len().expect("length reads"))
                                .expect("test path count fits"),
                        }],
                        &self._packages,
                        &self.symbols,
                    )
                    .expect("interface path belongs to the fixture"),
                )
                .expect("interface path interns")
        }

        fn input(
            &self,
            interfaces: &[LocalDefId],
            references: &[(&str, LocalDefId, LocalDefId, u32)],
        ) -> InterfaceUsesInput {
            InterfaceUsesInput {
                package: "acme:app@1.0.0".to_owned(),
                interfaces: interfaces.to_vec(),
                references: references
                    .iter()
                    .map(|(file, from, to, offset)| InterfaceUseReference {
                        from: *from,
                        to: *to,
                        location: Location::new(
                            *file,
                            Span::new(
                                FileId::new(*offset + 1),
                                TextRange::new(TextSize::new(*offset), TextSize::new(*offset + 1))
                                    .expect("test span is ordered"),
                            ),
                        ),
                    })
                    .collect(),
            }
        }
    }

    fn location(path: &str, offset: u32) -> Location {
        Location::new(
            path,
            Span::new(
                FileId::new(offset + 1),
                TextRange::new(TextSize::new(offset), TextSize::new(offset + 1))
                    .expect("test span is ordered"),
            ),
        )
    }

    #[test]
    fn self_use_is_excluded_from_edges_and_cycles() {
        let fixture = Fixture::new();
        let a = fixture.interface("a");
        let result = interface_uses(fixture.input(&[a], &[("a.sai", a, a, 10)]));

        assert_eq!(result.uses.get(&a), Some(&Vec::new()));
        assert!(result.cyclic.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn repeated_references_keep_the_first_location_and_edge_order() {
        let fixture = Fixture::new();
        let a = fixture.interface("a");
        let b = fixture.interface("b");
        let c = fixture.interface("c");
        let result = interface_uses(fixture.input(
            &[a, b, c],
            &[
                ("a.sai", a, b, 10),
                ("a.sai", a, c, 20),
                ("a.sai", a, b, 30),
            ],
        ));

        assert_eq!(
            result.uses.get(&a),
            Some(&vec![
                (b, location("a.sai", 10).span),
                (c, location("a.sai", 20).span)
            ])
        );
        assert!(result.cyclic.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn non_cycle_dag_has_no_cycle_members_or_diagnostics() {
        let fixture = Fixture::new();
        let a = fixture.interface("a");
        let b = fixture.interface("b");
        let c = fixture.interface("c");
        let d = fixture.interface("d");
        let result = interface_uses(fixture.input(
            &[a, b, c, d],
            &[
                ("a.sai", a, b, 10),
                ("a.sai", a, c, 20),
                ("b.sai", b, d, 30),
                ("c.sai", c, d, 40),
            ],
        ));

        assert!(result.cyclic.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn cycle_uses_first_generation_interface_and_one_deterministic_cycle() {
        let fixture = Fixture::new();
        let b = fixture.interface("b");
        let a = fixture.interface("a");
        let c = fixture.interface("c");
        let d = fixture.interface("d");
        let result = interface_uses(fixture.input(
            &[a, b, c, d],
            &[
                ("a.sai", a, b, 10),
                ("a.sai", a, c, 20),
                ("b.sai", b, c, 30),
                ("b.sai", b, a, 40),
                ("c.sai", c, a, 50),
                ("c.sai", c, b, 60),
            ],
        ));

        assert_eq!(result.cyclic, BTreeSet::from([a, b, c]));
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::InterfaceUseCycle);
        assert_eq!(diagnostic.package.as_deref(), Some("acme:app@1.0.0"));
        assert_eq!(diagnostic.primary(), Some(&location("a.sai", 10)));
        assert_eq!(
            diagnostic
                .children()
                .iter()
                .map(|child| child.location.clone().expect("cycle note has a location"))
                .collect::<Vec<_>>(),
            vec![location("b.sai", 30), location("c.sai", 50)]
        );
    }

    #[test]
    fn multiple_cycles_are_reported_in_generation_order() {
        let fixture = Fixture::new();
        let d = fixture.interface("d");
        let e = fixture.interface("e");
        let a = fixture.interface("a");
        let b = fixture.interface("b");
        let c = fixture.interface("c");
        let result = interface_uses(fixture.input(
            &[a, b, c, d, e],
            &[
                ("d.sai", d, e, 10),
                ("e.sai", e, d, 20),
                ("b.sai", b, c, 30),
                ("c.sai", c, b, 40),
            ],
        ));

        assert_eq!(result.diagnostics.len(), 2);
        assert_eq!(
            result.diagnostics[0].primary(),
            Some(&location("b.sai", 30))
        );
        assert_eq!(
            result.diagnostics[1].primary(),
            Some(&location("d.sai", 10))
        );
        assert_eq!(result.cyclic, BTreeSet::from([b, c, d, e]));
    }

    #[test]
    fn many_disjoint_cycles_each_produce_one_diagnostic() {
        const PAIR_COUNT: usize = 1_024;
        let fixture = Fixture::new();
        let interfaces = (0..PAIR_COUNT * 2)
            .map(|index| fixture.interface(&format!("iface_{index}")))
            .collect::<Vec<_>>();
        let references = (0..PAIR_COUNT)
            .flat_map(|pair| {
                let from = interfaces[pair * 2];
                let to = interfaces[pair * 2 + 1];
                let offset = u32::try_from(pair * 2).expect("test offset fits u32");
                [
                    ("many-cycles.sai", from, to, offset),
                    ("many-cycles.sai", to, from, offset + 1),
                ]
            })
            .collect::<Vec<_>>();

        let result = interface_uses(fixture.input(&interfaces, &references));

        assert_eq!(result.diagnostics.len(), PAIR_COUNT);
        assert_eq!(result.cyclic.len(), PAIR_COUNT * 2);
    }
}
