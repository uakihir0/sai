use std::collections::HashSet;
use std::ops::Range;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct SourceScan {
    pub parser_text: String,
    pub parser_to_source: Vec<usize>,
    pub package_name_range: Option<Range<usize>>,
    pub nested_package_ranges: Vec<Range<usize>>,
    pub named_result_function_ranges: Vec<Range<usize>>,
    pub function_declarations: Vec<SourceFunctionDeclaration>,
    pub alias_declarations: Vec<SourceInterfaceAlias>,
    pub record_declarations: Vec<SourceInterfaceRecord>,
    pub enum_declarations: Vec<SourceInterfaceEnum>,
    pub flags_declarations: Vec<SourceInterfaceFlags>,
    pub variant_declarations: Vec<SourceInterfaceVariant>,
    pub resource_declarations: Vec<SourceInterfaceResource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceFunctionDeclaration {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
}

/// Token sequence and source range for a freestanding function directly
/// declared by one named interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceFunctionTokens {
    /// Exact UTF-8 byte range from attached attributes through the semicolon.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token kind/text pairs. The function-name token
    /// has empty text while retaining its token kind.
    pub token_pairs: Vec<(String, String)>,
}

/// Source declaration and direct member boundaries for one authored
/// interface resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceResourceTokens {
    /// Exact UTF-8 byte range from attached attributes through `;` or `}`.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token kind/text pairs. The resource-name token
    /// is present with empty text while retaining its token kind.
    pub token_pairs: Vec<(String, String)>,
    /// Direct resource functions in source order.
    pub members: Vec<AuthoredResourceMemberTokens>,
}

/// Source declaration and direct case boundaries for one authored interface
/// enum.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceEnumTokens {
    /// Exact UTF-8 byte range from attached attributes through the closing `}`.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs for the whole enum. The enum name
    /// token is present with empty text.
    pub token_pairs: Vec<(String, String)>,
    /// Unit cases in source order.
    pub cases: Vec<AuthoredEnumCaseTokens>,
}

/// Source declaration and direct flag boundaries for one authored interface
/// flags type. Individual flag boundaries validate parent ownership and order;
/// they do not represent independent source symbols.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceFlagsTokens {
    /// Exact UTF-8 byte range from attached attributes through the closing `}`.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs for the whole flags type. The flags
    /// type name token is present with empty text; flag names remain visible.
    pub token_pairs: Vec<(String, String)>,
    /// Direct flag names in source order.
    pub flags: Vec<AuthoredFlagTokens>,
}

/// Source boundary and name-masked tokens for one flag within its parent type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredFlagTokens {
    pub name: String,
    /// Exact byte range of the flag-name token.
    pub name_range: Range<usize>,
    /// Exact UTF-8 byte range from attached attributes through the flag name.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs. This flag's name token is masked.
    pub token_pairs: Vec<(String, String)>,
}

/// Source declaration and direct case boundaries for one authored interface
/// variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceVariantTokens {
    /// Exact UTF-8 byte range from attached attributes through the closing `}`.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs for the whole variant. The variant
    /// name token is present with empty text.
    pub token_pairs: Vec<(String, String)>,
    /// Cases in source order. Payload syntax remains in each case's tokens.
    pub cases: Vec<AuthoredVariantCaseTokens>,
}

/// Source declaration and name-token boundary for one direct interface type
/// alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceAliasTokens {
    /// Exact UTF-8 byte range from attached attributes through the semicolon.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs. The alias name token is present
    /// with empty text.
    pub token_pairs: Vec<(String, String)>,
}

/// Source declaration and direct field boundaries for one authored interface
/// record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredInterfaceRecordTokens {
    /// Exact UTF-8 byte range from attached attributes through the closing `}`.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs for the whole record. The record-name
    /// token is present with empty text.
    pub token_pairs: Vec<(String, String)>,
    /// Named fields in source order, each with its own name-masked fingerprint
    /// tokens.
    pub fields: Vec<AuthoredRecordFieldTokens>,
}

/// Source declaration and name-token boundary for one named WIT record field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredRecordFieldTokens {
    pub name: String,
    /// Exact byte range of the field-name token.
    pub name_range: Range<usize>,
    /// Exact UTF-8 byte range from attached attributes through the field type.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs. The field-name token is present
    /// with empty text.
    pub token_pairs: Vec<(String, String)>,
}

/// Source declaration and name-token boundary for one unit enum case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredEnumCaseTokens {
    pub name: String,
    /// Exact byte range of the case name token.
    pub name_range: Range<usize>,
    /// Exact UTF-8 byte range from attached attributes through the case name.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token pairs. The case name token is present
    /// with empty text.
    pub token_pairs: Vec<(String, String)>,
}

/// Source declaration and name-token boundary for one named WIT variant case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredVariantCaseTokens {
    pub name: String,
    /// Exact byte range of the case name token.
    pub name_range: Range<usize>,
    /// Exact UTF-8 byte range from attached attributes through the case payload.
    pub declaration_range: Range<usize>,
    /// Whether this case has one parenthesized payload type.
    pub has_payload: bool,
    /// Ordered parser-aligned token pairs. The case name token is present
    /// with empty text while payload tokens are retained.
    pub token_pairs: Vec<(String, String)>,
}

/// Source declaration and name-token boundary for one direct resource
/// constructor, method, or static function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredResourceMemberTokens {
    pub name: String,
    pub kind: crate::WitMemberKind,
    /// Exact byte range of the constructor keyword or function name.
    pub name_range: Range<usize>,
    /// Exact UTF-8 byte range from attached attributes through `;`.
    pub declaration_range: Range<usize>,
    /// Ordered parser-aligned token kind/text pairs. The member-name token
    /// is present with empty text while retaining its token kind.
    pub token_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceInterfaceResource {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
    pub members: Vec<SourceResourceMember>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceInterfaceEnum {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
    pub cases: Vec<SourceEnumCase>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceInterfaceFlags {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
    pub flags: Vec<SourceFlag>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceInterfaceVariant {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
    pub cases: Vec<SourceVariantCase>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceInterfaceAlias {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceInterfaceRecord {
    pub interface_name: String,
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
    pub fields: Vec<SourceRecordField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceRecordField {
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceEnumCase {
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceFlag {
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceVariantCase {
    pub name: String,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub has_payload: bool,
    pub token_pairs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceResourceMember {
    pub name: String,
    pub kind: crate::WitMemberKind,
    pub name_range: Range<usize>,
    pub declaration_range: Range<usize>,
    pub token_pairs: Vec<(String, String)>,
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
    String,
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
    // Named-result rewriting historically treats quoted strings as trivia.
    // Keep that behavior for its recognition path while retaining atomic
    // string tokens for declaration-local WIT fingerprints.
    let rewrite_tokens = tokens
        .iter()
        .filter(|token| token.kind != TokenKind::String)
        .cloned()
        .collect::<Vec<_>>();
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
    for (index, token) in rewrite_tokens.iter().enumerate() {
        if token.kind != TokenKind::Arrow {
            continue;
        }
        let Some(open_index) = rewrite_tokens
            .get(index + 1)
            .and_then(|token| (token.kind == TokenKind::Punctuation(b'(')).then_some(index + 1))
        else {
            continue;
        };
        let Some(close_index) = matching_delimiter(&rewrite_tokens, open_index, b'(', b')') else {
            continue;
        };
        let Some(function_name_range) =
            function_name_range(source, &rewrite_tokens, index, open_index)
        else {
            continue;
        };
        let Some(fields_and_commas) =
            named_result_fields(source, &rewrite_tokens, open_index, close_index)
        else {
            continue;
        };
        let fields = fields_and_commas.0;
        named_results.push(NamedResults {
            range: rewrite_tokens[open_index].range.start..rewrite_tokens[close_index].range.end,
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
        function_declarations: scan_function_declarations(source, &tokens),
        alias_declarations: scan_interface_aliases(source, &tokens),
        record_declarations: scan_interface_records(source, &tokens),
        enum_declarations: scan_interface_enums(source, &tokens),
        flags_declarations: scan_interface_flags(source, &tokens),
        variant_declarations: scan_interface_variants(source, &tokens),
        resource_declarations: scan_interface_resources(source, &tokens),
    }
}

/// Scans authored WIT source and returns one direct interface type alias only
/// when its parsed name-token span identifies exactly one lexical declaration.
///
/// `use` declarations are intentionally not aliases: their re-exported names
/// have the canonical path of their defining declaration.
pub fn authored_interface_alias_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceAliasTokens> {
    let mut matches = scan_source(source)
        .alias_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceAliasTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
    })
}

/// Scans authored WIT source and returns one direct interface record only when
/// its parsed name-token span identifies exactly one lexical declaration.
///
/// Field ranges and token pairs are recovered from the same source bytes; the
/// caller binds them to the parsed model by ordered field names.
pub fn authored_interface_record_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceRecordTokens> {
    let mut matches = scan_source(source)
        .record_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceRecordTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
        fields: declaration
            .fields
            .into_iter()
            .map(|field| AuthoredRecordFieldTokens {
                name: field.name,
                name_range: field.name_range,
                declaration_range: field.declaration_range,
                token_pairs: field.token_pairs,
            })
            .collect(),
    })
}

fn scan_interface_records(source: &str, tokens: &[Token]) -> Vec<SourceInterfaceRecord> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(interface_end) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        while item_index < interface_end {
            let declaration_index = match skip_attributes(tokens, item_index, interface_end) {
                Some(index) if index < interface_end => index,
                _ => break,
            };
            if is_identifier(source, &tokens[declaration_index], "record")
                && let Some((declaration, next_index)) = scan_interface_record(
                    source,
                    tokens,
                    &interface_name,
                    declaration_index,
                    interface_end,
                )
            {
                declarations.push(declaration);
                item_index = next_index;
                continue;
            }

            // Skip the full direct item so nested record-like tokens in a
            // `use` list or another unsupported declaration cannot be
            // promoted as authored interface records.
            if tokens[declaration_index].kind == TokenKind::Punctuation(b'{')
                && let Some(nested_end) = matching_delimiter(tokens, declaration_index, b'{', b'}')
            {
                item_index = nested_end + 1;
            } else if let Some(end_index) =
                item_end_at_top_level(tokens, declaration_index, interface_end)
            {
                item_index = end_index;
            } else {
                item_index = declaration_index + 1;
            }
        }
    }
    declarations
}

fn scan_interface_record(
    source: &str,
    tokens: &[Token],
    interface_name: &str,
    record_index: usize,
    interface_end: usize,
) -> Option<(SourceInterfaceRecord, usize)> {
    let name_index = record_index.checked_add(1)?;
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let open_index = name_index.checked_add(1)?;
    if tokens.get(open_index)?.kind != TokenKind::Punctuation(b'{') {
        return None;
    }
    let close_index = matching_delimiter(tokens, open_index, b'{', b'}')?;
    if close_index >= interface_end {
        return None;
    }
    let declaration_start = attached_attributes_start(tokens, record_index)
        .map_or(tokens[record_index].range.start, |attribute_index| {
            tokens[attribute_index].range.start
        });
    let declaration_range = declaration_start..tokens[close_index].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    let fields = scan_record_fields(source, tokens, open_index + 1, close_index)?;
    Some((
        SourceInterfaceRecord {
            interface_name: interface_name.to_owned(),
            name: name.to_owned(),
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
            fields,
        },
        close_index + 1,
    ))
}

fn scan_record_fields(
    source: &str,
    tokens: &[Token],
    body_start: usize,
    body_end: usize,
) -> Option<Vec<SourceRecordField>> {
    if body_start == body_end {
        return Some(Vec::new());
    }

    let mut fields = Vec::new();
    let mut segment_start = body_start;
    let mut delimiter_stack = Vec::new();
    for index in body_start..body_end {
        let token = &tokens[index];
        match token.kind {
            TokenKind::Invalid => return None,
            TokenKind::Punctuation(b'(' | b'<' | b'[' | b'{') => {
                delimiter_stack.push(token.kind);
            }
            TokenKind::Punctuation(b')') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'(')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b'>') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'<')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b']') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'[')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b'}') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'{')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b',') if delimiter_stack.is_empty() => {
                if segment_start == index {
                    return None;
                }
                fields.push(scan_record_field(source, tokens, segment_start, index)?);
                segment_start = index + 1;
            }
            _ => {}
        }
    }
    if !delimiter_stack.is_empty() {
        return None;
    }
    if segment_start < body_end {
        fields.push(scan_record_field(source, tokens, segment_start, body_end)?);
    }
    Some(fields)
}

fn scan_record_field(
    source: &str,
    tokens: &[Token],
    segment_start: usize,
    segment_end: usize,
) -> Option<SourceRecordField> {
    let name_index = skip_attributes(tokens, segment_start, segment_end)?;
    if name_index >= segment_end {
        return None;
    }
    let name_token = &tokens[name_index];
    let name = wit_identifier_text(source, name_token)?;
    let colon_index = name_index.checked_add(1)?;
    if tokens.get(colon_index)?.kind != TokenKind::Punctuation(b':')
        || colon_index.checked_add(1)? >= segment_end
    {
        return None;
    }
    let declaration_start = attached_attributes_start(tokens, name_index)
        .map_or(name_token.range.start, |index| tokens[index].range.start);
    let declaration_range = declaration_start..tokens[segment_end - 1].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    Some(SourceRecordField {
        name: name.to_owned(),
        name_range: name_token.range.clone(),
        declaration_range,
        token_pairs,
    })
}

fn scan_interface_aliases(source: &str, tokens: &[Token]) -> Vec<SourceInterfaceAlias> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(interface_end) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        while item_index < interface_end {
            let declaration_index = match skip_attributes(tokens, item_index, interface_end) {
                Some(index) if index < interface_end => index,
                _ => break,
            };
            if is_identifier(source, &tokens[declaration_index], "type")
                && let Some((declaration, next_index)) = scan_interface_alias(
                    source,
                    tokens,
                    interface_name,
                    declaration_index,
                    interface_end,
                )
            {
                declarations.push(declaration);
                item_index = next_index;
                continue;
            }

            // Skip the current direct item, including nested `use` lists and
            // record/variant/resource bodies. A type alias-like token inside
            // an unsupported item must never be promoted as a direct alias.
            if tokens[declaration_index].kind == TokenKind::Punctuation(b'{')
                && let Some(nested_end) = matching_delimiter(tokens, declaration_index, b'{', b'}')
            {
                item_index = nested_end + 1;
            } else if let Some(end_index) =
                item_end_at_top_level(tokens, declaration_index, interface_end)
            {
                item_index = end_index;
            } else {
                item_index = declaration_index + 1;
            }
        }
    }
    declarations
}

fn scan_interface_alias(
    source: &str,
    tokens: &[Token],
    interface_name: &str,
    type_index: usize,
    interface_end: usize,
) -> Option<(SourceInterfaceAlias, usize)> {
    let name_index = type_index.checked_add(1)?;
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let equals_index = name_index.checked_add(1)?;
    if tokens.get(equals_index)?.kind != TokenKind::Punctuation(b'=') {
        return None;
    }

    let mut delimiter_stack = Vec::new();
    let mut index = equals_index + 1;
    let semicolon_index = loop {
        let token = tokens.get(index)?;
        if index >= interface_end {
            return None;
        }
        match token.kind {
            TokenKind::Invalid => return None,
            TokenKind::Punctuation(b'(' | b'<' | b'[' | b'{') => {
                delimiter_stack.push(token.kind);
            }
            TokenKind::Punctuation(b')') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'(')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b'>') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'<')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b']') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'[')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b'}') => {
                if delimiter_stack.pop() != Some(TokenKind::Punctuation(b'{')) {
                    return None;
                }
            }
            TokenKind::Punctuation(b';') if delimiter_stack.is_empty() => break index,
            _ => {}
        }
        index = index.checked_add(1)?;
    };
    if semicolon_index == equals_index + 1 {
        return None;
    }
    let declaration_start = attached_attributes_start(tokens, type_index)
        .map_or(tokens[type_index].range.start, |attribute_index| {
            tokens[attribute_index].range.start
        });
    let declaration_range = declaration_start..tokens[semicolon_index].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    Some((
        SourceInterfaceAlias {
            interface_name: interface_name.to_owned(),
            name: name.to_owned(),
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
        },
        semicolon_index + 1,
    ))
}

fn scan_function_declarations(source: &str, tokens: &[Token]) -> Vec<SourceFunctionDeclaration> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(close_index) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        let mut nested_braces = 0_u32;
        while item_index < close_index {
            if nested_braces == 0 {
                let name_token = &tokens[item_index];
                if let Some(name) = wit_identifier_text(source, name_token)
                    && tokens
                        .get(item_index + 1)
                        .is_some_and(|token| token.kind == TokenKind::Punctuation(b':'))
                {
                    let mut function_index = item_index + 2;
                    if tokens
                        .get(function_index)
                        .is_some_and(|token| is_identifier(source, token, "async"))
                    {
                        function_index += 1;
                    }
                    if tokens
                        .get(function_index)
                        .is_some_and(|token| is_identifier(source, token, "func"))
                        && let Some(end) =
                            function_declaration_end(tokens, function_index + 1, close_index)
                    {
                        let start = attached_attributes_start(tokens, item_index)
                            .map_or(name_token.range.start, |index| tokens[index].range.start);
                        if let Some(token_pairs) =
                            function_token_pairs(source, tokens, &(start..end), &name_token.range)
                        {
                            declarations.push(SourceFunctionDeclaration {
                                interface_name: interface_name.to_owned(),
                                name: name.to_owned(),
                                name_range: name_token.range.clone(),
                                declaration_range: start..end,
                                token_pairs,
                            });
                        }
                    }
                }
            }

            match tokens[item_index].kind {
                TokenKind::Punctuation(b'{') => nested_braces = nested_braces.saturating_add(1),
                TokenKind::Punctuation(b'}') => nested_braces = nested_braces.saturating_sub(1),
                _ => {}
            }
            item_index += 1;
        }
    }
    declarations
}

/// Scans authored WIT source and returns one direct interface function only
/// when its parsed name token span identifies exactly one lexical declaration.
///
/// The returned range and tokens are recovered from `source` itself, so
/// consumers can bind declaration identity to the exact VFS snapshot bytes
/// they validated separately.
pub fn authored_interface_function_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceFunctionTokens> {
    let mut matches = scan_source(source)
        .function_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceFunctionTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
    })
}

/// Scans authored WIT source and returns one direct interface resource only
/// when its parsed name-token span identifies exactly one lexical declaration.
///
/// Resource members are retained only when every direct member in the
/// resource body has a complete, bounded function declaration. The returned
/// source ranges are recovered from `source` itself.
pub fn authored_interface_resource_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceResourceTokens> {
    let mut matches = scan_source(source)
        .resource_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceResourceTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
        members: declaration
            .members
            .into_iter()
            .map(|member| AuthoredResourceMemberTokens {
                name: member.name,
                kind: member.kind,
                name_range: member.name_range,
                declaration_range: member.declaration_range,
                token_pairs: member.token_pairs,
            })
            .collect(),
    })
}

/// Scans authored WIT source and returns one direct interface enum only when
/// its parsed name-token span identifies exactly one lexical declaration.
///
/// Case ranges and token pairs are recovered from `source` itself, allowing a
/// consumer to bind every candidate to the exact tracked file snapshot.
pub fn authored_interface_enum_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceEnumTokens> {
    let mut matches = scan_source(source)
        .enum_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceEnumTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
        cases: declaration
            .cases
            .into_iter()
            .map(|case| AuthoredEnumCaseTokens {
                name: case.name,
                name_range: case.name_range,
                declaration_range: case.declaration_range,
                token_pairs: case.token_pairs,
            })
            .collect(),
    })
}

/// Scans authored WIT source and returns one direct interface flags type only
/// when its parsed name-token span identifies exactly one lexical declaration.
///
/// The parent token sequence retains ordered flag names while masking only the
/// flags type name. Per-flag ranges validate source/model order; flags are not
/// returned as independent source symbols.
pub fn authored_interface_flags_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceFlagsTokens> {
    let mut matches = scan_source(source)
        .flags_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceFlagsTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
        flags: declaration
            .flags
            .into_iter()
            .map(|flag| AuthoredFlagTokens {
                name: flag.name,
                name_range: flag.name_range,
                declaration_range: flag.declaration_range,
                token_pairs: flag.token_pairs,
            })
            .collect(),
    })
}

/// Scans authored WIT source and returns one direct interface variant only
/// when its parsed name-token span identifies exactly one lexical declaration.
///
/// Case ranges and token pairs are recovered from `source` itself. A case's
/// own name is masked, while its complete parenthesized payload remains part
/// of its fingerprint.
pub fn authored_interface_variant_tokens(
    source: &str,
    interface_name: &str,
    name: &str,
    name_range: &Range<usize>,
) -> Option<AuthoredInterfaceVariantTokens> {
    let mut matches = scan_source(source)
        .variant_declarations
        .into_iter()
        .filter(|declaration| {
            declaration.interface_name == interface_name
                && declaration.name == name
                && &declaration.name_range == name_range
        });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(AuthoredInterfaceVariantTokens {
        declaration_range: declaration.declaration_range,
        token_pairs: declaration.token_pairs,
        cases: declaration
            .cases
            .into_iter()
            .map(|case| AuthoredVariantCaseTokens {
                name: case.name,
                name_range: case.name_range,
                declaration_range: case.declaration_range,
                has_payload: case.has_payload,
                token_pairs: case.token_pairs,
            })
            .collect(),
    })
}

fn scan_interface_enums(source: &str, tokens: &[Token]) -> Vec<SourceInterfaceEnum> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(interface_end) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        while item_index < interface_end {
            let declaration_index = match skip_attributes(tokens, item_index, interface_end) {
                Some(index) if index < interface_end => index,
                _ => break,
            };
            if is_identifier(source, &tokens[declaration_index], "enum")
                && let Some((declaration, next_index)) = scan_interface_enum(
                    source,
                    tokens,
                    interface_name,
                    declaration_index,
                    interface_end,
                )
            {
                declarations.push(declaration);
                item_index = next_index;
                continue;
            }

            // Skip the current direct item, including nested record,
            // variant, resource, and use-list bodies. A keyword under an
            // unsupported declaration must not be promoted as an enum.
            if tokens[declaration_index].kind == TokenKind::Punctuation(b'{')
                && let Some(nested_end) = matching_delimiter(tokens, declaration_index, b'{', b'}')
            {
                item_index = nested_end + 1;
            } else if let Some(end_index) =
                item_end_at_top_level(tokens, declaration_index, interface_end)
            {
                item_index = end_index;
            } else {
                item_index = declaration_index + 1;
            }
        }
    }
    declarations
}

fn scan_interface_flags(source: &str, tokens: &[Token]) -> Vec<SourceInterfaceFlags> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(interface_end) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        while item_index < interface_end {
            let declaration_index = match skip_attributes(tokens, item_index, interface_end) {
                Some(index) if index < interface_end => index,
                _ => break,
            };
            if is_identifier(source, &tokens[declaration_index], "flags")
                && let Some((declaration, next_index)) = scan_interface_flags_type(
                    source,
                    tokens,
                    interface_name,
                    declaration_index,
                    interface_end,
                )
            {
                declarations.push(declaration);
                item_index = next_index;
                continue;
            }

            // Skip complete direct items, including use lists and nested
            // declaration bodies, so nested `flags` tokens are not promoted.
            if tokens[declaration_index].kind == TokenKind::Punctuation(b'{')
                && let Some(nested_end) = matching_delimiter(tokens, declaration_index, b'{', b'}')
            {
                item_index = nested_end + 1;
            } else if let Some(end_index) =
                item_end_at_top_level(tokens, declaration_index, interface_end)
            {
                item_index = end_index;
            } else {
                item_index = declaration_index + 1;
            }
        }
    }
    declarations
}

fn scan_interface_flags_type(
    source: &str,
    tokens: &[Token],
    interface_name: &str,
    flags_index: usize,
    interface_end: usize,
) -> Option<(SourceInterfaceFlags, usize)> {
    let name_index = flags_index.checked_add(1)?;
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let open_index = name_index.checked_add(1)?;
    if tokens.get(open_index)?.kind != TokenKind::Punctuation(b'{') {
        return None;
    }
    let close_index = matching_delimiter(tokens, open_index, b'{', b'}')?;
    if close_index >= interface_end {
        return None;
    }
    let declaration_start = attached_attributes_start(tokens, flags_index)
        .map_or(tokens[flags_index].range.start, |index| {
            tokens[index].range.start
        });
    let declaration_range = declaration_start..tokens[close_index].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    let flags = scan_enum_cases(source, tokens, open_index + 1, close_index)?
        .into_iter()
        .map(|flag| SourceFlag {
            name: flag.name,
            name_range: flag.name_range,
            declaration_range: flag.declaration_range,
            token_pairs: flag.token_pairs,
        })
        .collect::<Vec<_>>();
    // WIT names are unique case-insensitively within a scope. Enforce that
    // boundary here so an invalid declaration cannot leak a parent or a
    // partial set of flag constants into source inventory.
    let mut names = HashSet::new();
    if flags
        .iter()
        .any(|flag| !names.insert(flag.name.to_ascii_lowercase()))
    {
        return None;
    }
    Some((
        SourceInterfaceFlags {
            interface_name: interface_name.to_owned(),
            name: name.to_owned(),
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
            flags,
        },
        close_index + 1,
    ))
}

fn scan_interface_variants(source: &str, tokens: &[Token]) -> Vec<SourceInterfaceVariant> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(interface_end) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        while item_index < interface_end {
            let declaration_index = match skip_attributes(tokens, item_index, interface_end) {
                Some(index) if index < interface_end => index,
                _ => break,
            };
            if is_identifier(source, &tokens[declaration_index], "variant")
                && let Some((declaration, next_index)) = scan_interface_variant(
                    source,
                    tokens,
                    interface_name,
                    declaration_index,
                    interface_end,
                )
            {
                declarations.push(declaration);
                item_index = next_index;
                continue;
            }

            // Skip complete direct items, including use lists and nested
            // declaration bodies, so nested `variant` tokens are not promoted.
            if tokens[declaration_index].kind == TokenKind::Punctuation(b'{')
                && let Some(nested_end) = matching_delimiter(tokens, declaration_index, b'{', b'}')
            {
                item_index = nested_end + 1;
            } else if let Some(end_index) =
                item_end_at_top_level(tokens, declaration_index, interface_end)
            {
                item_index = end_index;
            } else {
                item_index = declaration_index + 1;
            }
        }
    }
    declarations
}

fn scan_interface_enum(
    source: &str,
    tokens: &[Token],
    interface_name: &str,
    enum_index: usize,
    interface_end: usize,
) -> Option<(SourceInterfaceEnum, usize)> {
    let name_index = enum_index.checked_add(1)?;
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let open_index = name_index.checked_add(1)?;
    if tokens.get(open_index)?.kind != TokenKind::Punctuation(b'{') {
        return None;
    }
    let close_index = matching_delimiter(tokens, open_index, b'{', b'}')?;
    if close_index >= interface_end {
        return None;
    }
    let declaration_start = attached_attributes_start(tokens, enum_index)
        .map_or(tokens[enum_index].range.start, |index| {
            tokens[index].range.start
        });
    let declaration_range = declaration_start..tokens[close_index].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    let cases = scan_enum_cases(source, tokens, open_index + 1, close_index)?;
    if cases.is_empty() {
        return None;
    }
    Some((
        SourceInterfaceEnum {
            interface_name: interface_name.to_owned(),
            name: name.to_owned(),
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
            cases,
        },
        close_index + 1,
    ))
}

fn scan_interface_variant(
    source: &str,
    tokens: &[Token],
    interface_name: &str,
    variant_index: usize,
    interface_end: usize,
) -> Option<(SourceInterfaceVariant, usize)> {
    let name_index = variant_index.checked_add(1)?;
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let open_index = name_index.checked_add(1)?;
    if tokens.get(open_index)?.kind != TokenKind::Punctuation(b'{') {
        return None;
    }
    let close_index = matching_delimiter(tokens, open_index, b'{', b'}')?;
    if close_index >= interface_end {
        return None;
    }
    let declaration_start = attached_attributes_start(tokens, variant_index)
        .map_or(tokens[variant_index].range.start, |index| {
            tokens[index].range.start
        });
    let declaration_range = declaration_start..tokens[close_index].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    let cases = scan_variant_cases(source, tokens, open_index + 1, close_index)?;
    if cases.is_empty() {
        return None;
    }
    Some((
        SourceInterfaceVariant {
            interface_name: interface_name.to_owned(),
            name: name.to_owned(),
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
            cases,
        },
        close_index + 1,
    ))
}

fn scan_variant_cases(
    source: &str,
    tokens: &[Token],
    body_start: usize,
    body_end: usize,
) -> Option<Vec<SourceVariantCase>> {
    if body_start == body_end {
        return None;
    }
    let mut cases = Vec::new();
    let mut segment_start = body_start;
    let mut delimiters = Vec::new();
    for index in body_start..body_end {
        let token = &tokens[index];
        if token.kind == TokenKind::Invalid {
            return None;
        }
        match token.kind {
            TokenKind::Punctuation(open @ (b'(' | b'<' | b'[' | b'{')) => {
                delimiters.push(open);
            }
            TokenKind::Punctuation(b')') => {
                if delimiters.pop() != Some(b'(') {
                    return None;
                }
            }
            TokenKind::Punctuation(b'>') => {
                if delimiters.pop() != Some(b'<') {
                    return None;
                }
            }
            TokenKind::Punctuation(b']') => {
                if delimiters.pop() != Some(b'[') {
                    return None;
                }
            }
            TokenKind::Punctuation(b'}') => {
                if delimiters.pop() != Some(b'{') {
                    return None;
                }
            }
            TokenKind::Punctuation(b',') if delimiters.is_empty() => {
                if segment_start == index {
                    return None;
                }
                cases.push(variant_case(source, tokens, segment_start, index)?);
                segment_start = index + 1;
            }
            _ => {}
        }
    }
    if !delimiters.is_empty() {
        return None;
    }
    if segment_start < body_end {
        cases.push(variant_case(source, tokens, segment_start, body_end)?);
    } else if cases.is_empty() {
        return None;
    }
    Some(cases)
}

fn variant_case(
    source: &str,
    tokens: &[Token],
    segment_start: usize,
    segment_end: usize,
) -> Option<SourceVariantCase> {
    let name_index = skip_attributes(tokens, segment_start, segment_end)?;
    if name_index >= segment_end {
        return None;
    }
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let (case_end_index, has_payload) = if name_index + 1 == segment_end {
        (name_index, false)
    } else {
        let payload_open = name_index.checked_add(1)?;
        if tokens.get(payload_open)?.kind != TokenKind::Punctuation(b'(') {
            return None;
        }
        let payload_close = matching_delimiter(tokens, payload_open, b'(', b')')?;
        if payload_close + 1 != segment_end || payload_close == payload_open + 1 {
            return None;
        }
        validate_balanced_tokens(tokens, payload_open + 1, payload_close)?;
        (payload_close, true)
    };
    let declaration_start = if name_index == segment_start {
        name_token.range.start
    } else {
        tokens[segment_start].range.start
    };
    let declaration_range = declaration_start..tokens[case_end_index].range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    Some(SourceVariantCase {
        name: name.to_owned(),
        name_range: name_token.range.clone(),
        declaration_range,
        has_payload,
        token_pairs,
    })
}

fn validate_balanced_tokens(tokens: &[Token], start: usize, end: usize) -> Option<()> {
    let mut delimiters = Vec::new();
    for token in tokens.get(start..end)? {
        if token.kind == TokenKind::Invalid {
            return None;
        }
        match token.kind {
            TokenKind::Punctuation(open @ (b'(' | b'<' | b'[' | b'{')) => {
                delimiters.push(open);
            }
            TokenKind::Punctuation(b')') => {
                if delimiters.pop() != Some(b'(') {
                    return None;
                }
            }
            TokenKind::Punctuation(b'>') => {
                if delimiters.pop() != Some(b'<') {
                    return None;
                }
            }
            TokenKind::Punctuation(b']') => {
                if delimiters.pop() != Some(b'[') {
                    return None;
                }
            }
            TokenKind::Punctuation(b'}') => {
                if delimiters.pop() != Some(b'{') {
                    return None;
                }
            }
            _ => {}
        }
    }
    delimiters.is_empty().then_some(())
}

fn scan_enum_cases(
    source: &str,
    tokens: &[Token],
    body_start: usize,
    body_end: usize,
) -> Option<Vec<SourceEnumCase>> {
    let mut cases = Vec::new();
    let mut segment_start = body_start;
    let mut depth = [0_u32; 3];
    for index in body_start..body_end {
        match tokens[index].kind {
            TokenKind::Punctuation(b'(') => depth[0] = depth[0].saturating_add(1),
            TokenKind::Punctuation(b')') => depth[0] = depth[0].saturating_sub(1),
            TokenKind::Punctuation(b'<') => depth[1] = depth[1].saturating_add(1),
            TokenKind::Punctuation(b'>') => depth[1] = depth[1].saturating_sub(1),
            TokenKind::Punctuation(b'[') => depth[2] = depth[2].saturating_add(1),
            TokenKind::Punctuation(b']') => depth[2] = depth[2].saturating_sub(1),
            TokenKind::Punctuation(b',') if depth.iter().all(|level| *level == 0) => {
                if segment_start == index {
                    return None;
                }
                cases.push(enum_case(source, tokens, segment_start, index)?);
                segment_start = index + 1;
            }
            _ => {}
        }
    }
    if segment_start < body_end {
        cases.push(enum_case(source, tokens, segment_start, body_end)?);
    } else if segment_start == body_start || cases.is_empty() {
        return None;
    }
    Some(cases)
}

fn enum_case(
    source: &str,
    tokens: &[Token],
    segment_start: usize,
    segment_end: usize,
) -> Option<SourceEnumCase> {
    let name_index = skip_attributes(tokens, segment_start, segment_end)?;
    if name_index.checked_add(1)? != segment_end {
        return None;
    }
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let declaration_start = if name_index == segment_start {
        name_token.range.start
    } else {
        tokens[segment_start].range.start
    };
    let declaration_range = declaration_start..name_token.range.end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    Some(SourceEnumCase {
        name: name.to_owned(),
        name_range: name_token.range.clone(),
        declaration_range,
        token_pairs,
    })
}

fn item_end_at_top_level(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut index = start;
    while index < limit {
        match tokens[index].kind {
            TokenKind::Punctuation(b';') => return Some(index + 1),
            TokenKind::Punctuation(b'{') => {
                let end = matching_delimiter(tokens, index, b'{', b'}')?;
                if end >= limit {
                    return None;
                }
                return Some(end + 1);
            }
            _ => index += 1,
        }
    }
    None
}

fn scan_interface_resources(source: &str, tokens: &[Token]) -> Vec<SourceInterfaceResource> {
    let mut declarations = Vec::new();
    for (interface_index, interface_token) in tokens.iter().enumerate() {
        if !is_identifier(source, interface_token, "interface") {
            continue;
        }
        let Some(interface_name_token) = tokens.get(interface_index + 1) else {
            continue;
        };
        let Some(interface_name) = wit_identifier_text(source, interface_name_token) else {
            continue;
        };
        let open_index = interface_index + 2;
        if !tokens
            .get(open_index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'{'))
        {
            continue;
        }
        let Some(close_index) = matching_delimiter(tokens, open_index, b'{', b'}') else {
            continue;
        };

        let mut item_index = open_index + 1;
        while item_index < close_index {
            if is_identifier(source, &tokens[item_index], "resource") {
                if let Some((declaration, next_index)) =
                    scan_interface_resource(source, tokens, interface_name, item_index, close_index)
                {
                    declarations.push(declaration);
                    item_index = next_index;
                    continue;
                }
            }

            // Skip nested declarations such as record/variant bodies. A
            // resource nested below one of them is not an interface item.
            if tokens[item_index].kind == TokenKind::Punctuation(b'{')
                && let Some(nested_end) = matching_delimiter(tokens, item_index, b'{', b'}')
            {
                item_index = nested_end + 1;
            } else {
                item_index += 1;
            }
        }
    }
    declarations
}

fn scan_interface_resource(
    source: &str,
    tokens: &[Token],
    interface_name: &str,
    resource_index: usize,
    interface_end: usize,
) -> Option<(SourceInterfaceResource, usize)> {
    let name_index = resource_index.checked_add(1)?;
    let name_token = tokens.get(name_index)?;
    let name = wit_identifier_text(source, name_token)?;
    let after_name = name_index.checked_add(1)?;
    let declaration_start = attached_attributes_start(tokens, resource_index)
        .map_or(tokens[resource_index].range.start, |index| {
            tokens[index].range.start
        });

    let (declaration_end, next_index, members) = match tokens.get(after_name)?.kind {
        TokenKind::Punctuation(b';') => {
            let end_token = &tokens[after_name];
            (end_token.range.end, after_name + 1, Vec::new())
        }
        TokenKind::Punctuation(b'{') => {
            let body_end = matching_delimiter(tokens, after_name, b'{', b'}')?;
            if body_end >= interface_end {
                return None;
            }
            let members = scan_resource_members(source, tokens, after_name + 1, body_end)?;
            (tokens[body_end].range.end, body_end + 1, members)
        }
        _ => return None,
    };
    let declaration_range = declaration_start..declaration_end;
    let token_pairs = function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
    Some((
        SourceInterfaceResource {
            interface_name: interface_name.to_owned(),
            name: name.to_owned(),
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
            members,
        },
        next_index,
    ))
}

fn scan_resource_members(
    source: &str,
    tokens: &[Token],
    mut item_index: usize,
    body_end: usize,
) -> Option<Vec<SourceResourceMember>> {
    let mut members = Vec::new();
    while item_index < body_end {
        let name_index = skip_attributes(tokens, item_index, body_end)?;
        if name_index >= body_end {
            return None;
        }
        let name_token = tokens.get(name_index)?;
        let name = if is_identifier(source, name_token, "constructor") {
            "constructor"
        } else {
            wit_identifier_text(source, name_token)?
        };
        let (kind, end) = if name == "constructor"
            && tokens
                .get(name_index + 1)
                .is_some_and(|token| token.kind == TokenKind::Punctuation(b'('))
        {
            (
                crate::WitMemberKind::Constructor,
                resource_function_declaration_end(tokens, name_index + 1, body_end)?,
            )
        } else {
            let colon_index = name_index.checked_add(1)?;
            if tokens.get(colon_index)?.kind != TokenKind::Punctuation(b':') {
                return None;
            }
            let mut function_index = colon_index + 1;
            let is_static = tokens
                .get(function_index)
                .is_some_and(|token| is_identifier(source, token, "static"));
            if is_static {
                function_index += 1;
            }
            if tokens
                .get(function_index)
                .is_some_and(|token| is_identifier(source, token, "async"))
            {
                function_index += 1;
            }
            if !tokens
                .get(function_index)
                .is_some_and(|token| is_identifier(source, token, "func"))
            {
                return None;
            }
            let end = resource_function_declaration_end(tokens, function_index + 1, body_end)?;
            (
                if is_static {
                    crate::WitMemberKind::Static
                } else {
                    crate::WitMemberKind::Method
                },
                end,
            )
        };
        let start = attached_attributes_start(tokens, name_index)
            .map_or(name_token.range.start, |index| tokens[index].range.start);
        let declaration_range = start..end;
        let token_pairs =
            function_token_pairs(source, tokens, &declaration_range, &name_token.range)?;
        members.push(SourceResourceMember {
            name: name.to_owned(),
            kind,
            name_range: name_token.range.clone(),
            declaration_range,
            token_pairs,
        });
        item_index = tokens.partition_point(|token| token.range.end <= end);
    }
    Some(members)
}

fn skip_attributes(tokens: &[Token], mut index: usize, limit: usize) -> Option<usize> {
    while index < limit {
        if tokens[index].kind != TokenKind::Punctuation(b'@') {
            break;
        }
        index = index.checked_add(1)?;
        if !matches!(
            tokens.get(index)?.kind,
            TokenKind::Identifier | TokenKind::ExplicitIdentifier
        ) {
            return None;
        }
        index += 1;
        if tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Punctuation(b'('))
        {
            index = matching_delimiter(tokens, index, b'(', b')')?.checked_add(1)?;
        }
        if index > limit {
            return None;
        }
    }
    Some(index)
}

fn function_token_pairs(
    source: &str,
    tokens: &[Token],
    declaration_range: &Range<usize>,
    name_range: &Range<usize>,
) -> Option<Vec<(String, String)>> {
    let mut matched_name = false;
    let mut token_pairs = Vec::new();
    for token in tokens.iter().filter(|token| {
        declaration_range.start <= token.range.start && token.range.end <= declaration_range.end
    }) {
        let is_name = &token.range == name_range;
        if is_name {
            if matched_name {
                return None;
            }
            matched_name = true;
        }
        token_pairs.push((
            stable_wit_token_kind(source, token),
            if is_name {
                String::new()
            } else {
                source[token.range.clone()].to_owned()
            },
        ));
    }
    matched_name.then_some(token_pairs)
}

fn stable_wit_token_kind(source: &str, token: &Token) -> String {
    match token.kind {
        TokenKind::Identifier => {
            let text = &source[token.range.clone()];
            if is_wit_keyword(text) {
                wit_keyword_token_name(text).to_owned()
            } else {
                "Id".to_owned()
            }
        }
        TokenKind::ExplicitIdentifier => "ExplicitId".to_owned(),
        TokenKind::Number => "Integer".to_owned(),
        TokenKind::String => "StringLiteral".to_owned(),
        TokenKind::Invalid => "Invalid".to_owned(),
        TokenKind::Arrow => "RArrow".to_owned(),
        TokenKind::Punctuation(byte) => wit_punctuation_token_name(byte).to_owned(),
    }
}

fn wit_keyword_token_name(keyword: &str) -> &'static str {
    match keyword {
        "use" => "Use",
        "type" => "Type",
        "func" => "Func",
        "u8" => "U8",
        "u16" => "U16",
        "u32" => "U32",
        "u64" => "U64",
        "s8" => "S8",
        "s16" => "S16",
        "s32" => "S32",
        "s64" => "S64",
        "f32" => "F32",
        "f64" => "F64",
        "char" => "Char",
        "resource" => "Resource",
        "own" => "Own",
        "borrow" => "Borrow",
        "record" => "Record",
        "flags" => "Flags",
        "variant" => "Variant",
        "enum" => "Enum",
        "bool" => "Bool",
        "string" => "String_",
        "option" => "Option_",
        "result" => "Result_",
        "future" => "Future",
        "stream" => "Stream",
        "error-context" => "ErrorContext",
        "list" => "List",
        "map" => "Map",
        "_" => "Underscore",
        "as" => "As",
        "from" => "From_",
        "static" => "Static",
        "interface" => "Interface",
        "tuple" => "Tuple",
        "import" => "Import",
        "export" => "Export",
        "world" => "World",
        "package" => "Package",
        "constructor" => "Constructor",
        "async" => "Async",
        "include" => "Include",
        "with" => "With",
        _ => "Id",
    }
}

fn wit_punctuation_token_name(byte: u8) -> &'static str {
    match byte {
        b'=' => "Equals",
        b',' => "Comma",
        b':' => "Colon",
        b'.' => "Period",
        b';' => "Semicolon",
        b'(' => "LeftParen",
        b')' => "RightParen",
        b'{' => "LeftBrace",
        b'}' => "RightBrace",
        b'<' => "LessThan",
        b'>' => "GreaterThan",
        b'*' => "Star",
        b'@' => "At",
        b'/' => "Slash",
        b'+' => "Plus",
        b'-' => "Minus",
        _ => "Invalid",
    }
}
fn function_declaration_end(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut depth = [0_u32; 4];
    for token in tokens.iter().take(limit).skip(start) {
        match token.kind {
            TokenKind::Punctuation(b'(') => depth[0] = depth[0].checked_add(1)?,
            TokenKind::Punctuation(b')') => {
                depth[0] = depth[0].checked_sub(1)?;
            }
            TokenKind::Punctuation(b'[') => depth[1] = depth[1].checked_add(1)?,
            TokenKind::Punctuation(b']') => {
                depth[1] = depth[1].checked_sub(1)?;
            }
            TokenKind::Punctuation(b'{') => depth[2] = depth[2].checked_add(1)?,
            TokenKind::Punctuation(b'}') => {
                depth[2] = depth[2].checked_sub(1)?;
            }
            TokenKind::Punctuation(b'<') => depth[3] = depth[3].checked_add(1)?,
            TokenKind::Punctuation(b'>') => {
                depth[3] = depth[3].checked_sub(1)?;
            }
            TokenKind::Punctuation(b';') if depth.iter().all(|level| *level == 0) => {
                return Some(token.range.end);
            }
            _ => {}
        }
    }
    None
}

fn resource_function_declaration_end(
    tokens: &[Token],
    start: usize,
    limit: usize,
) -> Option<usize> {
    let mut delimiters = Vec::new();
    for token in tokens.iter().take(limit).skip(start) {
        match token.kind {
            TokenKind::Punctuation(open @ (b'(' | b'[' | b'{' | b'<')) => {
                delimiters.push(open);
            }
            TokenKind::Punctuation(close @ (b')' | b']' | b'}' | b'>')) => {
                let expected_open = match close {
                    b')' => b'(',
                    b']' => b'[',
                    b'}' => b'{',
                    b'>' => b'<',
                    _ => unreachable!(),
                };
                if delimiters.pop()? != expected_open {
                    return None;
                }
            }
            TokenKind::Punctuation(b';') if delimiters.is_empty() => {
                return Some(token.range.end);
            }
            _ => {}
        }
    }
    None
}

fn attached_attributes_start(tokens: &[Token], name_index: usize) -> Option<usize> {
    let mut first_attribute = None;
    let mut end = name_index;
    while end > 0 {
        let attribute = if tokens[end - 1].kind == TokenKind::Punctuation(b')') {
            let close = end - 1;
            let open = matching_open_delimiter(tokens, close, b'(', b')')?;
            let name = open.checked_sub(1)?;
            let at = name.checked_sub(1)?;
            (tokens[name].kind == TokenKind::Identifier
                && tokens[at].kind == TokenKind::Punctuation(b'@'))
            .then_some(at)
        } else if end >= 2
            && tokens[end - 1].kind == TokenKind::Identifier
            && tokens[end - 2].kind == TokenKind::Punctuation(b'@')
        {
            Some(end - 2)
        } else {
            None
        };
        let Some(attribute) = attribute else {
            break;
        };
        first_attribute = Some(attribute);
        end = attribute;
    }
    first_attribute
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
    let text = &source[token.range.clone()];
    match token.kind {
        TokenKind::ExplicitIdentifier => {
            let identifier = text.strip_prefix('%')?;
            wit_parser::validate_id(identifier)
                .is_ok()
                .then_some(identifier)
        }
        TokenKind::String => {
            let identifier = text.strip_prefix('"')?.strip_suffix('"')?;
            if identifier.contains('\\') {
                return None;
            }
            wit_parser::validate_id(identifier)
                .is_ok()
                .then_some(identifier)
        }
        TokenKind::Identifier if !is_wit_keyword(text) => {
            wit_parser::validate_id(text).is_ok().then_some(text)
        }
        _ => None,
    }
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
    wit_identifier_text(source, token).is_some()
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

/// A tokenized WIT file used to recover documentation for its world includes.
///
/// Keeping this scanner for the whole source file avoids lexing the complete
/// file again for every include.
pub(super) struct WorldIncludeDocScanner<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    include_indices: Vec<usize>,
}

impl<'a> WorldIncludeDocScanner<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        let tokens = lex(source);
        let include_indices = tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| is_identifier(source, token, "include"))
            .map(|(index, _)| index)
            .collect();
        Self {
            source,
            tokens,
            include_indices,
        }
    }

    /// Recover the documentation trivia attached to the include whose target
    /// starts at `target_start`.
    ///
    /// `wit-parser` 0.246.0 parses the documentation passed to
    /// `WorldItem::Include` but drops it from its unresolved and resolved world
    /// models. The include's parser span points at the target name, so locate
    /// the preceding `include` token and reproduce `wit-parser`'s
    /// documentation normalization from the original source.
    pub(super) fn docs(&self, target_start: usize) -> Option<String> {
        let include_position = self
            .include_indices
            .partition_point(|index| self.tokens[*index].range.start < target_start);
        let include_index = *self.include_indices.get(include_position.checked_sub(1)?)?;

        // `parse_docs` runs before the include's attributes, so documentation
        // is immediately before the first attribute rather than the `include`
        // token.
        let mut docs_end = include_index;
        while let Some(attribute_start) = trailing_attribute_start(&self.tokens, docs_end) {
            docs_end = attribute_start;
        }
        let trivia_end = self.tokens[docs_end].range.start;
        let trivia_start = self.tokens[..docs_end]
            .last()
            .map_or(0, |token| token.range.end);
        let comments = comments_in_trivia(self.source, trivia_start..trivia_end)?;
        normalize_doc_comments(&comments)
    }
}

/// Convenience wrapper for single-use callers such as source-scanner tests.
#[cfg(test)]
fn world_include_docs(source: &str, target_start: usize) -> Option<String> {
    WorldIncludeDocScanner::new(source).docs(target_start)
}

fn trailing_attribute_start(tokens: &[Token], end: usize) -> Option<usize> {
    let close = end.checked_sub(1)?;
    if tokens[close].kind != TokenKind::Punctuation(b')') {
        return None;
    }
    let open = matching_open_delimiter(tokens, close, b'(', b')')?;
    let name = open.checked_sub(1)?;
    let at = name.checked_sub(1)?;
    (tokens[name].kind == TokenKind::Identifier && tokens[at].kind == TokenKind::Punctuation(b'@'))
        .then_some(at)
}

fn comments_in_trivia(source: &str, range: Range<usize>) -> Option<Vec<&str>> {
    let mut comments = Vec::new();
    let mut index = range.start;
    while index < range.end {
        let ch = source[index..].chars().next()?;
        if ch.is_ascii_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        if source[index..].starts_with("//") {
            let start = index;
            index += 2;
            while index < range.end {
                let ch = source[index..].chars().next()?;
                if ch == '\n' {
                    break;
                }
                index += ch.len_utf8();
            }
            comments.push(&source[start..index]);
            continue;
        }
        if source[index..].starts_with("/*") {
            let start = index;
            index += 2;
            let mut depth = 1_u32;
            while index < range.end && depth > 0 {
                if source[index..].starts_with("/*") {
                    depth += 1;
                    index += 2;
                } else if source[index..].starts_with("*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += source[index..].chars().next()?.len_utf8();
                }
            }
            if depth != 0 {
                return None;
            }
            comments.push(&source[start..index]);
            continue;
        }
        return None;
    }
    Some(comments)
}

fn normalize_doc_comments(comments: &[&str]) -> Option<String> {
    if comments.is_empty() {
        return None;
    }
    let docs = comments
        .iter()
        .map(|comment| {
            let contents = match comment.strip_prefix("/**") {
                Some(contents) => contents.strip_suffix("*/")?,
                None => comment.trim_start_matches('/'),
            };
            Some(contents.trim_end().to_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    let min_leading_ws = docs
        .iter()
        .filter(|doc| !doc.is_empty())
        .map(|doc| {
            doc.bytes()
                .take_while(|byte| byte.is_ascii_whitespace())
                .count()
        })
        .min()
        .unwrap_or(0);
    let leading_ws = " ".repeat(min_leading_ws);
    let docs = docs
        .iter()
        .map(|doc| {
            if min_leading_ws == 0 {
                doc.as_str()
            } else {
                doc.strip_prefix(&leading_ws).unwrap_or(doc)
            }
        })
        .collect::<Vec<_>>();

    let mut contents = String::new();
    for doc in docs {
        if doc.is_empty() {
            contents.push('\n');
        } else {
            for line in doc.lines() {
                contents.push_str(line);
                contents.push('\n');
            }
        }
    }
    while contents.ends_with('\n') {
        contents.pop();
    }
    Some(contents)
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
            let start = index;
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
            tokens.push(Token {
                kind: TokenKind::String,
                range: start..index,
            });
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
        if current == '-' {
            tokens.push(Token {
                kind: TokenKind::Punctuation(b'-'),
                range: index..index + 1,
            });
            index += 1;
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
    use super::{
        SourceFunctionDeclaration, SourceInterfaceAlias, SourceInterfaceEnum, SourceInterfaceFlags,
        SourceInterfaceRecord, SourceInterfaceResource, SourceInterfaceVariant,
        authored_interface_alias_tokens, authored_interface_enum_tokens,
        authored_interface_flags_tokens, authored_interface_record_tokens,
        authored_interface_resource_tokens, authored_interface_variant_tokens, lex,
        scan_foreign_references, scan_source, stable_wit_token_kind, world_include_docs,
    };

    fn function(source: &str, name: &str) -> SourceFunctionDeclaration {
        scan_source(source)
            .function_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface function")
    }

    fn resource(source: &str, name: &str) -> SourceInterfaceResource {
        scan_source(source)
            .resource_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface resource")
    }

    fn interface_enum(source: &str, name: &str) -> SourceInterfaceEnum {
        scan_source(source)
            .enum_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface enum")
    }

    fn interface_flags(source: &str, name: &str) -> SourceInterfaceFlags {
        scan_source(source)
            .flags_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface flags type")
    }

    fn interface_variant(source: &str, name: &str) -> SourceInterfaceVariant {
        scan_source(source)
            .variant_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface variant")
    }

    fn interface_alias(source: &str, name: &str) -> SourceInterfaceAlias {
        scan_source(source)
            .alias_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface alias")
    }

    fn interface_record(source: &str, name: &str) -> SourceInterfaceRecord {
        scan_source(source)
            .record_declarations
            .into_iter()
            .find(|declaration| declaration.name == name)
            .expect("direct interface record")
    }

    #[test]
    fn direct_alias_ranges_include_attributes_and_mask_only_the_name() {
        let source = concat!(
            "interface api {\n",
            "  @since(version = 1.2.3)\n",
            "  type %type = tuple<u32, result<string, u32>>;\n",
            "}\n",
        );
        let declaration = interface_alias(source, "type");
        let name_start = source.find("%type").expect("escaped alias name occurs");
        let name_range = name_start..name_start + "%type".len();
        assert_eq!(declaration.interface_name, "api");
        assert_eq!(declaration.name_range, name_range);
        assert_eq!(
            &source[declaration.declaration_range.clone()],
            "@since(version = 1.2.3)\n  type %type = tuple<u32, result<string, u32>>;"
        );
        assert_eq!(
            declaration
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1
        );
        assert_eq!(
            declaration
                .token_pairs
                .iter()
                .find(|(_, text)| text.is_empty())
                .expect("the alias name token is masked"),
            &("ExplicitId".into(), String::new())
        );

        let selected = authored_interface_alias_tokens(source, "api", "type", &name_range)
            .expect("the exact parser name span selects this direct alias");
        assert_eq!(selected.declaration_range, declaration.declaration_range);
        assert_eq!(selected.token_pairs, declaration.token_pairs);
        assert!(
            authored_interface_alias_tokens(source, "api", "type", &(0..0)).is_none(),
            "an absent parser name span cannot identify an alias"
        );
    }

    #[test]
    fn alias_fingerprints_ignore_only_own_name_and_scanner_excludes_non_aliases() {
        fn token_pairs(source: &str, name: &str) -> Vec<(String, String)> {
            interface_alias(source, name).token_pairs
        }

        let original = token_pairs("interface api { type count = u32; }", "count");
        let renamed = token_pairs("interface api {\n  type size = u32;\n}", "size");
        let changed_target = token_pairs("interface api { type count = u64; }", "count");
        assert_eq!(original, renamed);
        assert_ne!(original, changed_target);

        let source = concat!(
            "interface api {\n",
            "  use other.{%type as imported};\n",
            "  record container { value: tuple<u32, u32> }\n",
            "  enum state { type, ready }\n",
            "  type visible = option<tuple<u32, string>>;\n",
            "}\n",
        );
        assert_eq!(
            scan_source(source)
                .alias_declarations
                .iter()
                .map(|declaration| declaration.name.as_str())
                .collect::<Vec<_>>(),
            ["visible"],
            "use members and nested type-like tokens are not direct aliases"
        );

        for invalid in [
            "interface api { type empty = ; }",
            "interface api { type unclosed = tuple<u32, string; }",
            "interface api { type mismatched = tuple<u32, string] ; }",
            "interface api { type missing-semicolon = u32 }",
        ] {
            assert!(
                scan_source(invalid).alias_declarations.is_empty(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn direct_record_and_field_ranges_include_attributes_and_mask_own_names() {
        let source = concat!(
            "interface api {\n",
            "  @since(version = 1.2.3)\n",
            "  record %type {\n",
            "    @since(version = 1.2.3)\n",
            "    %type: tuple<u32, result<string, u32>>,\n",
            "    amount: option<tuple<u8, u16>>,\n",
            "  }\n",
            "  record empty {}\n",
            "}\n",
        );
        let record = interface_record(source, "type");
        let record_start = source.find("%type").expect("record name occurs");
        let record_name_range = record_start..record_start + "%type".len();
        assert_eq!(record.interface_name, "api");
        assert_eq!(record.name_range, record_name_range);
        assert_eq!(
            &source[record.declaration_range.clone()],
            "@since(version = 1.2.3)\n  record %type {\n    @since(version = 1.2.3)\n    %type: tuple<u32, result<string, u32>>,\n    amount: option<tuple<u8, u16>>,\n  }"
        );
        assert_eq!(
            record
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1,
            "only the record's own name is masked in its declaration"
        );
        assert_eq!(
            record
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["type", "amount"]
        );
        let field_start = source.find("%type:").expect("escaped field name occurs");
        assert_eq!(&source[record.fields[0].name_range.clone()], "%type");
        assert_eq!(
            &source[record.fields[0].declaration_range.clone()],
            "@since(version = 1.2.3)\n    %type: tuple<u32, result<string, u32>>"
        );
        assert_eq!(
            record.fields[0].name_range,
            field_start..field_start + "%type".len()
        );
        assert_eq!(
            record.fields[0]
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1,
            "the field fingerprint masks exactly its own name"
        );

        let selected = authored_interface_record_tokens(source, "api", "type", &record_name_range)
            .expect("the exact parsed record name span selects the declaration");
        assert_eq!(selected.declaration_range, record.declaration_range);
        assert_eq!(selected.token_pairs, record.token_pairs);
        assert_eq!(selected.fields.len(), 2);
        assert!(
            authored_interface_record_tokens(source, "api", "type", &(0..0)).is_none(),
            "a stale or missing parsed name span cannot select a record"
        );
        let empty = authored_interface_record_tokens(
            source,
            "api",
            "empty",
            &(source.find("empty").expect("empty record name occurs")
                ..source.find("empty").expect("empty record name occurs") + "empty".len()),
        )
        .expect("an empty record has an exact declaration boundary");
        assert!(empty.fields.is_empty());
    }

    #[test]
    fn record_and_field_fingerprints_omit_only_their_own_names() {
        fn record(source: &str, name: &str) -> SourceInterfaceRecord {
            interface_record(source, name)
        }

        let original_source = "interface api { record profile { user-id: u32, enabled: bool, } }";
        let renamed_record_source =
            "interface api { record account { user-id: u32, enabled: bool, } }";
        let renamed_field_source =
            "interface api { record profile { account-id: u32, enabled: bool, } }";
        let changed_field_type_source =
            "interface api { record profile { user-id: u64, enabled: bool, } }";
        let original = record(original_source, "profile");
        let renamed_record = record(renamed_record_source, "account");
        let renamed_field = record(renamed_field_source, "profile");
        let changed_field_type = record(changed_field_type_source, "profile");

        assert_eq!(original.token_pairs, renamed_record.token_pairs);
        assert_eq!(
            original.fields[0].token_pairs,
            renamed_field.fields[0].token_pairs
        );
        assert_ne!(original.token_pairs, renamed_field.token_pairs);
        assert_ne!(
            original.fields[0].token_pairs,
            changed_field_type.fields[0].token_pairs
        );
        assert_eq!(
            original.fields[1].token_pairs, renamed_field.fields[1].token_pairs,
            "an unrelated field rename does not alter a sibling field fingerprint"
        );
    }

    #[test]
    fn record_scanner_excludes_use_members_and_nested_record_like_tokens() {
        let source = concat!(
            "interface api {\n",
            "  use other.{record as imported};\n",
            "  variant state { ready(record { fake: u32 }) }\n",
            "  resource store { read: func(); }\n",
            "  record visible { value: tuple<u32, result<string, u32>>, }\n",
            "}\n",
        );
        assert_eq!(
            scan_source(source)
                .record_declarations
                .iter()
                .map(|declaration| declaration.name.as_str())
                .collect::<Vec<_>>(),
            ["visible"],
            "only direct interface records are source declarations"
        );

        for invalid in [
            "interface api { record missing-type { field: } }",
            "interface api { record missing-colon { field u32 } }",
            "interface api { record empty-field { , } }",
            "interface api { record mismatched { field: tuple<u32, string] } }",
            "interface api { record unclosed { field: tuple<u32, string } }",
        ] {
            assert!(
                scan_source(invalid).record_declarations.is_empty(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn direct_enum_and_case_ranges_preserve_order_and_mask_declared_names() {
        let source = concat!(
            "interface api {\n",
            "  @since(version = 1.2.3)\n",
            "  enum status {\n",
            "    ready,\n",
            "    not-ready,\n",
            "  }\n",
            "}\n",
        );
        let declaration = interface_enum(source, "status");
        assert_eq!(declaration.interface_name, "api");
        assert_eq!(
            &source[declaration.declaration_range.clone()],
            "@since(version = 1.2.3)\n  enum status {\n    ready,\n    not-ready,\n  }"
        );
        assert_eq!(
            declaration
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            ["ready", "not-ready"]
        );
        assert_eq!(
            &source[declaration.cases[1].declaration_range.clone()],
            "not-ready"
        );
        assert_eq!(
            declaration
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1
        );
        assert_eq!(
            declaration.cases[0]
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1
        );
        let selected =
            authored_interface_enum_tokens(source, "api", "status", &declaration.name_range)
                .expect("the exact parsed-name span selects this enum");
        assert_eq!(selected.declaration_range, declaration.declaration_range);
        assert_eq!(selected.token_pairs, declaration.token_pairs);
        assert_eq!(selected.cases.len(), 2);
        assert!(
            authored_interface_enum_tokens(source, "api", "status", &(0..0)).is_none(),
            "an absent parser name span cannot identify an enum"
        );
    }

    #[test]
    fn enum_case_fingerprints_ignore_case_spelling_trivia_and_siblings() {
        let first = interface_enum("interface api { enum state { ready, pending } }", "state");
        let renamed = interface_enum(
            "interface api {\n enum status { ready , pending , failed }\n}",
            "status",
        );
        assert_eq!(first.token_pairs[0].0, renamed.token_pairs[0].0);
        assert_ne!(
            first.token_pairs, renamed.token_pairs,
            "the enum fingerprint includes the ordered case names"
        );
        assert_eq!(
            first.cases[0].token_pairs, renamed.cases[0].token_pairs,
            "the first case fingerprint is independent of enum spelling, trivia, and siblings"
        );
        let changed = interface_enum("interface api { enum state { ready, completed } }", "state");
        assert_eq!(first.cases[0].token_pairs, changed.cases[0].token_pairs);
        assert_eq!(
            first.cases[1].token_pairs, changed.cases[1].token_pairs,
            "a unit case's declaration-local fingerprint masks its own name"
        );
    }

    #[test]
    fn enum_scanner_ignores_nested_declarations_and_rejects_payload_or_malformed_cases() {
        let source =
            "interface api { record outer { value: tuple<enum, u32> } enum visible { yes, no } }";
        let declarations = scan_source(source).enum_declarations;
        assert_eq!(
            declarations
                .iter()
                .map(|declaration| declaration.name.as_str())
                .collect::<Vec<_>>(),
            ["visible"]
        );

        for invalid in [
            "interface api { enum empty {} }",
            "interface api { enum payload { item(u32), other } }",
            "interface api { enum missing-comma { first second } }",
            "interface api { enum repeated { first, , second } }",
            "interface api { enum unfinished { first, second",
        ] {
            assert!(
                scan_source(invalid).enum_declarations.is_empty(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn direct_flags_ranges_preserve_order_and_mask_only_the_parent_name() {
        let source = concat!(
            "interface api {\n",
            "  @since(version = 1.2.3)\n",
            "  flags %permissions {\n",
            "    @deprecated(version = 2.0.0)\n",
            "    read-only,\n",
            "    write,\n",
            "    %type,\n",
            "  }\n",
            "}\n",
        );
        let declaration = interface_flags(source, "permissions");
        let name_start = source.find("%permissions").expect("flags type name occurs");
        let name_range = name_start..name_start + "%permissions".len();
        assert_eq!(declaration.interface_name, "api");
        assert_eq!(declaration.name_range, name_range);
        assert_eq!(
            &source[declaration.declaration_range.clone()],
            "@since(version = 1.2.3)\n  flags %permissions {\n    @deprecated(version = 2.0.0)\n    read-only,\n    write,\n    %type,\n  }"
        );
        assert_eq!(
            declaration
                .flags
                .iter()
                .map(|flag| flag.name.as_str())
                .collect::<Vec<_>>(),
            ["read-only", "write", "type"]
        );
        assert_eq!(
            declaration
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1,
            "only the parent type name is masked in the whole declaration"
        );
        assert!(
            declaration
                .token_pairs
                .iter()
                .any(|(_, text)| text == "read-only")
                && declaration
                    .token_pairs
                    .iter()
                    .any(|(_, text)| text == "%type"),
            "flag names remain visible in the parent fingerprint"
        );
        assert_eq!(
            declaration.flags[0].token_pairs[0].1, "@",
            "flag attributes are part of the flag's source span"
        );
        assert_eq!(
            declaration.flags[0]
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1,
            "each boundary can mask its own flag name for source identity checks"
        );

        let selected = authored_interface_flags_tokens(source, "api", "permissions", &name_range)
            .expect("the exact parsed parent-name span selects these flags");
        assert_eq!(selected.declaration_range, declaration.declaration_range);
        assert_eq!(selected.token_pairs, declaration.token_pairs);
        assert_eq!(selected.flags.len(), 3);
        assert!(
            authored_interface_flags_tokens(source, "other", "permissions", &name_range).is_none(),
            "another interface cannot claim the direct declaration"
        );
        assert!(
            authored_interface_flags_tokens(source, "api", "permissions", &(0..0)).is_none(),
            "an absent parser name span cannot select a flags type"
        );
    }

    #[test]
    fn flags_parent_fingerprint_keeps_ordered_flag_names() {
        fn parent(source: &str, name: &str) -> Vec<(String, String)> {
            interface_flags(source, name).token_pairs
        }

        let original = parent("interface api { flags access { read, write } }", "access");
        let renamed = parent(
            "interface api { flags permission-set { read, write } }",
            "permission-set",
        );
        let reordered = parent("interface api { flags access { write, read } }", "access");
        let changed = parent("interface api { flags access { read, execute } }", "access");
        assert_eq!(
            original, renamed,
            "the flags type's own name is the only masked name"
        );
        assert_ne!(
            original, reordered,
            "flag declaration order affects the parent fingerprint"
        );
        assert_ne!(
            original, changed,
            "changing a flag name affects the parent fingerprint"
        );
    }

    #[test]
    fn flags_scanner_excludes_use_members_and_nested_keyword_occurrences() {
        let source = concat!(
            "interface types { flags imported { remote } }\n",
            "interface api {\n",
            "  use types.{imported as flags};\n",
            "  record holder { value: tuple<flags, u32> }\n",
            "  variant outcome { nested(flags) }\n",
            "  resource store { read: func(); }\n",
            "  @since(version = 1.0.0) flags direct { visible, }\n",
            "}\n",
        );
        assert_eq!(
            scan_source(source)
                .flags_declarations
                .iter()
                .map(|declaration| (
                    declaration.interface_name.as_str(),
                    declaration.name.as_str()
                ))
                .collect::<Vec<_>>(),
            [("types", "imported"), ("api", "direct")],
            "only direct flags declarations are inventoried"
        );
        let declaration = interface_flags(source, "direct");
        assert_eq!(declaration.flags.len(), 1);
        assert!(
            authored_interface_flags_tokens(source, "api", "imported", &declaration.name_range)
                .is_none(),
            "a flag type from another interface cannot be selected through a use alias"
        );
    }

    #[test]
    fn flags_scanner_rejects_empty_payload_and_malformed_declarations() {
        for invalid in [
            "interface api { flags empty {} }",
            "interface api { flags payload { read(u32), write } }",
            "interface api { flags missing-comma { read write } }",
            "interface api { flags repeated { read, , write } }",
            "interface api { flags duplicate { read, read } }",
            "interface api { flags case-duplicate { foo-bar, FOO-BAR } }",
            "interface api { flags malformed-attribute { @since(version = 1.0.0 read } }",
            "interface api { flags mismatched { read, write] }",
            "interface api { flags unfinished { read, write",
        ] {
            assert!(
                scan_source(invalid).flags_declarations.is_empty(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn direct_variant_and_case_ranges_preserve_payloads_and_mask_only_names() {
        let source = concat!(
            "interface api {\n",
            "  @since(version = 1.2.3)\n",
            "  variant outcome {\n",
            "    ready,\n",
            "    @deprecated(version = 2.0.0)\n",
            "    failed(result<tuple<u32, string>, option<u64>>),\n",
            "    %type(string),\n",
            "  }\n",
            "}\n",
        );
        let declaration = interface_variant(source, "outcome");
        assert_eq!(declaration.interface_name, "api");
        assert_eq!(
            &source[declaration.declaration_range.clone()],
            "@since(version = 1.2.3)\n  variant outcome {\n    ready,\n    @deprecated(version = 2.0.0)\n    failed(result<tuple<u32, string>, option<u64>>),\n    %type(string),\n  }"
        );
        assert_eq!(
            declaration
                .cases
                .iter()
                .map(|case| (case.name.as_str(), case.has_payload))
                .collect::<Vec<_>>(),
            [("ready", false), ("failed", true), ("type", true)]
        );
        assert_eq!(
            &source[declaration.cases[1].declaration_range.clone()],
            "@deprecated(version = 2.0.0)\n    failed(result<tuple<u32, string>, option<u64>>)"
        );
        assert_eq!(
            declaration.token_pairs[0].1, "@",
            "variant attributes remain part of the parent fingerprint"
        );
        assert_eq!(
            declaration.cases[0]
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1
        );
        assert!(
            declaration.cases[1]
                .token_pairs
                .iter()
                .any(|(_, text)| text == "result"),
            "payload syntax remains in the case fingerprint"
        );
        assert_eq!(
            declaration.cases[1].token_pairs[0].1, "@",
            "case attributes remain in the case fingerprint"
        );

        let selected =
            authored_interface_variant_tokens(source, "api", "outcome", &declaration.name_range)
                .expect("the exact parsed name span selects this variant");
        assert_eq!(selected.declaration_range, declaration.declaration_range);
        assert_eq!(selected.token_pairs, declaration.token_pairs);
        assert_eq!(selected.cases.len(), 3);
        assert!(
            authored_interface_variant_tokens(source, "api", "outcome", &(0..0)).is_none(),
            "an absent parser name span cannot identify a variant"
        );
    }

    #[test]
    fn variant_case_fingerprints_ignore_own_name_trivia_and_sibling_cases() {
        let first = interface_variant(
            "interface api { variant outcome { failed(result<u32, string>), ready } }",
            "outcome",
        );
        let renamed = interface_variant(
            "interface api {\n variant result-state {\n failed ( result < u32 , string > ) ,\n ready,\n extra,\n }\n}",
            "result-state",
        );
        assert_ne!(
            first.token_pairs, renamed.token_pairs,
            "the parent fingerprint includes the variant and sibling names"
        );
        assert_eq!(
            first.cases[0].token_pairs, renamed.cases[0].token_pairs,
            "the case fingerprint masks only its own spelling and excludes trivia/siblings"
        );

        let changed_payload = interface_variant(
            "interface api { variant outcome { failed(result<u64, string>), ready } }",
            "outcome",
        );
        assert_ne!(
            first.cases[0].token_pairs, changed_payload.cases[0].token_pairs,
            "changing a payload type changes the case fingerprint"
        );
        assert_eq!(
            first.cases[1].token_pairs, changed_payload.cases[1].token_pairs,
            "another case's payload does not affect this case fingerprint"
        );
    }

    #[test]
    fn variant_scanner_keeps_direct_declarations_and_rejects_malformed_cases() {
        let source = concat!(
            "interface types { variant imported { foreign } }\n",
            "interface api {\n",
            "  use types.{imported};\n",
            "  enum state { variant, unit }\n",
            "  record holder { value: tuple<u32, u64> }\n",
            "  variant direct { yes, nested(tuple<u32, result<string, u32>>) }\n",
            "}\n",
        );
        let declarations = scan_source(source).variant_declarations;
        assert_eq!(
            declarations
                .iter()
                .map(|declaration| (
                    declaration.interface_name.as_str(),
                    declaration.name.as_str()
                ))
                .collect::<Vec<_>>(),
            [("types", "imported"), ("api", "direct")]
        );
        let direct =
            authored_interface_variant_tokens(source, "api", "direct", &declarations[1].name_range)
                .expect("the direct declaration is addressable by its exact name span");
        assert_eq!(direct.cases.len(), 2);
        assert!(
            authored_interface_variant_tokens(
                source,
                "api",
                "imported",
                &declarations[0].name_range,
            )
            .is_none(),
            "a variant from a different interface cannot be selected by a use"
        );

        for invalid in [
            "interface api { variant empty {} }",
            "interface api { variant missing-comma { first second } }",
            "interface api { variant repeated { first, , second } }",
            "interface api { variant empty-payload { item() } }",
            "interface api { variant trailing-token { item(u32) other } }",
            "interface api { variant mismatched-payload { item(tuple<u32, string]) } }",
            "interface api { variant unclosed-payload { item(tuple<u32, string) }",
        ] {
            assert!(
                scan_source(invalid).variant_declarations.is_empty(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn direct_resource_and_member_ranges_include_attributes_and_exact_terminators() {
        let source = concat!(
            "interface api {\n",
            "  /// resource docs are deliberately outside the declaration range\n",
            "  @since(version = 1.2.3)\n",
            "  resource handle {\n",
            "    @deprecated(version = 2.0.0)\n",
            "    constructor(seed: result<tuple<u32, string>, option<list<u8>>>);\n",
            "    // internal trivia is part of the exact byte range, but not tokens\n",
            "    paint: async func(color: u32);\n",
            "    create: static async func() -> handle;\n",
            "  }\n",
            "  resource empty;\n",
            "}\n",
        );
        let declaration = resource(source, "handle");
        assert_eq!(
            &source[declaration.declaration_range.clone()],
            "@since(version = 1.2.3)\n  resource handle {\n    @deprecated(version = 2.0.0)\n    constructor(seed: result<tuple<u32, string>, option<list<u8>>>);\n    // internal trivia is part of the exact byte range, but not tokens\n    paint: async func(color: u32);\n    create: static async func() -> handle;\n  }"
        );
        assert_eq!(declaration.interface_name, "api");
        assert_eq!(
            declaration
                .members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            ["constructor", "paint", "create"]
        );
        assert_eq!(
            declaration
                .members
                .iter()
                .map(|member| member.kind)
                .collect::<Vec<_>>(),
            [
                crate::WitMemberKind::Constructor,
                crate::WitMemberKind::Method,
                crate::WitMemberKind::Static,
            ]
        );
        assert_eq!(
            &source[declaration.members[0].declaration_range.clone()],
            "@deprecated(version = 2.0.0)\n    constructor(seed: result<tuple<u32, string>, option<list<u8>>>);"
        );
        assert_eq!(
            &source[declaration.members[1].declaration_range.clone()],
            "paint: async func(color: u32);"
        );
        assert_eq!(
            &source[declaration.members[2].declaration_range.clone()],
            "create: static async func() -> handle;"
        );
        assert_eq!(
            declaration.members[0]
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1
        );
        let empty = resource(source, "empty");
        assert_eq!(&source[empty.declaration_range.clone()], "resource empty;");
        assert!(empty.members.is_empty());
    }

    #[test]
    fn resource_fingerprints_ignore_comments_and_trivia_but_keep_nested_signature_tokens() {
        let first = resource(
            "interface api { resource store { /// docs\n read: func(value: result<tuple<u32, string>, option<list<u8>>>); } }",
            "store",
        );
        let changed_trivia = resource(
            "interface api {\n resource   store {\n /* changed comment */ read : func ( value : result < tuple < u32 , string > , option < list < u8 > > > ) ;\n }\n}",
            "store",
        );
        assert_eq!(first.token_pairs, changed_trivia.token_pairs);
        assert_eq!(
            first.members[0].token_pairs,
            changed_trivia.members[0].token_pairs
        );
        assert_eq!(
            first.members[0]
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1
        );
        let signature_changed = resource(
            "interface api { resource store { read: func(value: result<tuple<u64, string>, option<list<u8>>>); } }",
            "store",
        );
        assert_ne!(
            first.members[0].token_pairs,
            signature_changed.members[0].token_pairs
        );
    }

    #[test]
    fn exact_resource_name_span_disambiguates_siblings_and_rejects_missing_spans() {
        let source = "interface api { resource first { read: func(); } resource second { read: static func(); } }";
        let declarations = scan_source(source).resource_declarations;
        assert_eq!(declarations.len(), 2);
        assert_eq!(
            &source[declarations[0].declaration_range.clone()],
            "resource first { read: func(); }"
        );
        assert_eq!(
            &source[declarations[1].declaration_range.clone()],
            "resource second { read: static func(); }"
        );
        let second = authored_interface_resource_tokens(
            source,
            "api",
            "second",
            &declarations[1].name_range,
        )
        .expect("exact parser name span selects sibling");
        assert_eq!(second.members[0].kind, crate::WitMemberKind::Static);
        assert!(authored_interface_resource_tokens(source, "api", "second", &(0..0)).is_none());
        let wrong_range = declarations[1].name_range.start..declarations[1].name_range.end - 1;
        assert!(
            authored_interface_resource_tokens(source, "api", "second", &wrong_range).is_none()
        );
    }

    #[test]
    fn malformed_or_unsupported_resource_members_do_not_produce_partial_provenance() {
        for source in [
            "interface api { resource store { read: func(value: tuple<u32, string); } }",
            "interface api { resource store { read: func(value: [u32)); } }",
            "interface api { resource store { unsupported: record { field: u32 } } }",
            "interface api { resource store { read: func(); ",
        ] {
            assert!(
                scan_source(source).resource_declarations.is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn direct_function_fingerprint_tokens_ignore_name_trivia_and_siblings() {
        let first = function(
            "interface api { /// docs\n ping: func(value: result<tuple<u32, string>, option<list<u8>>>); other: func(); }",
            "ping",
        );
        let renamed = function(
            "interface api {\n // revised docs\n pong : func ( value : result < tuple < u32 , string > , option < list < u8 > > > ) ; other: func(); }",
            "pong",
        );
        assert_eq!(first.token_pairs, renamed.token_pairs);

        let sibling_changed = function(
            "interface api { ping: func(value: result<tuple<u32, string>, option<list<u8>>>); other: func(text: string); }",
            "ping",
        );
        assert_eq!(first.token_pairs, sibling_changed.token_pairs);

        let signature_changed = function(
            "interface api { ping: func(value: result<tuple<u64, string>, option<list<u8>>>); other: func(); }",
            "ping",
        );
        assert_ne!(first.token_pairs, signature_changed.token_pairs);
        assert!(
            first
                .token_pairs
                .iter()
                .any(|(kind, text)| kind == "U32" && text == "u32")
        );
        assert_eq!(
            first
                .token_pairs
                .iter()
                .filter(|(_, text)| text.is_empty())
                .count(),
            1,
            "the name token keeps its kind and only its text is omitted"
        );
    }

    #[test]
    fn direct_function_tokens_keep_attributes_and_atomic_strings() {
        let first = function(
            r#"interface api { @since(version = 1.2.3) ping: func() -> u32; }"#,
            "ping",
        );
        let changed = function(
            r#"interface api { @since(version = 2.0.0) ping: func() -> u32; }"#,
            "ping",
        );
        assert_ne!(first.token_pairs, changed.token_pairs);

        let source = r#"interface api { @example("inside ; and } punctuation") ping: func(); }"#;
        let declaration = function(source, "ping");
        assert_eq!(
            &source[declaration.declaration_range.clone()],
            r#"@example("inside ; and } punctuation") ping: func();"#
        );
        assert!(declaration.token_pairs.iter().any(|(kind, text)| {
            kind == "StringLiteral" && text == r#""inside ; and } punctuation""#
        }));
    }

    #[test]
    fn stable_token_kinds_match_every_locked_wit_keyword_and_punctuation() {
        let keywords = [
            ("use", "Use"),
            ("type", "Type"),
            ("func", "Func"),
            ("u8", "U8"),
            ("u16", "U16"),
            ("u32", "U32"),
            ("u64", "U64"),
            ("s8", "S8"),
            ("s16", "S16"),
            ("s32", "S32"),
            ("s64", "S64"),
            ("f32", "F32"),
            ("f64", "F64"),
            ("char", "Char"),
            ("resource", "Resource"),
            ("own", "Own"),
            ("borrow", "Borrow"),
            ("record", "Record"),
            ("flags", "Flags"),
            ("variant", "Variant"),
            ("enum", "Enum"),
            ("bool", "Bool"),
            ("string", "String_"),
            ("option", "Option_"),
            ("result", "Result_"),
            ("future", "Future"),
            ("stream", "Stream"),
            ("error-context", "ErrorContext"),
            ("list", "List"),
            ("map", "Map"),
            ("_", "Underscore"),
            ("as", "As"),
            ("from", "From_"),
            ("static", "Static"),
            ("interface", "Interface"),
            ("tuple", "Tuple"),
            ("import", "Import"),
            ("export", "Export"),
            ("world", "World"),
            ("package", "Package"),
            ("constructor", "Constructor"),
            ("async", "Async"),
            ("include", "Include"),
            ("with", "With"),
        ];
        for (spelling, expected_kind) in keywords {
            let tokens = lex(spelling);
            assert_eq!(tokens.len(), 1, "{spelling}");
            assert_eq!(
                stable_wit_token_kind(spelling, &tokens[0]),
                expected_kind,
                "{spelling}"
            );
        }

        for (spelling, expected_kind) in [
            ("=", "Equals"),
            (",", "Comma"),
            (":", "Colon"),
            (".", "Period"),
            (";", "Semicolon"),
            ("(", "LeftParen"),
            (")", "RightParen"),
            ("{", "LeftBrace"),
            ("}", "RightBrace"),
            ("<", "LessThan"),
            (">", "GreaterThan"),
            ("*", "Star"),
            ("@", "At"),
            ("/", "Slash"),
            ("+", "Plus"),
            ("-", "Minus"),
            ("->", "RArrow"),
            ("ordinary-id", "Id"),
            ("%func", "ExplicitId"),
            ("42", "Integer"),
            (r#""text""#, "StringLiteral"),
        ] {
            let tokens = lex(spelling);
            assert_eq!(tokens.len(), 1, "{spelling}");
            assert_eq!(
                stable_wit_token_kind(spelling, &tokens[0]),
                expected_kind,
                "{spelling}"
            );
        }
    }

    #[test]
    fn direct_function_scanner_excludes_nested_and_unbounded_forms() {
        let source = concat!(
            "interface api {\n",
            "  thing: resource { method: func(); static: static func(); constructor: constructor(); }\n",
            "  value: record { field: u32, nested: tuple<u8, u16> }\n",
            "  direct: func();\n",
            "}\n",
            "world app { imported: func(); }\n",
        );
        let declarations = scan_source(source).function_declarations;
        assert_eq!(declarations.len(), 1, "{declarations:#?}");
        assert_eq!(declarations[0].name, "direct");

        for invalid in [
            "interface api { incomplete: func() }",
            "interface api { incomplete: func(u32; }",
        ] {
            assert!(scan_source(invalid).function_declarations.is_empty());
        }
    }

    #[test]
    fn recovers_docs_before_world_include_attributes_and_multiple_crlf_includes() {
        let source = concat!(
            "world app {\r\n",
            "  /** first include /* nested block */ docs */\r\n",
            "  @since(version = 0.3.0)\r\n",
            "  include base;\r\n",
            "  // second include comment\r\n",
            "  @deprecated(version = 1.0.0)\r\n",
            "  include other;\r\n",
            "}\r\n",
        );

        let first = source.find("base;").expect("first include target exists");
        let second = source.find("other;").expect("second include target exists");
        assert_eq!(
            world_include_docs(source, first),
            Some("first include /* nested block */ docs".to_owned())
        );
        assert_eq!(
            world_include_docs(source, second),
            Some("second include comment".to_owned())
        );
    }

    #[test]
    fn follows_wit_parser_comment_rules_for_world_include_docs() {
        let source = concat!(
            "world app {\n",
            "  // ordinary line comment\n",
            "  include ordinary;\n",
            "  /// triple-slash doc comment\n",
            "  include documented;\n",
            "  /* ordinary nested /* block */ comment */\n",
            "  include block;\n",
            "}\n",
        );

        for (target, expected) in [
            ("ordinary;", "ordinary line comment"),
            ("documented;", "triple-slash doc comment"),
            ("block;", "* ordinary nested /* block */ comment */"),
        ] {
            assert_eq!(
                world_include_docs(source, source.find(target).unwrap()),
                Some(expected.to_owned()),
                "comment behavior follows wit-parser 0.246.0 for {target}"
            );
        }
    }

    #[test]
    fn does_not_attach_comments_inside_include_attributes() {
        let source = concat!(
            "world app {\n",
            "  /// attached docs\n",
            "  @since(version /* not documentation */ = 1.0.0)\n",
            "  include base;\n",
            "}\n",
        );
        assert_eq!(
            world_include_docs(source, source.find("base;").unwrap()),
            Some("attached docs".to_owned())
        );
    }

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
