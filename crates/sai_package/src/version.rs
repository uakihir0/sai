use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// A canonical SemVer version used by package contracts and lock files.
///
/// SAI versions omit build metadata. Every numeric component, including a
/// numeric pre-release identifier, fits in an unsigned 64-bit integer.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre_release: Vec<PreReleaseIdentifier>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum PreReleaseIdentifier {
    Numeric(u64),
    AlphaNumeric(Box<str>),
}

/// A package compatibility line, ordered by the precedence of its first
/// possible version.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompatibilityLine {
    first_version_core: (u64, u64, u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionParseError {
    InvalidVersion,
    BuildMetadataNotAllowed,
}

impl Version {
    pub fn parse(value: &str) -> Result<Self, VersionParseError> {
        if value.contains('+') {
            let prefix = value.split_once('+').map_or("", |(prefix, _)| prefix);
            return if Self::parse_without_build_metadata(prefix).is_ok() {
                Err(VersionParseError::BuildMetadataNotAllowed)
            } else {
                Err(VersionParseError::InvalidVersion)
            };
        }

        Self::parse_without_build_metadata(value)
    }

    fn parse_without_build_metadata(value: &str) -> Result<Self, VersionParseError> {
        let (core, pre_release) = match value.split_once('-') {
            Some((core, pre_release)) => (core, Some(pre_release)),
            None => (value, None),
        };
        let mut components = core.split('.');
        let major = parse_number(components.next())?;
        let minor = parse_number(components.next())?;
        let patch = parse_number(components.next())?;
        if components.next().is_some() {
            return Err(VersionParseError::InvalidVersion);
        }

        let pre_release = match pre_release {
            Some(value) => {
                if value.is_empty() {
                    return Err(VersionParseError::InvalidVersion);
                }
                value
                    .split('.')
                    .map(parse_pre_release_identifier)
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => Vec::new(),
        };

        Ok(Self {
            major,
            minor,
            patch,
            pre_release,
        })
    }

    pub const fn major(&self) -> u64 {
        self.major
    }

    pub const fn minor(&self) -> u64 {
        self.minor
    }

    pub const fn patch(&self) -> u64 {
        self.patch
    }

    pub fn is_pre_release(&self) -> bool {
        !self.pre_release.is_empty()
    }

    pub fn compatibility_line(&self) -> CompatibilityLine {
        if self.major >= 1 {
            CompatibilityLine {
                first_version_core: (self.major, 0, 0),
            }
        } else if self.minor >= 1 {
            CompatibilityLine {
                first_version_core: (0, self.minor, 0),
            }
        } else {
            CompatibilityLine {
                first_version_core: (0, 0, self.patch),
            }
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(
                || match (self.pre_release.is_empty(), other.pre_release.is_empty()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => self.pre_release.cmp(&other.pre_release),
                },
            )
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PreReleaseIdentifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::AlphaNumeric(_)) => Ordering::Less,
            (Self::AlphaNumeric(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::AlphaNumeric(left), Self::AlphaNumeric(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for PreReleaseIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre_release.is_empty() {
            formatter.write_str("-")?;
            for (index, identifier) in self.pre_release.iter().enumerate() {
                if index != 0 {
                    formatter.write_str(".")?;
                }
                match identifier {
                    PreReleaseIdentifier::Numeric(value) => write!(formatter, "{value}")?,
                    PreReleaseIdentifier::AlphaNumeric(value) => formatter.write_str(value)?,
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for CompatibilityLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (major, minor, patch) = self.first_version_core;
        if major >= 1 {
            write!(formatter, "{major}")
        } else if minor >= 1 {
            write!(formatter, "0.{minor}")
        } else {
            write!(formatter, "0.0.{patch}")
        }
    }
}

impl fmt::Display for VersionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersion => formatter.write_str("invalid SAI package version"),
            Self::BuildMetadataNotAllowed => {
                formatter.write_str("build metadata is not allowed in SAI package versions")
            }
        }
    }
}

impl std::error::Error for VersionParseError {}

impl FromStr for Version {
    type Err = VersionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

fn parse_number(value: Option<&str>) -> Result<u64, VersionParseError> {
    let value = value.ok_or(VersionParseError::InvalidVersion)?;
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(VersionParseError::InvalidVersion);
    }
    value.parse().map_err(|_| VersionParseError::InvalidVersion)
}

fn parse_pre_release_identifier(value: &str) -> Result<PreReleaseIdentifier, VersionParseError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(VersionParseError::InvalidVersion);
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        if value.len() > 1 && value.starts_with('0') {
            return Err(VersionParseError::InvalidVersion);
        }
        let value = value
            .parse()
            .map_err(|_| VersionParseError::InvalidVersion)?;
        Ok(PreReleaseIdentifier::Numeric(value))
    } else {
        Ok(PreReleaseIdentifier::AlphaNumeric(value.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Version, VersionParseError};
    use std::cmp::Ordering;

    #[test]
    fn parses_and_formats_release_and_pre_release_versions() {
        for source in ["0.0.0", "1.2.3", "1.2.3-alpha.1", "1.2.3-01a"] {
            let version = Version::parse(source).expect("valid version");
            assert_eq!(version.to_string(), source);
        }
    }

    #[test]
    fn compares_semver_precedence() {
        let versions = [
            "0.9.0",
            "0.10.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.10.0",
            "2.0.0",
        ]
        .map(|source| Version::parse(source).expect("valid version"));

        for pair in versions.windows(2) {
            assert_eq!(pair[0].cmp(&pair[1]), Ordering::Less);
        }
    }

    #[test]
    fn compares_numeric_pre_release_identifiers_numerically() {
        let low = Version::parse("1.0.0-beta.2").expect("valid version");
        let high = Version::parse("1.0.0-beta.11").expect("valid version");
        assert!(low < high);
    }

    #[test]
    fn compatibility_lines_follow_the_core_version() {
        let cases = [
            ("2.0.0-rc.1", (2, 0, 0), "2"),
            ("0.4.8", (0, 4, 0), "0.4"),
            ("0.0.7", (0, 0, 7), "0.0.7"),
        ];
        let lines = cases.map(|(source, expected, display)| {
            let version = Version::parse(source).expect("valid version");
            let line = version.compatibility_line();
            assert_eq!(line.first_version_core, expected);
            assert_eq!(version.compatibility_line().to_string(), display);
            line
        });

        assert!(lines[2] < lines[1] && lines[1] < lines[0]);
        assert_eq!(
            lines[0],
            Version::parse("2.4.8").unwrap().compatibility_line()
        );
    }

    #[test]
    fn rejects_noncanonical_or_out_of_range_components() {
        for source in [
            "",
            "1",
            "1.2",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "18446744073709551616.0.0",
            "1.0.0-",
            "1.0.0-alpha..1",
            "1.0.0-01",
            "1.0.0-18446744073709551616",
            "1.0.0-α",
            "1.2.3x+meta",
        ] {
            assert_eq!(
                Version::parse(source),
                Err(VersionParseError::InvalidVersion),
                "unexpectedly accepted {source:?}"
            );
        }
    }

    #[test]
    fn rejects_build_metadata_with_its_dedicated_error() {
        for source in ["1.2.3+build.5", "1.2.3-rc.1+build.5", "1.2.3+"] {
            assert_eq!(
                Version::parse(source),
                Err(VersionParseError::BuildMetadataNotAllowed)
            );
        }
    }
}
