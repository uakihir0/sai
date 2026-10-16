use std::collections::HashSet;
use std::ops::Range;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct SourceScan {
    pub parser_text: String,
    pub parser_to_source: Vec<usize>,
    pub package_name_range: Option<Range<usize>>,
    pub nested_package_ranges: Vec<Range<usize>>,
    pub named_result_function_ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ForeignReference {
    pub identity: String,
    pub version: Option<String>,
    /// The semantic WIT item name with any `%` escape removed.
    pub item: String,
    pub range: Range<usize>,
    pub version_range: Option<Range<usize>>,
    pub item_range: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    range: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenKind {
    Identifier,
    ExplicitIdentifier,
    Number,
    Invalid,
    Arrow,
    Punctuation(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NamedResultField {
    name: Range<usize>,
    colon: Range<usize>,
    end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NamedResults {
    range: Range<usize>,
    fields: Vec<NamedResultField>,
    commas: Vec<Range<usize>>,
    function_name: Range<usize>,
}

pub(super) fn scan_source(source: &str) -> SourceScan {
    let tokens = lex(source);
    let mut package_name_range = None;
    let mut nested_package_ranges = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if !is_identifier(source, token, "package") {
            continue;
        }
        let Some((name_range, delimiter)) = package_declaration_name(&tokens, index + 1) else {
            continue;
        };
        match delimiter {
            b';' if package_name_range.is_none() => package_name_range = Some(name_range),
            b'{' => nested_package_ranges.push(name_range),
            _ => {}
        }
    }

    let mut named_results = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Arrow {
            continue;
        }
        let Some(open_index) = tokens
            .get(index + 1)
            .and_then(|token| (token.kind == TokenKind::Punctuation(b'(')).then_some(index + 1))
        else {
            continue;
        };
        let Some(close_index) = matching_delimiter(&tokens, open_index, b'(', b')') else {
            continue;
        };
        let Some(function_name_range) = function_name_range(source, &tokens, index, open_index)
        else {
            continue;
        };
        let Some(fields_and_commas) = named_result_fields(source, &tokens, open_index, close_index)
        else {
            continue;
        };
        let fields = fields_and_commas.0;
        named_results.push(NamedResults {
            range: tokens[open_index].range.start..tokens[close_index].range.end,
            fields,
            commas: fields_and_commas.1,
            function_name: function_name_range,
        });
    }

    let mut parser_text = String::with_capacity(source.len());
    let mut parser_to_source = vec![0];
    let mut named_result_function_ranges = Vec::new();
    let mut source_cursor = 0;
    for result in named_results {
        if result.range.start < source_cursor {
            continue;
        }
        append_original(
            source,
            source_cursor..result.range.start,
            &mut parser_text,
            &mut parser_to_source,
        );
        if result.fields.len() == 1 {
            let field = &result.fields[0];
            let open_end = result.range.start + 1;
            append_original(
                source,
                open_end..field.name.start,
                &mut parser_text,
                &mut parser_to_source,
            );
            skip_source_until(field.name.end, &mut parser_to_source);
            append_original(
                source,
                field.name.end..field.colon.start,
                &mut parser_text,
                &mut parser_to_source,
            );
            skip_source_until(field.colon.end, &mut parser_to_source);
            append_original(
                source,
                field.colon.end..field.end,
                &mut parser_text,
                &mut parser_to_source,
            );
            let close_start = result.range.end - 1;
            if let Some(comma) = result.commas.first() {
                skip_source_until(comma.end, &mut parser_to_source);
                append_original(
                    source,
                    comma.end..close_start,
                    &mut parser_text,
                    &mut parser_to_source,
                );
            }
            source_cursor = result.range.end;
            continue;
        }

        if let Some(mapped_name) =
            parser_range_for_source_range(&parser_to_source, &result.function_name)
        {
            named_result_function_ranges.push(mapped_name);
        }
        let open_end = result.range.start + 1;
        append_generated(
            "tuple<",
            result.range.start,
            open_end,
            &mut parser_text,
            &mut parser_to_source,
        );
        let mut cursor = open_end;
        for (index, field) in result.fields.iter().enumerate() {
            append_original(
                source,
                cursor..field.name.start,
                &mut parser_text,
                &mut parser_to_source,
            );
            skip_source_until(field.name.end, &mut parser_to_source);
            append_original(
                source,
                field.name.end..field.colon.start,
                &mut parser_text,
                &mut parser_to_source,
            );
            skip_source_until(field.colon.end, &mut parser_to_source);
            let comma = result.commas.get(index);
            let field_end = field.end;
            append_original(
                source,
                field.colon.end..field_end,
                &mut parser_text,
                &mut parser_to_source,
            );
            if let Some(comma) = comma {
                append_original(
                    source,
                    comma.clone(),
                    &mut parser_text,
                    &mut parser_to_source,
                );
                cursor = comma.end;
            } else {
                cursor = field_end;
            }
        }
        let close_start = result.range.end - 1;
        append_original(
            source,
            cursor..close_start,
            &mut parser_text,
            &mut parser_to_source,
        );
        append_generated(
            ">",
            close_start,
            result.range.end,
            &mut parser_text,
            &mut parser_to_source,
        );
        source_cursor = result.range.end;
    }
    append_original(
        source,
        source_cursor..source.len(),
        &mut parser_text,
        &mut parser_to_source,
    );

    SourceScan {
        parser_text,
        parser_to_source,
        package_name_range,
        nested_package_ranges,
        named_result_function_ranges,
    }
}

pub(super) fn scan_foreign_references(source: &str) -> Vec<ForeignReference> {
    let tokens = lex(source);
    let world_external_directive_indices = world_external_directive_indices(source, &tokens);
    let mut references = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        let is_use = is_identifier(source, token, "use");
        if !is_use && !world_external_directive_indices.contains(&index) {
            continue;
        }
        if let Some(reference) = parse_foreign_reference(source, &tokens, index + 1) {
            references.push(reference);
        }
    }

    references
}

fn world_external_directive_indices(source: &str, tokens: &[Token]) -> HashSet<usize> {
    let mut indices = HashSet::new();
    for (world_index, token) in tokens.iter().enumerate() {
        if !is_identifier(source, token, "world") {
            continue;
        }
        let Some(open_index) = tokens.get(world_index + 2).and_then(|token| {
            (token.kind == TokenKind::Punctuation(b'{')).then_some(world_index + 2)
        }) else {
            continue;
        };
        let Some(close_index) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut depth = 1_u32;
        for index in open_index + 1..close_index {
            let token = &tokens[index];
            if depth == 1
                && (is_identifier(source, token, "import")
                    || is_identifier(source, token, "export")
                    || is_identifier(source, token, "include"))
            {
                indices.insert(index);
            }
            match token.kind {
                TokenKind::Punctuation(b'{') => depth += 1,
                TokenKind::Punctuation(b'}') => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    indices
}

fn parse_foreign_reference(
    source: &str,
    tokens: &[Token],
    start_index: usize,
) -> Option<ForeignReference> {
    let namespace = tokens.get(start_index)?;
    let colon = tokens.get(start_index + 1)?;
    let package = tokens.get(start_index + 2)?;
    let slash = tokens.get(start_index + 3)?;
    let interface = tokens.get(start_index + 4)?;
    if colon.kind != TokenKind::Punctuation(b':') || slash.kind != TokenKind::Punctuation(b'/') {
        return None;
    }

    let range_start = namespace.range.start;
    let namespace = wit_identifier_text(source, namespace)?;
    let package = wit_identifier_text(source, package)?;
    let item = wit_identifier_text(source, interface)?.to_owned();

    let (range_end, version, version_range) = match tokens.get(start_index + 5) {
        Some(token) if token.kind == TokenKind::Punctuation(b'@') => {
            let version_start = tokens.get(start_index + 6)?.range.start;
            let version_end = semver_end(source, version_start)?;
            (
                version_end,
                Some(source[version_start..version_end].to_owned()),
                Some(version_start..version_end),
            )
        }
        _ => (interface.range.end, None, None),
    };

    Some(ForeignReference {
        identity: format!("{namespace}:{package}"),
        version,
        item,
        range: range_start..range_end,
        version_range,
        item_range: interface.range.clone(),
    })
}

fn wit_identifier_text<'a>(source: &'a str, token: &Token) -> Option<&'a str> {
    if !is_wit_identifier(source, token) {
        return None;
    }
    Some(
        source[token.range.clone()]
            .strip_prefix('%')
            .unwrap_or(&source[token.range.clone()]),
    )
}

fn semver_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = take_decimal_component(bytes, start)?;
    for _ in 0..2 {
        if bytes.get(cursor) != Some(&b'.') {
            return None;
        }
        cursor = take_decimal_component(bytes, cursor + 1)?;
    }

    if bytes.get(cursor) == Some(&b'-') {
        cursor = take_semver_identifiers(bytes, cursor + 1)?;
    }
    if bytes.get(cursor) == Some(&b'+') {
        cursor = take_semver_identifiers(bytes, cursor + 1)?;
    }
    Some(cursor)
}

fn take_decimal_component(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    (cursor > start).then_some(cursor)
}

fn take_semver_identifiers(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    let mut has_identifier = false;
    loop {
        let component_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            cursor += 1;
        }
        if cursor == component_start {
            return has_identifier.then_some(cursor);
        }
        has_identifier = true;
        if bytes.get(cursor) != Some(&b'.')
            || !bytes
                .get(cursor + 1)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Some(cursor);
        }
        cursor += 1;
    }
}

fn append_original(
    source: &str,
    range: Range<usize>,
    output: &mut String,
    parser_to_source: &mut Vec<usize>,
) {
    *parser_to_source
        .last_mut()
        .expect("source map has an initial boundary") = range.start;
    output.push_str(&source[range.clone()]);
    parser_to_source.extend((range.start..range.end).map(|offset| offset + 1));
}

fn append_generated(
    generated: &str,
    source_start: usize,
    source_end: usize,
    output: &mut String,
    parser_to_source: &mut Vec<usize>,
) {
    *parser_to_source
        .last_mut()
        .expect("source map has an initial boundary") = source_start;
    for (index, byte) in generated.bytes().enumerate() {
        output.push(char::from(byte));
        parser_to_source.push(if index + 1 == generated.len() {
            source_end
        } else {
            source_start
        });
    }
}

fn skip_source_until(source_offset: usize, parser_to_source: &mut [usize]) {
    *parser_to_source
        .last_mut()
        .expect("source map has an initial boundary") = source_offset;
}

fn parser_range_for_source_range(
    parser_to_source: &[usize],
    source_range: &Range<usize>,
) -> Option<Range<usize>> {
    let length = source_range.end.checked_sub(source_range.start)?;
    parser_to_source
        .windows(length.checked_add(1)?)
        .position(|window| {
            window
                .iter()
                .enumerate()
                .all(|(offset, mapped)| *mapped == source_range.start + offset)
        })
        .map(|start| start..start + length)
}

fn package_declaration_name(tokens: &[Token], start: usize) -> Option<(Range<usize>, u8)> {
    let first = tokens.get(start)?;
    if first.kind != TokenKind::Identifier {
        return None;
    }
    let mut last = first;
    for token in &tokens[start + 1..] {
        match token.kind {
            TokenKind::Punctuation(b';' | b'{') => {
                return Some((
                    first.range.start..last.range.end,
                    match token.kind {
                        TokenKind::Punctuation(delimiter) => delimiter,
                        _ => unreachable!(),
                    },
                ));
            }
            _ => last = token,
        }
    }
    None
}

fn function_name_range(
    source: &str,
    tokens: &[Token],
    arrow_index: usize,
    result_open_index: usize,
) -> Option<Range<usize>> {
    // The arrow must follow the closing parenthesis of a `func` parameter
    // list. The scanner ignores comments and strings, so these are adjacent
    // lexical tokens even when whitespace is present in the source.
    let parameter_close_index = arrow_index.checked_sub(1)?;
    if tokens.get(parameter_close_index)?.kind != TokenKind::Punctuation(b')') {
        return None;
    }
    let parameter_open_index = matching_open_delimiter(tokens, parameter_close_index, b'(', b')')?;
    if parameter_open_index >= result_open_index {
        return None;
    }
    let mut index = parameter_open_index.checked_sub(1)?;
    if !is_identifier(source, tokens.get(index)?, "func") {
        return None;
    }
    index = index.checked_sub(1)?;
    while tokens.get(index).is_some_and(|token| {
        is_identifier(source, token, "async") || is_identifier(source, token, "static")
    }) {
        index = index.checked_sub(1)?;
    }
    if tokens.get(index)?.kind != TokenKind::Punctuation(b':') {
        return None;
    }
    let name = tokens.get(index.checked_sub(1)?)?;
    is_wit_identifier(source, name).then(|| name.range.clone())
}

fn named_result_fields(
    source: &str,
    tokens: &[Token],
    open_index: usize,
    close_index: usize,
) -> Option<(Vec<NamedResultField>, Vec<Range<usize>>)> {
    let mut segment_start = open_index + 1;
    let mut segments = Vec::new();
    let mut commas = Vec::new();
    let mut depth = [0_u32; 4];
    for (index, token) in tokens
        .iter()
        .enumerate()
        .take(close_index)
        .skip(open_index + 1)
    {
        match token.kind {
            TokenKind::Punctuation(b'(') => depth[0] = depth[0].saturating_add(1),
            TokenKind::Punctuation(b')') => depth[0] = depth[0].saturating_sub(1),
            TokenKind::Punctuation(b'[') => depth[1] = depth[1].saturating_add(1),
            TokenKind::Punctuation(b']') => depth[1] = depth[1].saturating_sub(1),
            TokenKind::Punctuation(b'{') => depth[2] = depth[2].saturating_add(1),
            TokenKind::Punctuation(b'}') => depth[2] = depth[2].saturating_sub(1),
            TokenKind::Punctuation(b'<') => depth[3] = depth[3].saturating_add(1),
            TokenKind::Punctuation(b'>') => depth[3] = depth[3].saturating_sub(1),
            TokenKind::Punctuation(b',') if depth.iter().all(|level| *level == 0) => {
                if segment_start == index {
                    return None;
                }
                segments.push(segment_start..index);
                commas.push(token.range.clone());
                segment_start = index + 1;
                continue;
            }
            _ => {}
        }
    }
    if segment_start < close_index {
        segments.push(segment_start..close_index);
    }

    if segments.is_empty() {
        return None;
    }
    let close_start = tokens[close_index].range.start;
    let mut fields = Vec::with_capacity(segments.len());
    let mut names = HashSet::with_capacity(segments.len());
    for (index, segment) in segments.into_iter().enumerate() {
        let name = tokens.get(segment.start)?;
        let colon = tokens.get(segment.start + 1)?;
        if !is_wit_identifier(source, name)
            || colon.kind != TokenKind::Punctuation(b':')
            || segment.end <= segment.start + 2
        {
            return None;
        }
        let name_text = source[name.range.clone()]
            .strip_prefix('%')
            .unwrap_or(&source[name.range.clone()]);
        if !names.insert(name_text) {
            return None;
        }
        fields.push(NamedResultField {
            name: name.range.clone(),
            colon: colon.range.clone(),
            end: commas.get(index).map_or(close_start, |comma| comma.start),
        });
    }
    Some((fields, commas))
}

fn matching_delimiter(tokens: &[Token], open_index: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(open_index) {
        match token.kind {
            TokenKind::Punctuation(byte) if byte == open => depth += 1,
            TokenKind::Punctuation(byte) if byte == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn matching_open_delimiter(
    tokens: &[Token],
    close_index: usize,
    open: u8,
    close: u8,
) -> Option<usize> {
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().take(close_index + 1).rev() {
        match token.kind {
            TokenKind::Punctuation(byte) if byte == close => depth += 1,
            TokenKind::Punctuation(byte) if byte == open => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_identifier(source: &str, token: &Token, identifier: &str) -> bool {
    token.kind == TokenKind::Identifier && &source[token.range.clone()] == identifier
}

fn is_wit_identifier(source: &str, token: &Token) -> bool {
    let text = &source[token.range.clone()];
    let (identifier, explicit) = match token.kind {
        TokenKind::Identifier => (text, false),
        TokenKind::ExplicitIdentifier => {
            let Some(identifier) = text.strip_prefix('%') else {
                return false;
            };
            (identifier, true)
        }
        TokenKind::Number | TokenKind::Invalid | TokenKind::Arrow | TokenKind::Punctuation(_) => {
            return false;
        }
    };
    (explicit || !is_wit_keyword(identifier)) && wit_parser::validate_id(identifier).is_ok()
}

pub(super) fn is_wit_keyword(identifier: &str) -> bool {
    matches!(
        identifier,
        "use"
            | "type"
            | "func"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "s8"
            | "s16"
            | "s32"
            | "s64"
            | "f32"
            | "f64"
            | "char"
            | "resource"
            | "own"
            | "borrow"
            | "record"
            | "flags"
            | "variant"
            | "enum"
            | "bool"
            | "string"
            | "option"
            | "result"
            | "future"
            | "stream"
            | "error-context"
            | "list"
            | "map"
            | "_"
            | "as"
            | "from"
            | "static"
            | "interface"
            | "tuple"
            | "world"
            | "import"
            | "export"
            | "package"
            | "constructor"
            | "include"
            | "with"
            | "async"
    )
}

fn lex(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let current = source[index..]
            .chars()
            .next()
            .expect("index is a UTF-8 boundary");
        if matches!(current, ' ' | '\t' | '\n' | '\r') {
            index += current.len_utf8();
            continue;
        }
        if source[index..].starts_with("//") {
            index += 2;
            while index < source.len() {
                let next = source[index..]
                    .chars()
                    .next()
                    .expect("index is a UTF-8 boundary");
                index += next.len_utf8();
                if next == '\n' {
                    break;
                }
            }
            continue;
        }
        if source[index..].starts_with("/*") {
            index += 2;
            let mut depth = 1_u32;
            while index < source.len() && depth > 0 {
                if source[index..].starts_with("/*") {
                    depth += 1;
                    index += 2;
                } else if source[index..].starts_with("*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += source[index..]
                        .chars()
                        .next()
                        .expect("index is a UTF-8 boundary")
                        .len_utf8();
                }
            }
            continue;
        }
        if current == '"' {
            index += 1;
            while index < source.len() {
                let next = source[index..]
                    .chars()
                    .next()
                    .expect("index is a UTF-8 boundary");
                match next {
                    '\\' => {
                        index += next.len_utf8();
                        if index < source.len() {
                            index += source[index..]
                                .chars()
                                .next()
                                .expect("index is a UTF-8 boundary")
                                .len_utf8();
                        }
                    }
                    '"' => {
                        index += next.len_utf8();
                        break;
                    }
                    _ => index += next.len_utf8(),
                }
            }
            continue;
        }
        if source[index..].starts_with("->") {
            tokens.push(Token {
                kind: TokenKind::Arrow,
                range: index..index + 2,
            });
            index += 2;
            continue;
        }
        if current == '%' {
            let start = index;
            index += current.len_utf8();
            if index < source.len() && is_keylike_start(source[index..].chars().next().unwrap()) {
                while index < source.len() {
                    let next = source[index..]
                        .chars()
                        .next()
                        .expect("index is a UTF-8 boundary");
                    if !is_keylike_continue(next) {
                        break;
                    }
                    index += next.len_utf8();
                }
            }
            tokens.push(Token {
                kind: TokenKind::ExplicitIdentifier,
                range: start..index,
            });
            continue;
        }
        if current.is_ascii_digit() {
            let start = index;
            index += current.len_utf8();
            while index < source.len() {
                let next = source[index..]
                    .chars()
                    .next()
                    .expect("index is a UTF-8 boundary");
                if !next.is_ascii_digit() {
                    break;
                }
                index += next.len_utf8();
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                range: start..index,
            });
            continue;
        }
        if is_keylike_start(current) {
            let start = index;
            index += current.len_utf8();
            while index < source.len() {
                let next = source[index..]
                    .chars()
                    .next()
                    .expect("index is a UTF-8 boundary");
                if !is_keylike_continue(next) {
                    break;
                }
                index += next.len_utf8();
            }
            tokens.push(Token {
                kind: TokenKind::Identifier,
                range: start..index,
            });
            continue;
        }
        if current.is_ascii_punctuation() {
            tokens.push(Token {
                kind: TokenKind::Punctuation(current as u8),
                range: index..index + 1,
            });
            index += 1;
            continue;
        }
        tokens.push(Token {
            kind: TokenKind::Invalid,
            range: index..index + current.len_utf8(),
        });
        index += current.len_utf8();
    }
    tokens
}

fn is_keylike_start(character: char) -> bool {
    unicode_ident::is_xid_start(character) || character == '_' || character == '-'
}

fn is_keylike_continue(character: char) -> bool {
    unicode_ident::is_xid_continue(character) || character == '-'
}

#[cfg(test)]
mod tests {
    use super::{scan_foreign_references, scan_source};

    #[test]
    fn ignores_package_keywords_inside_comments_and_strings() {
        let source = r#"
// package wrong:line;
/* package wrong:block { /* package wrong:nested; */ } */
package acme:graphics@1.2.3;
interface shapes {
    annotation note(string);
    @note("package fake:unit; -> (first: u8, second: u8)")
    draw: func();
}
"#;

        let scan = scan_source(source);
        let declaration = scan
            .package_name_range
            .expect("root package declaration is recognized");
        assert_eq!(&source[declaration], "acme:graphics@1.2.3");
        assert!(scan.nested_package_ranges.is_empty());
        assert!(scan.named_result_function_ranges.is_empty());
        assert_eq!(scan.parser_text.len(), source.len());
        assert_eq!(scan.parser_to_source.len(), scan.parser_text.len() + 1);
        assert_eq!(scan.parser_to_source.last(), Some(&source.len()));
        assert!(scan.parser_text.contains("package fake:unit;"));
    }

    #[test]
    fn finds_nested_package_location_without_counting_repeated_headers() {
        let source = "package acme:graphics;\npackage acme:child { interface child {} }\n";
        let scan = scan_source(source);

        assert_eq!(
            scan.package_name_range.map(|range| &source[range]),
            Some("acme:graphics")
        );
        assert_eq!(scan.nested_package_ranges.len(), 1);
        assert_eq!(&source[scan.nested_package_ranges[0].clone()], "acme:child");

        let repeated = scan_source("package acme:graphics;\ninterface first {}\n");
        assert!(repeated.nested_package_ranges.is_empty());
    }

    #[test]
    fn rewrites_only_multiple_named_results_and_preserves_byte_offsets() {
        let source = r#"
package acme:graphics;
interface shapes {
  named: func(value: tuple<u32, string>) -> (left: u32, right: result<u32, string>);
  ordinary: func() -> tuple<u32, string>;
  note: func() -> string;
}
"#;
        let scan = scan_source(source);

        assert_eq!(scan.named_result_function_ranges.len(), 1);
        assert_eq!(
            &scan.parser_text[scan.named_result_function_ranges[0].clone()],
            "named"
        );
        assert!(scan.parser_text.contains("-> tuple<"));
        assert!(scan.parser_text.contains("u32,"));
        assert!(scan.parser_text.contains("result<u32, string>"));
        assert!(
            scan.parser_text
                .contains("ordinary: func() -> tuple<u32, string>")
        );
        assert!(scan.parser_text.contains("note: func() -> string"));
        assert_eq!(scan.parser_to_source.len(), scan.parser_text.len() + 1);
        assert_eq!(scan.parser_to_source.last(), Some(&source.len()));
    }

    #[test]
    fn retains_newlines_and_ignores_commas_inside_nested_types() {
        let source = "f: func() -> (first: tuple<u32, string>,\n second: list<u8>);";
        let scan = scan_source(source);

        assert_eq!(scan.named_result_function_ranges.len(), 1);
        let newline = source.find('\n').expect("source contains one newline");
        let parser_newline = scan
            .parser_text
            .find('\n')
            .expect("rewritten source retains the newline");
        assert_eq!(scan.parser_to_source[parser_newline], newline);
        assert_eq!(
            scan.parser_text
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
            source.bytes().filter(|byte| *byte == b'\n').count()
        );
    }

    #[test]
    fn rewrites_trailing_named_result_commas_into_valid_tuple_syntax() {
        let source = "f: func() -> (first: u8, second: u16,);";
        let scan = scan_source(source);

        assert_eq!(scan.named_result_function_ranges.len(), 1);
        assert!(scan.parser_text.contains("tuple<"));
        assert!(scan.parser_text.contains("u16,>"));
        assert_eq!(scan.parser_to_source.len(), scan.parser_text.len() + 1);
    }

    #[test]
    fn accepts_explicit_identifiers_and_preserves_invalid_result_names() {
        let escaped = "f: func() -> (%func: u8, other: u16);";
        let scan = scan_source(escaped);
        assert_eq!(scan.named_result_function_ranges.len(), 1);
        assert!(scan.parser_text.contains("tuple<"));
        assert!(scan.parser_text.contains("u8"));
        assert!(scan.parser_text.contains("u16"));
        assert!(!scan.parser_text.contains("%func"));

        for invalid in [
            "f: func() -> (1: u8, other: u16);",
            "f: func() -> (func: u8, other: u16);",
        ] {
            let scan = scan_source(invalid);
            assert!(scan.named_result_function_ranges.is_empty());
            assert_eq!(scan.parser_text, invalid);
        }
    }

    #[test]
    fn rewrites_single_named_results() {
        let source = "f: func() -> (answer: u8);";
        let scan = scan_source(source);

        assert!(scan.named_result_function_ranges.is_empty());
        assert!(scan.parser_text.contains("u8"));
        assert!(!scan.parser_text.contains("answer"));
        assert!(!scan.parser_text.contains("tuple<"));
    }

    #[test]
    fn duplicate_names_and_invalid_characters_are_never_erased() {
        for source in [
            "f: func() -> (value: u8, value: u16);",
            "f: func() -> (value\u{202e}: u8, other: u16);",
            "f: func() -> (value\u{0085}: u8, other: u16);",
        ] {
            let scan = scan_source(source);
            assert!(scan.named_result_function_ranges.is_empty());
            assert_eq!(scan.parser_text, source);
        }
    }

    #[test]
    fn preserves_comments_and_strings_between_result_labels_and_types() {
        let source = "f: func() -> (value /*\u{202e}*/: u8, other: u16);";
        let scan = scan_source(source);
        assert_eq!(scan.named_result_function_ranges.len(), 1);
        assert!(scan.parser_text.contains("/*\u{202e}*/"));

        let source = "f: func() -> (value \"not-a-label\": u8, other: u16);";
        let scan = scan_source(source);
        assert_eq!(scan.named_result_function_ranges.len(), 1);
        assert!(scan.parser_text.contains("\"not-a-label\""));
    }

    #[test]
    fn scans_foreign_references_in_use_include_import_and_export_forms() {
        let source = r#"
package root:app@1.0.0;
use acme:types/%model@1.2.3.{Thing};
world app {
    include acme:worlds/base@2.0.0;
    import wasi:cli/stdout@0.2.3;
    export wasi:http/types;
}
"#;

        let references = scan_foreign_references(source);
        assert_eq!(references.len(), 4);
        assert_eq!(references[0].identity, "acme:types");
        assert_eq!(references[0].version.as_deref(), Some("1.2.3"));
        assert_eq!(references[0].item, "model");
        assert_eq!(
            &source[references[0].range.clone()],
            "acme:types/%model@1.2.3"
        );
        assert_eq!(&source[references[0].item_range.clone()], "%model");
        assert_eq!(references[1].identity, "acme:worlds");
        assert_eq!(references[1].version.as_deref(), Some("2.0.0"));
        assert_eq!(
            &source[references[1].range.clone()],
            "acme:worlds/base@2.0.0"
        );
        assert_eq!(references[2].identity, "wasi:cli");
        assert_eq!(references[2].version.as_deref(), Some("0.2.3"));
        assert_eq!(
            &source[references[2].range.clone()],
            "wasi:cli/stdout@0.2.3"
        );
        assert_eq!(references[3].identity, "wasi:http");
        assert_eq!(references[3].version, None);
        assert_eq!(&source[references[3].range.clone()], "wasi:http/types");
    }

    #[test]
    fn scans_prerelease_and_build_version_text() {
        let source = "use acme:types/model@1.2.3-alpha.1+build.7.{Thing};";
        let references = scan_foreign_references(source);

        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].version.as_deref(),
            Some("1.2.3-alpha.1+build.7")
        );
        assert_eq!(
            &source[references[0].range.clone()],
            "acme:types/model@1.2.3-alpha.1+build.7"
        );
    }

    #[test]
    fn version_ranges_skip_comments_between_at_and_version() {
        let source = "use acme:types/model@ /* version */ 1.2.3.{Thing};";
        let references = scan_foreign_references(source);

        assert_eq!(references.len(), 1);
        assert_eq!(references[0].version.as_deref(), Some("1.2.3"));
        let version_range = references[0]
            .version_range
            .as_ref()
            .expect("explicit version has a token range");
        assert_eq!(&source[version_range.clone()], "1.2.3");
        assert_eq!(
            &source[references[0].range.clone()],
            "acme:types/model@ /* version */ 1.2.3"
        );
    }

    #[test]
    fn ignores_comments_strings_and_package_declarations() {
        let source = r#"
// use fake:comment/interface@1.0.0.{Thing};
/* include fake:comment/world; /* import fake:nested/path; */ */
package fake:root@1.0.0;
package fake:nested { interface hidden {} }
@doc("use fake:string/interface; import fake:string/path;")
world app {
    import local-api;
    export local-api;
    import acme:real/api;
}
"#;
        let references = scan_foreign_references(source);

        assert_eq!(references.len(), 1);
        assert_eq!(references[0].identity, "acme:real");
        assert_eq!(references[0].version, None);
        assert_eq!(&source[references[0].range.clone()], "acme:real/api");
    }

    #[test]
    fn foreign_reference_offsets_are_utf8_byte_ranges() {
        let prefix = "// 日本語のコメント\n";
        let reference_text = "wasi:io/error@0.2.1";
        let source = format!("{prefix}world app {{ import {reference_text}; }}");
        let references = scan_foreign_references(&source);

        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].range.start,
            prefix.len() + "world app { import ".len()
        );
        assert_eq!(&source[references[0].range.clone()], reference_text);
    }
}
