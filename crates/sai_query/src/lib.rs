#![forbid(unsafe_code)]

use sai_stable_hash::{Fingerprint, StableHash, fingerprint};
use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

/// A session-local input-change frequency.
///
/// Lower values change more often. A derived query inherits the least durable
/// value among the queries it reads.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Durability {
    Low,
    Medium,
    High,
}

impl Durability {
    const fn index(self) -> usize {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
        }
    }
}

/// A clonable cancellation flag that can be raised while a query is running.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Requests cancellation of the current query work.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Clears a prior cancellation request before starting new work.
    pub fn reset(&self) {
        self.0.store(false, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// An error raised by the query runtime rather than by the language being
/// compiled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryError {
    Cancelled,
    Cycle { stack: Vec<String> },
    MissingInput { query: String },
    Panicked { query: String, message: String },
    InputMutationDuringQuery { input: String, query: String },
    QueryIdExhausted,
    RevisionExhausted,
    QueryKindMismatch { query: String },
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("query was cancelled"),
            Self::Cycle { stack } => write!(formatter, "query cycle: {}", stack.join(" -> ")),
            Self::MissingInput { query } => write!(formatter, "input query `{query}` has no value"),
            Self::Panicked { query, message } => {
                write!(formatter, "query `{query}` panicked: {message}")
            }
            Self::InputMutationDuringQuery { input, query } => write!(
                formatter,
                "query `{query}` attempted to change input `{input}`"
            ),
            Self::QueryIdExhausted => formatter.write_str("query ID space is exhausted"),
            Self::RevisionExhausted => formatter.write_str("query revision space is exhausted"),
            Self::QueryKindMismatch { query } => {
                write!(
                    formatter,
                    "query `{query}` was used with an incompatible query kind"
                )
            }
        }
    }
}

impl std::error::Error for QueryError {}

/// Synchronous, session-local storage for inputs and derived query memos.
///
/// Query functions receive exclusive access to the database and must read
/// compiler data through query handles. This first runtime slice is
/// single-threaded; parallel query execution is a later extension.
pub struct QueryDatabase {
    revision: u64,
    durability_changed_at: [u64; 3],
    memos: HashMap<NodeKey, Vec<Box<dyn Any + Send + Sync>>>,
    active: Vec<ActiveQuery>,
    cancellation: CancellationToken,
}

impl Default for QueryDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryDatabase {
    pub fn new() -> Self {
        Self {
            revision: 1,
            durability_changed_at: [1; 3],
            memos: HashMap::new(),
            active: Vec::new(),
            cancellation: CancellationToken::default(),
        }
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the last revision in which an input of this durability changed.
    pub fn durability_changed_at(&self, durability: Durability) -> u64 {
        self.durability_changed_at[durability.index()]
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn check_cancellation(&mut self) -> Result<(), QueryError> {
        if self.cancellation.is_cancelled() {
            let error = QueryError::Cancelled;
            self.record_query_error(&error);
            Err(error)
        } else {
            Ok(())
        }
    }

    fn record_dependency(&mut self, dependency: Dependency) {
        if let Some(active) = self.active.last_mut() {
            active.dependencies.push(dependency);
        }
    }

    fn memo_state<K, V>(&self, node: NodeKey, key: &K) -> Result<Option<MemoState<V>>, QueryError>
    where
        K: Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        let Some(bucket) = self.memos.get(&node) else {
            return Ok(None);
        };
        for entry in bucket {
            if let Some(entry) = entry.downcast_ref::<MemoEntry<K, V>>() {
                if &entry.key == key {
                    return Ok(Some(entry.state.clone()));
                }
            }
        }
        Ok(None)
    }

    fn store_state<K, V>(&mut self, node: NodeKey, key: K, state: MemoState<V>)
    where
        K: Eq + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let bucket = self.memos.entry(node).or_default();
        for entry in bucket.iter_mut() {
            if let Some(entry) = entry.downcast_mut::<MemoEntry<K, V>>() {
                if entry.key == key {
                    entry.state = state;
                    return;
                }
            }
        }
        bucket.push(Box::new(MemoEntry { key, state }));
    }

    fn active_cycle<K>(&self, query_id: QueryId, key: &K) -> Option<usize>
    where
        K: Eq + Send + Sync + 'static,
    {
        self.active.iter().position(|active| {
            active.query_id == query_id
                && active
                    .key
                    .downcast_ref::<K>()
                    .is_some_and(|active_key| active_key == key)
        })
    }

    fn next_revision(&self) -> Result<u64, QueryError> {
        self.revision
            .checked_add(1)
            .ok_or(QueryError::RevisionExhausted)
    }

    fn ensure_input_mutation_allowed(&mut self, input: &str) -> Result<(), QueryError> {
        if let Some(query) = self.active.last() {
            let error = QueryError::InputMutationDuringQuery {
                input: input.to_owned(),
                query: query.name.to_string(),
            };
            self.record_query_error(&error);
            return Err(error);
        }
        Ok(())
    }

    fn record_query_error(&mut self, error: &QueryError) {
        if let Some(active) = self.active.last_mut() {
            if active.failure.is_none() {
                active.failure = Some(error.clone());
            }
        }
    }

    fn active_failure(&self) -> Option<QueryError> {
        self.active.last().and_then(|active| active.failure.clone())
    }
}

/// A typed, mutable input query.
pub struct InputQuery<K, V> {
    id: QueryId,
    name: Arc<str>,
    durability: Durability,
    marker: std::marker::PhantomData<fn(K) -> V>,
}

impl<K, V> Clone for InputQuery<K, V> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            name: Arc::clone(&self.name),
            durability: self.durability,
            marker: std::marker::PhantomData,
        }
    }
}

impl<K, V> InputQuery<K, V>
where
    K: Clone + Eq + StableHash + Send + Sync + 'static,
    V: Clone + Eq + StableHash + Send + Sync + 'static,
{
    pub fn new(name: impl Into<Arc<str>>, durability: Durability) -> Result<Self, QueryError> {
        Ok(Self {
            id: allocate_query_id()?,
            name: name.into(),
            durability,
            marker: std::marker::PhantomData,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set(&self, database: &mut QueryDatabase, key: K, value: V) -> Result<bool, QueryError> {
        database.ensure_input_mutation_allowed(&self.name)?;
        let node = NodeKey::new(self.id, &key);
        if let Some(state) = database.memo_state::<K, V>(node, &key)? {
            match state {
                MemoState::Input(InputMemo {
                    value: Some(old), ..
                }) if *old == value => return Ok(false),
                MemoState::Input(_) => {}
                MemoState::Derived(_) => {
                    return Err(QueryError::QueryKindMismatch {
                        query: self.name.to_string(),
                    });
                }
            }
        }

        let revision = database.next_revision()?;
        let durability = self.durability;
        database.revision = revision;
        for index in 0..=durability.index() {
            database.durability_changed_at[index] = revision;
        }
        database.store_state(
            node,
            key,
            MemoState::Input(InputMemo {
                value: Some(Arc::new(value)),
                changed_at: revision,
                durability,
            }),
        );
        Ok(true)
    }

    /// Removes a previously set input and records the removal as a revision.
    pub fn remove(&self, database: &mut QueryDatabase, key: K) -> Result<bool, QueryError> {
        database.ensure_input_mutation_allowed(&self.name)?;
        let node = NodeKey::new(self.id, &key);
        match database.memo_state::<K, V>(node, &key)? {
            Some(MemoState::Input(InputMemo { value: Some(_), .. })) => {}
            Some(MemoState::Input(InputMemo { value: None, .. })) | None => {
                return Ok(false);
            }
            Some(MemoState::Derived(_)) => {
                return Err(QueryError::QueryKindMismatch {
                    query: self.name.to_string(),
                });
            }
        }

        let revision = database.next_revision()?;
        let durability = self.durability;
        database.revision = revision;
        for index in 0..=durability.index() {
            database.durability_changed_at[index] = revision;
        }
        database.store_state(
            node,
            key,
            MemoState::Input(InputMemo::<V> {
                value: None,
                changed_at: revision,
                durability,
            }),
        );
        Ok(true)
    }

    pub fn get(&self, database: &mut QueryDatabase, key: K) -> Result<Arc<V>, QueryError> {
        self.get_opt(database, key)?.ok_or_else(|| {
            let error = QueryError::MissingInput {
                query: self.name.to_string(),
            };
            database.record_query_error(&error);
            error
        })
    }

    /// Reads an input while representing an unset or removed value as `None`.
    ///
    /// The absence is tracked as a dependency, so setting the input later
    /// invalidates derived queries that observed it.
    pub fn get_opt(
        &self,
        database: &mut QueryDatabase,
        key: K,
    ) -> Result<Option<Arc<V>>, QueryError> {
        database.check_cancellation()?;
        let result = match self.evaluate(database, &key) {
            Ok(result) => result,
            Err(error) => {
                database.record_query_error(&error);
                return Err(error);
            }
        };
        let query = self.clone();
        let dependency_key = key.clone();
        let dependency = Dependency::new(Arc::clone(&self.name), result.version, move |database| {
            query
                .evaluate(database, &dependency_key)
                .map(|value| value.version)
        });
        database.record_dependency(dependency);
        Ok(result.value)
    }

    fn evaluate(&self, database: &mut QueryDatabase, key: &K) -> Result<InputValue<V>, QueryError> {
        database.check_cancellation()?;
        let node = NodeKey::new(self.id, key);
        match database.memo_state::<K, V>(node, key)? {
            Some(MemoState::Input(input)) => Ok(InputValue {
                value: input.value,
                version: NodeVersion {
                    changed_at: input.changed_at,
                    durability: input.durability,
                },
            }),
            Some(MemoState::Derived(_)) => Err(QueryError::QueryKindMismatch {
                query: self.name.to_string(),
            }),
            None => Ok(InputValue {
                value: None,
                version: NodeVersion {
                    changed_at: 1,
                    durability: self.durability,
                },
            }),
        }
    }
}

type QueryFunction<K, V> =
    dyn Fn(&mut QueryDatabase, &K) -> Result<V, QueryError> + Send + Sync + 'static;
type DependencyRefresh =
    dyn Fn(&mut QueryDatabase) -> Result<NodeVersion, QueryError> + Send + Sync + 'static;

/// A typed, memoized derived query.
pub struct DerivedQuery<K, V> {
    id: QueryId,
    name: Arc<str>,
    compute: Arc<QueryFunction<K, V>>,
}

impl<K, V> Clone for DerivedQuery<K, V> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            name: Arc::clone(&self.name),
            compute: Arc::clone(&self.compute),
        }
    }
}

impl<K, V> DerivedQuery<K, V>
where
    K: Clone + Eq + StableHash + Send + Sync + 'static,
    V: Clone + Eq + StableHash + Send + Sync + 'static,
{
    pub fn new(
        name: impl Into<Arc<str>>,
        compute: impl Fn(&mut QueryDatabase, &K) -> Result<V, QueryError> + Send + Sync + 'static,
    ) -> Result<Self, QueryError> {
        Ok(Self {
            id: allocate_query_id()?,
            name: name.into(),
            compute: Arc::new(compute),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn get(&self, database: &mut QueryDatabase, key: K) -> Result<Arc<V>, QueryError> {
        database.check_cancellation()?;
        if let Some(cycle_start) = database.active_cycle(self.id, &key) {
            let mut stack = database.active[cycle_start..]
                .iter()
                .map(|active| active.name.to_string())
                .collect::<Vec<_>>();
            stack.push(self.name.to_string());
            let error = QueryError::Cycle { stack };
            database.record_query_error(&error);
            return Err(error);
        }

        let result = match self.evaluate(database, &key) {
            Ok(result) => result,
            Err(error) => {
                database.record_query_error(&error);
                return Err(error);
            }
        };
        let query = self.clone();
        let dependency_key = key.clone();
        let dependency = Dependency::new(Arc::clone(&self.name), result.version, move |database| {
            query
                .evaluate(database, &dependency_key)
                .map(|value| value.version)
        });
        database.record_dependency(dependency);
        Ok(result.value)
    }

    fn evaluate(&self, database: &mut QueryDatabase, key: &K) -> Result<QueryValue<V>, QueryError> {
        database.check_cancellation()?;
        if let Some(cycle_start) = database.active_cycle(self.id, key) {
            let mut stack = database.active[cycle_start..]
                .iter()
                .map(|active| active.name.to_string())
                .collect::<Vec<_>>();
            stack.push(self.name.to_string());
            return Err(QueryError::Cycle { stack });
        }

        let node = NodeKey::new(self.id, key);
        let previous = match database.memo_state::<K, V>(node, key)? {
            Some(MemoState::Derived(memo)) => Some(memo),
            Some(MemoState::Input(_)) => {
                return Err(QueryError::QueryKindMismatch {
                    query: self.name.to_string(),
                });
            }
            None => None,
        };

        if let Some(memo) = &previous {
            if memo.verified_at == database.revision {
                return Ok(QueryValue {
                    value: Arc::clone(&memo.value),
                    version: memo.version(),
                });
            }
        }

        database.active.push(ActiveQuery {
            query_id: self.id,
            key: Box::new(key.clone()),
            name: Arc::clone(&self.name),
            dependencies: Vec::new(),
            failure: None,
        });
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.evaluate_active(database, key, node, previous.as_ref())
        }));
        database.active.pop();

        match result {
            Ok(result) => result,
            Err(payload) => Err(QueryError::Panicked {
                query: self.name.to_string(),
                message: panic_message(payload.as_ref()),
            }),
        }
    }

    fn evaluate_active(
        &self,
        database: &mut QueryDatabase,
        key: &K,
        node: NodeKey,
        previous: Option<&DerivedMemo<V>>,
    ) -> Result<QueryValue<V>, QueryError> {
        if let Some(previous) = previous {
            let durability_is_unchanged =
                database.durability_changed_at[previous.durability.index()] <= previous.verified_at;
            if durability_is_unchanged {
                return self.mark_green(database, key, node, previous.clone());
            }

            let mut dependencies = Vec::with_capacity(previous.dependencies.len());
            let mut changed = false;
            for dependency in &previous.dependencies {
                database.check_cancellation()?;
                let version = (dependency.refresh)(database)?;
                changed |= version.changed_at > previous.verified_at;
                dependencies.push(dependency.with_version(version));
            }
            database.check_cancellation()?;
            if !changed {
                let durability = minimum_durability(&dependencies);
                let memo = DerivedMemo {
                    value: Arc::clone(&previous.value),
                    fingerprint: previous.fingerprint,
                    dependencies,
                    verified_at: database.revision,
                    changed_at: previous.changed_at,
                    durability,
                };
                let result = QueryValue {
                    value: Arc::clone(&memo.value),
                    version: memo.version(),
                };
                database.store_state(node, key.clone(), MemoState::Derived(memo));
                return Ok(result);
            }
        }

        let value = match (self.compute)(database, key) {
            Ok(value) => value,
            Err(error) => {
                database.record_query_error(&error);
                return Err(error);
            }
        };
        if let Some(error) = database.active_failure() {
            return Err(error);
        }
        database.check_cancellation()?;
        let dependencies = database
            .active
            .last()
            .expect("the running query has an active frame")
            .dependencies
            .clone();
        let durability = minimum_durability(&dependencies);
        let fingerprint = fingerprint(&value);
        let (value, changed_at) = match previous {
            Some(previous) if previous.fingerprint == fingerprint && *previous.value == value => {
                (Arc::clone(&previous.value), previous.changed_at)
            }
            _ => (Arc::new(value), database.revision),
        };
        let memo = DerivedMemo {
            value,
            fingerprint,
            dependencies,
            verified_at: database.revision,
            changed_at,
            durability,
        };
        let result = QueryValue {
            value: Arc::clone(&memo.value),
            version: memo.version(),
        };
        database.store_state(node, key.clone(), MemoState::Derived(memo));
        Ok(result)
    }

    fn mark_green(
        &self,
        database: &mut QueryDatabase,
        key: &K,
        node: NodeKey,
        mut memo: DerivedMemo<V>,
    ) -> Result<QueryValue<V>, QueryError> {
        database.check_cancellation()?;
        memo.verified_at = database.revision;
        let result = QueryValue {
            value: Arc::clone(&memo.value),
            version: memo.version(),
        };
        database.store_state(node, key.clone(), MemoState::Derived(memo));
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct QueryId(u64);

fn allocate_query_id() -> Result<QueryId, QueryError> {
    loop {
        let id = NEXT_QUERY_ID.load(Ordering::Relaxed);
        let next = id.checked_add(1).ok_or(QueryError::QueryIdExhausted)?;
        if NEXT_QUERY_ID
            .compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return Ok(QueryId(id));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct NodeKey {
    query_id: QueryId,
    key_fingerprint: Fingerprint,
}

impl NodeKey {
    fn new<K: StableHash + ?Sized>(query_id: QueryId, key: &K) -> Self {
        Self {
            query_id,
            key_fingerprint: fingerprint(key),
        }
    }
}

struct ActiveQuery {
    query_id: QueryId,
    key: Box<dyn Any + Send + Sync>,
    name: Arc<str>,
    dependencies: Vec<Dependency>,
    failure: Option<QueryError>,
}

#[derive(Clone)]
struct Dependency {
    name: Arc<str>,
    durability: Durability,
    refresh: Arc<DependencyRefresh>,
}

impl Dependency {
    fn new(
        name: Arc<str>,
        version: NodeVersion,
        refresh: impl Fn(&mut QueryDatabase) -> Result<NodeVersion, QueryError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            durability: version.durability,
            refresh: Arc::new(refresh),
        }
    }

    fn with_version(&self, version: NodeVersion) -> Self {
        Self {
            name: Arc::clone(&self.name),
            durability: version.durability,
            refresh: Arc::clone(&self.refresh),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct NodeVersion {
    changed_at: u64,
    durability: Durability,
}

struct QueryValue<V> {
    value: Arc<V>,
    version: NodeVersion,
}

struct MemoEntry<K, V> {
    key: K,
    state: MemoState<V>,
}

#[derive(Clone)]
enum MemoState<V> {
    Input(InputMemo<V>),
    Derived(DerivedMemo<V>),
}

#[derive(Clone)]
struct InputMemo<V> {
    value: Option<Arc<V>>,
    changed_at: u64,
    durability: Durability,
}

struct InputValue<V> {
    value: Option<Arc<V>>,
    version: NodeVersion,
}

#[derive(Clone)]
struct DerivedMemo<V> {
    value: Arc<V>,
    fingerprint: Fingerprint,
    dependencies: Vec<Dependency>,
    verified_at: u64,
    changed_at: u64,
    durability: Durability,
}

impl<V> DerivedMemo<V> {
    fn version(&self) -> NodeVersion {
        NodeVersion {
            changed_at: self.changed_at,
            durability: self.durability,
        }
    }
}

fn minimum_durability(dependencies: &[Dependency]) -> Durability {
    dependencies
        .iter()
        .map(|dependency| dependency.durability)
        .min()
        .unwrap_or(Durability::High)
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&'static str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_stable_hash::StableHasher;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn input<V>(name: &'static str, durability: Durability) -> InputQuery<u32, V>
    where
        V: Clone + Eq + StableHash + Send + Sync + 'static,
    {
        InputQuery::new(name, durability).expect("input query ID is available")
    }

    fn derived<V>(
        name: &'static str,
        compute: impl Fn(&mut QueryDatabase, &u32) -> Result<V, QueryError> + Send + Sync + 'static,
    ) -> DerivedQuery<u32, V>
    where
        V: Clone + Eq + StableHash + Send + Sync + 'static,
    {
        DerivedQuery::new(name, compute).expect("derived query ID is available")
    }

    #[test]
    fn inputs_advance_only_the_changed_durability_revision() {
        let mut database = QueryDatabase::new();
        let low = input::<&'static str>("source_text", Durability::Low);
        let initial_revision = database.revision();

        assert!(low.set(&mut database, 1, "one").expect("set succeeds"));
        assert_eq!(database.revision(), initial_revision + 1);
        assert_eq!(
            database.durability_changed_at(Durability::Low),
            database.revision()
        );
        assert_eq!(
            database.durability_changed_at(Durability::Medium),
            initial_revision
        );

        assert!(
            !low.set(&mut database, 1, "one")
                .expect("equal set succeeds")
        );
        assert_eq!(database.revision(), initial_revision + 1);
        assert_eq!(*low.get(&mut database, 1).expect("input exists"), "one");

        assert!(
            low.set(&mut database, 1, "two")
                .expect("changed set succeeds")
        );
        assert_eq!(database.revision(), initial_revision + 2);
        assert_eq!(*low.get(&mut database, 1).expect("input exists"), "two");
    }

    #[test]
    fn newly_set_and_removed_inputs_invalidate_queries_that_observed_absence() {
        let mut database = QueryDatabase::new();
        let source = input::<u32>("optional_source", Durability::Low);
        let missing = input::<u32>("required_source", Durability::Low);
        let source_in_query = source.clone();
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_query = Arc::clone(&runs);
        let fallback = derived("source_or_zero", move |database, key| {
            runs_in_query.fetch_add(1, Ordering::SeqCst);
            source_in_query
                .get_opt(database, *key)
                .map(|value| value.map_or(0, |value| *value))
        });

        assert_eq!(
            *fallback.get(&mut database, 4).expect("absence is handled"),
            0
        );
        assert!(source.set(&mut database, 4, 9).expect("input sets"));
        assert_eq!(*fallback.get(&mut database, 4).expect("input appears"), 9);
        assert_eq!(runs.load(Ordering::SeqCst), 2);

        assert!(source.remove(&mut database, 4).expect("input is removed"));
        assert_eq!(
            *fallback.get(&mut database, 4).expect("input disappears"),
            0
        );
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert!(
            !source
                .remove(&mut database, 4)
                .expect("repeated removal is unchanged")
        );

        let missing_in_query = missing.clone();
        let swallowing_missing =
            derived(
                "swallowing_missing_input",
                move |database, key| match missing_in_query.get(database, *key) {
                    Ok(value) => Ok(*value),
                    Err(_) => Ok(0),
                },
            );
        assert_eq!(
            swallowing_missing.get(&mut database, 4),
            Err(QueryError::MissingInput {
                query: "required_source".to_owned()
            })
        );
    }

    #[test]
    fn query_functions_cannot_mutate_inputs() {
        let mut database = QueryDatabase::new();
        let source = input::<u32>("source", Durability::Low);
        source.set(&mut database, 0, 1).expect("input sets");
        let source_in_query = source.clone();
        let query = derived("impure_query", move |database, key| {
            let _ = source_in_query.set(database, *key, 2);
            Ok(0_u32)
        });

        assert_eq!(
            query.get(&mut database, 0),
            Err(QueryError::InputMutationDuringQuery {
                input: "source".to_owned(),
                query: "impure_query".to_owned()
            })
        );
        let source_in_remove_query = source.clone();
        let remove_query = derived("impure_remove_query", move |database, key| {
            let _ = source_in_remove_query.remove(database, *key);
            Ok(0_u32)
        });
        assert_eq!(
            remove_query.get(&mut database, 0),
            Err(QueryError::InputMutationDuringQuery {
                input: "source".to_owned(),
                query: "impure_remove_query".to_owned()
            })
        );
        assert_eq!(
            *source
                .get(&mut database, 0)
                .expect("original value remains"),
            1
        );
    }

    #[test]
    fn derived_queries_are_memoized_and_backdate_equal_values() {
        let mut database = QueryDatabase::new();
        let source = input::<u32>("source", Durability::Low);
        source.set(&mut database, 0, 2).expect("set succeeds");
        let source_in_query = source.clone();
        let executions = Arc::new(AtomicUsize::new(0));
        let executions_in_query = Arc::clone(&executions);
        let doubled = derived("doubled", move |database, key| {
            executions_in_query.fetch_add(1, Ordering::SeqCst);
            Ok(*source_in_query.get(database, *key)? * 2)
        });

        assert_eq!(*doubled.get(&mut database, 0).expect("query succeeds"), 4);
        assert_eq!(*doubled.get(&mut database, 0).expect("memo is reused"), 4);
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        source.set(&mut database, 0, 2).expect("equal set succeeds");
        assert_eq!(*doubled.get(&mut database, 0).expect("memo is reused"), 4);
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        source
            .set(&mut database, 0, 12)
            .expect("changed set succeeds");
        assert_eq!(
            *doubled.get(&mut database, 0).expect("query is rechecked"),
            24
        );
        assert_eq!(executions.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn unchanged_derived_values_backdate_through_multiple_query_levels() {
        let mut database = QueryDatabase::new();
        let source = input::<u32>("source", Durability::Low);
        source.set(&mut database, 0, 12).expect("set succeeds");
        let source_in_remainder = source.clone();
        let lower_runs = Arc::new(AtomicUsize::new(0));
        let lower_runs_in_query = Arc::clone(&lower_runs);
        let remainder = derived("remainder", move |database, key| {
            lower_runs_in_query.fetch_add(1, Ordering::SeqCst);
            Ok(*source_in_remainder.get(database, *key)? % 10)
        });
        let upper_runs = Arc::new(AtomicUsize::new(0));
        let upper_runs_in_query = Arc::clone(&upper_runs);
        let is_two = derived("is_two", move |database, key| {
            upper_runs_in_query.fetch_add(1, Ordering::SeqCst);
            Ok(*remainder.get(database, *key)? == 2)
        });

        assert!(*is_two.get(&mut database, 0).expect("query succeeds"));
        source.set(&mut database, 0, 22).expect("set succeeds");
        assert!(*is_two.get(&mut database, 0).expect("query remains equal"));
        assert_eq!(lower_runs.load(Ordering::SeqCst), 2);
        assert_eq!(upper_runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn durability_skips_unrelated_lower_durability_inputs() {
        let mut database = QueryDatabase::new();
        let high = input::<u32>("embedded_library", Durability::High);
        let low = input::<u32>("editor_overlay", Durability::Low);
        high.set(&mut database, 0, 7).expect("set succeeds");
        low.set(&mut database, 0, 1).expect("set succeeds");
        let runs = Arc::new(AtomicUsize::new(0));
        let runs_in_query = Arc::clone(&runs);
        let high_in_query = high.clone();
        let query = derived("high_only", move |database, key| {
            runs_in_query.fetch_add(1, Ordering::SeqCst);
            high_in_query.get(database, *key).map(|value| *value)
        });

        assert_eq!(*query.get(&mut database, 0).expect("query succeeds"), 7);
        low.set(&mut database, 0, 2).expect("low input changes");
        assert_eq!(*query.get(&mut database, 0).expect("memo stays green"), 7);
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        high.set(&mut database, 0, 8).expect("high input changes");
        assert_eq!(*query.get(&mut database, 0).expect("query reruns"), 8);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn higher_durability_changes_invalidate_less_durable_queries() {
        let mut database = QueryDatabase::new();
        let low = input::<u32>("workspace_source", Durability::Low);
        let medium = input::<u32>("path_dependency", Durability::Medium);
        let high = input::<u32>("embedded_library", Durability::High);
        low.set(&mut database, 0, 1).expect("low input sets");
        medium.set(&mut database, 0, 2).expect("medium input sets");
        high.set(&mut database, 0, 4).expect("high input sets");
        let low_only_runs = Arc::new(AtomicUsize::new(0));
        let low_only_runs_in_query = Arc::clone(&low_only_runs);
        let low_only_input = low.clone();
        let low_only = derived("low_only", move |database, key| {
            low_only_runs_in_query.fetch_add(1, Ordering::SeqCst);
            low_only_input.get(database, *key).map(|value| *value)
        });
        let mixed_runs = Arc::new(AtomicUsize::new(0));
        let mixed_runs_in_query = Arc::clone(&mixed_runs);
        let low_input = low.clone();
        let high_input = high.clone();
        let mixed = derived("low_and_high", move |database, key| {
            mixed_runs_in_query.fetch_add(1, Ordering::SeqCst);
            Ok(*low_input.get(database, *key)? + *high_input.get(database, *key)?)
        });

        assert_eq!(*low_only.get(&mut database, 0).expect("query succeeds"), 1);
        assert_eq!(*mixed.get(&mut database, 0).expect("query succeeds"), 5);
        let before_medium_change = database.revision();
        medium
            .set(&mut database, 0, 3)
            .expect("medium input changes");
        assert_eq!(
            database.durability_changed_at(Durability::Low),
            database.revision()
        );
        assert_eq!(
            database.durability_changed_at(Durability::Medium),
            database.revision()
        );
        assert_eq!(
            database.durability_changed_at(Durability::High),
            before_medium_change
        );
        assert_eq!(
            *low_only.get(&mut database, 0).expect("low query verifies"),
            1
        );
        assert_eq!(
            *mixed.get(&mut database, 0).expect("mixed query verifies"),
            5
        );
        assert_eq!(low_only_runs.load(Ordering::SeqCst), 1);
        assert_eq!(mixed_runs.load(Ordering::SeqCst), 1);

        high.set(&mut database, 0, 5).expect("high input changes");
        assert_eq!(
            *mixed
                .get(&mut database, 0)
                .expect("high change invalidates memo"),
            6
        );
        assert_eq!(mixed_runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn query_keys_remain_distinct_when_their_fingerprints_collide() {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        struct CollidingKey(u8);

        impl StableHash for CollidingKey {
            fn stable_hash(&self, hasher: &mut StableHasher) {
                hasher.write_tag(0xFE);
            }
        }

        let mut database = QueryDatabase::new();
        let values = InputQuery::<CollidingKey, u8>::new("colliding_keys", Durability::Low)
            .expect("input query ID is available");
        values
            .set(&mut database, CollidingKey(1), 10)
            .expect("first value sets");
        values
            .set(&mut database, CollidingKey(2), 20)
            .expect("second value sets");

        assert_eq!(
            *values
                .get(&mut database, CollidingKey(1))
                .expect("first key is distinct"),
            10
        );
        assert_eq!(
            *values
                .get(&mut database, CollidingKey(2))
                .expect("second key is distinct"),
            20
        );
    }

    #[test]
    fn dynamic_dependencies_are_replaced_after_recomputation() {
        let mut database = QueryDatabase::new();
        let selector = input::<bool>("selector", Durability::Low);
        let left = input::<u32>("left", Durability::Low);
        let right = input::<u32>("right", Durability::Low);
        selector.set(&mut database, 0, false).expect("set succeeds");
        left.set(&mut database, 0, 11).expect("set succeeds");
        right.set(&mut database, 0, 20).expect("set succeeds");
        let selector_in_query = selector.clone();
        let left_in_query = left.clone();
        let right_in_query = right.clone();
        let query = derived("selected_value", move |database, key| {
            if *selector_in_query.get(database, *key)? {
                right_in_query.get(database, *key).map(|value| *value)
            } else {
                left_in_query.get(database, *key).map(|value| *value)
            }
        });

        assert_eq!(*query.get(&mut database, 0).expect("query succeeds"), 11);
        selector
            .set(&mut database, 0, true)
            .expect("selector changes");
        assert_eq!(*query.get(&mut database, 0).expect("query switches"), 20);
        left.set(&mut database, 0, 12)
            .expect("unselected input changes");
        assert_eq!(
            *query
                .get(&mut database, 0)
                .expect("unselected input is ignored"),
            20
        );
    }

    #[test]
    fn cycles_are_reported_with_the_query_path() {
        let mut database = QueryDatabase::new();
        let recursive_query = Arc::new(std::sync::OnceLock::<DerivedQuery<u32, u32>>::new());
        let recursive_query_in_compute = Arc::clone(&recursive_query);
        let query = DerivedQuery::<u32, u32>::new("recursive", move |database, key| {
            recursive_query_in_compute
                .get()
                .expect("query initialized")
                .get(database, *key)
                .map(|value| *value)
        })
        .expect("query ID is available");
        assert!(recursive_query.set(query.clone()).is_ok());

        assert_eq!(
            query.get(&mut database, 0),
            Err(QueryError::Cycle {
                stack: vec!["recursive".to_owned(), "recursive".to_owned()]
            })
        );
    }

    #[test]
    fn parent_queries_cannot_swallow_child_cycles() {
        let mut database = QueryDatabase::new();
        let recursive_query = Arc::new(std::sync::OnceLock::<DerivedQuery<u32, u32>>::new());
        let recursive_query_in_compute = Arc::clone(&recursive_query);
        let child = DerivedQuery::<u32, u32>::new("recursive_child", move |database, key| {
            recursive_query_in_compute
                .get()
                .expect("query initialized")
                .get(database, *key)
                .map(|value| *value)
        })
        .expect("query ID is available");
        assert!(recursive_query.set(child.clone()).is_ok());

        let parent_runs = Arc::new(AtomicUsize::new(0));
        let parent_runs_in_query = Arc::clone(&parent_runs);
        let parent = derived("cycle_swallowing_parent", move |database, key| {
            parent_runs_in_query.fetch_add(1, Ordering::SeqCst);
            match child.get(database, *key) {
                Ok(value) => Ok(*value),
                Err(_) => Ok(0),
            }
        });
        let expected = Err(QueryError::Cycle {
            stack: vec!["recursive_child".to_owned(), "recursive_child".to_owned()],
        });

        assert_eq!(parent.get(&mut database, 0), expected);
        assert_eq!(parent.get(&mut database, 0), expected);
        assert_eq!(parent_runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parent_queries_cannot_swallow_child_panics() {
        let mut database = QueryDatabase::new();
        let child_attempts = Arc::new(AtomicUsize::new(0));
        let child_attempts_in_query = Arc::clone(&child_attempts);
        let child = derived("panic_once_child", move |_database, _key| {
            if child_attempts_in_query.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("child failure");
            }
            Ok(7_u32)
        });
        let parent_runs = Arc::new(AtomicUsize::new(0));
        let parent_runs_in_query = Arc::clone(&parent_runs);
        let parent = derived("panic_swallowing_parent", move |database, key| {
            parent_runs_in_query.fetch_add(1, Ordering::SeqCst);
            match child.get(database, *key) {
                Ok(value) => Ok(*value),
                Err(_) => Ok(0),
            }
        });

        assert_eq!(
            parent.get(&mut database, 0),
            Err(QueryError::Panicked {
                query: "panic_once_child".to_owned(),
                message: "child failure".to_owned()
            })
        );
        assert_eq!(*parent.get(&mut database, 0).expect("child recovers"), 7);
        assert_eq!(
            *parent.get(&mut database, 0).expect("parent memo is reused"),
            7
        );
        assert_eq!(parent_runs.load(Ordering::SeqCst), 2);
        assert_eq!(child_attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cancellation_is_checked_at_query_boundaries_and_inside_loops() {
        let mut database = QueryDatabase::new();
        let token = database.cancellation_token();
        let token_in_query = token.clone();
        let query = derived("cancelled", move |database, _key| {
            for _ in 0..4 {
                database.check_cancellation()?;
                token_in_query.cancel();
            }
            Ok(1_u32)
        });

        token.cancel();
        assert_eq!(query.get(&mut database, 0), Err(QueryError::Cancelled));
        token.reset();
        assert_eq!(query.get(&mut database, 0), Err(QueryError::Cancelled));

        token.reset();
        let successful = derived("after_cancel", |_database, _key| Ok(2_u32));
        assert_eq!(
            *successful.get(&mut database, 0).expect("query succeeds"),
            2
        );
    }

    #[test]
    fn panics_do_not_leave_stale_active_frames_or_memos() {
        let mut database = QueryDatabase::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_in_query = Arc::clone(&attempts);
        let query = derived("panic_once", move |_database, _key| {
            if attempts_in_query.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first attempt");
            }
            Ok(42_u32)
        });

        assert_eq!(
            query.get(&mut database, 0),
            Err(QueryError::Panicked {
                query: "panic_once".to_owned(),
                message: "first attempt".to_owned()
            })
        );
        assert_eq!(
            *query
                .get(&mut database, 0)
                .expect("second attempt succeeds"),
            42
        );
    }
}
