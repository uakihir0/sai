//! Derive the local binding introduced by one imported use item.

use sai_hir::item_tree::UseItem;
use sai_intern::{Interner, LookupError, Symbol};
use sai_syntax::identifier_case_kind;

/// The names and case-class relationship for one `UseItem`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UseItemBinding {
    /// The item spelling named by the use declaration.
    pub source_name: Symbol,
    /// The local name introduced by the declaration.
    ///
    /// This is the alias when present and otherwise the source name.
    pub binding_name: Symbol,
    /// Whether an explicit alias uses a different identifier case class than
    /// the source name.
    pub alias_case_mismatch: bool,
}

/// An error symbol, malformed recovered name, or symbol from another interner
/// while deriving a `UseItem` binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UseItemBindingError {
    MissingSourceName,
    MissingAlias,
    MalformedSourceName,
    MalformedAlias,
    SourceSymbol(LookupError),
    AliasSymbol(LookupError),
}

/// Derives the source and local binding names for one HIR use item.
///
/// The helper preserves both symbols as written. It does not reject a case
/// mismatch; callers can report the language diagnostic using
/// `alias_case_mismatch`. HIR normally contains lexer-produced identifier
/// spellings, but this helper also rejects the error sentinel, empty spellings,
/// and the standalone `_` spelling from malformed or recovered input.
pub fn derive_use_item_binding(
    item: &UseItem,
    symbols: &Interner,
) -> Result<UseItemBinding, UseItemBindingError> {
    let source_spelling = resolve_identifier(item.name, symbols).map_err(|error| match error {
        ResolveIdentifierError::Missing => UseItemBindingError::MissingSourceName,
        ResolveIdentifierError::Malformed => UseItemBindingError::MalformedSourceName,
        ResolveIdentifierError::Lookup(error) => UseItemBindingError::SourceSymbol(error),
    })?;

    let Some(alias) = item.alias else {
        return Ok(UseItemBinding {
            source_name: item.name,
            binding_name: item.name,
            alias_case_mismatch: false,
        });
    };

    let alias_spelling = resolve_identifier(alias, symbols).map_err(|error| match error {
        ResolveIdentifierError::Missing => UseItemBindingError::MissingAlias,
        ResolveIdentifierError::Malformed => UseItemBindingError::MalformedAlias,
        ResolveIdentifierError::Lookup(error) => UseItemBindingError::AliasSymbol(error),
    })?;

    Ok(UseItemBinding {
        source_name: item.name,
        binding_name: alias,
        alias_case_mismatch: identifier_case_kind(&source_spelling)
            != identifier_case_kind(&alias_spelling),
    })
}

enum ResolveIdentifierError {
    Missing,
    Malformed,
    Lookup(LookupError),
}

fn resolve_identifier<'a>(
    symbol: Symbol,
    symbols: &'a Interner,
) -> Result<std::sync::Arc<str>, ResolveIdentifierError> {
    if symbol.is_error() {
        return Err(ResolveIdentifierError::Missing);
    }
    let spelling = symbols
        .resolve(symbol)
        .map_err(ResolveIdentifierError::Lookup)?;
    if spelling.is_empty() || spelling.as_ref() == "_" {
        return Err(ResolveIdentifierError::Malformed);
    }
    Ok(spelling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_syntax::SyntaxKind;
    use sai_vfs::{TextRange, TextSize};

    fn item(name: Symbol, alias: Option<Symbol>) -> UseItem {
        UseItem {
            name,
            alias,
            range: TextRange::empty(TextSize::new(0)),
        }
    }

    fn symbol(symbols: &Interner, spelling: &str) -> Symbol {
        symbols.intern(spelling).expect("intern symbol")
    }

    #[test]
    fn preserves_matching_upper_and_lower_aliases() {
        let symbols = Interner::new();
        for (source, alias) in [("Widget", "Widgets"), ("widget", "widgets")] {
            let source = symbol(&symbols, source);
            let alias = symbol(&symbols, alias);
            let binding = derive_use_item_binding(&item(source, Some(alias)), &symbols)
                .expect("valid item binding");

            assert_eq!(binding.source_name, source);
            assert_eq!(binding.binding_name, alias);
            assert!(!binding.alias_case_mismatch);
        }
    }

    #[test]
    fn reports_case_mismatch_in_either_direction() {
        let symbols = Interner::new();
        for (source, alias) in [("Widget", "widget"), ("widget", "Widget")] {
            let binding = derive_use_item_binding(
                &item(symbol(&symbols, source), Some(symbol(&symbols, alias))),
                &symbols,
            )
            .expect("valid item binding");

            assert!(binding.alias_case_mismatch);
        }
    }

    #[test]
    fn no_alias_binds_the_source_name() {
        let symbols = Interner::new();
        let source = symbol(&symbols, "Widget");
        let binding =
            derive_use_item_binding(&item(source, None), &symbols).expect("valid item binding");

        assert_eq!(binding.source_name, source);
        assert_eq!(binding.binding_name, source);
        assert!(!binding.alias_case_mismatch);
    }

    #[test]
    fn ignores_leading_underscores_and_uses_unicode_case_categories() {
        let symbols = Interner::new();
        for (source, alias, mismatch) in [
            ("__Widget", "_Widgets", false),
            ("__widget", "_widgets", false),
            ("__ǅelta", "_Éclair", false),
            ("__ǅelta", "_delta", true),
        ] {
            let binding = derive_use_item_binding(
                &item(symbol(&symbols, source), Some(symbol(&symbols, alias))),
                &symbols,
            )
            .expect("valid item binding");

            assert_eq!(binding.alias_case_mismatch, mismatch, "{source} as {alias}");
        }
        assert_eq!(identifier_case_kind("_ǅelta"), SyntaxKind::UIDENT);
        assert_eq!(identifier_case_kind("__Éclair"), SyntaxKind::UIDENT);
        assert_eq!(identifier_case_kind("__delta"), SyntaxKind::LIDENT);
    }

    #[test]
    fn rejects_error_and_malformed_name_symbols() {
        let symbols = Interner::new();
        assert_eq!(
            derive_use_item_binding(&item(Symbol::ERROR, None), &symbols),
            Err(UseItemBindingError::MissingSourceName)
        );

        let source = symbol(&symbols, "source");
        assert_eq!(
            derive_use_item_binding(&item(source, Some(Symbol::ERROR)), &symbols),
            Err(UseItemBindingError::MissingAlias)
        );
        assert_eq!(
            derive_use_item_binding(&item(symbol(&symbols, ""), None), &symbols),
            Err(UseItemBindingError::MalformedSourceName)
        );
        assert_eq!(
            derive_use_item_binding(&item(source, Some(symbol(&symbols, "_"))), &symbols),
            Err(UseItemBindingError::MalformedAlias)
        );
    }

    #[test]
    fn rejects_symbols_from_a_different_interner() {
        let symbols = Interner::new();
        let other_symbols = Interner::new();
        let source = symbol(&other_symbols, "source");

        assert_eq!(
            derive_use_item_binding(&item(source, None), &symbols),
            Err(UseItemBindingError::SourceSymbol(
                LookupError::UnknownSymbol
            ))
        );
    }
}
