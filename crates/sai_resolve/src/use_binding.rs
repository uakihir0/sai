//! Derive the local binding name introduced by a `use` declaration.
//!
//! Explicit aliases are preserved exactly. Only names derived from a package
//! or module path replace hyphens with underscores.

use super::use_path::{ParsedUsePath, UsePathError, parse_use_path};
use sai_hir::item_tree::UseEntry;
use sai_intern::{InternError, Interner, LookupError, Symbol};

/// Whether a binding name is usable as written or requires an explicit alias.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UseBindingName {
    /// An explicit alias. Its original symbol is returned unchanged.
    ExplicitAlias(Symbol),
    /// A derived name that is not a strict keyword or reserved word.
    Derived(Symbol),
    /// A derived strict keyword or reserved word; the declaration needs `as`.
    RequiresAlias(Symbol),
}

impl UseBindingName {
    pub const fn symbol(self) -> Symbol {
        match self {
            Self::ExplicitAlias(symbol) | Self::Derived(symbol) | Self::RequiresAlias(symbol) => {
                symbol
            }
        }
    }

    pub const fn requires_alias(self) -> bool {
        matches!(self, Self::RequiresAlias(_))
    }
}

/// A malformed path or invalid/session-mismatched symbol while deriving a
/// use binding name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UseBindingNameError {
    Path(UsePathError),
    MissingSymbol,
    Symbol(LookupError),
    Intern(InternError),
    MalformedParsedPath,
}

impl From<UsePathError> for UseBindingNameError {
    fn from(error: UsePathError) -> Self {
        Self::Path(error)
    }
}

impl From<LookupError> for UseBindingNameError {
    fn from(error: LookupError) -> Self {
        Self::Symbol(error)
    }
}

impl From<InternError> for UseBindingNameError {
    fn from(error: InternError) -> Self {
        Self::Intern(error)
    }
}

/// Derives the module-alias binding name from a HIR use entry.
///
/// An explicit alias takes precedence and does not require parsing the package
/// path. If the entry imports items without an alias, it binds only those
/// items and this function returns `None`. Otherwise, the final module segment
/// is used, or the package name when the path names the package root. Hyphens
/// are converted only in these derived names.
pub fn derive_use_binding_name(
    entry: &UseEntry,
    symbols: &Interner,
) -> Result<Option<UseBindingName>, UseBindingNameError> {
    if let Some(alias) = entry.alias {
        resolve_symbol(alias, symbols)?;
        return Ok(Some(UseBindingName::ExplicitAlias(alias)));
    }
    if entry.has_items {
        return Ok(None);
    }

    let parsed = parse_use_path(entry, symbols)?;
    derive_parsed_use_binding_name(&parsed, symbols).map(Some)
}

fn derive_parsed_use_binding_name(
    parsed: &ParsedUsePath,
    symbols: &Interner,
) -> Result<UseBindingName, UseBindingNameError> {
    let spellings = parsed
        .segments
        .iter()
        .copied()
        .map(|segment| resolve_symbol(segment, symbols))
        .collect::<Result<Vec<_>, _>>()?;
    let (source_symbol, spelling) = if let Some(segment) = parsed.segments.last().copied() {
        let spelling = spellings
            .last()
            .ok_or(UseBindingNameError::MalformedParsedPath)?;
        (Some(segment), spelling.to_string())
    } else {
        let (namespace, package_name) = parsed
            .package_identity
            .split_once(':')
            .ok_or(UseBindingNameError::MalformedParsedPath)?;
        if namespace.is_empty()
            || package_name.is_empty()
            || package_name.contains(':')
            || package_name.contains('/')
        {
            return Err(UseBindingNameError::MalformedParsedPath);
        }
        (None, package_name.to_owned())
    };

    let derived_spelling = spelling.replace('-', "_");
    let name = match source_symbol {
        Some(symbol) if derived_spelling == spelling => symbol,
        _ => symbols.intern(&derived_spelling)?,
    };

    if sai_syntax::is_reserved_identifier(&derived_spelling)
        || sai_syntax::is_strict_keyword(&derived_spelling)
    {
        Ok(UseBindingName::RequiresAlias(name))
    } else {
        Ok(UseBindingName::Derived(name))
    }
}

fn resolve_symbol(
    symbol: Symbol,
    symbols: &Interner,
) -> Result<std::sync::Arc<str>, UseBindingNameError> {
    if symbol.is_error() {
        return Err(UseBindingNameError::MissingSymbol);
    }
    symbols.resolve(symbol).map_err(UseBindingNameError::Symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::item_tree::{ErasedAstId, UseItem};

    fn entry(pkg_path: &str, alias: Option<Symbol>) -> UseEntry {
        UseEntry {
            ast_id: ErasedAstId::new(0),
            is_pub: false,
            pkg_path: pkg_path.to_owned(),
            alias,
            has_items: false,
            items: Vec::<UseItem>::new(),
            raw_attrs: Vec::new(),
        }
    }

    fn entry_with_items(pkg_path: &str, alias: Option<Symbol>) -> UseEntry {
        let mut entry = entry(pkg_path, alias);
        entry.has_items = true;
        entry.items.push(UseItem {
            name: Symbol::ERROR,
            alias: None,
            range: sai_vfs::TextRange::empty(sai_vfs::TextSize::new(0)),
        });
        entry
    }

    fn spelling(name: UseBindingName, symbols: &Interner) -> String {
        symbols
            .resolve(name.symbol())
            .expect("binding symbol resolves")
            .to_string()
    }

    #[test]
    fn preserves_an_explicit_alias_without_path_normalization() {
        let symbols = Interner::new();
        let alias = symbols.intern("my-alias").unwrap();

        let result =
            derive_use_binding_name(&entry("acme:bad//path", Some(alias)), &symbols).unwrap();

        let result = result.unwrap();
        assert_eq!(result, UseBindingName::ExplicitAlias(alias));
        assert_eq!(spelling(result, &symbols), "my-alias");
    }

    #[test]
    fn item_only_imports_do_not_bind_the_use_target() {
        let symbols = Interner::new();

        assert_eq!(
            derive_use_binding_name(&entry_with_items("sai:core/option", None), &symbols),
            Ok(None)
        );
        let mut empty_items = entry("sai:core/option", None);
        empty_items.has_items = true;
        assert_eq!(derive_use_binding_name(&empty_items, &symbols), Ok(None));

        let alias = symbols.intern("options").unwrap();
        assert_eq!(
            derive_use_binding_name(&entry_with_items("sai:core/option", Some(alias)), &symbols),
            Ok(Some(UseBindingName::ExplicitAlias(alias)))
        );
    }

    #[test]
    fn maps_hyphens_in_a_root_package_name() {
        let symbols = Interner::new();
        let result = derive_use_binding_name(&entry("acme:user-client", None), &symbols).unwrap();

        let result = result.unwrap();
        assert_eq!(spelling(result, &symbols), "user_client");
        assert_eq!(result, UseBindingName::Derived(result.symbol()));
    }

    #[test]
    fn maps_hyphens_in_the_last_module_segment() {
        let symbols = Interner::new();
        let result =
            derive_use_binding_name(&entry("acme:app/model/user-client", None), &symbols).unwrap();

        let result = result.unwrap();
        assert_eq!(spelling(result, &symbols), "user_client");
        assert!(!result.requires_alias());
    }

    #[test]
    fn derived_strict_keywords_require_an_alias() {
        let symbols = Interner::new();

        for path in ["acme:type", "acme:app/self"] {
            let result = derive_use_binding_name(&entry(path, None), &symbols)
                .unwrap()
                .unwrap();
            assert!(result.requires_alias(), "{path}");
            assert!(matches!(result, UseBindingName::RequiresAlias(_)));
        }
    }

    #[test]
    fn reserved_derived_names_require_an_alias() {
        let symbols = Interner::new();
        let result = derive_use_binding_name(&entry("acme:class", None), &symbols)
            .unwrap()
            .unwrap();

        assert_eq!(result, UseBindingName::RequiresAlias(result.symbol()));
    }

    #[test]
    fn contextual_and_builtin_effect_names_are_allowed() {
        let symbols = Interner::new();
        let result = derive_use_binding_name(&entry("acme:app/fs", None), &symbols)
            .unwrap()
            .unwrap();

        assert_eq!(spelling(result, &symbols), "fs");
        assert!(!result.requires_alias());
    }

    #[test]
    fn malformed_paths_and_error_or_foreign_symbols_are_typed_failures() {
        let symbols = Interner::new();
        assert!(matches!(
            derive_use_binding_name(&entry("acme:app//bad", None), &symbols),
            Err(UseBindingNameError::Path(UsePathError::EmptySegment { .. }))
        ));
        assert_eq!(
            derive_use_binding_name(&entry("acme:app", Some(Symbol::ERROR)), &symbols),
            Err(UseBindingNameError::MissingSymbol)
        );

        let other_symbols = Interner::new();
        let foreign_alias = other_symbols.intern("alias").unwrap();
        assert!(matches!(
            derive_use_binding_name(&entry("acme:app", Some(foreign_alias)), &symbols),
            Err(UseBindingNameError::Symbol(LookupError::UnknownSymbol))
        ));
    }

    #[test]
    fn malformed_parsed_paths_and_unknown_segments_do_not_panic() {
        let symbols = Interner::new();
        let malformed = ParsedUsePath {
            package_identity: "acme".to_owned(),
            segments: Vec::new(),
        };
        assert_eq!(
            derive_parsed_use_binding_name(&malformed, &symbols),
            Err(UseBindingNameError::MalformedParsedPath)
        );

        let other_symbols = Interner::new();
        let foreign_segment = other_symbols.intern("foreign").unwrap();
        let valid_segment = symbols.intern("last").unwrap();
        let unknown_segment = ParsedUsePath {
            package_identity: "acme:app".to_owned(),
            segments: vec![foreign_segment, valid_segment],
        };
        assert!(matches!(
            derive_parsed_use_binding_name(&unknown_segment, &symbols),
            Err(UseBindingNameError::Symbol(LookupError::UnknownSymbol))
        ));

        let error_segment = ParsedUsePath {
            package_identity: "acme:app".to_owned(),
            segments: vec![Symbol::ERROR],
        };
        assert_eq!(
            derive_parsed_use_binding_name(&error_segment, &symbols),
            Err(UseBindingNameError::MissingSymbol)
        );
    }
}
