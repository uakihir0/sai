use std::{collections::BTreeSet, env, fmt::Write as _, fs, path::PathBuf};

const LEGACY_CODE_COUNT: usize = 76;
const LEGACY_CODE_ORDER_HASH: u64 = 0x0873_2526_4379_ef14;

fn main() {
    let source_path = PathBuf::from("src/diagnostic_code_names.txt");
    println!("cargo:rerun-if-changed={}", source_path.display());
    let legacy_order_path = PathBuf::from("src/diagnostic_code_legacy_order.txt");
    println!("cargo:rerun-if-changed={}", legacy_order_path.display());

    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", source_path.display()));
    let names = source.split_ascii_whitespace().collect::<Vec<_>>();
    assert!(
        !names.is_empty(),
        "diagnostic code registry must not be empty"
    );

    let mut previous = None;
    let mut variants = BTreeSet::new();
    let mut registry_entries = Vec::with_capacity(names.len());

    for name in names {
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
            "invalid diagnostic code name {name:?}"
        );
        assert!(
            name.as_bytes().first().is_some_and(u8::is_ascii_uppercase),
            "diagnostic code name must start with a letter: {name:?}"
        );
        assert!(
            name.split('_').all(|part| !part.is_empty()),
            "diagnostic code name has an empty segment: {name:?}"
        );
        if let Some(previous) = previous {
            assert!(
                previous < name,
                "diagnostic code names must be unique and sorted: {previous:?}, {name:?}"
            );
        }
        previous = Some(name);

        let variant = variant_name(name);
        assert!(
            variant != "Self",
            "diagnostic code name {name:?} maps to reserved Rust variant Self"
        );
        assert!(
            variants.insert(variant.clone()),
            "diagnostic code names collide at Rust variant {variant}"
        );
        registry_entries.push((name, variant));
    }

    let legacy_source = fs::read_to_string(&legacy_order_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", legacy_order_path.display()));
    let legacy_names = legacy_source.split_ascii_whitespace().collect::<Vec<_>>();
    assert_eq!(
        legacy_names.len(),
        LEGACY_CODE_COUNT,
        "the frozen legacy diagnostic order changed size; review the public Ord behavior"
    );
    assert_eq!(
        legacy_order_hash(&legacy_names),
        LEGACY_CODE_ORDER_HASH,
        "the frozen legacy diagnostic order changed; review the public Ord behavior"
    );
    let mut seen_legacy = BTreeSet::new();
    let mut entries = Vec::with_capacity(registry_entries.len());
    for name in legacy_names {
        assert!(
            seen_legacy.insert(name),
            "legacy diagnostic code order repeats {name:?}"
        );
        let entry = registry_entries
            .iter()
            .find(|(registered, _)| *registered == name)
            .unwrap_or_else(|| panic!("legacy diagnostic code {name:?} is not registered"));
        entries.push((entry.0, entry.1.clone()));
    }
    entries.extend(
        registry_entries
            .into_iter()
            .filter(|(name, _)| !seen_legacy.contains(name)),
    );

    let mut generated = String::from(
        "#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]\n\
         #[non_exhaustive]\n\
         pub enum Code {\n",
    );
    for (_, variant) in &entries {
        writeln!(generated, "    {variant},").expect("write to String");
    }
    generated.push_str(
        "}\n\
         \n\
         impl Code {\n\
             /// Every registry code, preserving the original variant order first.\n\
             pub const ALL: &'static [Self] = &[\n",
    );
    for (_, variant) in &entries {
        writeln!(generated, "        Self::{variant},").expect("write to String");
    }
    generated.push_str(
        "    ];\n\
         \n\
         pub const fn as_str(self) -> &'static str {\n\
             match self {\n",
    );
    for (name, variant) in entries {
        writeln!(generated, "            Self::{variant} => {name:?},").expect("write to String");
    }
    generated.push_str(
        "        }\n\
             }\n\
         \n\
             pub fn from_name(name: &str) -> Option<Self> {\n\
                 Self::ALL\n\
                     .iter()\n\
                     .copied()\n\
                     .find(|code| code.as_str() == name)\n\
             }\n\
         }\n",
    );

    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set")).join("diagnostic_codes.rs");
    fs::write(&output, generated)
        .unwrap_or_else(|error| panic!("could not write {}: {error}", output.display()));
}

fn variant_name(name: &str) -> String {
    let mut variant = String::new();
    for segment in name.split('_') {
        let mut characters = segment.chars();
        if let Some(first) = characters.next() {
            variant.extend(first.to_uppercase());
            variant.extend(characters.flat_map(char::to_lowercase));
        }
    }
    variant
}

fn legacy_order_hash(names: &[&str]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for name in names {
        for byte in name.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}
