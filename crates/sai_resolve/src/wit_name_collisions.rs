//! WIT-name collision detection within one already-selected name scope.

use crate::sai_identifier_to_wit_name;
use sai_hir::{OwnerId, Span};
use std::collections::{HashMap, HashSet};

/// One semantic source declaration whose name crosses the WIT boundary.
///
/// `source_name` is the semantic SAI identifier spelling. Callers provide
/// declarations in the scope's generation order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitNameDefinition {
    pub source_name: String,
    pub owner: OwnerId,
    pub span: Span,
}

/// A later declaration whose WIT name duplicates an earlier declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitNameCollision {
    pub wit_name: String,
    pub earlier_span: Span,
    pub later_owner: OwnerId,
    pub later_span: Span,
}

#[derive(Debug)]
struct FirstWitName {
    span: Span,
    source_names: HashSet<String>,
}

/// Finds collisions among representable names in one WIT namespace.
///
/// Input order is preserved. Unrepresentable names are skipped because their
/// separate `WIT_NAME_NOT_REPRESENTABLE` diagnostic owns them. Repeated
/// semantic SAI spellings are also skipped because the SAI duplicate-name
/// diagnostic owns those declarations. Each later distinct source spelling
/// that maps to an existing WIT name is reported against the first declaration.
pub fn wit_name_collisions(
    definitions: impl IntoIterator<Item = WitNameDefinition>,
) -> Vec<WitNameCollision> {
    let mut first_by_wit_name = HashMap::<String, FirstWitName>::new();
    let mut collisions = Vec::new();

    for definition in definitions {
        let Some(wit_name) = sai_identifier_to_wit_name(&definition.source_name) else {
            continue;
        };

        if let Some(first) = first_by_wit_name.get_mut(&wit_name) {
            if first.source_names.insert(definition.source_name) {
                collisions.push(WitNameCollision {
                    wit_name,
                    earlier_span: first.span,
                    later_owner: definition.owner,
                    later_span: definition.span,
                });
            }
        } else {
            let mut source_names = HashSet::new();
            source_names.insert(definition.source_name);
            first_by_wit_name.insert(
                wit_name,
                FirstWitName {
                    span: definition.span,
                    source_names,
                },
            );
        }
    }

    collisions
}

#[cfg(test)]
mod tests {
    use super::{WitNameDefinition, wit_name_collisions};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, OwnerId, Span, SpanCtxt,
    };
    use sai_intern::{Interner, PackageInterner};
    use sai_vfs::{FileId, TextRange, TextSize};

    fn owners(count: usize) -> Vec<OwnerId> {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#1.0")
            .expect("test package interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols)
            .expect("test definition paths initialize");

        (0..count)
            .map(|index| {
                let spelling = format!("owner_{index}");
                let symbol = symbols.intern(&spelling).expect("owner symbol interns");
                let path = DefPath::new(
                    package,
                    vec![DisambiguatedDefPathData {
                        data: DefPathData::ValueNs(symbol),
                        disambiguator: 0,
                    }],
                    &packages,
                    &symbols,
                )
                .expect("owner path is valid");
                OwnerId {
                    def_id: paths.intern(path).expect("owner path interns"),
                }
            })
            .collect()
    }

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end))
                .expect("test span range is ordered"),
            ctxt: SpanCtxt::Root,
        }
    }

    fn definition(source_name: &str, owner: OwnerId, start: u32, end: u32) -> WitNameDefinition {
        WitNameDefinition {
            source_name: source_name.to_owned(),
            owner,
            span: span(start, end),
        }
    }

    #[test]
    fn detects_snake_case_and_camel_case_normalization_collisions() {
        let [earlier_owner, later_owner] = owners(2).try_into().expect("two test owners");
        let earlier_span = span(4, 14);
        let later_span = span(24, 33);

        let collisions = wit_name_collisions([
            definition("fetch_user", earlier_owner, 4, 14),
            definition("FetchUser", later_owner, 24, 33),
        ]);

        assert_eq!(
            collisions,
            [super::WitNameCollision {
                wit_name: "fetch-user".to_owned(),
                earlier_span,
                later_owner,
                later_span,
            }]
        );
    }

    #[test]
    fn preserves_input_order_and_anchors_each_name_to_its_first_declaration() {
        let [first_owner, second_owner, third_owner, fourth_owner] =
            owners(4).try_into().expect("four test owners");
        let first_span = span(0, 3);
        let second_span = span(10, 13);
        let third_span = span(20, 22);
        let fourth_span = span(30, 32);

        let collisions = wit_name_collisions([
            definition("a_b", first_owner, 0, 3),
            definition("x_y", second_owner, 10, 13),
            definition("AB", third_owner, 20, 22),
            definition("XY", fourth_owner, 30, 32),
        ]);

        assert_eq!(
            collisions,
            [
                super::WitNameCollision {
                    wit_name: "a-b".to_owned(),
                    earlier_span: first_span,
                    later_owner: third_owner,
                    later_span: third_span,
                },
                super::WitNameCollision {
                    wit_name: "x-y".to_owned(),
                    earlier_span: second_span,
                    later_owner: fourth_owner,
                    later_span: fourth_span,
                },
            ]
        );
    }

    #[test]
    fn ignores_repeated_identical_semantic_source_spellings() {
        let [
            first_owner,
            duplicate_owner,
            collision_owner,
            repeated_collision_owner,
        ] = owners(4).try_into().expect("four test owners");
        let first_span = span(0, 10);
        let collision_span = span(22, 31);

        let collisions = wit_name_collisions([
            definition("fetch_user", first_owner, 0, 10),
            definition("fetch_user", duplicate_owner, 11, 21),
            definition("FetchUser", collision_owner, 22, 31),
            definition("FetchUser", repeated_collision_owner, 32, 41),
        ]);

        assert_eq!(
            collisions,
            [super::WitNameCollision {
                wit_name: "fetch-user".to_owned(),
                earlier_span: first_span,
                later_owner: collision_owner,
                later_span: collision_span,
            }]
        );
    }

    #[test]
    fn ignores_unrepresentable_names() {
        let [first_owner, invalid_owner] = owners(2).try_into().expect("two test owners");

        let collisions = wit_name_collisions([
            definition("fetch_user", first_owner, 0, 10),
            definition("fetch__user", invalid_owner, 20, 31),
        ]);

        assert!(collisions.is_empty());
    }

    #[test]
    fn emits_multiple_later_collisions_in_input_order() {
        let [first_a, first_b, later_a, later_b] = owners(4).try_into().expect("four test owners");

        let collisions = wit_name_collisions([
            definition("read_file", first_a, 0, 9),
            definition("write_file", first_b, 10, 21),
            definition("ReadFile", later_a, 22, 30),
            definition("WriteFile", later_b, 31, 40),
        ]);

        assert_eq!(
            collisions
                .iter()
                .map(|collision| (
                    collision.wit_name.as_str(),
                    collision.later_owner,
                    collision.earlier_span,
                ))
                .collect::<Vec<_>>(),
            [
                ("read-file", later_a, span(0, 9)),
                ("write-file", later_b, span(10, 21)),
            ]
        );
    }
}
