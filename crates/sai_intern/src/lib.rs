#![deny(unsafe_op_in_unsafe_fn)]

use sai_stable_hash::{StableHash, StableHasher, fingerprint};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

const SHARD_COUNT: usize = 16;
const RADIX: usize = 256;
static NEXT_INTERNER_SESSION_ID: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct InternerSessionId(u32);

impl std::fmt::Debug for InternerSessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InternerSessionId")
    }
}

fn next_interner_session_id() -> InternerSessionId {
    let mut id = NEXT_INTERNER_SESSION_ID.load(Ordering::Relaxed);
    loop {
        let next = id
            .checked_add(1)
            .expect("interner session ID space exhausted");
        match NEXT_INTERNER_SESSION_ID.compare_exchange_weak(
            id,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return InternerSessionId(id),
            Err(observed) => id = observed,
        }
    }
}

/// A session-local interned string identity.
///
/// Symbols are intentionally not `StableHash`: persisted or cross-session
/// values must hash the resolved string, never this session-local integer.
#[repr(C)]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Symbol {
    index: u32,
    session_id: InternerSessionId,
}

impl Symbol {
    /// Sentinel for a missing or already-diagnosed name.
    ///
    /// It is not owned by an interner session and cannot be resolved. The
    /// maximum `u32` index is reserved so a source symbol can never alias it.
    pub const ERROR: Self = Self {
        index: u32::MAX,
        session_id: InternerSessionId(0),
    };

    pub const fn is_error(self) -> bool {
        self.index == u32::MAX && self.session_id.0 == 0
    }

    pub const fn raw(self) -> u32 {
        self.index
    }

    pub const fn session_id(self) -> InternerSessionId {
        self.session_id
    }
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_error() {
            formatter.write_str("Symbol::ERROR")
        } else {
            formatter.debug_tuple("Symbol").field(&self.index).finish()
        }
    }
}

/// A session-local identity for an interned package key.
///
/// The numeric value is an implementation detail and must not be persisted;
/// stable package identity is the canonical package key string.
#[repr(C)]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct PackageId {
    index: u32,
    session_id: InternerSessionId,
}

impl PackageId {
    pub const fn raw(self) -> u32 {
        self.index
    }

    pub const fn session_id(self) -> InternerSessionId {
        self.session_id
    }
}

impl std::fmt::Debug for PackageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PackageId")
            .field(&self.index)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageIdError {
    TooManyPackages,
    UnknownPackage,
    LockPoisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InternError {
    TooManySymbols,
    LockPoisoned,
    StorageInvariant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupError {
    UnknownSymbol,
    LockPoisoned,
    TooManySymbols,
}

#[derive(Default)]
struct Shard {
    symbols: HashMap<Arc<str>, Symbol>,
}

enum AppendNode<T> {
    Branch(Box<[OnceLock<Box<AppendNode<T>>>; RADIX]>),
    Leaf(Box<[OnceLock<T>; RADIX]>),
}

impl<T> AppendNode<T> {
    fn branch() -> Self {
        Self::Branch(Box::new(std::array::from_fn(|_| OnceLock::new())))
    }

    fn leaf() -> Self {
        Self::Leaf(Box::new(std::array::from_fn(|_| OnceLock::new())))
    }
}

/// A sparse radix tree with immutable, lock-free reads after initialization.
struct AppendOnlyVec<T> {
    root: OnceLock<Box<AppendNode<T>>>,
}

impl<T> AppendOnlyVec<T> {
    fn new() -> Self {
        Self {
            root: OnceLock::new(),
        }
    }

    fn set(&self, index: u32, value: T) -> Result<(), T> {
        let bytes = index.to_be_bytes();
        let root = self.root.get_or_init(|| Box::new(AppendNode::branch()));
        let mut node = root.as_ref();

        for (depth, component) in bytes[..3].iter().enumerate() {
            let AppendNode::Branch(children) = node else {
                return Err(value);
            };
            let child = children[*component as usize].get_or_init(|| {
                Box::new(if depth == 2 {
                    AppendNode::leaf()
                } else {
                    AppendNode::branch()
                })
            });
            node = child.as_ref();
        }

        let AppendNode::Leaf(values) = node else {
            return Err(value);
        };
        values[bytes[3] as usize].set(value)
    }

    fn get(&self, index: u32) -> Option<&T> {
        let bytes = index.to_be_bytes();
        let mut node = self.root.get()?.as_ref();

        for component in &bytes[..3] {
            let AppendNode::Branch(children) = node else {
                return None;
            };
            node = children[*component as usize].get()?.as_ref();
        }

        let AppendNode::Leaf(values) = node else {
            return None;
        };
        values[bytes[3] as usize].get()
    }
}

/// A concurrent, sharded, append-only string interner.
pub struct Interner {
    shards: [RwLock<Shard>; SHARD_COUNT],
    values: AppendOnlyVec<Arc<str>>,
    next_id: Mutex<u64>,
    session_id: InternerSessionId,
}

impl Interner {
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(Shard::default())),
            values: AppendOnlyVec::new(),
            next_id: Mutex::new(0),
            session_id: next_interner_session_id(),
        }
    }

    pub const fn session_id(&self) -> InternerSessionId {
        self.session_id
    }

    pub fn intern(&self, value: &str) -> Result<Symbol, InternError> {
        let shard_index = shard_for(value);
        let mut shard = self.shards[shard_index]
            .write()
            .map_err(|_| InternError::LockPoisoned)?;
        if let Some(symbol) = shard.symbols.get(value) {
            return Ok(*symbol);
        }

        let mut next_id = self.next_id.lock().map_err(|_| InternError::LockPoisoned)?;
        if *next_id >= u32::MAX as u64 {
            return Err(InternError::TooManySymbols);
        }
        let raw = u32::try_from(*next_id).map_err(|_| InternError::TooManySymbols)?;
        let symbol = Symbol {
            index: raw,
            session_id: self.session_id,
        };
        let value: Arc<str> = Arc::from(value);
        self.values
            .set(raw, Arc::clone(&value))
            .map_err(|_| InternError::StorageInvariant)?;
        *next_id += 1;
        shard.symbols.insert(value, symbol);
        Ok(symbol)
    }

    pub fn resolve(&self, symbol: Symbol) -> Result<Arc<str>, LookupError> {
        if symbol.is_error() || symbol.session_id != self.session_id {
            return Err(LookupError::UnknownSymbol);
        }
        self.values
            .get(symbol.index)
            .cloned()
            .ok_or(LookupError::UnknownSymbol)
    }

    pub fn len(&self) -> Result<usize, LookupError> {
        let len = *self.next_id.lock().map_err(|_| LookupError::LockPoisoned)?;
        usize::try_from(len).map_err(|_| LookupError::TooManySymbols)
    }

    pub fn is_empty(&self) -> Result<bool, LookupError> {
        self.len().map(|len| len == 0)
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct PackageStorage {
    keys: Vec<Arc<str>>,
    by_key: HashMap<Arc<str>, PackageId>,
}

/// A session-local interner for canonical package identity keys.
///
/// Callers must validate and canonicalize keys before interning them.
pub struct PackageInterner {
    storage: Mutex<PackageStorage>,
    session_id: InternerSessionId,
}

impl PackageInterner {
    pub fn new() -> Self {
        Self {
            storage: Mutex::new(PackageStorage::default()),
            session_id: next_interner_session_id(),
        }
    }

    pub const fn session_id(&self) -> InternerSessionId {
        self.session_id
    }

    pub fn intern(&self, key: &str) -> Result<PackageId, PackageIdError> {
        let mut storage = self
            .storage
            .lock()
            .map_err(|_| PackageIdError::LockPoisoned)?;
        if let Some(id) = storage.by_key.get(key) {
            return Ok(*id);
        }

        let raw = u32::try_from(storage.keys.len()).map_err(|_| PackageIdError::TooManyPackages)?;
        let id = PackageId {
            index: raw,
            session_id: self.session_id,
        };
        let key: Arc<str> = Arc::from(key);
        storage.keys.push(Arc::clone(&key));
        storage.by_key.insert(key, id);
        Ok(id)
    }

    pub fn resolve(&self, id: PackageId) -> Result<Arc<str>, PackageIdError> {
        if id.session_id != self.session_id {
            return Err(PackageIdError::UnknownPackage);
        }
        let storage = self
            .storage
            .lock()
            .map_err(|_| PackageIdError::LockPoisoned)?;
        let index = usize::try_from(id.index).map_err(|_| PackageIdError::UnknownPackage)?;
        storage
            .keys
            .get(index)
            .cloned()
            .ok_or(PackageIdError::UnknownPackage)
    }

    pub fn len(&self) -> Result<usize, PackageIdError> {
        self.storage
            .lock()
            .map(|storage| storage.keys.len())
            .map_err(|_| PackageIdError::LockPoisoned)
    }

    pub fn is_empty(&self) -> Result<bool, PackageIdError> {
        self.len().map(|len| len == 0)
    }
}

impl Default for PackageInterner {
    fn default() -> Self {
        Self::new()
    }
}

fn shard_for(value: &str) -> usize {
    (fingerprint(value).raw() as usize) & (SHARD_COUNT - 1)
}

/// A typed index used for per-query vectors.
mod index_sealed {
    pub trait Sealed {}
}

pub trait Idx: index_sealed::Sealed + Copy + Eq + Ord + std::fmt::Debug {
    const STABLE_TAG: u8;

    fn from_raw(raw: u32) -> Self;
    fn raw(self) -> u32;
}

macro_rules! define_index_type {
    ($name:ident, $stable_tag:literal) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl index_sealed::Sealed for $name {}

        impl $name {
            pub const fn raw(self) -> u32 {
                self.0
            }
        }

        impl Idx for $name {
            const STABLE_TAG: u8 = $stable_tag;

            fn from_raw(raw: u32) -> Self {
                Self(raw)
            }

            fn raw(self) -> u32 {
                self.0
            }
        }

        impl StableHash for $name {
            fn stable_hash(&self, hasher: &mut StableHasher) {
                hasher.write_tag($stable_tag);
                self.0.stable_hash(hasher);
            }
        }
    };
}

define_index_type!(ItemLocalId, 0x81);
define_index_type!(BindingId, 0x82);
define_index_type!(BasicBlock, 0x83);
define_index_type!(Local, 0x84);

macro_rules! define_session_index_type {
    ($name:ident, $stable_tag:literal) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl index_sealed::Sealed for $name {}

        impl $name {
            pub const fn raw(self) -> u32 {
                self.0
            }
        }

        impl Idx for $name {
            const STABLE_TAG: u8 = $stable_tag;

            fn from_raw(raw: u32) -> Self {
                Self(raw)
            }

            fn raw(self) -> u32 {
                self.0
            }
        }
    };
}

define_session_index_type!(DefIndex, 0x85);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexVecError {
    TooManyElements,
}

/// A vector whose indices carry a compile-time index type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexVec<I, T> {
    values: Vec<T>,
    marker: std::marker::PhantomData<fn() -> I>,
}

impl<I: Idx, T> IndexVec<I, T> {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            marker: std::marker::PhantomData,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            marker: std::marker::PhantomData,
        }
    }

    pub fn push(&mut self, value: T) -> Result<I, IndexVecError> {
        let raw = u32::try_from(self.values.len()).map_err(|_| IndexVecError::TooManyElements)?;
        self.values.push(value);
        Ok(I::from_raw(raw))
    }

    pub fn get(&self, index: I) -> Option<&T> {
        self.values.get(usize::try_from(index.raw()).ok()?)
    }

    pub fn get_mut(&mut self, index: I) -> Option<&mut T> {
        self.values.get_mut(usize::try_from(index.raw()).ok()?)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter_enumerated(&self) -> impl ExactSizeIterator<Item = (I, &T)> {
        self.values
            .iter()
            .enumerate()
            .map(|(index, value)| (I::from_raw(index as u32), value))
    }
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Idx, T: StableHash> StableHash for IndexVec<I, T> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x90);
        hasher.write_tag(I::STABLE_TAG);
        hasher.write_len(u64::try_from(self.values.len()).unwrap_or(u64::MAX));
        for value in &self.values {
            value.stable_hash(hasher);
        }
    }
}

/// A per-session arena for copyable values with stable addresses.
///
/// Restricting values to `Copy` ensures that dropping arena entries cannot run
/// user destructors that observe other entries while the arena is being torn
/// down.
pub struct Arena<T: Copy> {
    values: Mutex<Vec<Box<T>>>,
}

impl<T: Copy> Arena<T> {
    pub fn new() -> Self {
        Self {
            values: Mutex::new(Vec::new()),
        }
    }

    pub fn alloc(&self, value: T) -> &T {
        let boxed = Box::new(value);
        let pointer = &*boxed as *const T;
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(boxed);

        // SAFETY: the Box allocation is retained in `self.values`, which never
        // removes or moves its pointee. The returned reference is tied to
        // `&self`, so it cannot outlive the arena. `T: Copy` also guarantees
        // that arena teardown does not run a destructor that could observe
        // another allocation after its lifetime ends. The mutex only protects
        // the owning vector; the pointee's address is stable after unlock.
        unsafe { &*pointer }
    }

    pub fn len(&self) -> usize {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Copy> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A thread-safe arena that canonicalizes copyable values with consistent
/// `Hash` and `Eq` implementations.
///
/// Interning the same structural value more than once returns the same address,
/// so references can use pointer equality as value equality. Every allocation
/// remains retained even if a key violates the `Hash`/`Eq` consistency contract.
pub struct TypeArena<T: Copy + Eq + Hash> {
    storage: Mutex<TypeArenaStorage<T>>,
}

struct TypeArenaStorage<T> {
    values: Vec<Box<T>>,
    by_value: HashMap<T, usize>,
}

impl<T: Copy + Eq + Hash> TypeArena<T> {
    pub fn new() -> Self {
        Self {
            storage: Mutex::new(TypeArenaStorage {
                values: Vec::new(),
                by_value: HashMap::new(),
            }),
        }
    }

    pub fn intern(&self, value: T) -> &T {
        let mut storage = self
            .storage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pointer = if let Some(index) = storage.by_value.get(&value).copied() {
            &*storage.values[index] as *const T
        } else {
            let boxed = Box::new(value);
            let pointer = &*boxed as *const T;
            let index = storage.values.len();
            storage.values.push(boxed);
            storage.by_value.insert(value, index);
            pointer
        };

        // SAFETY: every allocation is retained in `storage.values` and that
        // vector never removes or drops entries before the arena. Even if a
        // caller violates the Hash/Eq consistency contract and `by_value.insert`
        // replaces an index, the old Box remains in `storage.values`. The
        // returned reference is tied to `&self`, and the mutex makes lookup and
        // first publication atomic across concurrent callers.
        unsafe { &*pointer }
    }

    pub fn len(&self) -> usize {
        self.storage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_value
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Copy + Eq + Hash> Default for TypeArena<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn interning_is_idempotent_and_resolves_values() {
        let interner = Interner::new();
        let first = interner.intern("sai").expect("intern succeeds");
        let second = interner.intern("sai").expect("intern succeeds");

        assert_eq!(first, second);
        assert_eq!(&*interner.resolve(first).expect("symbol exists"), "sai");
        assert_eq!(interner.len(), Ok(1));
    }

    #[test]
    fn error_symbol_is_a_non_resolvable_sentinel() {
        let first = Interner::new();
        let second = Interner::new();
        let symbol = first.intern("sai").expect("intern succeeds");

        assert!(Symbol::ERROR.is_error());
        assert_ne!(Symbol::ERROR, symbol);
        assert_eq!(
            first.resolve(Symbol::ERROR),
            Err(LookupError::UnknownSymbol)
        );
        assert_eq!(
            second.resolve(Symbol::ERROR),
            Err(LookupError::UnknownSymbol)
        );
        assert_eq!(format!("{:?}", Symbol::ERROR), "Symbol::ERROR");
    }

    #[test]
    fn concurrent_interning_of_the_same_value_returns_one_symbol() {
        let interner = Arc::new(Interner::new());
        let workers = (0..16)
            .map(|_| {
                let interner = Arc::clone(&interner);
                thread::spawn(move || interner.intern("shared").expect("intern succeeds"))
            })
            .collect::<Vec<_>>();
        let symbols = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker completes"))
            .collect::<Vec<_>>();

        assert!(symbols.iter().all(|symbol| *symbol == symbols[0]));
        assert_eq!(interner.len(), Ok(1));
    }

    #[test]
    fn distinct_interners_have_session_local_symbols() {
        let first = Interner::new();
        let second = Interner::new();
        let first_symbol = first.intern("one").unwrap();
        let second_symbol = second.intern("two").unwrap();

        assert_eq!(first_symbol.raw(), 0);
        assert_eq!(second_symbol.raw(), 0);
        assert_ne!(first_symbol, second_symbol);
        assert_eq!(&*first.resolve(first_symbol).expect("symbol exists"), "one");
        assert_eq!(
            first.resolve(second_symbol),
            Err(LookupError::UnknownSymbol)
        );
        assert_eq!(
            &*second.resolve(second_symbol).expect("symbol exists"),
            "two"
        );
    }

    #[test]
    fn package_interning_is_idempotent_and_resolves_canonical_keys() {
        let packages = PackageInterner::new();
        let app = packages
            .intern("acme:app#1.0")
            .expect("package key interns");
        let core = packages.intern("sai:core").expect("core interns");

        assert_eq!(packages.intern("acme:app#1.0"), Ok(app));
        assert_ne!(app, core);
        assert_eq!(
            &*packages.resolve(app).expect("app package resolves"),
            "acme:app#1.0"
        );
        let other_session_package = PackageInterner::new().intern("acme:other#1").unwrap();
        assert_eq!(other_session_package.raw(), app.raw());
        assert_ne!(other_session_package, app);
        assert_eq!(
            packages.resolve(other_session_package),
            Err(PackageIdError::UnknownPackage)
        );
        assert_eq!(packages.len(), Ok(2));
        assert_eq!(packages.is_empty(), Ok(false));
        assert_eq!(PackageInterner::new().is_empty(), Ok(true));
    }

    #[test]
    fn index_vectors_keep_indices_typed_and_stable() {
        let mut items = IndexVec::<ItemLocalId, &str>::new();
        let first = items.push("first").expect("index fits");
        let second = items.push("second").expect("index fits");

        assert_eq!(first.raw(), 0);
        assert_eq!(items.get(second), Some(&"second"));
        assert_eq!(
            items.iter_enumerated().collect::<Vec<_>>(),
            [(first, &"first"), (second, &"second")]
        );
        assert_ne!(first.fingerprint(), BindingId::default().fingerprint());

        let mut definitions = IndexVec::<DefIndex, &str>::new();
        let definition = definitions.push("definition").expect("index fits");
        assert_eq!(definition.raw(), 0);
        assert_eq!(definitions.get(definition), Some(&"definition"));
        let mut another_index_space = IndexVec::<ItemLocalId, &str>::new();
        another_index_space.push("definition").expect("index fits");
        assert_ne!(definitions.fingerprint(), another_index_space.fingerprint());
    }

    #[test]
    fn arena_allocations_keep_their_addresses_stable() {
        let arena = Arena::new();
        let first = arena.alloc([1_u32, 2, 3]);
        let first_ptr = first as *const [u32; 3];

        for value in 0..128_u32 {
            arena.alloc([value; 3]);
        }

        assert_eq!(first_ptr, first as *const [u32; 3]);
        assert_eq!(first, &[1, 2, 3]);
        assert_eq!(arena.len(), 129);
    }

    #[test]
    fn type_arena_interns_equal_values_at_one_address() {
        let arena = TypeArena::new();
        let first = arena.intern((1_u32, 2_u32));
        let second = arena.intern((1_u32, 2_u32));

        assert!(std::ptr::eq(first, second));
        assert_eq!(*first, (1, 2));
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn type_arena_canonicalizes_values_across_threads() {
        let arena = Arc::new(TypeArena::new());
        let workers = (0..16)
            .map(|_| {
                let arena = Arc::clone(&arena);
                thread::spawn(move || arena.intern((0xCAFE_u32, 0xBABE_u32)) as *const _ as usize)
            })
            .collect::<Vec<_>>();
        let addresses = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker completes"))
            .collect::<Vec<_>>();

        assert!(addresses.iter().all(|address| *address == addresses[0]));
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn type_arena_keeps_replaced_allocations_alive_if_hash_eq_contract_is_broken() {
        use std::hash::{Hash, Hasher};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static EQ_CALLS: AtomicUsize = AtomicUsize::new(0);

        #[derive(Clone, Copy)]
        struct UnstableEquality {
            value: u32,
        }

        impl PartialEq for UnstableEquality {
            fn eq(&self, other: &Self) -> bool {
                let call = EQ_CALLS.fetch_add(1, Ordering::SeqCst);
                call % 2 == 1 && self.value == other.value
            }
        }

        impl Eq for UnstableEquality {}

        impl Hash for UnstableEquality {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.value.hash(state);
            }
        }

        EQ_CALLS.store(0, Ordering::SeqCst);
        let arena = TypeArena::new();
        let first = arena.intern(UnstableEquality { value: 7 });
        let second = arena.intern(UnstableEquality { value: 7 });

        assert_ne!(
            first as *const UnstableEquality,
            second as *const UnstableEquality
        );
        assert_eq!(first.value, 7);
        assert_eq!(second.value, 7);
    }

    #[test]
    fn arena_supports_concurrent_stable_allocations() {
        let arena = Arc::new(Arena::new());
        let first = arena.alloc(0xABCD_u32);
        let first_address = first as *const u32 as usize;
        let workers = (0..16)
            .map(|worker| {
                let arena = Arc::clone(&arena);
                thread::spawn(move || {
                    for offset in 0..32_u32 {
                        arena.alloc(worker * 32 + offset);
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("worker completes");
        }

        assert_eq!(first as *const u32 as usize, first_address);
        assert_eq!(*first, 0xABCD);
        assert_eq!(arena.len(), 1 + 16 * 32);
    }
}
