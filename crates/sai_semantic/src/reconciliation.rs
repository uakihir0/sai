use crate::{
    SymbolAllocationError, SymbolHex, SymbolIdMapping, SymbolIndexCodecError, SymbolIndexEntry,
    SymbolKind, UnvalidatedSymbolIndex, allocate_fresh, canonical_path::is_canonical_symbol_path,
};
use sai_stable_hash::Fingerprint;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One current declaration in a complete package symbol inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurrentSymbol {
    path: Box<str>,
    def_path_hash: Fingerprint,
    kind: SymbolKind,
    fingerprint: Fingerprint,
}

impl CurrentSymbol {
    pub fn new(
        path: impl Into<Box<str>>,
        def_path_hash: Fingerprint,
        kind: SymbolKind,
        fingerprint: Fingerprint,
    ) -> Self {
        Self {
            path: path.into(),
            def_path_hash,
            kind,
            fingerprint,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub const fn def_path_hash(&self) -> Fingerprint {
        self.def_path_hash
    }

    pub const fn kind(&self) -> SymbolKind {
        self.kind
    }

    pub const fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }
}

/// Whether a package inventory contains every declaration that can receive a
/// symbol ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InventoryCoverage {
    Complete,
    Incomplete { reason: String },
}

/// The current semantic symbols for one workspace package.
///
/// The caller must pair `previous_file` with the exact prior package node from
/// the package graph. This type deliberately does not infer package-node
/// correspondence from a package identity when several graph nodes could
/// produce the same index filename.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageSymbolInventory {
    file: crate::PackageIndexFileName,
    package_node_key: String,
    coverage: InventoryCoverage,
    symbols: Vec<CurrentSymbol>,
}

impl PackageSymbolInventory {
    /// Creates an inventory the caller certifies is complete.
    pub fn complete(
        file: crate::PackageIndexFileName,
        package_node_key: impl Into<String>,
        symbols: impl IntoIterator<Item = CurrentSymbol>,
    ) -> Self {
        Self {
            file,
            package_node_key: package_node_key.into(),
            coverage: InventoryCoverage::Complete,
            symbols: symbols.into_iter().collect(),
        }
    }

    /// Creates an inventory that must be rejected by reconciliation.
    pub fn incomplete(
        file: crate::PackageIndexFileName,
        package_node_key: impl Into<String>,
        reason: impl Into<String>,
        symbols: impl IntoIterator<Item = CurrentSymbol>,
    ) -> Self {
        Self {
            file,
            package_node_key: package_node_key.into(),
            coverage: InventoryCoverage::Incomplete {
                reason: reason.into(),
            },
            symbols: symbols.into_iter().collect(),
        }
    }

    pub fn file(&self) -> &crate::PackageIndexFileName {
        &self.file
    }

    pub fn package_node_key(&self) -> &str {
        &self.package_node_key
    }

    pub fn coverage(&self) -> &InventoryCoverage {
        &self.coverage
    }

    pub fn symbols(&self) -> &[CurrentSymbol] {
        &self.symbols
    }
}

/// Reconciles one complete package inventory with its exact prior package
/// entries. The returned entries use current paths, `DefPathHash` values,
/// kinds, and declaration fingerprints.
///
/// `previous_file` must identify the package node selected by the package
/// graph. When its compatibility line differs from the current line,
/// canonical paths replace `DefPathHash` as the primary key. The previous
/// entries must come from a committed index; this function performs no I/O
/// and does not promote decoded, unvalidated bytes into a trusted index.
pub fn reconcile_package(
    previous_file: Option<&crate::PackageIndexFileName>,
    previous_entries: &[SymbolIndexEntry],
    current: &PackageSymbolInventory,
    recorded_changes: &[SymbolIdMapping],
) -> Result<Vec<SymbolIndexEntry>, SymbolIndexReconciliationError> {
    if let InventoryCoverage::Incomplete { reason } = &current.coverage {
        return Err(SymbolIndexReconciliationError::IncompleteInventory(
            reason.clone(),
        ));
    }
    if current.package_node_key.is_empty() {
        return Err(SymbolIndexReconciliationError::EmptyPackageNodeKey);
    }
    if crate::index_journal::package_index_file_for_node_key(&current.package_node_key).as_ref()
        != Some(&current.file)
    {
        return Err(SymbolIndexReconciliationError::PackageNodeKeyMismatch);
    }
    let package_id = current.file.package_id();

    match previous_file {
        Some(previous_file) if previous_file.package_id() != package_id => {
            return Err(SymbolIndexReconciliationError::PreviousPackageMismatch);
        }
        None if !previous_entries.is_empty() => {
            return Err(SymbolIndexReconciliationError::PreviousFileRequired);
        }
        _ => {}
    }

    let mut previous_paths = BTreeMap::new();
    let mut previous_hashes = BTreeMap::new();
    let mut previous_hexes = BTreeMap::new();
    for (index, entry) in previous_entries.iter().enumerate() {
        if !is_canonical_symbol_path(entry.path()) {
            return Err(SymbolIndexReconciliationError::InvalidPreviousIndex(
                SymbolIndexCodecError::InvalidSymbolPath,
            ));
        }
        if !path_belongs_to_package(entry.path(), &package_id) {
            return Err(SymbolIndexReconciliationError::InvalidPreviousIndex(
                SymbolIndexCodecError::SymbolPathOutsidePackage,
            ));
        }
        if previous_paths.insert(entry.path(), index).is_some() {
            return Err(SymbolIndexReconciliationError::InvalidPreviousIndex(
                SymbolIndexCodecError::DuplicateSymbolPath,
            ));
        }
        if previous_hashes
            .insert(entry.def_path_hash(), index)
            .is_some()
        {
            return Err(SymbolIndexReconciliationError::InvalidPreviousIndex(
                SymbolIndexCodecError::DuplicateDefPathHash,
            ));
        }
        if previous_hexes.insert(entry.hex(), index).is_some() {
            return Err(SymbolIndexReconciliationError::InvalidPreviousIndex(
                SymbolIndexCodecError::DuplicateSymbolHex,
            ));
        }
    }

    let mut current_indices = (0..current.symbols.len()).collect::<Vec<_>>();
    current_indices.sort_by(|left, right| {
        current.symbols[*left]
            .path
            .as_bytes()
            .cmp(current.symbols[*right].path.as_bytes())
    });
    let mut current_paths = BTreeMap::new();
    let mut current_hashes = BTreeMap::new();
    for index in current_indices.iter().copied() {
        let symbol = &current.symbols[index];
        if !is_canonical_symbol_path(&symbol.path) {
            return Err(SymbolIndexReconciliationError::InvalidCurrentPath);
        }
        if !path_belongs_to_package(&symbol.path, &package_id) {
            return Err(SymbolIndexReconciliationError::CurrentSymbolOutsidePackage);
        }
        if current_paths.insert(symbol.path.as_ref(), index).is_some() {
            return Err(SymbolIndexReconciliationError::DuplicateCurrentPath);
        }
        if current_hashes.insert(symbol.def_path_hash, index).is_some() {
            return Err(SymbolIndexReconciliationError::DuplicateCurrentDefPathHash);
        }
    }

    let compatibility_line_changed = previous_file.is_some_and(|previous_file| {
        previous_file.compatibility_line() != current.file.compatibility_line()
    });
    let mut assigned_hex = vec![None; current.symbols.len()];
    let mut matched_previous = vec![None; current.symbols.len()];
    let mut deleted_previous = vec![false; previous_entries.len()];

    // Apply the transaction's explicit identities first. A deletion followed
    // by a declaration at the same path must not let the old DefPathHash claim
    // the newly created symbol before its recorded ID is applied.
    apply_recorded_changes(
        previous_entries,
        current,
        recorded_changes,
        &current_paths,
        &previous_hexes,
        &mut assigned_hex,
        &mut matched_previous,
        &mut deleted_previous,
    )?;

    if !previous_entries.is_empty() {
        for current_index in current_indices.iter().copied() {
            if assigned_hex[current_index].is_some() {
                continue;
            }
            let symbol = &current.symbols[current_index];
            let previous_index = if compatibility_line_changed {
                previous_paths.get(symbol.path()).copied()
            } else {
                previous_hashes.get(&symbol.def_path_hash).copied()
            };
            if let Some(previous_index) = previous_index {
                if deleted_previous[previous_index]
                    || matched_previous
                        .iter()
                        .any(|matched| *matched == Some(previous_index))
                {
                    continue;
                }
                assign_previous(
                    current_index,
                    previous_index,
                    previous_entries,
                    &mut assigned_hex,
                    &mut matched_previous,
                    &deleted_previous,
                )?;
            }
        }
    }

    follow_matched_parents(
        previous_entries,
        current,
        &current_paths,
        &mut assigned_hex,
        &mut matched_previous,
        &deleted_previous,
    )?;

    loop {
        let mut current_signatures = BTreeMap::new();
        for current_index in current_indices.iter().copied() {
            if assigned_hex[current_index].is_some() {
                continue;
            }
            let symbol = &current.symbols[current_index];
            *current_signatures
                .entry((symbol.kind, symbol.fingerprint))
                .or_insert(0_usize) += 1;
        }

        let mut used_previous = vec![false; previous_entries.len()];
        for previous_index in matched_previous.iter().flatten().copied() {
            used_previous[previous_index] = true;
        }
        let mut previous_signatures = BTreeMap::new();
        for (previous_index, previous) in previous_entries.iter().enumerate() {
            if deleted_previous[previous_index] || used_previous[previous_index] {
                continue;
            }
            let count_and_index = previous_signatures
                .entry((previous.kind(), previous.fingerprint()))
                .or_insert((0_usize, None));
            count_and_index.0 += 1;
            count_and_index.1 = Some(previous_index);
        }

        let next_match = current_indices.iter().copied().find_map(|current_index| {
            if assigned_hex[current_index].is_some() {
                return None;
            }
            let symbol = &current.symbols[current_index];
            let signature = (symbol.kind, symbol.fingerprint);
            if current_signatures.get(&signature) != Some(&1) {
                return None;
            }
            let (previous_count, previous_index) = previous_signatures.get(&signature).copied()?;
            if previous_count == 1 {
                previous_index.map(|previous_index| (current_index, previous_index))
            } else {
                None
            }
        });
        let Some((current_index, previous_index)) = next_match else {
            break;
        };

        assign_previous(
            current_index,
            previous_index,
            previous_entries,
            &mut assigned_hex,
            &mut matched_previous,
            &deleted_previous,
        )?;
        follow_matched_parents(
            previous_entries,
            current,
            &current_paths,
            &mut assigned_hex,
            &mut matched_previous,
            &deleted_previous,
        )?;
    }

    let reserved = previous_entries
        .iter()
        .map(SymbolIndexEntry::hex)
        .chain(assigned_hex.iter().flatten().copied())
        .chain(
            recorded_changes
                .iter()
                .filter(|change| change.package_node_key == current.package_node_key)
                .map(|change| change.hex),
        )
        .collect::<BTreeSet<_>>();
    let fresh_paths = current_indices
        .iter()
        .filter(|index| assigned_hex[**index].is_none())
        .map(|index| current.symbols[*index].path.as_ref())
        .collect::<Vec<_>>();
    let fresh = allocate_fresh(&package_id, fresh_paths, &reserved)
        .map_err(SymbolIndexReconciliationError::Allocation)?;

    let mut entries = Vec::with_capacity(current.symbols.len());
    for current_index in current_indices {
        let symbol = &current.symbols[current_index];
        let hex = assigned_hex[current_index].unwrap_or_else(|| {
            *fresh
                .get(symbol.path())
                .expect("every unmatched current symbol was allocated")
        });
        entries.push(
            SymbolIndexEntry::new(
                hex,
                symbol.def_path_hash,
                symbol.path.clone(),
                symbol.kind,
                symbol.fingerprint,
            )
            .map_err(SymbolIndexReconciliationError::InvalidReconciledIndex)?,
        );
    }

    UnvalidatedSymbolIndex::new([(current.file.clone(), entries.clone())])
        .map_err(SymbolIndexReconciliationError::InvalidReconciledIndex)?;
    Ok(entries)
}

fn assign_previous(
    current_index: usize,
    previous_index: usize,
    previous_entries: &[SymbolIndexEntry],
    assigned_hex: &mut [Option<SymbolHex>],
    matched_previous: &mut [Option<usize>],
    deleted_previous: &[bool],
) -> Result<(), SymbolIndexReconciliationError> {
    if deleted_previous[previous_index] {
        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
    }
    if let Some(existing) = matched_previous[current_index]
        && existing != previous_index
    {
        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
    }
    if let Some(existing) = assigned_hex[current_index]
        && existing != previous_entries[previous_index].hex()
    {
        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
    }
    if matched_previous
        .iter()
        .enumerate()
        .any(|(candidate_index, matched)| {
            candidate_index != current_index && *matched == Some(previous_index)
        })
    {
        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
    }
    if assigned_hex
        .iter()
        .enumerate()
        .any(|(candidate_index, assigned)| {
            candidate_index != current_index
                && *assigned == Some(previous_entries[previous_index].hex())
        })
    {
        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
    }

    assigned_hex[current_index] = Some(previous_entries[previous_index].hex());
    matched_previous[current_index] = Some(previous_index);
    Ok(())
}

fn apply_recorded_changes(
    previous_entries: &[SymbolIndexEntry],
    current: &PackageSymbolInventory,
    recorded_changes: &[SymbolIdMapping],
    current_paths: &BTreeMap<&str, usize>,
    previous_hexes: &BTreeMap<SymbolHex, usize>,
    assigned_hex: &mut [Option<SymbolHex>],
    matched_previous: &mut [Option<usize>],
    deleted_previous: &mut [bool],
) -> Result<(), SymbolIndexReconciliationError> {
    let package_id = current.file.package_id();
    let mut seen_hexes = BTreeSet::new();
    let mut seen_targets = BTreeSet::new();
    for change in recorded_changes
        .iter()
        .filter(|change| change.package_node_key == current.package_node_key)
    {
        if change.before_path.is_none() && change.after_path.is_none()
            || change.before_path == change.after_path
            || !seen_hexes.insert(change.hex)
        {
            return Err(SymbolIndexReconciliationError::InvalidRecordedChange);
        }
        for path in change.before_path.iter().chain(change.after_path.iter()) {
            if !is_canonical_symbol_path(path) || !path_belongs_to_package(path, &package_id) {
                return Err(SymbolIndexReconciliationError::InvalidRecordedChange);
            }
        }

        let previous_index = previous_hexes.get(&change.hex).copied();
        if let Some(before_path) = &change.before_path {
            let Some(previous_index) = previous_index else {
                // A retry can observe the already-committed after generation.
                if change.after_path.is_none() {
                    // A deleted symbol is absent from both the committed
                    // package entries and the current inventory on retry.
                    continue;
                }
                return Err(SymbolIndexReconciliationError::RecordedChangeDoesNotMatchIndex);
            };
            if previous_entries[previous_index].path() != before_path {
                // A retry can observe the already-committed after generation.
                if let Some(after_path) = &change.after_path
                    && previous_entries[previous_index].path() == after_path
                {
                    let Some(current_index) = current_paths.get(after_path.as_str()).copied()
                    else {
                        return Err(SymbolIndexReconciliationError::RecordedTargetMissing);
                    };
                    if !seen_targets.insert(current_index) {
                        return Err(SymbolIndexReconciliationError::InvalidRecordedChange);
                    }
                    assign_previous(
                        current_index,
                        previous_index,
                        previous_entries,
                        assigned_hex,
                        matched_previous,
                        deleted_previous,
                    )?;
                    continue;
                }
                return Err(SymbolIndexReconciliationError::RecordedChangeDoesNotMatchIndex);
            }
        }

        match &change.after_path {
            Some(after_path) => {
                let Some(current_index) = current_paths.get(after_path.as_str()).copied() else {
                    return Err(SymbolIndexReconciliationError::RecordedTargetMissing);
                };
                if !seen_targets.insert(current_index) {
                    return Err(SymbolIndexReconciliationError::InvalidRecordedChange);
                }

                if let Some(previous_index) = previous_index {
                    if change.before_path.is_none() {
                        if previous_entries[previous_index].path() == after_path {
                            assign_previous(
                                current_index,
                                previous_index,
                                previous_entries,
                                assigned_hex,
                                matched_previous,
                                deleted_previous,
                            )?;
                            continue;
                        }
                        return Err(
                            SymbolIndexReconciliationError::RecordedChangeDoesNotMatchIndex,
                        );
                    }
                    if deleted_previous[previous_index] {
                        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
                    }
                    assign_previous(
                        current_index,
                        previous_index,
                        previous_entries,
                        assigned_hex,
                        matched_previous,
                        deleted_previous,
                    )?;
                } else {
                    if assigned_hex.iter().enumerate().any(|(index, assigned)| {
                        index != current_index && *assigned == Some(change.hex)
                    }) {
                        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
                    }
                    if assigned_hex[current_index].is_some_and(|hex| hex != change.hex) {
                        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
                    }
                    assigned_hex[current_index] = Some(change.hex);
                }
            }
            None => {
                if let Some(previous_index) = previous_index {
                    if previous_entries[previous_index].path()
                        != change.before_path.as_deref().unwrap_or_default()
                        || matched_previous
                            .iter()
                            .any(|matched| *matched == Some(previous_index))
                    {
                        return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
                    }
                    deleted_previous[previous_index] = true;
                }
                // When the old entry is already absent, this is an idempotent
                // replay of a completed deletion.
            }
        }
    }
    Ok(())
}

fn follow_matched_parents(
    previous_entries: &[SymbolIndexEntry],
    current: &PackageSymbolInventory,
    current_paths: &BTreeMap<&str, usize>,
    assigned_hex: &mut [Option<SymbolHex>],
    matched_previous: &mut [Option<usize>],
    deleted_previous: &[bool],
) -> Result<(), SymbolIndexReconciliationError> {
    loop {
        let parents = matched_previous
            .iter()
            .enumerate()
            .filter_map(|(current_index, previous_index)| {
                previous_index.map(|previous_index| (current_index, previous_index))
            })
            .collect::<Vec<_>>();
        let mut matched_member = false;

        for (current_index, previous_index) in parents {
            let old_parent_path = previous_entries[previous_index].path();
            let new_parent_path = current.symbols[current_index].path();
            if old_parent_path == new_parent_path {
                continue;
            }

            let old_prefix = format!("{old_parent_path}.");
            for (old_member_index, old_member) in previous_entries.iter().enumerate() {
                if deleted_previous[old_member_index]
                    || matched_previous
                        .iter()
                        .any(|matched| *matched == Some(old_member_index))
                {
                    continue;
                }
                let Some(suffix) = old_member.path().strip_prefix(&old_prefix) else {
                    continue;
                };
                let new_member_path = format!("{new_parent_path}.{suffix}");
                let Some(new_member_index) = current_paths.get(new_member_path.as_str()).copied()
                else {
                    continue;
                };
                if matched_previous[new_member_index].is_some()
                    || assigned_hex[new_member_index].is_some()
                {
                    return Err(SymbolIndexReconciliationError::ConflictingSymbolMatch);
                }

                assign_previous(
                    new_member_index,
                    old_member_index,
                    previous_entries,
                    assigned_hex,
                    matched_previous,
                    deleted_previous,
                )?;
                matched_member = true;
            }
        }

        if !matched_member {
            return Ok(());
        }
    }
}

fn path_belongs_to_package(path: &str, package_id: &str) -> bool {
    path.strip_prefix(package_id)
        .and_then(|suffix| suffix.as_bytes().first())
        .is_some_and(|separator| matches!(separator, b'.' | b'/'))
}

/// Fail-closed conditions for package symbol reconciliation.
#[derive(Debug)]
pub enum SymbolIndexReconciliationError {
    IncompleteInventory(String),
    EmptyPackageNodeKey,
    PackageNodeKeyMismatch,
    PreviousFileRequired,
    PreviousPackageMismatch,
    InvalidCurrentPath,
    CurrentSymbolOutsidePackage,
    DuplicateCurrentPath,
    DuplicateCurrentDefPathHash,
    InvalidPreviousIndex(SymbolIndexCodecError),
    InvalidRecordedChange,
    RecordedChangeDoesNotMatchIndex,
    RecordedTargetMissing,
    ConflictingSymbolMatch,
    Allocation(SymbolAllocationError),
    InvalidReconciledIndex(SymbolIndexCodecError),
}

impl fmt::Display for SymbolIndexReconciliationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteInventory(reason) => {
                write!(
                    formatter,
                    "package symbol inventory is incomplete: {reason}"
                )
            }
            Self::EmptyPackageNodeKey => formatter.write_str("package node key is empty"),
            Self::PackageNodeKeyMismatch => {
                formatter.write_str("package node key does not identify its index file")
            }
            Self::PreviousFileRequired => {
                formatter.write_str("previous package file is required for prior entries")
            }
            Self::PreviousPackageMismatch => {
                formatter.write_str("previous and current package identities differ")
            }
            Self::InvalidCurrentPath => {
                formatter.write_str("current inventory contains an invalid canonical symbol path")
            }
            Self::CurrentSymbolOutsidePackage => {
                formatter.write_str("current symbol path is outside its package")
            }
            Self::DuplicateCurrentPath => {
                formatter.write_str("current inventory contains a duplicate symbol path")
            }
            Self::DuplicateCurrentDefPathHash => {
                formatter.write_str("current inventory contains a duplicate DefPathHash")
            }
            Self::InvalidPreviousIndex(error) => {
                write!(formatter, "previous package index is invalid: {error}")
            }
            Self::InvalidRecordedChange => {
                formatter.write_str("recorded symbol change is invalid or duplicated")
            }
            Self::RecordedChangeDoesNotMatchIndex => {
                formatter.write_str("recorded symbol change does not match the previous index")
            }
            Self::RecordedTargetMissing => {
                formatter.write_str("recorded symbol change target is absent from the inventory")
            }
            Self::ConflictingSymbolMatch => {
                formatter.write_str("symbol reconciliation produced conflicting identities")
            }
            Self::Allocation(error) => write!(formatter, "symbol ID allocation failed: {error}"),
            Self::InvalidReconciledIndex(error) => {
                write!(formatter, "reconciled package index is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for SymbolIndexReconciliationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PackageIndexFileName, SymbolIndexEntry};

    const PACKAGE_NODE_KEY: &str = "package:acme:user@0.1.0";

    fn file(line: &str) -> PackageIndexFileName {
        PackageIndexFileName::new("acme", "user", line).unwrap()
    }

    fn current(
        path: &str,
        def_path_hash: u128,
        kind: SymbolKind,
        fingerprint: u128,
    ) -> CurrentSymbol {
        CurrentSymbol::new(
            path,
            Fingerprint::new(def_path_hash),
            kind,
            Fingerprint::new(fingerprint),
        )
    }

    fn previous(
        hex: &str,
        path: &str,
        def_path_hash: u128,
        kind: SymbolKind,
        fingerprint: u128,
    ) -> SymbolIndexEntry {
        SymbolIndexEntry::new(
            SymbolHex::parse(hex).unwrap(),
            Fingerprint::new(def_path_hash),
            path,
            kind,
            Fingerprint::new(fingerprint),
        )
        .unwrap()
    }

    fn reconcile(
        previous_file: Option<&PackageIndexFileName>,
        previous_entries: &[SymbolIndexEntry],
        symbols: impl IntoIterator<Item = CurrentSymbol>,
        mappings: &[SymbolIdMapping],
    ) -> Result<Vec<SymbolIndexEntry>, SymbolIndexReconciliationError> {
        reconcile_package(
            previous_file,
            previous_entries,
            &PackageSymbolInventory::complete(file("0.1"), PACKAGE_NODE_KEY, symbols),
            mappings,
        )
    }

    fn by_path<'a>(entries: &'a [SymbolIndexEntry], path: &str) -> &'a SymbolIndexEntry {
        entries.iter().find(|entry| entry.path() == path).unwrap()
    }

    #[test]
    fn preserves_ids_by_def_path_hash_and_refreshes_current_metadata() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/old.Item",
            1,
            SymbolKind::Record,
            2,
        )];
        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/new.Item", 1, SymbolKind::Alias, 3)],
            &[],
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hex(), SymbolHex::parse("123456").unwrap());
        assert_eq!(result[0].path(), "acme:user/new.Item");
        assert_eq!(result[0].kind(), SymbolKind::Alias);
        assert_eq!(result[0].fingerprint(), Fingerprint::new(3));
    }

    #[test]
    fn compatibility_line_change_matches_by_canonical_path() {
        let old_file = file("0.1");
        let new_file = file("0.2");
        let old = [previous(
            "123456",
            "acme:user/model.Item",
            1,
            SymbolKind::Record,
            2,
        )];
        let inventory = PackageSymbolInventory::complete(
            new_file,
            "package:acme:user@0.2.0",
            [current("acme:user/model.Item", 99, SymbolKind::Record, 2)],
        );
        let result = reconcile_package(Some(&old_file), &old, &inventory, &[]).unwrap();

        assert_eq!(result[0].hex(), SymbolHex::parse("123456").unwrap());
        assert_eq!(result[0].def_path_hash(), Fingerprint::new(99));
    }

    #[test]
    fn recorded_rename_preserves_id_and_replay_is_idempotent() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.Old",
            1,
            SymbolKind::Record,
            2,
        )];
        let current = [current("acme:user/model.New", 99, SymbolKind::Record, 20)];
        let changes = [SymbolIdMapping {
            package_node_key: PACKAGE_NODE_KEY.into(),
            hex: SymbolHex::parse("123456").unwrap(),
            before_path: Some("acme:user/model.Old".into()),
            after_path: Some("acme:user/model.New".into()),
        }];

        let result = reconcile(Some(&old_file), &old, current.clone(), &changes).unwrap();
        assert_eq!(
            by_path(&result, "acme:user/model.New").hex(),
            SymbolHex::parse("123456").unwrap()
        );
        let replayed = reconcile(Some(&old_file), &result, current, &changes).unwrap();
        assert_eq!(replayed, result);
    }

    #[test]
    fn recorded_rename_does_not_reuse_the_old_id_for_a_replacement_at_the_old_path() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.Old",
            1,
            SymbolKind::Record,
            2,
        )];
        let current = [
            current("acme:user/model.Old", 1, SymbolKind::Record, 99),
            current("acme:user/model.New", 99, SymbolKind::Record, 2),
        ];
        let changes = [SymbolIdMapping {
            package_node_key: PACKAGE_NODE_KEY.into(),
            hex: SymbolHex::parse("123456").unwrap(),
            before_path: Some("acme:user/model.Old".into()),
            after_path: Some("acme:user/model.New".into()),
        }];

        let result = reconcile(Some(&old_file), &old, current, &changes).unwrap();

        assert_eq!(
            by_path(&result, "acme:user/model.New").hex(),
            SymbolHex::parse("123456").unwrap()
        );
        assert_ne!(
            by_path(&result, "acme:user/model.Old").hex(),
            SymbolHex::parse("123456").unwrap()
        );
    }

    #[test]
    fn parent_rename_rekeys_nested_members_at_component_boundaries() {
        let old_file = file("0.1");
        let old = [
            previous("000001", "acme:user/model.Old", 1, SymbolKind::Record, 1),
            previous(
                "000002",
                "acme:user/model.Old.field[value]",
                2,
                SymbolKind::Field,
                2,
            ),
            previous(
                "000003",
                "acme:user/model.Old.Nested.field[name]",
                3,
                SymbolKind::Field,
                3,
            ),
            previous(
                "000004",
                "acme:user/model.Oldish.field[value]",
                4,
                SymbolKind::Field,
                4,
            ),
        ];
        let current = [
            current("acme:user/model.New", 10, SymbolKind::Record, 10),
            current(
                "acme:user/model.New.field[value]",
                20,
                SymbolKind::Field,
                20,
            ),
            current(
                "acme:user/model.New.Nested.field[name]",
                30,
                SymbolKind::Field,
                30,
            ),
            current(
                "acme:user/model.Oldish.field[value]",
                4,
                SymbolKind::Field,
                4,
            ),
        ];
        let changes = [SymbolIdMapping {
            package_node_key: PACKAGE_NODE_KEY.into(),
            hex: SymbolHex::parse("000001").unwrap(),
            before_path: Some("acme:user/model.Old".into()),
            after_path: Some("acme:user/model.New".into()),
        }];
        let result = reconcile(Some(&old_file), &old, current, &changes).unwrap();

        assert_eq!(
            by_path(&result, "acme:user/model.New.field[value]").hex(),
            SymbolHex::parse("000002").unwrap()
        );
        assert_eq!(
            by_path(&result, "acme:user/model.New.Nested.field[name]").hex(),
            SymbolHex::parse("000003").unwrap()
        );
        assert_eq!(
            by_path(&result, "acme:user/model.Oldish.field[value]").hex(),
            SymbolHex::parse("000004").unwrap()
        );
    }

    #[test]
    fn unique_kind_and_fingerprint_rematches_a_text_editor_rename() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.Old",
            1,
            SymbolKind::Fn,
            2,
        )];
        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/model.New", 3, SymbolKind::Fn, 2)],
            &[],
        )
        .unwrap();

        assert_eq!(result[0].hex(), SymbolHex::parse("123456").unwrap());
    }

    #[test]
    fn changed_declaration_body_does_not_rematch_by_path_alone() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.old_name",
            1,
            SymbolKind::Fn,
            2,
        )];
        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/model.new_name", 3, SymbolKind::Fn, 99)],
            &[],
        )
        .unwrap();

        assert_ne!(result[0].hex(), SymbolHex::parse("123456").unwrap());
        assert_eq!(result[0].path(), "acme:user/model.new_name");
    }

    #[test]
    fn ambiguous_fingerprints_are_allocated_fresh() {
        let old_file = file("0.1");
        let old = [
            previous("000001", "acme:user/model.First", 1, SymbolKind::Fn, 9),
            previous("000002", "acme:user/model.Second", 2, SymbolKind::Fn, 9),
        ];
        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/model.Renamed", 3, SymbolKind::Fn, 9)],
            &[],
        )
        .unwrap();

        assert_ne!(result[0].hex(), SymbolHex::parse("000001").unwrap());
        assert_ne!(result[0].hex(), SymbolHex::parse("000002").unwrap());
    }

    #[test]
    fn deletion_ids_remain_reserved_during_fresh_allocation() {
        let old_file = file("0.1");
        let fresh_without_reservation =
            crate::allocate_fresh("acme:user", ["acme:user/model.New"], &BTreeSet::new()).unwrap()
                ["acme:user/model.New"];
        let old = [SymbolIndexEntry::new(
            fresh_without_reservation,
            Fingerprint::new(1),
            "acme:user/model.Deleted",
            SymbolKind::Fn,
            Fingerprint::new(2),
        )
        .unwrap()];
        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/model.New", 3, SymbolKind::Fn, 4)],
            &[],
        )
        .unwrap();

        assert_ne!(result[0].hex(), fresh_without_reservation);
    }

    #[test]
    fn rejects_incomplete_inventory_without_reconciling_entries() {
        let inventory = PackageSymbolInventory::incomplete(
            file("0.1"),
            PACKAGE_NODE_KEY,
            "a declaration failed to lower",
            [],
        );

        assert!(matches!(
            reconcile_package(None, &[], &inventory, &[]),
            Err(SymbolIndexReconciliationError::IncompleteInventory(reason))
                if reason == "a declaration failed to lower"
        ));
    }

    #[test]
    fn empty_complete_inventory_removes_every_prior_symbol() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.Item",
            1,
            SymbolKind::Fn,
            2,
        )];

        let result = reconcile(Some(&old_file), &old, [], &[]).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn rejects_ambiguous_or_invalid_package_inputs() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/old.Item",
            1,
            SymbolKind::Fn,
            2,
        )];
        let mismatched = PackageSymbolInventory::complete(
            PackageIndexFileName::new("other", "package", "0.1").unwrap(),
            "package:other:package@0.1.0",
            [],
        );
        assert!(matches!(
            reconcile_package(Some(&old_file), &old, &mismatched, &[]),
            Err(SymbolIndexReconciliationError::PreviousPackageMismatch)
        ));

        let invalid = PackageSymbolInventory::complete(
            file("0.1"),
            PACKAGE_NODE_KEY,
            [current("acme:userish.Item", 3, SymbolKind::Fn, 2)],
        );
        assert!(matches!(
            reconcile_package(None, &[], &invalid, &[]),
            Err(SymbolIndexReconciliationError::CurrentSymbolOutsidePackage)
        ));
    }

    #[test]
    fn recorded_creation_and_deletion_are_applied_by_package_node_key() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.Removed",
            1,
            SymbolKind::Fn,
            2,
        )];
        let changes = [
            SymbolIdMapping {
                package_node_key: PACKAGE_NODE_KEY.into(),
                hex: SymbolHex::parse("123456").unwrap(),
                before_path: Some("acme:user/model.Removed".into()),
                after_path: None,
            },
            SymbolIdMapping {
                package_node_key: PACKAGE_NODE_KEY.into(),
                hex: SymbolHex::parse("ABCDEF").unwrap(),
                before_path: None,
                after_path: Some("acme:user/model.Created".into()),
            },
        ];
        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/model.Created", 3, SymbolKind::Fn, 4)],
            &changes,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hex(), SymbolHex::parse("ABCDEF").unwrap());
        let replayed = reconcile(
            Some(&old_file),
            &result,
            [current("acme:user/model.Created", 3, SymbolKind::Fn, 4)],
            &changes,
        )
        .unwrap();
        assert_eq!(replayed, result);
    }

    #[test]
    fn recorded_replacement_at_the_same_path_uses_the_new_symbol_id() {
        let old_file = file("0.1");
        let old = [previous(
            "123456",
            "acme:user/model.Item",
            1,
            SymbolKind::Fn,
            2,
        )];
        let changes = [
            SymbolIdMapping {
                package_node_key: PACKAGE_NODE_KEY.into(),
                hex: SymbolHex::parse("123456").unwrap(),
                before_path: Some("acme:user/model.Item".into()),
                after_path: None,
            },
            SymbolIdMapping {
                package_node_key: PACKAGE_NODE_KEY.into(),
                hex: SymbolHex::parse("ABCDEF").unwrap(),
                before_path: None,
                after_path: Some("acme:user/model.Item".into()),
            },
        ];

        let result = reconcile(
            Some(&old_file),
            &old,
            [current("acme:user/model.Item", 1, SymbolKind::Fn, 99)],
            &changes,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hex(), SymbolHex::parse("ABCDEF").unwrap());
    }

    #[test]
    fn reconciliation_is_independent_of_inventory_iteration_order() {
        let first = current("acme:user/model.First", 1, SymbolKind::Fn, 2);
        let second = current("acme:user/model.Second", 3, SymbolKind::Record, 4);
        let forward = reconcile(None, &[], [first.clone(), second.clone()], &[]).unwrap();
        let reverse = reconcile(None, &[], [second, first], &[]).unwrap();

        assert_eq!(forward, reverse);
    }
}
