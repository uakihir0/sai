use std::fmt;
use std::str::FromStr;

/// The declared `namespace:name` identity of a SAI package.
///
/// Package paths with a slash are intentionally not package identities.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageIdentity {
    namespace: Box<str>,
    name: Box<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageIdentityError {
    InvalidPackageName,
    ReservedNamespace,
    ReservedPackageName,
}

impl PackageIdentity {
    pub fn parse(value: &str) -> Result<Self, PackageIdentityError> {
        let (namespace, name) = value
            .split_once(':')
            .ok_or(PackageIdentityError::InvalidPackageName)?;
        if name.contains(':') || !is_valid_segment(namespace) || !is_valid_segment(name) {
            return Err(PackageIdentityError::InvalidPackageName);
        }
        if namespace == "wasi" {
            return Err(PackageIdentityError::ReservedNamespace);
        }
        if namespace == "sai" && matches!(name, "core" | "alloc") {
            return Err(PackageIdentityError::ReservedPackageName);
        }

        Ok(Self {
            namespace: namespace.into(),
            name: name.into(),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.namespace, self.name)
    }
}

impl fmt::Display for PackageIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPackageName => "invalid SAI package identity",
            Self::ReservedNamespace => "the `wasi` package namespace is reserved",
            Self::ReservedPackageName => "the SAI package identity is reserved by the toolchain",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for PackageIdentityError {}

impl FromStr for PackageIdentity {
    type Err = PackageIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

fn is_valid_segment(value: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::{PackageIdentity, PackageIdentityError};

    #[test]
    fn parses_and_formats_canonical_package_identities() {
        let identity = PackageIdentity::parse("acme:image-codec2").expect("valid identity");
        assert_eq!(identity.namespace(), "acme");
        assert_eq!(identity.name(), "image-codec2");
        assert_eq!(identity.to_string(), "acme:image-codec2");
    }

    #[test]
    fn accepts_maximum_length_segments() {
        let namespace = format!("a{}", "0".repeat(63));
        let name = format!("b{}", "9".repeat(63));
        assert!(PackageIdentity::parse(&format!("{namespace}:{name}")).is_ok());
    }

    #[test]
    fn rejects_malformed_package_names_and_paths() {
        for source in [
            "",
            "acme",
            ":image",
            "acme:",
            "acme:image/path",
            "acme:image:extra",
            "1acme:image",
            "acme:2image",
            "acme:two--hyphens",
            "acme:trailing-",
            "acme:under_score",
            "Acme:image",
            "acme:é",
            &format!("{}:image", "a".repeat(65)),
            &format!("acme:{}", "b".repeat(65)),
        ] {
            assert_eq!(
                PackageIdentity::parse(source),
                Err(PackageIdentityError::InvalidPackageName),
                "unexpectedly accepted {source:?}"
            );
        }
    }

    #[test]
    fn rejects_reserved_namespaces_and_toolchain_packages() {
        for source in ["wasi:http", "wasi:filesystem"] {
            assert_eq!(
                PackageIdentity::parse(source),
                Err(PackageIdentityError::ReservedNamespace)
            );
        }
        for source in ["sai:core", "sai:alloc"] {
            assert_eq!(
                PackageIdentity::parse(source),
                Err(PackageIdentityError::ReservedPackageName)
            );
        }
        assert!(PackageIdentity::parse("sai:collections").is_ok());
    }
}
