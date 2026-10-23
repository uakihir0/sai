use std::collections::BTreeSet;

use sai_syntax::{SyntaxKind, identifier_case_kind, is_reserved_identifier, is_strict_keyword};
use unicode_ident::{is_xid_continue, is_xid_start};
use unicode_normalization::is_nfc;

/// Checks the structural grammar of a canonical symbol path and its
/// `CanonType` syntax.
///
/// This parser deliberately stops short of resolving paths and type names or
/// checking declaration-specific properties. Those checks require the
/// analyzed package inventory.
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
    !is_strict_keyword(value) && !is_reserved_identifier(value) && value != "lib"
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

    quote.is_none()
        && !escaped
        && delimiters.is_empty()
        && canonical_type_paths_are_well_formed(value)
}

/// Validates the type-path atoms and generic arguments that the shape scanner
/// above deliberately treats as untrusted text.
///
/// Function-pointer syntax is parsed without resolving effect names. Tuples,
/// borrows, and nominal/parameter paths are walked here so malformed types
/// cannot hide inside structural forms.
fn canonical_type_paths_are_well_formed(value: &str) -> bool {
    CanonicalTypeParser::new(value).is_some_and(|parser| parser.check())
}

#[derive(Clone, Copy)]
struct TypeRange {
    start: usize,
    end: usize,
}

struct CanonicalTypeParser<'a> {
    value: &'a str,
    characters: Vec<char>,
    byte_offsets: Vec<usize>,
    matching_delimiters: Vec<Option<usize>>,
    enclosing_delimiters: Vec<Option<usize>>,
    effect_suffix_starts: Vec<usize>,
    quoted: Vec<bool>,
}

impl<'a> CanonicalTypeParser<'a> {
    fn new(value: &'a str) -> Option<Self> {
        let characters = value.chars().collect::<Vec<_>>();
        let byte_offsets = value
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(value.len()))
            .collect::<Vec<_>>();
        let mut matching_delimiters = vec![None; characters.len()];
        let mut enclosing_delimiters = vec![None; characters.len()];
        let mut effect_suffix_starts = Vec::new();
        let mut quoted = vec![false; characters.len()];
        let mut delimiters = Vec::new();
        let mut quote = None;
        let mut escaped = false;

        for (index, character) in characters.iter().copied().enumerate() {
            if quote.is_none() {
                enclosing_delimiters[index] = delimiters.last().map(|(open_index, _)| *open_index);
                if value[byte_offsets[index]..].starts_with(" effects {") {
                    effect_suffix_starts.push(index);
                }
            }
            if escaped {
                quoted[index] = true;
                escaped = false;
                continue;
            }
            if let Some(quote_character) = quote {
                quoted[index] = true;
                match character {
                    '\\' => escaped = true,
                    character if character == quote_character => quote = None,
                    _ => {}
                }
                continue;
            }
            if matches!(character, '"' | '\'') {
                quoted[index] = true;
                quote = Some(character);
                continue;
            }

            match character {
                '(' | '[' | '{' | '<' => delimiters.push((index, matching_closer(character)?)),
                '>' if index > 0 && characters[index - 1] == '-' => {}
                ')' | ']' | '}' | '>' => {
                    let (open_index, close) = delimiters.pop()?;
                    if close != character {
                        return None;
                    }
                    matching_delimiters[open_index] = Some(index);
                    matching_delimiters[index] = Some(open_index);
                }
                _ => {}
            }
        }
        if quote.is_some() || escaped || !delimiters.is_empty() {
            return None;
        }

        Some(Self {
            value,
            characters,
            byte_offsets,
            matching_delimiters,
            enclosing_delimiters,
            effect_suffix_starts,
            quoted,
        })
    }

    fn check(&self) -> bool {
        let mut pending = vec![(
            TypeRange {
                start: 0,
                end: self.characters.len(),
            },
            true,
        )];

        while let Some((range, allow_effects)) = pending.pop() {
            let text = self.text(range);
            if let Some(referent) = text.strip_prefix("borrow mut ") {
                let start = range.start + "borrow mut ".chars().count();
                pending.push((
                    TypeRange {
                        start,
                        end: range.end,
                    },
                    true,
                ));
                debug_assert_eq!(
                    referent,
                    self.text(TypeRange {
                        start,
                        end: range.end
                    })
                );
                continue;
            }
            if text.starts_with("borrow ") {
                let start = range.start + "borrow ".chars().count();
                pending.push((
                    TypeRange {
                        start,
                        end: range.end,
                    },
                    true,
                ));
                continue;
            }

            if self.starts_with(range, "fn(") || self.starts_with(range, "unsafe fn(") {
                let Some((parameters, result, effects)) = self.function_pointer(range) else {
                    return false;
                };
                if effects.is_some_and(|effects| !is_canonical_effect_set(effects)) {
                    return false;
                }
                if effects.is_some() && !allow_effects {
                    return false;
                }
                pending.extend(parameters.into_iter().map(|parameter| (parameter, true)));
                if let Some(result) = result {
                    pending.push((result, false));
                }
                continue;
            }

            if self.starts_with(range, "(")
                && self.matching_delimiters[range.start] == Some(range.end - 1)
            {
                let contents = TypeRange {
                    start: range.start + 1,
                    end: range.end - 1,
                };
                if contents.start == contents.end {
                    continue;
                }
                let Some((elements, had_separator)) = self.split_top_level_commas(contents, true)
                else {
                    return false;
                };
                if had_separator {
                    pending.extend(elements.into_iter().map(|element| (element, true)));
                } else {
                    pending.push((contents, true));
                }
                continue;
            }

            if let Some(open) = self.generic_open(range) {
                let Some(close) = self.matching_delimiters[open] else {
                    return false;
                };
                if close + 1 != range.end {
                    return false;
                }
                let path = TypeRange {
                    start: range.start,
                    end: open,
                };
                if !is_canonical_type_path(self.text(path)) {
                    return false;
                }
                let arguments = TypeRange {
                    start: open + 1,
                    end: close,
                };
                let Some((arguments, _)) = self.split_top_level_commas(arguments, false) else {
                    return false;
                };
                for argument in arguments {
                    let text = self.text(argument);
                    if !is_canonical_const_argument(text) {
                        pending.push((argument, true));
                    }
                }
            } else if !is_canonical_type_path(text) {
                return false;
            }
        }
        true
    }

    fn text(&self, range: TypeRange) -> &'a str {
        &self.value[self.byte_offsets[range.start]..self.byte_offsets[range.end]]
    }

    fn starts_with(&self, range: TypeRange, prefix: &str) -> bool {
        self.text(range).starts_with(prefix)
    }

    fn generic_open(&self, range: TypeRange) -> Option<usize> {
        let mut index = range.start;
        while index < range.end {
            if self.quoted[index] {
                index += 1;
            } else if self.characters[index] == '<' {
                return Some(index);
            } else if self.matching_delimiters[index].is_some_and(|end| end > index) {
                index = self.matching_delimiters[index].unwrap() + 1;
            } else {
                index += 1;
            }
        }
        None
    }

    fn split_top_level_commas(
        &self,
        range: TypeRange,
        allow_trailing: bool,
    ) -> Option<(Vec<TypeRange>, bool)> {
        if range.start == range.end {
            return Some((Vec::new(), false));
        }
        let mut ranges = Vec::new();
        let mut start = range.start;
        let mut index = range.start;
        let mut had_separator = false;
        while index < range.end {
            if self.quoted[index] {
                index += 1;
                continue;
            }
            if let Some(end) = self.matching_delimiters[index]
                && end > index
            {
                index = end + 1;
                continue;
            }
            if self.characters[index] != ',' {
                index += 1;
                continue;
            }
            if start == index {
                return None;
            }
            ranges.push(TypeRange { start, end: index });
            had_separator = true;
            if index + 1 == range.end && allow_trailing {
                return Some((ranges, had_separator));
            }
            if self.characters.get(index + 1) != Some(&' ') || index + 2 == range.end {
                return None;
            }
            start = index + 2;
            index = start;
        }
        if start == range.end {
            return None;
        }
        ranges.push(TypeRange {
            start,
            end: range.end,
        });
        Some((ranges, had_separator))
    }

    fn function_pointer(
        &self,
        range: TypeRange,
    ) -> Option<(Vec<TypeRange>, Option<TypeRange>, Option<&'a str>)> {
        let text = self.text(range);
        let prefix = if text.starts_with("unsafe fn(") {
            "unsafe fn"
        } else if text.starts_with("fn(") {
            "fn"
        } else {
            return None;
        };
        let open = range.start + prefix.chars().count();
        let close = self.matching_delimiters[open]?;
        let parameter_range = TypeRange {
            start: open + 1,
            end: close,
        };
        let parameters = if parameter_range.start == parameter_range.end {
            Vec::new()
        } else {
            self.split_top_level_commas(parameter_range, true)?.0
        };

        let mut suffix = TypeRange {
            start: close + 1,
            end: range.end,
        };
        let suffix_text = self.text(suffix);
        let has_result = suffix_text.starts_with(" -> ");
        if has_result {
            suffix.start += " -> ".chars().count();
        }
        let (type_range, effects) = self.split_effects_suffix(suffix)?;
        let result = if has_result {
            Some((type_range.start < type_range.end).then_some(type_range)?)
        } else if type_range.start == type_range.end {
            None
        } else {
            return None;
        };
        Some((parameters, result, effects))
    }

    fn split_effects_suffix(&self, range: TypeRange) -> Option<(TypeRange, Option<&'a str>)> {
        let first = self
            .effect_suffix_starts
            .partition_point(|index| *index < range.start);
        let mut after_last = self
            .effect_suffix_starts
            .partition_point(|index| *index < range.end);
        while after_last > first {
            after_last -= 1;
            let index = self.effect_suffix_starts[after_last];
            if let Some(open) = self.enclosing_delimiters[index]
                && open >= range.start
            {
                after_last = self
                    .effect_suffix_starts
                    .partition_point(|candidate| *candidate < open);
                continue;
            }

            let effects_start = index + " effects ".chars().count();
            let effects_close = self.matching_delimiters[effects_start]?;
            if effects_close + 1 != range.end {
                return None;
            }
            return Some((
                TypeRange {
                    start: range.start,
                    end: index,
                },
                Some(self.text(TypeRange {
                    start: effects_start,
                    end: range.end,
                })),
            ));
        }
        Some((range, None))
    }
}

fn is_canonical_type_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }

    if path.contains(':') || path.contains('/') {
        return is_canonical_nominal_type_path(path);
    }

    is_canonical_type_name(path)
}

fn is_canonical_nominal_type_path(path: &str) -> bool {
    let Some((_, package_and_path)) = path.split_once(':') else {
        return false;
    };
    let Some(item_path_start) = package_and_path.find('.') else {
        return false;
    };
    let item_path = &package_and_path[item_path_start + 1..];
    // A CanonType item path names one nominal declaration. Dotted suffixes
    // after its name are associated-type projections, which are forbidden in
    // trait impl headers. Check this before the full path parser so malformed
    // nested `.impl[...]` text cannot recursively re-enter type validation.
    is_canonical_type_name(item_path) && is_canonical_symbol_path(path)
}

fn is_canonical_type_name(name: &str) -> bool {
    // `Int` and `UInt` are primitive aliases when resolved through the
    // prelude, but the validator has no scope inventory to distinguish those
    // from an in-scope type parameter with the same spelling.
    is_canonical_name(name) && identifier_case_kind(name) == SyntaxKind::UIDENT
}

fn is_canonical_const_argument(value: &str) -> bool {
    matches!(value, "true" | "false")
        || is_canonical_integer_literal(value)
        || is_canonical_char_literal(value)
}

fn is_canonical_integer_literal(value: &str) -> bool {
    let value = value.strip_prefix('-').unwrap_or(value);
    let (digits, radix) = if let Some(digits) = value.strip_prefix("0x") {
        (digits, 16)
    } else if let Some(digits) = value.strip_prefix("0o") {
        (digits, 8)
    } else if let Some(digits) = value.strip_prefix("0b") {
        (digits, 2)
    } else {
        (value, 10)
    };
    if radix == 10 && digits.starts_with('0') && digits != "0" {
        return false;
    }
    let mut previous_was_digit = false;
    let mut saw_digit = false;
    for character in digits.chars() {
        if character == '_' {
            if !previous_was_digit {
                return false;
            }
            previous_was_digit = false;
        } else if character.to_digit(radix).is_some() {
            saw_digit = true;
            previous_was_digit = true;
        } else {
            return false;
        }
    }
    saw_digit && previous_was_digit
}

fn is_canonical_char_literal(value: &str) -> bool {
    let Some(contents) = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    else {
        return false;
    };
    if contents.is_empty() {
        return false;
    }

    let mut chars = contents.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '\\' {
        return chars.next().is_none()
            && first != '\''
            && first != '\n'
            && first != '\r'
            && first != '\t'
            && !is_bidi_control(first);
    }

    match chars.next() {
        Some('n' | 'r' | 't' | '\\' | '\'' | '"' | '0') => chars.next().is_none(),
        Some('u') if chars.next() == Some('{') => {
            let mut digits = String::new();
            let mut closed = false;
            while let Some(character) = chars.next() {
                if character == '}' {
                    closed = true;
                    break;
                }
                if !character.is_ascii_hexdigit() || digits.len() == 6 {
                    return false;
                }
                digits.push(character);
            }
            closed
                && !digits.is_empty()
                && chars.next().is_none()
                && u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .is_some_and(|character| !is_bidi_control(character))
        }
        _ => false,
    }
}

fn is_bidi_control(character: char) -> bool {
    matches!(character as u32, 0x202A..=0x202E | 0x2066..=0x2069)
}

fn is_canonical_effect_set(value: &str) -> bool {
    let Some(contents) = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    else {
        return false;
    };
    if contents.is_empty() {
        return true;
    }
    let Some(contents) = contents
        .strip_prefix(' ')
        .and_then(|value| value.strip_suffix(' '))
    else {
        return false;
    };
    let effects = contents.split(", ").collect::<Vec<_>>();
    if effects.iter().any(|effect| {
        effect.is_empty()
            || effect
                .split('.')
                .any(|part| !is_canonical_effect_ref_segment(part))
    }) {
        return false;
    }

    let builtin_order = [
        "alloc",
        "fs.read",
        "fs.write",
        "net.connect",
        "net.http",
        "clock.read",
        "random.read",
        "stdio.read",
        "stdio.write",
        "env.read",
        "process.exit",
        "process.spawn",
        "thread.spawn",
        "host",
        "ffi",
    ];
    let mut previous_builtin_rank = None;
    let mut saw_non_builtin = false;
    let mut seen = BTreeSet::new();
    for effect in effects {
        if !seen.insert(effect) {
            return false;
        }
        let builtin_rank = builtin_order.iter().position(|builtin| *builtin == effect);
        let has_builtin_root = matches!(
            effect.split('.').next(),
            Some(
                "alloc"
                    | "fs"
                    | "net"
                    | "clock"
                    | "random"
                    | "stdio"
                    | "env"
                    | "process"
                    | "thread"
                    | "host"
                    | "ffi"
            )
        );
        if has_builtin_root && builtin_rank.is_none() {
            return false;
        }
        if let Some(rank) = builtin_rank {
            if saw_non_builtin || previous_builtin_rank.is_some_and(|previous| previous >= rank) {
                return false;
            }
            previous_builtin_rank = Some(rank);
        } else {
            // Interface/user ordering depends on resolved atom identity and
            // category, which this syntax-only codec does not have.
            saw_non_builtin = true;
        }
    }
    true
}

fn is_canonical_effect_ref_segment(value: &str) -> bool {
    let (name, raw) = match value.strip_prefix("r#") {
        Some(name) => (name, true),
        None => (value, false),
    };
    is_canonical_name(name)
        && identifier_case_kind(value) == SyntaxKind::LIDENT
        && if raw {
            is_strict_keyword(name) || is_reserved_identifier(name)
        } else {
            !is_strict_keyword(name) && !is_reserved_identifier(name)
        }
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
        is_canonical_effect_set, is_canonical_name, is_canonical_symbol_path, is_module_path,
        is_package_segment, read_tag,
    };

    #[test]
    fn accepts_canonical_paths_with_modules_names_and_members() {
        assert!(is_package_segment("acme"));
        assert!(is_package_segment("user"));
        assert!(is_module_path("/codec"));
        assert!(is_module_path("/internal_tools"));
        assert!(is_module_path("/service/result"));
        assert!(is_module_path("/service/fs"));
        assert!(is_module_path("/outgoing-handler"));
        assert!(!is_module_path("/service/and"));
        assert!(!is_module_path("/service/type"));
        assert!(!is_module_path("/service/null"));
        assert!(!is_module_path("/service/lib"));
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
            "acme:user.impl[acme:core.Option<acme:user/model.Order, 4> for acme:user/model.Box<acme:user/model.Order>].run",
            "acme:user.impl[Array<acme:user.Model, 4> for (acme:user.Model,)].run",
            "acme:user.impl[acme:core.Option<acme:user.Model, '\\u{41}'> for borrow mut acme:user.Model].run",
            "acme:user.impl[Tr<','> for Self].f",
            "acme:user.impl[acme:core.Option<(fn(I32) -> Str,)> for acme:user.Model].run",
            "acme:user.impl[Tr<fn(I32, Str) -> Bool effects { alloc, fs.read }> for Self].f",
            "acme:user.impl[Tr<fn() effects {}> for Self].f",
            "acme:user.impl[Tr<unsafe fn() -> (fn() effects { alloc })> for Self].f",
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
            "acme:user/service/and.item",
            "acme:user/service/type.item",
            "acme:user/service/null.item",
            "acme:user/service/try.item",
            "acme:user/service/lib.item",
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
            "acme:user.impl[Tr<r#Self> for Self].f",
            "acme:user.impl[acme:user//model.Order for acme:user.Model].run",
            "acme:user.impl[acme:user/model.order for acme:user.Model].run",
            "acme:user.impl[acme:core.Option<I32, 4.0> for acme:user.Model].run",
            "acme:user.impl[Tr<T.Item> for Self].f",
            "acme:user.impl[Tr<acme:user.Model.Item> for Self].f",
            "acme:user.impl[Tr<foo> for Self].f",
            "acme:user.impl[Tr<#+> for Self].f",
            "acme:user.impl[Tr<fn(I32) -> foo> for Self].f",
            "acme:user.impl[Tr<fn(I32) garbage> for Self].f",
            "acme:user.impl[Tr<fn(I32) -> owned I32> for Self].f",
            "acme:user.impl[Tr<fn() effects { host, alloc }> for Self].f",
            "acme:user.impl[Tr<fn() effects { fs.read, alloc }> for Self].f",
            "acme:user.impl[Tr<fn() effects { fs.unknown }> for Self].f",
            "acme:user.impl[Tr<fn() effects { alloc, alloc }> for Self].f",
            "acme:user.impl[Tr<04> for Self].f",
            "acme:user.impl[Tr<0_4> for Self].f",
            "acme:user.impl[Tr<-0_4> for Self].f",
            "acme:user.impl[Tr<'\u{202e}'> for Self].f",
            "acme:user.impl[Tr<'\\u{202E}'> for Self].f",
            "acme:user.e\u{301}",
        ] {
            assert!(!is_canonical_symbol_path(path), "{path:?}");
        }
    }

    #[test]
    fn effect_reference_identifiers_follow_raw_keyword_rules() {
        assert!(is_canonical_effect_set("{ r#type }"));
        assert!(is_canonical_effect_set("{ r#null }"));
        assert!(is_canonical_effect_set("{ r#self }"));
        assert!(is_canonical_effect_set("{ effect }"));
        assert!(!is_canonical_effect_set("{ if }"));
        assert!(!is_canonical_effect_set("{ null }"));
        assert!(!is_canonical_effect_set("{ r#name }"));
        assert!(!is_canonical_effect_set("{ r#effect }"));
        assert!(!is_canonical_effect_set("{ r#Self }"));
        assert!(!is_canonical_effect_set("{ r#fs.read }"));
        assert!(!is_canonical_effect_set("{ r#_ }"));
        assert!(is_canonical_symbol_path(
            "acme:user.impl[Tr<fn() effects { r#type }> for Self].f"
        ));
    }

    #[test]
    fn syntax_only_effect_validation_defers_non_builtin_order() {
        // The canonical order of interface and user atoms needs resolution,
        // so this structural check accepts either spelling order provisionally.
        assert!(is_canonical_effect_set("{ zeta, alpha }"));
        assert!(!is_canonical_effect_set("{ fs.read, alloc }"));
    }

    #[test]
    fn syntax_only_type_validation_defers_alias_resolution() {
        // A resolved primitive alias prints as I64/U64, but these names can
        // also refer to an in-scope type parameter and need semantic context.
        assert!(is_canonical_symbol_path(
            "acme:user.impl[Tr<Int> for Self].f"
        ));
        assert!(is_canonical_symbol_path(
            "acme:user.impl[Tr<UInt> for Self].f"
        ));
    }

    #[test]
    fn rejects_deeply_nested_impl_paths_without_recursive_type_validation() {
        let mut nested = "acme:user.Model".to_owned();
        for _ in 0..128 {
            nested = format!("acme:user.impl[Tr for {nested}].Assoc");
        }
        let path = format!("acme:user.impl[Tr for {nested}].f");
        assert!(!is_canonical_symbol_path(&path));
    }

    #[test]
    fn accepts_deeply_nested_generic_types_iteratively() {
        let mut nested = "I32".to_owned();
        for _ in 0..512 {
            nested = format!("Tr<{nested}>");
        }
        let path = format!("acme:user.impl[Tr for {nested}].f");
        assert!(is_canonical_symbol_path(&path));
    }
}
