use crate::version::{CompatibilityLine, Version, VersionParseError};
use std::fmt;

/// One of the version operators supported by package contracts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ComparatorOperator {
    Caret,
    Tilde,
    Equal,
    GreaterEqual,
    Less,
    LessEqual,
}

/// A version with one, two, or three explicitly written core components.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PartialVersion {
    completed: Version,
    component_count: u8,
}

/// A version requirement comparator and its written version precision.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RequirementComparator {
    operator: ComparatorOperator,
    version: PartialVersion,
}

/// An inclusive or exclusive upper endpoint of a version interval.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UpperBound {
    endpoint: UpperBoundEndpoint,
    inclusive: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum UpperBoundEndpoint {
    Version(Version),
    ReleaseCore {
        major: u128,
        minor: u128,
        patch: u128,
    },
}

/// A validated intersection of package version comparator intervals.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VersionReq {
    comparators: Vec<RequirementComparator>,
    minimum: Version,
    upper_bound: Option<UpperBound>,
    compatibility_line: CompatibilityLine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionReqParseError {
    EmptyRequirement,
    BareVersionRequirement,
    UnsupportedVersionOperator,
    MissingLowerBound,
    InvalidVersion(VersionParseError),
    InvalidRequirement,
    EmptyVersionRequirement,
}

impl PartialVersion {
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
        let components = core.split('.').collect::<Vec<_>>();
        if !(1..=3).contains(&components.len()) || (pre_release.is_some() && components.len() != 3)
        {
            return Err(VersionParseError::InvalidVersion);
        }

        let major = components[0];
        let minor = components.get(1).copied().unwrap_or("0");
        let patch = components.get(2).copied().unwrap_or("0");
        let completed = match pre_release {
            Some(pre_release) => format!("{major}.{minor}.{patch}-{pre_release}"),
            None => format!("{major}.{minor}.{patch}"),
        };

        Ok(Self {
            completed: Version::parse(&completed)?,
            component_count: components.len() as u8,
        })
    }

    pub const fn completed(&self) -> &Version {
        &self.completed
    }

    pub const fn component_count(&self) -> u8 {
        self.component_count
    }

    pub const fn is_full(&self) -> bool {
        self.component_count == 3
    }
}

impl RequirementComparator {
    pub const fn operator(&self) -> ComparatorOperator {
        self.operator
    }

    pub const fn version(&self) -> &PartialVersion {
        &self.version
    }
}

impl UpperBound {
    pub const fn endpoint(&self) -> &UpperBoundEndpoint {
        &self.endpoint
    }

    pub const fn version(&self) -> Option<&Version> {
        match &self.endpoint {
            UpperBoundEndpoint::Version(version) => Some(version),
            UpperBoundEndpoint::ReleaseCore { .. } => None,
        }
    }

    pub const fn is_inclusive(&self) -> bool {
        self.inclusive
    }
}

impl VersionReq {
    /// Parses and validates one comma-separated package version requirement.
    pub fn parse(value: &str) -> Result<Self, VersionReqParseError> {
        if value.trim().is_empty() {
            return Err(VersionReqParseError::EmptyRequirement);
        }

        let comparators = value
            .split(',')
            .map(RequirementComparator::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if comparators.is_empty() {
            return Err(VersionReqParseError::EmptyRequirement);
        }

        let mut lower = None;
        let mut explicit_upper = None;
        for comparator in &comparators {
            match comparator.operator {
                ComparatorOperator::Caret
                | ComparatorOperator::Tilde
                | ComparatorOperator::Equal
                | ComparatorOperator::GreaterEqual => {
                    if lower.replace(comparator).is_some() {
                        return Err(VersionReqParseError::InvalidRequirement);
                    }
                }
                ComparatorOperator::Less | ComparatorOperator::LessEqual => {
                    if explicit_upper.replace(comparator).is_some() {
                        return Err(VersionReqParseError::InvalidRequirement);
                    }
                }
            }
        }

        let lower = lower.ok_or(VersionReqParseError::MissingLowerBound)?;
        let lower_version = &lower.version.completed;

        if (lower.operator == ComparatorOperator::Equal && comparators.len() != 1)
            || (matches!(
                lower.operator,
                ComparatorOperator::Equal | ComparatorOperator::LessEqual
            ) && !lower.version.is_full())
            || explicit_upper.is_some_and(|upper| {
                upper.operator == ComparatorOperator::LessEqual && !upper.version.is_full()
            })
        {
            return Err(VersionReqParseError::InvalidRequirement);
        }

        if lower.operator == ComparatorOperator::Caret
            && lower.version.completed.major() == 0
            && (lower.version.component_count == 1
                || lower.version.completed.minor() == 0 && lower.version.component_count == 2)
        {
            return Err(VersionReqParseError::InvalidRequirement);
        }

        let implied_upper = match lower.operator {
            ComparatorOperator::Caret => Some(caret_upper_bound(lower_version)),
            ComparatorOperator::Tilde => Some(tilde_upper_bound(
                lower_version,
                lower.version.component_count,
            )),
            ComparatorOperator::Equal => Some(UpperBound {
                endpoint: UpperBoundEndpoint::Version(lower_version.clone()),
                inclusive: true,
            }),
            ComparatorOperator::GreaterEqual => None,
            ComparatorOperator::Less | ComparatorOperator::LessEqual => {
                return Err(VersionReqParseError::MissingLowerBound);
            }
        };

        let explicit_upper = explicit_upper.map(|comparator| UpperBound {
            endpoint: UpperBoundEndpoint::Version(comparator.version.completed.clone()),
            inclusive: comparator.operator == ComparatorOperator::LessEqual,
        });
        let upper_bound = intersect_upper_bounds(implied_upper, explicit_upper);
        if upper_bound.as_ref().is_some_and(|upper| {
            upper.endpoint.compare_version(lower_version).is_gt()
                || upper.endpoint.compare_version(lower_version).is_eq() && !upper.inclusive
        }) {
            return Err(VersionReqParseError::EmptyVersionRequirement);
        }

        let minimum = lower_version.clone();
        let compatibility_line = minimum.compatibility_line();
        Ok(Self {
            comparators,
            minimum,
            upper_bound,
            compatibility_line,
        })
    }

    pub fn comparators(&self) -> &[RequirementComparator] {
        &self.comparators
    }

    pub const fn minimum(&self) -> &Version {
        &self.minimum
    }

    pub const fn upper_bound(&self) -> Option<&UpperBound> {
        self.upper_bound.as_ref()
    }

    pub const fn compatibility_line(&self) -> CompatibilityLine {
        self.compatibility_line
    }

    /// Checks the comparator interval using SemVer precedence.
    ///
    /// This is the interval check in package/resolver.md §2.3.2. The resolver
    /// additionally restricts a selected version to the line of the minimum;
    /// use [`Self::matches_for_resolution`] when making that check.
    pub fn matches(&self, version: &Version) -> bool {
        version >= &self.minimum
            && self.upper_bound.as_ref().is_none_or(|upper| {
                upper.endpoint.compare_version(version).is_lt()
                    || upper.inclusive && upper.endpoint.compare_version(version).is_eq()
            })
    }

    /// Checks both the comparator interval and this requirement's line.
    pub fn matches_for_resolution(&self, version: &Version) -> bool {
        self.matches(version) && version.compatibility_line() == self.compatibility_line
    }
}

impl RequirementComparator {
    fn parse(value: &str) -> Result<Self, VersionReqParseError> {
        let value = value.trim();
        let (operator, version) = if let Some(version) = value.strip_prefix(">=") {
            (ComparatorOperator::GreaterEqual, version)
        } else if let Some(version) = value.strip_prefix("<=") {
            (ComparatorOperator::LessEqual, version)
        } else if let Some(version) = value.strip_prefix('>') {
            let _ = version;
            return Err(VersionReqParseError::UnsupportedVersionOperator);
        } else if let Some(version) = value.strip_prefix('^') {
            (ComparatorOperator::Caret, version)
        } else if let Some(version) = value.strip_prefix('~') {
            (ComparatorOperator::Tilde, version)
        } else if let Some(version) = value.strip_prefix('=') {
            (ComparatorOperator::Equal, version)
        } else if let Some(version) = value.strip_prefix('<') {
            (ComparatorOperator::Less, version)
        } else {
            return match PartialVersion::parse(value) {
                Ok(_) => Err(VersionReqParseError::BareVersionRequirement),
                Err(error) => Err(VersionReqParseError::InvalidVersion(error)),
            };
        };

        let version =
            PartialVersion::parse(version.trim()).map_err(VersionReqParseError::InvalidVersion)?;
        Ok(Self { operator, version })
    }
}

impl ComparatorOperator {
    const fn spelling(self) -> &'static str {
        match self {
            Self::Caret => "^",
            Self::Tilde => "~",
            Self::Equal => "=",
            Self::GreaterEqual => ">=",
            Self::Less => "<",
            Self::LessEqual => "<=",
        }
    }
}

impl UpperBoundEndpoint {
    /// Compares a representable version to this interval endpoint.
    fn compare_version(&self, version: &Version) -> std::cmp::Ordering {
        match self {
            Self::Version(bound) => version.cmp(bound),
            Self::ReleaseCore {
                major,
                minor,
                patch,
            } => compare_version_to_release_core(version, (*major, *minor, *patch)),
        }
    }

    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Version(left), Self::Version(right)) => left.cmp(right),
            (
                Self::ReleaseCore {
                    major: left_major,
                    minor: left_minor,
                    patch: left_patch,
                },
                Self::ReleaseCore {
                    major: right_major,
                    minor: right_minor,
                    patch: right_patch,
                },
            ) => (left_major, left_minor, left_patch).cmp(&(right_major, right_minor, right_patch)),
            (Self::Version(version), Self::ReleaseCore { .. }) => other.compare_version(version),
            (Self::ReleaseCore { .. }, Self::Version(version)) => {
                self.compare_version(version).reverse()
            }
        }
    }
}

impl fmt::Display for PartialVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.completed.major())?;
        if self.component_count >= 2 {
            write!(formatter, ".{}", self.completed.minor())?;
        }
        if self.component_count >= 3 {
            write!(formatter, ".{}", self.completed.patch())?;
            if self.completed.is_pre_release() {
                let full = self.completed.to_string();
                let pre_release = full
                    .split_once('-')
                    .expect("pre-release version has a hyphen")
                    .1;
                write!(formatter, "-{pre_release}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for RequirementComparator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.operator.spelling())?;
        self.version.fmt(formatter)
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, comparator) in self.comparators.iter().enumerate() {
            if index != 0 {
                formatter.write_str(", ")?;
            }
            comparator.fmt(formatter)?;
        }
        Ok(())
    }
}

impl fmt::Display for VersionReqParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyRequirement => "empty version requirement",
            Self::BareVersionRequirement => "a version requirement needs an operator",
            Self::UnsupportedVersionOperator => "the `>` version operator is unsupported",
            Self::MissingLowerBound => "a version requirement needs a lower bound",
            Self::InvalidVersion(_) => "invalid version in requirement",
            Self::InvalidRequirement => "invalid version requirement",
            Self::EmptyVersionRequirement => "version requirement admits no versions",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for VersionReqParseError {}

fn caret_upper_bound(version: &Version) -> UpperBound {
    let core = if version.major() >= 1 {
        (u128::from(version.major()) + 1, 0, 0)
    } else if version.minor() >= 1 {
        (0, u128::from(version.minor()) + 1, 0)
    } else {
        (0, 0, u128::from(version.patch()) + 1)
    };
    exclusive_core_upper(core)
}

fn tilde_upper_bound(version: &Version, component_count: u8) -> UpperBound {
    let core = if component_count == 1 {
        (u128::from(version.major()) + 1, 0, 0)
    } else {
        (
            u128::from(version.major()),
            u128::from(version.minor()) + 1,
            0,
        )
    };
    exclusive_core_upper(core)
}

fn exclusive_core_upper((major, minor, patch): (u128, u128, u128)) -> UpperBound {
    UpperBound {
        endpoint: UpperBoundEndpoint::ReleaseCore {
            major,
            minor,
            patch,
        },
        inclusive: false,
    }
}

fn compare_version_to_release_core(
    version: &Version,
    (major, minor, patch): (u128, u128, u128),
) -> std::cmp::Ordering {
    (
        u128::from(version.major()),
        u128::from(version.minor()),
        u128::from(version.patch()),
    )
        .cmp(&(major, minor, patch))
        .then_with(|| {
            if version.is_pre_release() {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
}

fn intersect_upper_bounds(
    left: Option<UpperBound>,
    right: Option<UpperBound>,
) -> Option<UpperBound> {
    match (left, right) {
        (None, bound) | (bound, None) => bound,
        (Some(left), Some(right)) => match left.endpoint.cmp(&right.endpoint) {
            std::cmp::Ordering::Less => Some(left),
            std::cmp::Ordering::Greater => Some(right),
            std::cmp::Ordering::Equal => Some(UpperBound {
                endpoint: left.endpoint,
                inclusive: left.inclusive && right.inclusive,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{VersionReq, VersionReqParseError};
    use crate::version::{Version, VersionParseError};

    #[test]
    fn completes_partial_versions_and_checks_explicit_ranges() {
        let requirement = VersionReq::parse(">=1.2, <2.0").expect("valid requirement");
        assert_eq!(requirement.minimum().to_string(), "1.2.0");
        assert_eq!(requirement.to_string(), ">=1.2, <2.0");
        assert!(requirement.matches(&version("1.2.0")));
        assert!(requirement.matches(&version("1.9.9")));
        assert!(!requirement.matches(&version("2.0.0")));
        assert!(!requirement.matches(&version("1.1.9")));
    }

    #[test]
    fn caret_and_tilde_requirements_use_documented_compatibility_ranges() {
        let caret_minor = VersionReq::parse("^0.4.2").expect("valid requirement");
        assert!(caret_minor.matches(&version("0.4.9")));
        assert!(!caret_minor.matches(&version("0.5.0")));

        let caret_patch = VersionReq::parse("^0.0.3").expect("valid requirement");
        assert!(caret_patch.matches(&version("0.0.3")));
        assert!(!caret_patch.matches(&version("0.0.4")));

        let tilde_major = VersionReq::parse("~1").expect("valid requirement");
        assert_eq!(tilde_major.minimum().to_string(), "1.0.0");
        assert!(tilde_major.matches(&version("1.9.0")));
        assert!(!tilde_major.matches(&version("2.0.0")));

        let tilde_minor = VersionReq::parse("~1.4.2").expect("valid requirement");
        assert!(tilde_minor.matches(&version("1.4.9")));
        assert!(!tilde_minor.matches(&version("1.5.0")));
    }

    #[test]
    fn inclusive_upper_bounds_include_the_endpoint() {
        let requirement = VersionReq::parse(">=1.0, <=1.4.7").expect("valid requirement");
        assert!(requirement.matches(&version("1.4.7")));
        assert!(!requirement.matches(&version("1.4.8")));
        assert!(
            requirement
                .upper_bound()
                .expect("upper bound")
                .is_inclusive()
        );
    }

    #[test]
    fn prerelease_bounds_use_semver_precedence() {
        let requirement = VersionReq::parse("^1.0.0-rc.1").expect("valid requirement");
        assert!(!requirement.matches(&version("1.0.0-beta")));
        assert!(requirement.matches(&version("1.0.0-rc.2")));
        assert!(requirement.matches(&version("1.0.0")));
        assert!(requirement.matches(&version("1.3.1")));
        assert!(!requirement.matches(&version("2.0.0")));
        let next_line_prerelease = version("2.0.0-alpha");
        assert!(requirement.matches(&next_line_prerelease));
        assert!(!requirement.matches_for_resolution(&next_line_prerelease));
    }

    #[test]
    fn resolution_membership_also_checks_the_minimum_compatibility_line() {
        let requirement = VersionReq::parse(">=1.2, <3.0").expect("valid requirement");
        let next_line = version("2.0.0");
        assert!(requirement.matches(&next_line));
        assert!(!requirement.matches_for_resolution(&next_line));
        assert!(requirement.matches_for_resolution(&version("1.9.9")));
    }

    #[test]
    fn rejects_requirement_shapes_and_empty_intersections() {
        let cases = [
            ("", VersionReqParseError::EmptyRequirement),
            ("1.2", VersionReqParseError::BareVersionRequirement),
            (">1.2", VersionReqParseError::UnsupportedVersionOperator),
            ("<2.0", VersionReqParseError::MissingLowerBound),
            (">=1.0, ^1.1", VersionReqParseError::InvalidRequirement),
            (
                ">=1.0, <2.0, <=1.9.0",
                VersionReqParseError::InvalidRequirement,
            ),
            ("=1.0.0, >=1.0", VersionReqParseError::InvalidRequirement),
            ("=1.2", VersionReqParseError::InvalidRequirement),
            ("<=1.2", VersionReqParseError::MissingLowerBound),
            (">=1, <=2", VersionReqParseError::InvalidRequirement),
            ("^0", VersionReqParseError::InvalidRequirement),
            ("^0.0", VersionReqParseError::InvalidRequirement),
            (">=2.0, <1.5", VersionReqParseError::EmptyVersionRequirement),
            ("^1.4, <1.4", VersionReqParseError::EmptyVersionRequirement),
        ];

        for (source, expected) in cases {
            assert_eq!(VersionReq::parse(source), Err(expected), "{source:?}");
        }
    }

    #[test]
    fn distinguishes_invalid_versions_from_forbidden_metadata() {
        assert_eq!(
            VersionReq::parse(">=1.2.3x+meta"),
            Err(VersionReqParseError::InvalidVersion(
                VersionParseError::InvalidVersion
            ))
        );
        assert_eq!(
            VersionReq::parse(">=1.2+meta"),
            Err(VersionReqParseError::InvalidVersion(
                VersionParseError::BuildMetadataNotAllowed
            ))
        );
    }

    #[test]
    fn overflowing_implicit_endpoints_keep_their_exact_interval() {
        let maximum = "18446744073709551615";
        let caret = VersionReq::parse(&format!("^{maximum}.0.0")).expect("valid requirement");
        assert!(caret.upper_bound().is_some());
        assert!(caret.upper_bound().unwrap().version().is_none());
        assert!(caret.matches(&version(&format!("{maximum}.0.0"))));
        assert!(caret.matches(&version(&format!("{maximum}.99.99"))));

        let tilde = VersionReq::parse(&format!("~{maximum}")).expect("valid requirement");
        assert!(tilde.upper_bound().unwrap().version().is_none());
        assert!(tilde.matches(&version(&format!("{maximum}.99.99"))));

        let max_minor = VersionReq::parse(&format!("^0.{maximum}")).expect("valid requirement");
        assert!(max_minor.matches(&version(&format!("0.{maximum}.99"))));
        assert!(!max_minor.matches(&version("1.0.0-alpha")));

        let max_patch = VersionReq::parse(&format!("^0.0.{maximum}")).expect("valid requirement");
        assert!(max_patch.matches(&version(&format!("0.0.{maximum}"))));
        assert!(!max_patch.matches(&version("0.1.0-alpha")));
    }

    fn version(value: &str) -> Version {
        Version::parse(value).expect("valid version")
    }
}
