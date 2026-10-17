//! Local world-include graph analysis.
//!
//! The caller supplies local worlds in package generation order and resolved
//! include items in world-generation and source order. This module only
//! filters that resolved data, computes strongly connected components, and
//! creates cycle diagnostics; it does not inspect HIR or resolve paths.

use sai_diagnostics::{Child, ChildKind, Code, Diagnostic, Location, Severity};
use sai_hir::{DefId, OwnerId};
use std::collections::HashMap;

/// A resolved world include reference supplied by the package adapter.
///
/// `from` and `to` are definition IDs because `resolve_owner` records general
/// `DefId`s. The collector keeps an edge only when both IDs occur in the
/// package's ordered list of local worlds. `owner` identifies the include HIR
/// owner; its ordering is supplied explicitly by [`WorldIncludesInput::includes`]
/// and is never inferred from session-local IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldIncludeReference {
    pub from: DefId,
    pub to: DefId,
    pub owner: OwnerId,
    pub location: Location,
}

/// Input to [`world_includes`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldIncludesInput {
    /// Canonical package identity used to attribute diagnostics.
    pub package: String,
    /// Local worlds in compiler generation order.
    ///
    /// This vector, rather than the ordering of `DefId`s or `OwnerId`s,
    /// determines SCC and diagnostic order.
    pub worlds: Vec<DefId>,
    /// Resolved include items in package generation order, then source order
    /// within each world. References to non-local worlds are ignored.
    pub includes: Vec<WorldIncludeReference>,
}

/// The local world-include graph and its cycle diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldIncludes {
    /// Every include edge between local worlds, in the input generation and
    /// source order. Repeated include items remain distinct.
    pub includes: Vec<WorldIncludeReference>,
    /// Worlds that belong to cyclic components, in package generation order.
    pub cyclic: Vec<DefId>,
    /// One diagnostic per cyclic component, ordered by its first world in
    /// package generation order.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    target: usize,
    include_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct TarjanFrame {
    node: usize,
    next_edge: usize,
    parent: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct CycleSearchFrame {
    node: usize,
    next_edge: usize,
}

/// Builds the local world-include graph and reports cyclic SCCs.
///
/// Each nontrivial strongly connected component, and each singleton with a
/// self-include, produces one `WORLD_INCLUDE_CYCLE`. A component's diagnostic
/// starts at the first in-component include edge of its first world in
/// generation order. Further edges are selected by depth-first search in
/// source order, making both the primary location and notes deterministic.
pub fn world_includes(input: WorldIncludesInput) -> WorldIncludes {
    let mut world_index = HashMap::with_capacity(input.worlds.len());
    for (index, world) in input.worlds.iter().copied().enumerate() {
        world_index.entry(world).or_insert(index);
    }

    let mut includes = Vec::new();
    let mut adjacency = vec![Vec::<Edge>::new(); input.worlds.len()];
    for include in input.includes {
        let (Some(&from), Some(&to)) =
            (world_index.get(&include.from), world_index.get(&include.to))
        else {
            continue;
        };
        let include_index = includes.len();
        includes.push(include);
        adjacency[from].push(Edge {
            target: to,
            include_index,
        });
    }

    let mut components = strongly_connected_components(&adjacency)
        .into_iter()
        .filter(|component| {
            component.len() > 1
                || adjacency[component[0]]
                    .iter()
                    .any(|edge| edge.target == component[0])
        })
        .collect::<Vec<_>>();
    for component in &mut components {
        component.sort_unstable();
    }
    components.sort_by_key(|component| component[0]);

    let mut cyclic_by_index = vec![false; input.worlds.len()];
    let mut diagnostics = Vec::with_capacity(components.len());
    let mut in_component = vec![false; input.worlds.len()];
    let mut visit_marks = vec![0usize; input.worlds.len()];
    let mut search_generation = 0usize;
    for component in components {
        search_generation = search_generation.checked_add(1).unwrap_or_else(|| {
            visit_marks.fill(0);
            1
        });
        for &node in &component {
            cyclic_by_index[node] = true;
            in_component[node] = true;
        }

        let start = component[0];
        let cycle = find_cycle(
            start,
            &in_component,
            &adjacency,
            &mut visit_marks,
            search_generation,
        )
        .expect("every cyclic strongly connected component has a cycle");
        diagnostics.push(cycle_diagnostic(
            &input.package,
            &cycle,
            &includes,
            &adjacency,
        ));

        for &node in &component {
            in_component[node] = false;
        }
    }

    let cyclic = input
        .worlds
        .iter()
        .copied()
        .zip(cyclic_by_index)
        .filter_map(|(world, is_cyclic)| is_cyclic.then_some(world))
        .collect();

    WorldIncludes {
        includes,
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

#[derive(Clone, Copy, Debug)]
struct CycleEdge {
    from: usize,
    edge_index: usize,
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
        .find(|(_, edge)| in_component[edge.target])?;
    let mut cycle = vec![CycleEdge {
        from: start,
        edge_index: first_edge_index,
    }];
    if first_edge.target == start {
        return Some(cycle);
    }

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

fn cycle_diagnostic(
    package: &str,
    cycle: &[CycleEdge],
    includes: &[WorldIncludeReference],
    adjacency: &[Vec<Edge>],
) -> Diagnostic {
    let first = cycle.first().expect("a cycle has a first edge");
    let first_include = &includes[adjacency[first.from][first.edge_index].include_index];
    let mut diagnostic = Diagnostic::new(
        Code::WorldIncludeCycle,
        Severity::Error,
        "local worlds include each other cyclically",
        Some(first_include.location.clone()),
    );
    diagnostic.primary_label = "first include in this world-include cycle".to_owned();
    diagnostic.package = Some(package.to_owned());
    for cycle_edge in cycle.iter().skip(1) {
        let include = &includes[adjacency[cycle_edge.from][cycle_edge.edge_index].include_index];
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "this include continues the world-include cycle".to_owned(),
                location: Some(include.location.clone()),
            })
            .expect("unbound diagnostics accept cycle notes");
    }
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::{DefIndex, DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData};
    use sai_intern::{Idx, Interner, PackageInterner};
    use sai_vfs::{FileId, Span, TextRange, TextSize};

    struct Fixture {
        packages: PackageInterner,
        symbols: sai_intern::Interner,
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
                packages,
                symbols,
                paths,
                package,
            }
        }

        fn world(&self, name: &str) -> DefId {
            let name = self.symbols.intern(name).expect("world name interns");
            self.paths
                .intern(
                    DefPath::new(
                        self.package,
                        vec![DisambiguatedDefPathData {
                            data: DefPathData::World(name),
                            disambiguator: u32::try_from(self.paths.len().expect("length reads"))
                                .expect("test path count fits"),
                        }],
                        &self.packages,
                        &self.symbols,
                    )
                    .expect("world path belongs to the fixture"),
                )
                .expect("world path interns")
                .to_def_id()
        }

        fn include_owner(&self, name: &str) -> OwnerId {
            let name = self.symbols.intern(name).expect("include name interns");
            OwnerId {
                def_id: self
                    .paths
                    .intern(
                        DefPath::new(
                            self.package,
                            vec![DisambiguatedDefPathData {
                                data: DefPathData::WorldInclude(name),
                                disambiguator: u32::try_from(
                                    self.paths.len().expect("length reads"),
                                )
                                .expect("test path count fits"),
                            }],
                            &self.packages,
                            &self.symbols,
                        )
                        .expect("include path belongs to the fixture"),
                    )
                    .expect("include path interns"),
            }
        }

        fn reference(
            &self,
            from: DefId,
            to: DefId,
            label: &str,
            offset: u32,
        ) -> WorldIncludeReference {
            WorldIncludeReference {
                from,
                to,
                owner: self.include_owner(label),
                location: location("worlds.sai", offset),
            }
        }

        fn input(
            &self,
            worlds: &[DefId],
            includes: Vec<WorldIncludeReference>,
        ) -> WorldIncludesInput {
            WorldIncludesInput {
                package: "acme:app@1.0.0".to_owned(),
                worlds: worlds.to_vec(),
                includes,
            }
        }

        fn long_world(&self, index: u32) -> DefId {
            DefId::from_foreign(self.package, DefIndex::from_raw(index))
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
    fn self_include_is_one_cycle_with_the_self_edge_as_primary() {
        let fixture = Fixture::new();
        let a = fixture.world("a");
        let result =
            world_includes(fixture.input(&[a], vec![fixture.reference(a, a, "a-self", 10)]));

        assert_eq!(result.includes.len(), 1);
        assert_eq!(result.cyclic, vec![a]);
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::WorldIncludeCycle);
        assert_eq!(diagnostic.package.as_deref(), Some("acme:app@1.0.0"));
        assert_eq!(diagnostic.primary(), Some(&location("worlds.sai", 10)));
        assert!(diagnostic.children().is_empty());
    }

    #[test]
    fn acyclic_graph_has_no_cyclic_worlds_or_diagnostics() {
        let fixture = Fixture::new();
        let a = fixture.world("a");
        let b = fixture.world("b");
        let c = fixture.world("c");
        let d = fixture.world("d");
        let result = world_includes(fixture.input(
            &[a, b, c, d],
            vec![
                fixture.reference(a, b, "a-b", 10),
                fixture.reference(a, c, "a-c", 20),
                fixture.reference(b, d, "b-d", 30),
                fixture.reference(c, d, "c-d", 40),
            ],
        ));

        assert!(result.cyclic.is_empty());
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.includes.len(), 4);
    }

    #[test]
    fn multiple_sccs_are_diagnosed_in_generation_order_and_cycle_edges_are_noted() {
        let fixture = Fixture::new();
        // Deliberately allocate `zeta` first while placing `beta` first in the
        // package generation order, which alone governs diagnostic order.
        let zeta = fixture.world("zeta");
        let beta = fixture.world("beta");
        let alpha = fixture.world("alpha");
        let gamma = fixture.world("gamma");
        let delta = fixture.world("delta");
        let result = world_includes(fixture.input(
            &[beta, alpha, zeta, gamma, delta],
            vec![
                fixture.reference(beta, alpha, "beta-alpha", 10),
                fixture.reference(alpha, beta, "alpha-beta", 20),
                fixture.reference(zeta, gamma, "zeta-gamma", 30),
                fixture.reference(gamma, delta, "gamma-delta", 40),
                fixture.reference(delta, zeta, "delta-zeta", 50),
            ],
        ));

        assert_eq!(result.cyclic, vec![beta, alpha, zeta, gamma, delta]);
        assert_eq!(result.diagnostics.len(), 2);
        assert_eq!(
            result.diagnostics[0].primary(),
            Some(&location("worlds.sai", 10))
        );
        assert_eq!(
            result.diagnostics[1].primary(),
            Some(&location("worlds.sai", 30))
        );
        assert_eq!(
            result.diagnostics[1]
                .children()
                .iter()
                .map(|child| child.location.clone().expect("cycle note has a location"))
                .collect::<Vec<_>>(),
            vec![location("worlds.sai", 40), location("worlds.sai", 50)]
        );
    }

    #[test]
    fn first_in_component_edge_and_source_order_choose_a_deterministic_cycle() {
        let fixture = Fixture::new();
        let a = fixture.world("a");
        let b = fixture.world("b");
        let c = fixture.world("c");
        let d = fixture.world("d");
        let outside = fixture.world("outside");
        let result = world_includes(fixture.input(
            &[a, b, c, d, outside],
            vec![
                // This edge is not part of A's SCC and must not become primary.
                fixture.reference(a, outside, "a-outside", 1),
                fixture.reference(a, b, "a-b", 2),
                fixture.reference(a, c, "a-c", 3),
                fixture.reference(b, d, "b-d", 4),
                fixture.reference(d, a, "d-a", 5),
                fixture.reference(c, a, "c-a", 6),
            ],
        ));

        assert_eq!(result.cyclic, vec![a, b, c, d]);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].primary(),
            Some(&location("worlds.sai", 2))
        );
        assert_eq!(
            result.diagnostics[0]
                .children()
                .iter()
                .map(|child| child.location.clone().expect("cycle note has a location"))
                .collect::<Vec<_>>(),
            vec![location("worlds.sai", 4), location("worlds.sai", 5)],
            "depth-first source order follows A → B → D → A instead of the shorter A → C → A"
        );
    }

    #[test]
    fn repeated_edges_are_preserved_and_diagnostic_selection_is_repeatable() {
        let fixture = Fixture::new();
        let a = fixture.world("a");
        let b = fixture.world("b");
        let first = fixture.reference(a, b, "a-b-first", 10);
        let second = fixture.reference(a, b, "a-b-second", 11);
        let back = fixture.reference(b, a, "b-a", 12);
        let input = fixture.input(&[a, b], vec![first.clone(), second.clone(), back.clone()]);

        let result = world_includes(input.clone());
        let repeated = world_includes(input);

        assert_eq!(result.includes, vec![first, second, back]);
        assert_eq!(result, repeated);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].primary(),
            Some(&location("worlds.sai", 10))
        );
        assert_eq!(
            result.diagnostics[0]
                .children()
                .iter()
                .map(|child| child.location.clone().expect("cycle note has a location"))
                .collect::<Vec<_>>(),
            vec![location("worlds.sai", 12)]
        );
    }

    #[test]
    fn non_local_world_edges_are_excluded_from_the_local_graph() {
        let fixture = Fixture::new();
        let a = fixture.world("a");
        let foreign_package = fixture
            .packages
            .intern("acme:dependency#1.0")
            .expect("foreign package key interns");
        let external = DefId::from_foreign(foreign_package, DefIndex::from_raw(1));
        let result = world_includes(
            fixture.input(&[a], vec![fixture.reference(a, external, "external", 10)]),
        );

        assert!(result.includes.is_empty());
        assert!(result.cyclic.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn long_acyclic_chain_is_processed_without_recursive_graph_walks() {
        const WORLD_COUNT: u32 = 12_000;
        let fixture = Fixture::new();
        let worlds = (0..WORLD_COUNT)
            .map(|index| fixture.long_world(index))
            .collect::<Vec<_>>();
        let includes = (0..WORLD_COUNT - 1)
            .map(|index| WorldIncludeReference {
                from: worlds[index as usize],
                to: worlds[index as usize + 1],
                owner: fixture.include_owner(&format!("include_{index}")),
                location: location("long-chain.sai", index),
            })
            .collect::<Vec<_>>();
        let result = world_includes(fixture.input(&worlds, includes));

        assert!(result.cyclic.is_empty());
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.includes.len(), WORLD_COUNT as usize - 1);
    }

    #[test]
    fn long_cycle_is_processed_without_recursive_graph_walks() {
        const WORLD_COUNT: u32 = 12_000;
        let fixture = Fixture::new();
        let worlds = (0..WORLD_COUNT)
            .map(|index| fixture.long_world(index))
            .collect::<Vec<_>>();
        let includes = (0..WORLD_COUNT)
            .map(|index| WorldIncludeReference {
                from: worlds[index as usize],
                to: worlds[((index + 1) % WORLD_COUNT) as usize],
                owner: fixture.include_owner(&format!("include_{index}")),
                location: location("long-cycle.sai", index),
            })
            .collect::<Vec<_>>();
        let result = world_includes(fixture.input(&worlds, includes));

        assert_eq!(result.cyclic.len(), WORLD_COUNT as usize);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].children().len(),
            WORLD_COUNT as usize - 1
        );
    }
}
