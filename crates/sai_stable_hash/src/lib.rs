#![forbid(unsafe_code)]

use siphasher::sip128::{Hasher128, SipHasher13};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

/// The 128-bit fingerprint used for compiler values and query keys.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Fingerprint(u128);

impl Fingerprint {
    pub const fn new(raw: u128) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u128 {
        self.0
    }

    pub const fn to_le_bytes(self) -> [u8; 16] {
        self.0.to_le_bytes()
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:032x}", self.0)
    }
}

/// A 256-bit BLAKE3 content digest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Incremental BLAKE3 content hasher.
pub struct ContentHasher(blake3::Hasher);

impl ContentHasher {
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> ContentHash {
        ContentHash(*self.0.finalize().as_bytes())
    }
}

impl Default for ContentHasher {
    fn default() -> Self {
        Self::new()
    }
}

pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash(*blake3::hash(bytes).as_bytes())
}

/// A deterministic encoder and SipHash-1-3 hasher with both keys set to zero.
///
/// Callers write an explicit type tag before fields, encode integers with the
/// little-endian methods, and length-prefix every variable-length value.
pub struct StableHasher(SipHasher13);

impl StableHasher {
    pub fn new() -> Self {
        Self(SipHasher13::new())
    }

    pub fn write_tag(&mut self, tag: u8) {
        self.0.write(&[tag]);
    }

    pub fn write_len(&mut self, len: u64) {
        self.0.write(&len.to_le_bytes());
    }

    pub fn write_raw_bytes(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }

    pub fn write_u8(&mut self, value: u8) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_u16(&mut self, value: u16) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_u64(&mut self, value: u64) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_u128(&mut self, value: u128) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_i8(&mut self, value: i8) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_i16(&mut self, value: i16) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_i32(&mut self, value: i32) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_i64(&mut self, value: i64) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn write_i128(&mut self, value: i128) {
        self.0.write(&value.to_le_bytes());
    }

    pub fn finish(self) -> Fingerprint {
        Fingerprint(self.0.finish128().into())
    }
}

impl Default for StableHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Hashes a value using the compiler's stable structural encoding.
pub trait StableHash {
    fn stable_hash(&self, hasher: &mut StableHasher);

    fn fingerprint(&self) -> Fingerprint {
        let mut hasher = StableHasher::new();
        self.stable_hash(&mut hasher);
        hasher.finish()
    }
}

macro_rules! impl_unsigned {
    ($ty:ty, $tag:literal, $write:ident) => {
        impl StableHash for $ty {
            fn stable_hash(&self, hasher: &mut StableHasher) {
                hasher.write_tag($tag);
                hasher.$write(*self);
            }
        }
    };
}

macro_rules! impl_signed {
    ($ty:ty, $tag:literal, $write:ident) => {
        impl StableHash for $ty {
            fn stable_hash(&self, hasher: &mut StableHasher) {
                hasher.write_tag($tag);
                hasher.$write(*self);
            }
        }
    };
}

impl_unsigned!(u8, 0x01, write_u8);
impl_unsigned!(u16, 0x02, write_u16);
impl_unsigned!(u32, 0x03, write_u32);
impl_unsigned!(u64, 0x04, write_u64);
impl_unsigned!(u128, 0x05, write_u128);
impl_signed!(i8, 0x11, write_i8);
impl_signed!(i16, 0x12, write_i16);
impl_signed!(i32, 0x13, write_i32);
impl_signed!(i64, 0x14, write_i64);
impl_signed!(i128, 0x15, write_i128);

impl StableHash for bool {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x20);
        hasher.write_u8(u8::from(*self));
    }
}

impl StableHash for char {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x21);
        hasher.write_u32(*self as u32);
    }
}

impl StableHash for str {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x30);
        hasher.write_len(u64::try_from(self.len()).unwrap_or(u64::MAX));
        hasher.write_raw_bytes(self.as_bytes());
    }
}

impl StableHash for String {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.as_str().stable_hash(hasher);
    }
}

impl<T: StableHash + ?Sized> StableHash for &T {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        T::stable_hash(*self, hasher);
    }
}

impl<T: StableHash> StableHash for [T] {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        stable_hash_sequence(self, 0x40, hasher);
    }
}

impl<T: StableHash> StableHash for Vec<T> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        stable_hash_sequence(self, 0x41, hasher);
    }
}

impl<T: StableHash, const N: usize> StableHash for [T; N] {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        stable_hash_sequence(self, 0x42, hasher);
    }
}

fn stable_hash_sequence<T: StableHash>(values: &[T], tag: u8, hasher: &mut StableHasher) {
    hasher.write_tag(tag);
    hasher.write_len(u64::try_from(values.len()).unwrap_or(u64::MAX));
    for value in values {
        value.stable_hash(hasher);
    }
}

impl<K: StableHash + Ord, V: StableHash> StableHash for BTreeMap<K, V> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x43);
        hasher.write_len(u64::try_from(self.len()).unwrap_or(u64::MAX));
        for (key, value) in self {
            key.stable_hash(hasher);
            value.stable_hash(hasher);
        }
    }
}

impl<T: StableHash + Ord> StableHash for BTreeSet<T> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x44);
        hasher.write_len(u64::try_from(self.len()).unwrap_or(u64::MAX));
        for value in self {
            value.stable_hash(hasher);
        }
    }
}

impl<T: StableHash> StableHash for Option<T> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x50);
        match self {
            None => hasher.write_tag(0),
            Some(value) => {
                hasher.write_tag(1);
                value.stable_hash(hasher);
            }
        }
    }
}

impl<A: StableHash, B: StableHash> StableHash for (A, B) {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x60);
        self.0.stable_hash(hasher);
        self.1.stable_hash(hasher);
    }
}

impl<T: StableHash, E: StableHash> StableHash for Result<T, E> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x61);
        match self {
            Ok(value) => {
                hasher.write_tag(0);
                value.stable_hash(hasher);
            }
            Err(error) => {
                hasher.write_tag(1);
                error.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for Fingerprint {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x70);
        hasher.write_u128(self.0);
    }
}

impl StableHash for ContentHash {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x71);
        hasher.write_raw_bytes(&self.0);
    }
}

pub fn fingerprint<T: StableHash + ?Sized>(value: &T) -> Fingerprint {
    value.fingerprint()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_fingerprints_repeat_and_distinguish_types() {
        assert_eq!(fingerprint(&42_u32), fingerprint(&42_u32));
        assert_ne!(fingerprint(&42_u32), fingerprint(&42_u64));
        assert_ne!(fingerprint(&42_i32), fingerprint(&42_u32));
    }

    #[test]
    fn strings_and_sequences_are_length_prefixed() {
        assert_ne!(fingerprint("ab"), fingerprint("a"));
        assert_ne!(fingerprint(&vec!["a", "bc"]), fingerprint(&vec!["ab", "c"]));
        assert_ne!(fingerprint(&Some(1_u8)), fingerprint(&None::<u8>));
    }

    #[test]
    fn struct_fields_can_be_encoded_with_explicit_type_tags() {
        struct Pair(u32, u32);

        impl StableHash for Pair {
            fn stable_hash(&self, hasher: &mut StableHasher) {
                hasher.write_tag(0xA0);
                self.0.stable_hash(hasher);
                self.1.stable_hash(hasher);
            }
        }

        assert_ne!(fingerprint(&Pair(1, 2)), fingerprint(&Pair(2, 1)));
    }

    #[test]
    fn content_hash_matches_the_blake3_empty_vector() {
        assert_eq!(
            hash_bytes(b"").to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn streaming_content_hash_matches_one_shot_hashing() {
        let mut hasher = ContentHasher::new();
        hasher.update(b"SAI ");
        hasher.update(b"compiler");

        assert_eq!(hasher.finish(), hash_bytes(b"SAI compiler"));
    }
}
