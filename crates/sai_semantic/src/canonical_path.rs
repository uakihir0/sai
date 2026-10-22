use unicode_ident::{is_xid_continue, is_xid_start};
use unicode_normalization::is_nfc;

/// Checks the structural grammar of a canonical symbol path.
///
/// This parser deliberately stops short of resolving paths or interpreting
/// canonical types. Those checks require the analyzed package inventory.
pub(crate) fn is_canonical_symbol_path(path: &str) -> bool {
    let mut pending = vec![(path, true)];
    while let Some((path, requires_symbol)) = pending.pop() {
        if !parse_path(path, requires_symbol, &mut pending) {
            return false;
        }
    }
    true
}

fn parse_path<'a>(
    path: &'a str,
    requires_symbol: bool,
    pending: &mut Vec<(&'a str, bool)>,
) -> bool {
    let Some((namespace, remainder)) = path.split_once(':') else {
        return false;
    };
    if !is_package_segment(namespace) {
        return false;
    }

    let package_len = remainder.find(['/', '.']).unwrap_or(remainder.len());
    let package = &remainder[..package_len];
    if !is_package_segment(package) {
        return false;
    }
    let package_id = &path[..namespace.len() + 1 + package.len()];
    let mut remainder = &remainder[package_len..];

    if remainder.starts_with('/') {
        let module_end = remainder.find('.').unwrap_or(remainder.len());
        let module_path = &remainder[..module_end];
        if !is_module_path(module_path) {
            return false;
        }
        remainder = &remainder[module_end..];
    }

    if remainder.is_empty() {
        return !requires_symbol;
    }

    let mut component_count = 0;
    while !remainder.is_empty() {
        let Some(component) = remainder.strip_prefix('.') else {
            return false;
        };
        if component.is_empty() {
            return false;
        }

        if let Some((payload, tail)) = read_tag(component, "field") {
            if payload.is_empty() || !(is_canonical_name(payload) || is_canonical_decimal(payload))
            {
                return false;
            }
            remainder = tail;
        } else if let Some((payload, tail)) = read_tag(component, "generic") {
            if !is_canonical_name(payload) {
                return false;
            }
            remainder = tail;
        } else if let Some((payload, tail)) = read_tag(component, "test") {
            if !is_canonical_test_string(payload) {
                return false;
            }
            remainder = tail;
        } else if let Some((payload, tail)) = read_tag(component, "property") {
            if !is_canonical_test_string(payload) {
                return false;
            }
            remainder = tail;
        } else if let Some((payload, tail)) = read_tag(component, "impl") {
            if payload.is_empty() {
                return false;
            }
            match split_impl_header(payload) {
                Some((trait_type, self_type)) => {
                    if !is_canonical_type_shape(trait_type) || !is_canonical_type_shape(self_type) {
                        return false;
                    }
                }
                // Interface impl headers contain a canonical interface path,
                // which may stop at a package/module path before WIT inventory
                // resolution identifies the interface.
                None => pending.push((payload, false)),
            }
            remainder = tail;
        } else {
            let Some(name_len) = canonical_name_prefix_len(component) else {
                return false;
            };
            remainder = &component[name_len..];
        }

        if !remainder.is_empty() && !remainder.starts_with('.') {
            return false;
        }
        component_count += 1;
    }

    // A canonical symbol path has at least one item or member component.
    // Package and module paths alone identify packages and modules.
    (component_count > 0 || !requires_symbol) && path.starts_with(package_id)
}

fn is_package_segment(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return false;
    }

    let mut previous_was_hyphen = false;
    for (index, byte) in bytes.iter().copied().enumerate().skip(1) {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_was_hyphen = false;
        } else if byte == b'-' && !previous_was_hyphen && index + 1 < bytes.len() {
            previous_was_hyphen = true;
        } else {
            return false;
        }
    }
    true
}

fn is_module_path(value: &str) -> bool {
    if !value.starts_with('/') {
        return false;
    }
    let mut segments = value[1..].split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    let mut segment_count = 1;
    let mut sai_module_path = is_sai_module_segment(first);
    for segment in segments {
        segment_count += 1;
        sai_module_path &= is_sai_module_segment(segment);
    }
    let wit_interface_path = segment_count == 1 && is_wit_interface_segment(first);
    sai_module_path || wit_interface_path
}

fn is_sai_module_segment(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_lowercase() {
        return false;
    }

    for byte in bytes.iter().copied().skip(1) {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
        } else if byte == b'_' {
            // SAI module segments use [a-z][a-z0-9_]*.
        } else {
            return false;
        }
    }
    true
}

fn is_wit_interface_segment(value: &str) -> bool {
    value.split('-').all(|word| {
        let bytes = word.as_bytes();
        !bytes.is_empty()
            && bytes[0].is_ascii_lowercase()
            && bytes[1..]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn canonical_name_prefix_len(value: &str) -> Option<usize> {
    let mut chars = value.char_indices();
    let (_, first) = chars.next()?;
    if first != '_' && !is_xid_start(first) {
        return None;
    }

    let mut end = first.len_utf8();
    for (index, character) in chars {
        if !is_xid_continue(character) {
            break;
        }
        end = index + character.len_utf8();
    }
    let name = &value[..end];
    if name == "_" || !is_nfc(name) {
        return None;
    }
    Some(end)
}

fn is_canonical_name(value: &str) -> bool {
    canonical_name_prefix_len(value) == Some(value.len())
}

fn is_canonical_decimal(value: &str) -> bool {
    let bytes = value.as_bytes();
    match bytes {
        [b'0'] => true,
        [b'1'..=b'9', rest @ ..] => rest.iter().all(u8::is_ascii_digit),
        _ => false,
    }
}

fn read_tag<'a>(value: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let rest = value.strip_prefix(name)?.strip_prefix('[')?;
    let (payload, tail) = read_bracketed_payload(rest)?;
    Some((payload, tail))
}

fn read_bracketed_payload(value: &str) -> Option<(&str, &str)> {
    let mut delimiters = vec![']'];
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if let Some(quote_character) = quote {
            match character {
                '\\' => escaped = true,
                character if character == quote_character => quote = None,
                _ => {}
            }
            continue;
        }

        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' | '{' | '<' => {
                let Some(closer) = matching_closer(character) else {
                    return None;
                };
                delimiters.push(closer);
            }
            '>' if value[..index].chars().next_back() == Some('-') => {}
            ')' | ']' | '}' | '>' if delimiters.last() == Some(&character) => {
                delimiters.pop();
                if delimiters.is_empty() {
                    return Some((&value[..index], &value[index + character.len_utf8()..]));
                }
            }
            ')' | ']' | '}' | '>' => return None,
            _ => {}
        }
    }

    None
}

fn matching_closer(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        _ => None,
    }
}

fn split_impl_header(value: &str) -> Option<(&str, &str)> {
    let mut delimiters = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut separator = None;

    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if let Some(quote_character) = quote {
            match character {
                '\\' => escaped = true,
                character if character == quote_character => quote = None,
                _ => {}
            }
            continue;
        }

        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' | '{' | '<' => delimiters.push(matching_closer(character)?),
            '>' if value[..index].chars().next_back() == Some('-') => {}
            ')' | ']' | '}' | '>' if delimiters.last() == Some(&character) => {
                delimiters.pop();
            }
            ')' | ']' | '}' | '>' => return None,
            _ => {}
        }

        if delimiters.is_empty() && value[index..].starts_with(" for ") {
            if separator.replace(index).is_some() {
                return None;
            }
        }
    }

    if quote.is_some() || !delimiters.is_empty() {
        return None;
    }

    let index = separator?;
    let trait_type = &value[..index];
    let self_type = &value[index + " for ".len()..];
    (!trait_type.is_empty() && !self_type.is_empty()).then_some((trait_type, self_type))
}

fn is_canonical_type_shape(value: &str) -> bool {
    if value.is_empty()
        || value.trim() != value
        || !is_nfc(value)
        || value.chars().any(char::is_control)
        || value.as_bytes().windows(2).any(|pair| pair == b"..")
    {
        return false;
    }

    #[derive(Clone, Copy)]
    struct Delimiter {
        close: char,
        generic: bool,
        generic_argument_has_token: bool,
    }

    let mut delimiters: Vec<Delimiter> = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut previous_character = None;
    let characters: Vec<char> = value.chars().collect();
    for (index, character) in characters.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            previous_character = Some(character);
            continue;
        }

        if let Some(quote_character) = quote {
            match character {
                '\\' => escaped = true,
                character if character == quote_character => quote = None,
                _ => {}
            }
            previous_character = Some(character);
            continue;
        }

        match character {
            '"' | '\'' => {
                quote = Some(character);
                if let Some(top) = delimiters.last_mut() {
                    if top.generic {
                        top.generic_argument_has_token = true;
                    }
                }
            }
            '(' | '[' | '{' => {
                let Some(close) = matching_closer(character) else {
                    return false;
                };
                delimiters.push(Delimiter {
                    close,
                    generic: false,
                    generic_argument_has_token: false,
                });
            }
            '<' => {
                // Canonical generic arguments are attached to the nominal
                // type name: `Map<Key, Value>`, never `Map <Key, Value>`.
                if !matches!(previous_character, Some(previous) if is_xid_continue(previous) || previous == '>')
                    || matches!(characters.get(index + 1), None | Some('>' | ' '))
                {
                    return false;
                }
                delimiters.push(Delimiter {
                    close: '>',
                    generic: true,
                    generic_argument_has_token: false,
                });
            }
            '>' if previous_character == Some('-') => {}
            ')' | ']' | '}' | '>'
                if delimiters.last().is_some_and(|top| top.close == character) =>
            {
                let top = delimiters.pop().expect("delimiter was observed above");
                if top.generic
                    && (!top.generic_argument_has_token
                        || previous_character.is_some_and(char::is_whitespace))
                {
                    return false;
                }
                if let Some(parent) = delimiters.last_mut() {
                    if parent.generic {
                        parent.generic_argument_has_token = true;
                    }
                }
            }
            ',' if delimiters.last().is_some_and(|top| top.generic) => {
                let Some(top) = delimiters.last_mut() else {
                    unreachable!();
                };
                if !top.generic_argument_has_token
                    || previous_character.is_some_and(char::is_whitespace)
                    || characters.get(index + 1) != Some(&' ')
                {
                    return false;
                }
                let Some(next) = characters.get(index + 2) else {
                    return false;
                };
                if matches!(*next, ',' | '>' | ')' | ']' | '}') || next.is_whitespace() {
                    return false;
                }
                top.generic_argument_has_token = false;
            }
            ',' => {
                // Tuple types permit the canonical singleton spelling `(T,)`.
                let is_singleton_tuple_end = delimiters.last().is_some_and(|top| top.close == ')')
                    && characters.get(index + 1) == Some(&')');
                if !is_singleton_tuple_end && characters.get(index + 1) != Some(&' ') {
                    return false;
                }
                if let Some(parent) = delimiters.last_mut() {
                    if parent.generic {
                        parent.generic_argument_has_token = true;
                    }
                }
            }
            ')' | ']' | '}' | '>' => return false,
            '-' if characters.get(index + 1) == Some(&'>') => {
                if index < 2
                    || characters.get(index - 1) != Some(&' ')
                    || characters.get(index - 2).is_some_and(|c| c.is_whitespace())
                    || characters.get(index + 2) != Some(&' ')
                    || characters.get(index + 3).is_some_and(|c| c.is_whitespace())
                {
                    return false;
                }
                if let Some(top) = delimiters.last_mut() {
                    if top.generic {
                        top.generic_argument_has_token = true;
                    }
                }
            }
            character if is_xid_continue(character) || is_type_punctuation(character) => {
                if let Some(top) = delimiters.last_mut() {
                    if top.generic {
                        top.generic_argument_has_token = true;
                    }
                }
            }
            _ if character.is_whitespace() => {}
            _ => return false,
        }

        previous_character = Some(character);
    }

    quote.is_none() && !escaped && delimiters.is_empty()
}

fn is_type_punctuation(character: char) -> bool {
    matches!(
        character,
        '_' | ':' | '/' | '.' | ',' | '+' | '-' | '*' | '&' | '?' | '!' | '=' | '#'
    )
}

fn is_canonical_test_string(value: &str) -> bool {
    let Some(contents) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return false;
    };

    let mut decoded_characters = 0;
    let mut chars = contents.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if !matches!(chars.next(), Some('\\' | '"')) {
                return false;
            }
        } else if character == '"' || character.is_control() {
            return false;
        }
        decoded_characters += 1;
    }

    decoded_characters > 0
}

#[cfg(test)]
mod tests {
    use super::{
        is_canonical_name, is_canonical_symbol_path, is_module_path, is_package_segment, read_tag,
    };

    #[test]
    fn accepts_canonical_paths_with_modules_names_and_members() {
        assert!(is_package_segment("acme"));
        assert!(is_package_segment("user"));
        assert!(is_module_path("/codec"));
        assert!(is_module_path("/internal_tools"));
        assert!(is_module_path("/outgoing-handler"));
        assert!(!is_module_path("/a_-b"));
        assert!(!is_module_path("/foo-bar/baz"));
        assert!(is_canonical_name("Parser"));
        assert_eq!(read_tag("field[name]", "field"), Some(("name", "")));
        assert!(is_canonical_name("name"));
        for path in [
            "acme:user/service.create_user",
            "acme:user/fmt-core.Format",
            "acme:user/codec.Parser.field[name]",
            "sai:core/alloc.vec.Vec.generic[T]",
            "acme:user.型.field[é]",
            "acme:user.test[\"roundtrip\"]",
            "acme:user.test[\"has ] bracket\"]",
            "acme:user.test[\"e\u{301}\"]",
            "acme:user.property[\"quote: \\\" and slash: \\\\\"]",
            "acme:user.impl[sai:core/fmt.Format for acme:shop/model.Order].fmt",
            "acme:user.impl[acme:core/fn.Fn<fn(I32) -> Str> for acme:user.Model].call",
            "acme:user.impl[acme:shop/model.api.Api].call",
            "acme:user.impl[wasi:http/outgoing-handler].handle",
            "acme:user.impl[wasi:http/outgoing-handler]",
        ] {
            assert!(is_canonical_symbol_path(path), "{path:?}");
        }
    }

    #[test]
    fn rejects_paths_outside_the_canonical_symbol_grammar() {
        for path in [
            "acme:user",
            "acme:user/module",
            "acme:user.",
            "acme:user..outside",
            "acme:user/../../outside",
            "acme:user//outside.item",
            "acme:user/Upper.item",
            "acme:user/trailing-.item",
            "acme:user/double--dash.item",
            "acme:user/a_-b.item",
            "acme:user/foo-bar/baz.item",
            "acme:user/item.field[01]",
            "acme:user/item.generic[_]",
            "acme:user.test[\"\"]",
            "acme:user.test[\"line\\nbreak\"]",
            "acme:user.test[\"bad\\nescape\"]",
            "acme:user.impl[]",
            "acme:user.impl[acme:foo.]",
            "acme:user.impl[Tr for ]",
            "acme:user.impl[Tr< for Self]",
            "acme:user.impl[Tr<,> for Self].f",
            "acme:user.impl[Tr <T> for Self].f",
            "acme:user.impl[Tr< T> for Self].f",
            "acme:user.impl[Tr<T > for Self].f",
            "acme:user.impl[Tr<T , U> for Self].f",
            "acme:user.impl[Tr<fn(I32)->Str> for Self].f",
            "acme:user.impl[Tr<e\u{301}> for Self].f",
            "acme:user.e\u{301}",
        ] {
            assert!(!is_canonical_symbol_path(path), "{path:?}");
        }
    }
}
