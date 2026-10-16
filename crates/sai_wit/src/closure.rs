use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;

use crate::WitUnit;

/// A cycle encountered while computing a WIT dependency closure.
///
/// `cycle_path` starts and ends at the same node. Nodes before the cycle are
/// omitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitClosureError<K> {
    cycle_path: Vec<K>,
}

impl<K> WitClosureError<K> {
    /// Returns the closed cycle path.
    pub fn cycle_path(&self) -> &[K] {
        &self.cycle_path
    }
}

impl<K: fmt::Debug> fmt::Display for WitClosureError<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WIT dependency cycle: {:?}", self.cycle_path)
    }
}

impl<K: fmt::Debug> std::error::Error for WitClosureError<K> {}

/// Computes a deterministic dependency closure in post-order.
///
/// Each node's dependency vector must already be in the caller's desired
/// first-reference order. A node is emitted after its dependencies, and only
/// once even if multiple paths reference it. Self references are ignored.
/// Non-self cycles return a closed cycle path.
///
/// The traversal is iterative, so dependency graphs do not consume call stack
/// depth.
pub fn wit_post_order_closure<K>(
    root: K,
    dependencies: &HashMap<K, Vec<K>>,
) -> Result<Vec<K>, WitClosureError<K>>
where
    K: Clone + Eq + Hash,
{
    let mut completed = HashSet::new();
    let mut active_positions = HashMap::new();
    let mut active_path = Vec::new();
    let mut stack = Vec::new();
    let mut post_order = Vec::new();

    active_positions.insert(root.clone(), 0);
    active_path.push(root.clone());
    stack.push((root, 0));

    while let Some((node, dependency_index)) = stack.last().cloned() {
        let next_dependency = dependencies
            .get(&node)
            .and_then(|node_dependencies| node_dependencies.get(dependency_index))
            .cloned();

        let Some(dependency) = next_dependency else {
            stack.pop();
            active_positions.remove(&node);
            active_path.pop();
            completed.insert(node.clone());
            post_order.push(node);
            continue;
        };

        // Advance the current frame before descending into its dependency.
        stack.last_mut().expect("the frame still exists").1 += 1;

        if dependency == node || completed.contains(&dependency) {
            continue;
        }

        if let Some(&cycle_start) = active_positions.get(&dependency) {
            let mut cycle_path = active_path[cycle_start..].to_vec();
            cycle_path.push(dependency);
            return Err(WitClosureError { cycle_path });
        }

        active_positions.insert(dependency.clone(), active_path.len());
        active_path.push(dependency.clone());
        stack.push((dependency, 0));
    }

    Ok(post_order)
}

/// Computes the deterministic post-order closure of WIT units.
///
/// Each unit's dependencies must be ordered by the first source reference.
/// This typed wrapper keeps callers from mixing WIT units with other graph
/// node identities.
pub fn wit_unit_closure(
    root: WitUnit,
    dependencies: &HashMap<WitUnit, Vec<WitUnit>>,
) -> Result<Vec<WitUnit>, WitClosureError<WitUnit>> {
    wit_post_order_closure(root, dependencies)
}

#[cfg(test)]
mod tests {
    use super::{WitClosureError, wit_post_order_closure};
    use std::collections::HashMap;

    fn dependencies(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(node, dependencies)| {
                (
                    (*node).to_owned(),
                    dependencies
                        .iter()
                        .map(|dependency| (*dependency).to_owned())
                        .collect(),
                )
            })
            .collect()
    }

    fn names(nodes: Vec<String>) -> Vec<String> {
        nodes
    }

    #[test]
    fn emits_leaf_middle_root_in_post_order() {
        let graph = dependencies(&[("root", &["middle"]), ("middle", &["leaf"])]);

        assert_eq!(
            wit_post_order_closure("root".to_owned(), &graph).unwrap(),
            names(vec!["leaf".into(), "middle".into(), "root".into()])
        );
    }

    #[test]
    fn follows_first_reference_order() {
        let graph = dependencies(&[
            ("root", &["second", "first"]),
            ("second", &["second-leaf"]),
            ("first", &["first-leaf"]),
        ]);

        assert_eq!(
            wit_post_order_closure("root".to_owned(), &graph).unwrap(),
            names(vec![
                "second-leaf".into(),
                "second".into(),
                "first-leaf".into(),
                "first".into(),
                "root".into(),
            ])
        );
    }

    #[test]
    fn emits_a_diamond_dependency_only_once() {
        let graph = dependencies(&[
            ("root", &["left", "right"]),
            ("left", &["shared"]),
            ("right", &["shared"]),
        ]);

        assert_eq!(
            wit_post_order_closure("root".to_owned(), &graph).unwrap(),
            names(vec![
                "shared".into(),
                "left".into(),
                "right".into(),
                "root".into(),
            ])
        );
    }

    #[test]
    fn ignores_self_references() {
        let graph = dependencies(&[("root", &["root", "child"])]);

        assert_eq!(
            wit_post_order_closure("root".to_owned(), &graph).unwrap(),
            names(vec!["child".into(), "root".into()])
        );
    }

    #[test]
    fn reports_a_closed_cycle_path() {
        let graph = dependencies(&[
            ("root", &["prefix"]),
            ("prefix", &["a"]),
            ("a", &["b"]),
            ("b", &["a"]),
        ]);

        let error: WitClosureError<String> =
            wit_post_order_closure("root".to_owned(), &graph).unwrap_err();

        assert_eq!(
            error.cycle_path(),
            &["a".to_owned(), "b".to_owned(), "a".to_owned()]
        );
    }
}
