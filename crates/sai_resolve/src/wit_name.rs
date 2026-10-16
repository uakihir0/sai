//! SAI identifier conversion at the WIT boundary.
//!
//! These functions implement the identifier patterns from spec/13 §13.8.4.
//! The returned WIT spelling excludes `%` keyword escaping; callers that print
//! WIT syntax apply that escape separately.

/// Converts a representable SAI identifier to its WIT spelling.
///
/// Lowercase snake_case becomes lowercase kebab-case. UpperCamel names get a
/// hyphen before each uppercase letter after the first, with all letters
/// lowercased. A raw-identifier prefix is syntax-only and is removed before
/// conversion. Names outside either pattern return `None`.
pub fn sai_identifier_to_wit_name(identifier: &str) -> Option<String> {
    let identifier = identifier.strip_prefix("r#").unwrap_or(identifier);
    if is_lower_sai_identifier(identifier) {
        return Some(identifier.replace('_', "-"));
    }
    if !is_upper_sai_identifier(identifier) {
        return None;
    }

    let mut wit_name = String::with_capacity(identifier.len() + 4);
    for (index, byte) in identifier.bytes().enumerate() {
        if index != 0 && byte.is_ascii_uppercase() {
            wit_name.push('-');
        }
        wit_name.push(char::from(byte.to_ascii_lowercase()));
    }
    Some(wit_name)
}

/// Suggests a compact representable SAI identifier for an invalid name.
///
/// This preserves the package WIT-name collector's existing recovery rule:
/// strip a raw prefix, remove all non-ASCII-alphanumeric characters, and
/// return the compact form only if it is representable.
pub fn suggested_representable_name(identifier: &str) -> Option<String> {
    let identifier = identifier.strip_prefix("r#").unwrap_or(identifier);
    let compact = identifier
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    sai_identifier_to_wit_name(&compact).map(|_| compact)
}

fn is_lower_sai_identifier(identifier: &str) -> bool {
    let mut segments = identifier.split('_');
    let Some(first) = segments.next() else {
        return false;
    };
    if !is_lower_segment(first) {
        return false;
    }
    segments.all(is_lower_segment)
}

fn is_lower_segment(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn is_upper_sai_identifier(identifier: &str) -> bool {
    let bytes = identifier.as_bytes();
    if bytes.is_empty() || !identifier.is_ascii() {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_uppercase() {
            return false;
        }
        index += 1;
        while index < bytes.len()
            && (bytes[index].is_ascii_lowercase() || bytes[index].is_ascii_digit())
        {
            index += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{sai_identifier_to_wit_name, suggested_representable_name};

    #[test]
    fn converts_lowercase_snake_case_to_kebab_case() {
        assert_eq!(
            sai_identifier_to_wit_name("fetch_user").as_deref(),
            Some("fetch-user")
        );
        assert_eq!(
            sai_identifier_to_wit_name("read_v2_config").as_deref(),
            Some("read-v2-config")
        );
    }

    #[test]
    fn converts_upper_camel_names() {
        assert_eq!(
            sai_identifier_to_wit_name("UserId").as_deref(),
            Some("user-id")
        );
        assert_eq!(
            sai_identifier_to_wit_name("HTTPClient").as_deref(),
            Some("h-t-t-p-client")
        );
        assert_eq!(
            sai_identifier_to_wit_name("Ipv4Address").as_deref(),
            Some("ipv4-address")
        );
    }

    #[test]
    fn strips_raw_identifier_prefixes_before_conversion() {
        assert_eq!(
            sai_identifier_to_wit_name("r#type").as_deref(),
            Some("type")
        );
        assert_eq!(
            sai_identifier_to_wit_name("r#fetch_user").as_deref(),
            Some("fetch-user")
        );
    }

    #[test]
    fn rejects_names_outside_the_sai_patterns() {
        assert_eq!(sai_identifier_to_wit_name("_hidden"), None);
        assert_eq!(sai_identifier_to_wit_name("fetch__user"), None);
        assert_eq!(sai_identifier_to_wit_name("userID"), None);
        assert_eq!(sai_identifier_to_wit_name("Éclair"), None);
    }

    #[test]
    fn suggests_compact_representable_names() {
        assert_eq!(
            suggested_representable_name("User_Id").as_deref(),
            Some("UserId")
        );
        assert_eq!(
            suggested_representable_name("utf_8").as_deref(),
            Some("utf8")
        );
        assert_eq!(
            suggested_representable_name("r#bad_name").as_deref(),
            Some("badname")
        );
        assert_eq!(suggested_representable_name("___"), None);
    }
}
