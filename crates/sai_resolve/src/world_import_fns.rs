//! Data-only lookup construction for import functions of one local world.
//!
//! The caller supplies each local world's items in source order, replacing
//! includes with their already-resolved target `DefId`s. This module does not
//! inspect HIR or resolve paths. It expands the requested world's includes
//! depth-first, skips worlds already visited for this lookup, and keeps the
//! first import function for each SAI name.
//!
//! Names are owned, canonical SAI spellings instead of [`sai_hir::Symbol`]:
//! symbols are session-local and do not implement `Ord`, while this query
//! returns a deterministically ordered `BTreeMap`. The adapter supplies the
//! canonical spelling associated with each interned name.

use sai_hir::DefId;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One import function declared directly in a local world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldImportFunction {
    /// The function's canonical SAI identifier spelling.
    pub name: String,
    /// The HIR definition ID of this `import fn`.
    pub def_id: DefId,
}

/// One source-ordered item relevant to `world_import_fns`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorldImportFnItem {
    /// A directly declared `import fn`.
    ImportFunction(WorldImportFunction),
    /// An include whose target has already been resolved by the caller.
    Include(DefId),
}

/// Source-ordered import-function and include items for one local world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldImportFnWorld {
    pub world: DefId,
    /// Relevant world items in source order. Non-function, non-include items
    /// are omitted by the caller.
    pub items: Vec<WorldImportFnItem>,
}

/// Package world data needed by [`world_import_fns`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldImportFnsInput {
    /// Local worlds in package generation order. Each `world` ID should occur
    /// once; if an ID is repeated, its first record is authoritative.
    pub worlds: Vec<WorldImportFnWorld>,
    /// Import-function maps for externally supplied view worlds. These maps
    /// are already keyed by canonical SAI name; if a world ID is repeated, its
    /// first record is authoritative.
    pub external_worlds: Vec<(DefId, BTreeMap<String, DefId>)>,
}

/// The requested root is absent from the supplied local-world data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MissingLocalWorld {
    pub world: DefId,
}

/// Builds the ordered import-function lookup for one local world.
///
/// Items are visited in source order. An include is expanded immediately and
/// depth-first before the containing world's next item is visited. A visited
/// set prevents cycles and repeated expansion of the same local or external
/// world. External worlds contribute the supplied map at the point where the
/// include is encountered.
///
/// The lookup is computed only for `root`. This keeps work proportional to the
/// supplied package index plus the part of the include graph reachable from
/// that root, rather than materializing maps for every local world.
pub fn world_import_fns(
    root: DefId,
    input: WorldImportFnsInput,
) -> Result<BTreeMap<String, DefId>, MissingLocalWorld> {
    let mut local_indices = HashMap::with_capacity(input.worlds.len());
    for (index, world) in input.worlds.iter().enumerate() {
        // `DefId` identifies one definition. If malformed input repeats it,
        // keep the first generation-order record deterministically.
        local_indices.entry(world.world).or_insert(index);
    }
    if !local_indices.contains_key(&root) {
        return Err(MissingLocalWorld { world: root });
    }

    let mut external_indices = HashMap::with_capacity(input.external_worlds.len());
    for (index, (world, _)) in input.external_worlds.iter().enumerate() {
        // As with local IDs, malformed duplicate external IDs use the first
        // supplied map deterministically.
        external_indices.entry(*world).or_insert(index);
    }

    let mut imports = BTreeMap::new();
    let mut visited = HashSet::new();
    visited.insert(root);
    let mut stack = vec![(root, 0usize)];

    while let Some(&(current, item_index)) = stack.last() {
        if let Some(&local_index) = local_indices.get(&current) {
            let items = &input.worlds[local_index].items;
            let Some(item) = items.get(item_index) else {
                stack.pop();
                continue;
            };
            stack.last_mut().expect("current world remains on stack").1 += 1;

            match item {
                WorldImportFnItem::ImportFunction(function) => {
                    imports
                        .entry(function.name.clone())
                        .or_insert(function.def_id);
                }
                WorldImportFnItem::Include(target) => {
                    if visited.insert(*target) {
                        stack.push((*target, 0));
                    }
                }
            }
            continue;
        }

        if let Some(&external_index) = external_indices.get(&current) {
            for (name, def_id) in &input.external_worlds[external_index].1 {
                imports.entry(name.clone()).or_insert(*def_id);
            }
        }
        stack.pop();
    }

    Ok(imports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::{DefIndex, PackageId};
    use sai_intern::{Idx, PackageInterner};

    struct Fixture {
        package: PackageId,
    }

    impl Fixture {
        fn new() -> Self {
            let packages = PackageInterner::new();
            let package = packages
                .intern("acme:app#1.0")
                .expect("package key interns");
            Self { package }
        }

        fn def(&self, index: u32) -> DefId {
            DefId::from_foreign(self.package, DefIndex::from_raw(index))
        }

        fn import(&self, name: &str, index: u32) -> WorldImportFnItem {
            WorldImportFnItem::ImportFunction(WorldImportFunction {
                name: name.to_owned(),
                def_id: self.def(index),
            })
        }

        fn world(&self, world: u32, items: Vec<WorldImportFnItem>) -> WorldImportFnWorld {
            WorldImportFnWorld {
                world: self.def(world),
                items,
            }
        }

        fn input(&self, worlds: Vec<WorldImportFnWorld>) -> WorldImportFnsInput {
            WorldImportFnsInput {
                worlds,
                external_worlds: Vec::new(),
            }
        }
    }

    #[test]
    fn direct_imports_build_a_name_to_definition_lookup() {
        let fixture = Fixture::new();
        let imports = world_import_fns(
            fixture.def(1),
            fixture.input(vec![fixture.world(
                1,
                vec![fixture.import("read", 101), fixture.import("write", 102)],
            )]),
        )
        .expect("requested world exists");

        assert_eq!(
            imports,
            BTreeMap::from([
                ("read".to_owned(), fixture.def(101)),
                ("write".to_owned(), fixture.def(102)),
            ])
        );
    }

    #[test]
    fn includes_expand_depth_first_at_their_source_position() {
        let fixture = Fixture::new();
        let imports = world_import_fns(
            fixture.def(1),
            fixture.input(vec![
                fixture.world(
                    1,
                    vec![
                        WorldImportFnItem::Include(fixture.def(2)),
                        WorldImportFnItem::Include(fixture.def(3)),
                    ],
                ),
                fixture.world(
                    2,
                    vec![
                        WorldImportFnItem::Include(fixture.def(4)),
                        fixture.import("collision", 202),
                    ],
                ),
                fixture.world(3, vec![fixture.import("collision", 303)]),
                fixture.world(4, vec![fixture.import("collision", 404)]),
            ]),
        )
        .expect("requested world exists");

        // DFS expands B's include of D before returning to B and continuing
        // with C. A breadth-first walk would keep B's function instead.
        assert_eq!(imports.get("collision"), Some(&fixture.def(404)));
    }

    #[test]
    fn first_name_wins_across_direct_and_depth_first_included_items() {
        let fixture = Fixture::new();
        let imports = world_import_fns(
            fixture.def(1),
            fixture.input(vec![
                fixture.world(
                    1,
                    vec![
                        fixture.import("shared", 101),
                        WorldImportFnItem::Include(fixture.def(2)),
                        fixture.import("late", 102),
                    ],
                ),
                fixture.world(
                    2,
                    vec![
                        fixture.import("late", 201),
                        WorldImportFnItem::Include(fixture.def(3)),
                    ],
                ),
                fixture.world(3, vec![fixture.import("shared", 301)]),
            ]),
        )
        .expect("requested world exists");

        assert_eq!(imports.get("shared"), Some(&fixture.def(101)));
        assert_eq!(imports.get("late"), Some(&fixture.def(201)));
    }

    #[test]
    fn external_worlds_contribute_their_supplied_imports() {
        let fixture = Fixture::new();
        let external = BTreeMap::from([
            ("external".to_owned(), fixture.def(901)),
            ("overlap".to_owned(), fixture.def(902)),
        ]);
        let imports = world_import_fns(
            fixture.def(1),
            WorldImportFnsInput {
                worlds: vec![fixture.world(
                    1,
                    vec![
                        WorldImportFnItem::Include(fixture.def(90)),
                        fixture.import("overlap", 101),
                    ],
                )],
                external_worlds: vec![(fixture.def(90), external)],
            },
        )
        .expect("requested world exists");

        assert_eq!(imports.get("external"), Some(&fixture.def(901)));
        assert_eq!(imports.get("overlap"), Some(&fixture.def(902)));
    }

    #[test]
    fn cycles_terminate_and_each_world_is_expanded_once_for_the_root() {
        let fixture = Fixture::new();
        let imports = world_import_fns(
            fixture.def(1),
            fixture.input(vec![
                fixture.world(
                    1,
                    vec![
                        fixture.import("root", 101),
                        WorldImportFnItem::Include(fixture.def(2)),
                    ],
                ),
                fixture.world(
                    2,
                    vec![
                        fixture.import("child", 201),
                        WorldImportFnItem::Include(fixture.def(1)),
                        WorldImportFnItem::Include(fixture.def(3)),
                    ],
                ),
                fixture.world(3, vec![fixture.import("leaf", 301)]),
            ]),
        )
        .expect("requested world exists");

        assert_eq!(imports.len(), 3);
        assert_eq!(imports.get("root"), Some(&fixture.def(101)));
        assert_eq!(imports.get("child"), Some(&fixture.def(201)));
        assert_eq!(imports.get("leaf"), Some(&fixture.def(301)));
    }

    #[test]
    fn only_the_requested_world_is_required_and_duplicate_ids_keep_first_record() {
        let fixture = Fixture::new();
        let imports = world_import_fns(
            fixture.def(1),
            fixture.input(vec![
                fixture.world(1, vec![fixture.import("first", 101)]),
                fixture.world(1, vec![fixture.import("second", 102)]),
            ]),
        )
        .expect("requested world exists");

        assert_eq!(
            imports,
            BTreeMap::from([("first".to_owned(), fixture.def(101))])
        );
        assert!(matches!(
            world_import_fns(fixture.def(9), fixture.input(Vec::new())),
            Err(MissingLocalWorld { world }) if world == fixture.def(9)
        ));
    }

    #[test]
    fn output_is_deterministic_and_sai_names_are_sorted() {
        let fixture = Fixture::new();
        let input = fixture.input(vec![
            fixture.world(1, vec![fixture.import("z", 102), fixture.import("a", 101)]),
        ]);

        let first = world_import_fns(fixture.def(1), input.clone()).expect("root exists");
        let second = world_import_fns(fixture.def(1), input).expect("root exists");

        assert_eq!(first, second);
        assert_eq!(
            first.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["a", "z"]
        );
    }
}
