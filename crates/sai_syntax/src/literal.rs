use std::borrow::Cow;

use crate::{EscapeMode, EscapeValue};

const REPLACEMENT_CHAR: char = '\u{fffd}';
const REPLACEMENT_BYTES: &[u8] = "\u{fffd}".as_bytes();

#[derive(Clone, Copy)]
pub(crate) enum FStringTextSegment {
    Start,
    Middle,
    End,
}

pub(crate) fn decode_fstring_text(literal: &str, segment: FStringTextSegment) -> Cow<'_, str> {
    let content = match segment {
        FStringTextSegment::Start => literal.strip_prefix("f\"").unwrap_or(""),
        FStringTextSegment::Middle => literal,
        FStringTextSegment::End if has_unescaped_closing_quote(literal) => {
            literal.strip_suffix('"').unwrap_or(literal)
        }
        FStringTextSegment::End => literal,
    };
    decode_fstring_content(content)
}

pub(crate) fn decode_string_value(literal: &str) -> Cow<'_, str> {
    if literal.starts_with('r') {
        return raw_string_body(literal, 1)
            .map(decode_plain_text)
            .unwrap_or(Cow::Borrowed(""));
    }
    if literal.starts_with("\"\"\"") {
        return decode_multiline_string(literal);
    }

    decode_escaped_content(simple_content(literal, "\""), EscapeMode::SimpleString)
}

pub(crate) fn decode_byte_string_value(literal: &str) -> Cow<'_, [u8]> {
    if literal.starts_with("br") {
        return raw_string_body(literal, 2)
            .map(decode_raw_bytes)
            .unwrap_or(Cow::Borrowed(&[]));
    }

    decode_byte_content(simple_content(literal, "b\""), EscapeMode::ByteString)
}

fn raw_string_body(literal: &str, prefix_len: usize) -> Option<&str> {
    let bytes = literal.as_bytes();
    let mut quote = prefix_len;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }

    let body_start = quote + 1;
    let hashes = quote - prefix_len;
    let suffix_len = hashes.checked_add(1)?;
    let body_end = if let Some(candidate) = literal.len().checked_sub(suffix_len)
        && candidate >= body_start
        && bytes.get(candidate) == Some(&b'"')
        && bytes
            .get(candidate + 1..)
            .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
    {
        candidate
    } else {
        literal.len()
    };
    literal.get(body_start..body_end)
}

fn simple_content<'a>(literal: &'a str, prefix: &str) -> &'a str {
    let Some(content) = literal.strip_prefix(prefix) else {
        return "";
    };
    if has_unescaped_closing_quote(literal) {
        content.strip_suffix('"').unwrap_or(content)
    } else {
        content
    }
}

fn has_unescaped_closing_quote(literal: &str) -> bool {
    let bytes = literal.as_bytes();
    if bytes.last() != Some(&b'"') {
        return false;
    }
    let backslashes = bytes[..bytes.len() - 1]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    backslashes % 2 == 0
}

fn decode_plain_text(text: &str) -> Cow<'_, str> {
    if !plain_text_needs_decoding(text) {
        return Cow::Borrowed(text);
    }

    let mut decoded = String::with_capacity(text.len());
    let mut position = 0;
    while position < text.len() {
        let Some(ch) = text[position..].chars().next() else {
            decoded.push(REPLACEMENT_CHAR);
            break;
        };
        let end = position + ch.len_utf8();
        if crate::forbidden_class(ch) == Some(crate::ForbiddenClass::Bidi) {
            decoded.push(REPLACEMENT_CHAR);
        } else {
            decoded.push(ch);
        }
        position = end;
    }
    Cow::Owned(decoded)
}

fn plain_text_needs_decoding(text: &str) -> bool {
    let mut position = 0;
    while position < text.len() {
        let Some(ch) = text[position..].chars().next() else {
            return true;
        };
        let end = position + ch.len_utf8();
        if crate::forbidden_class(ch) == Some(crate::ForbiddenClass::Bidi) {
            return true;
        }
        position = end;
    }
    false
}

fn decode_escaped_content<'a>(content: &'a str, mode: EscapeMode) -> Cow<'a, str> {
    if !content.contains('\\') && !plain_text_needs_decoding(content) {
        return Cow::Borrowed(content);
    }

    let mut decoded = String::with_capacity(content.len());
    let mut position = 0;
    while position < content.len() {
        let Some(ch) = content[position..].chars().next() else {
            decoded.push(REPLACEMENT_CHAR);
            break;
        };
        if ch == '\\' {
            let escape = crate::scan_escape(content, position, mode);
            if escape.valid {
                match escape.value {
                    Some(EscapeValue::Scalar(value)) => decoded.push(value),
                    Some(EscapeValue::Byte(value)) => decoded.push(char::from(value)),
                    None => {}
                }
            } else {
                decoded.push(REPLACEMENT_CHAR);
            }
            position = escape.end.max(position + 1).min(content.len());
            continue;
        }

        let end = position + ch.len_utf8();
        if crate::forbidden_class(ch) == Some(crate::ForbiddenClass::Bidi) {
            decoded.push(REPLACEMENT_CHAR);
        } else {
            decoded.push(ch);
        }
        position = end;
    }
    Cow::Owned(decoded)
}

fn decode_fstring_content(content: &str) -> Cow<'_, str> {
    if !content.contains('\\')
        && !content.contains('{')
        && !content.contains('}')
        && !plain_text_needs_decoding(content)
    {
        return Cow::Borrowed(content);
    }

    let mut decoded = String::with_capacity(content.len());
    let mut position = 0;
    while position < content.len() {
        let Some(ch) = content[position..].chars().next() else {
            decoded.push(REPLACEMENT_CHAR);
            break;
        };
        if ch == '\\' {
            let escape = crate::scan_escape(content, position, EscapeMode::SimpleString);
            if escape.valid {
                if let Some(EscapeValue::Scalar(value)) = escape.value {
                    decoded.push(value);
                }
            } else {
                decoded.push(REPLACEMENT_CHAR);
            }
            position = escape.end.max(position + 1).min(content.len());
            continue;
        }
        if matches!(ch, '{' | '}') {
            if content.as_bytes().get(position + 1) == Some(&(ch as u8)) {
                decoded.push(ch);
                position += 2;
            } else {
                decoded.push(REPLACEMENT_CHAR);
                position += ch.len_utf8();
            }
            continue;
        }

        let end = position + ch.len_utf8();
        if crate::forbidden_class(ch) == Some(crate::ForbiddenClass::Bidi) {
            decoded.push(REPLACEMENT_CHAR);
        } else {
            decoded.push(ch);
        }
        position = end;
    }
    Cow::Owned(decoded)
}

fn decode_multiline_string(literal: &str) -> Cow<'_, str> {
    let opener_end = 3;
    let content_start = opener_end + line_break_len(literal, opener_end).unwrap_or(0);
    let closing_line = multiline_closing_line(literal, content_start);
    let (mut content_end, indentation) = if let Some((line_start, quote_start)) = closing_line {
        (line_start, &literal[line_start..quote_start])
    } else {
        (literal.len(), "")
    };

    if closing_line.is_some() && content_end > content_start {
        content_end = previous_line_break_start(literal, content_end).unwrap_or(content_end);
    }

    let mut decoded = String::with_capacity(content_end.saturating_sub(content_start));
    let mut line_start = content_start;
    while line_start < content_end {
        let line_end = literal[line_start..content_end]
            .find(['\r', '\n'])
            .map_or(content_end, |offset| line_start + offset);
        let line = &literal[line_start..line_end];
        if !line.is_empty() && !line.starts_with(indentation) {
            decoded.push(REPLACEMENT_CHAR);
        } else {
            let content = line.strip_prefix(indentation).unwrap_or(line);
            decoded.push_str(&decode_escaped_content(
                content,
                EscapeMode::MultilineString,
            ));
        }

        if line_end < content_end {
            let break_len = line_break_len(literal, line_end).unwrap_or(1);
            decoded.push_str(&literal[line_end..line_end + break_len]);
            line_start = line_end + break_len;
        } else {
            line_start = content_end;
        }
    }
    Cow::Owned(decoded)
}

fn multiline_closing_line(literal: &str, mut line_start: usize) -> Option<(usize, usize)> {
    let bytes = literal.as_bytes();
    loop {
        let mut line_end = line_start;
        while line_end < bytes.len() && !matches!(bytes[line_end], b'\r' | b'\n') {
            line_end += 1;
        }

        let mut quote_start = line_start;
        while matches!(bytes.get(quote_start), Some(b' ' | b'\t')) {
            quote_start += 1;
        }
        if bytes.get(quote_start..quote_start.checked_add(3)?) == Some(b"\"\"\"")
            && quote_start + 3 == line_end
            && line_end == literal.len()
        {
            return Some((line_start, quote_start));
        }
        if line_end == literal.len() {
            return None;
        }
        line_start = line_end + line_break_len(literal, line_end)?;
    }
}

fn line_break_len(text: &str, position: usize) -> Option<usize> {
    if text.as_bytes().get(position..position.checked_add(2)?) == Some(b"\r\n") {
        Some(2)
    } else if matches!(text.as_bytes().get(position), Some(b'\r' | b'\n')) {
        Some(1)
    } else {
        None
    }
}

fn previous_line_break_start(text: &str, position: usize) -> Option<usize> {
    if position >= 2 && text.as_bytes().get(position - 2..position) == Some(b"\r\n") {
        Some(position - 2)
    } else if position > 0 && matches!(text.as_bytes().get(position - 1), Some(b'\r' | b'\n')) {
        Some(position - 1)
    } else {
        None
    }
}

fn decode_raw_bytes(content: &str) -> Cow<'_, [u8]> {
    if content.is_ascii() && !plain_text_needs_decoding(content) {
        return Cow::Borrowed(content.as_bytes());
    }

    let mut decoded = Vec::with_capacity(content.len());
    let mut position = 0;
    while position < content.len() {
        let Some(ch) = content[position..].chars().next() else {
            decoded.extend_from_slice(REPLACEMENT_BYTES);
            break;
        };
        let end = position + ch.len_utf8();
        if !ch.is_ascii() || crate::forbidden_class(ch) == Some(crate::ForbiddenClass::Bidi) {
            decoded.extend_from_slice(REPLACEMENT_BYTES);
        } else {
            let mut bytes = [0; 4];
            decoded.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
        }
        position = end;
    }
    Cow::Owned(decoded)
}

fn decode_byte_content<'a>(content: &'a str, mode: EscapeMode) -> Cow<'a, [u8]> {
    if content.is_ascii() && !content.contains('\\') && !plain_text_needs_decoding(content) {
        return Cow::Borrowed(content.as_bytes());
    }

    let mut decoded = Vec::with_capacity(content.len());
    let mut position = 0;
    while position < content.len() {
        let Some(ch) = content[position..].chars().next() else {
            decoded.extend_from_slice(REPLACEMENT_BYTES);
            break;
        };
        if ch == '\\' {
            let escape = crate::scan_escape(content, position, mode);
            if escape.valid {
                match escape.value {
                    Some(EscapeValue::Scalar(value)) if value.is_ascii() => {
                        decoded.push(value as u8);
                    }
                    Some(EscapeValue::Scalar(_)) => decoded.extend_from_slice(REPLACEMENT_BYTES),
                    Some(EscapeValue::Byte(value)) => decoded.push(value),
                    None => {}
                }
            } else {
                decoded.extend_from_slice(REPLACEMENT_BYTES);
            }
            position = escape.end.max(position + 1).min(content.len());
            continue;
        }

        let end = position + ch.len_utf8();
        if !ch.is_ascii() || crate::forbidden_class(ch) == Some(crate::ForbiddenClass::Bidi) {
            decoded.extend_from_slice(REPLACEMENT_BYTES);
        } else {
            decoded.push(ch as u8);
        }
        position = end;
    }
    Cow::Owned(decoded)
}
