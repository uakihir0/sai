//! Parses release tags from an advertised Git reference list.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::str;

use crate::package_path::RepositoryPackageDirectory;
use crate::version::Version;

/// Maximum number of canonical release tags considered in one listing.
pub const MAX_RELEASE_TAGS_PER_LISTING: usize = 100_000;
/// Maximum number of refs accepted from one Git advertisement.
pub const MAX_GIT_REFS_PER_LISTING: u64 = 1_000_000;
/// Maximum cumulative ref-name bytes accepted from one Git advertisement.
pub const MAX_GIT_REF_NAME_BYTES_PER_LISTING: u64 = 64 * 1024 * 1024;
/// Maximum pre-release identifiers parsed in one Git release-tag listing.
pub const MAX_RELEASE_TAG_IDENTIFIERS_PER_LISTING: u64 = 1_000_000;

#[derive(Clone, Copy)]
struct ListingLimits {
    git_refs: u64,
    ref_name_bytes: u64,
    version_identifiers: u64,
    release_tags: usize,
}

impl ListingLimits {
    const SPEC: Self = Self {
        git_refs: MAX_GIT_REFS_PER_LISTING,
        ref_name_bytes: MAX_GIT_REF_NAME_BYTES_PER_LISTING,
        version_identifiers: MAX_RELEASE_TAG_IDENTIFIERS_PER_LISTING,
        release_tags: MAX_RELEASE_TAGS_PER_LISTING,
    };
}

/// One canonical SAI release tag advertised by a Git remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitReleaseTag {
    name: Box<str>,
    version: Version,
    object_id: gix::ObjectId,
}

impl GitReleaseTag {
    /// The complete Git ref name, such as `refs/tags/v1.2.3`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The canonical package version encoded in the ref name.
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// The object ID advertised for this ref.
    pub const fn object_id(&self) -> &gix::ObjectId {
        &self.object_id
    }
}

/// An error while collecting release tags from a remote ref advertisement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitReleaseTagListError {
    /// A fixed limit for one remote ref listing was exceeded.
    LimitExceeded {
        /// The exceeded limit.
        limit: GitReleaseTagListLimit,
        /// The value reached when the limit was exceeded.
        actual: u64,
    },
    /// One canonical ref name was advertised with conflicting object IDs.
    ConflictingReleaseTag {
        /// The complete conflicting Git ref name.
        name: Box<str>,
    },
    /// The Git package directory is not normalized within the repository.
    InvalidPackageDirectory,
}

/// A fixed resource limit for one Git ref listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitReleaseTagListLimit {
    /// Maximum number of refs in the advertisement.
    GitRefCount,
    /// Maximum cumulative byte length of advertised ref names.
    GitRefNameBytes,
    /// Maximum number of pre-release identifiers in considered version tags.
    VersionIdentifierCount,
    /// Maximum number of canonical package release tags.
    ReleaseTagCount,
}

impl GitReleaseTagListLimit {
    fn name(self) -> &'static str {
        match self {
            Self::GitRefCount => "advertised Git ref count",
            Self::GitRefNameBytes => "advertised Git ref-name bytes",
            Self::VersionIdentifierCount => "release-tag version identifier count",
            Self::ReleaseTagCount => "canonical release-tag count",
        }
    }
}

impl fmt::Display for GitReleaseTagListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded { limit, actual } => {
                write!(
                    formatter,
                    "{} limit exceeded (reached {actual})",
                    limit.name()
                )
            }
            Self::ConflictingReleaseTag { name } => {
                write!(formatter, "Git ref {name} has conflicting object IDs")
            }
            Self::InvalidPackageDirectory => {
                formatter.write_str("Git package directory is not normalized within the repository")
            }
        }
    }
}

impl Error for GitReleaseTagListError {}

/// Parse canonical package release tags from advertised Git refs.
///
/// Refs outside the package's exact tag prefix and names that are not
/// canonical SAI versions are ignored. The returned tags are ordered by
/// version precedence.
pub fn parse_release_tag_refs<I, N>(
    refs: I,
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<Vec<GitReleaseTag>, GitReleaseTagListError>
where
    I: IntoIterator<Item = (N, gix::ObjectId)>,
    N: AsRef<[u8]>,
{
    parse_release_tag_refs_with_limits(refs, directory, ListingLimits::SPEC)
}

fn parse_release_tag_refs_with_limits<I, N>(
    refs: I,
    directory: Option<&RepositoryPackageDirectory>,
    limits: ListingLimits,
) -> Result<Vec<GitReleaseTag>, GitReleaseTagListError>
where
    I: IntoIterator<Item = (N, gix::ObjectId)>,
    N: AsRef<[u8]>,
{
    let prefix = release_tag_prefix(directory)?;
    let mut tags: BTreeMap<Version, GitReleaseTag> = BTreeMap::new();
    let mut ref_count = 0u64;
    let mut ref_name_bytes = 0u64;
    let mut version_identifiers = 0u64;

    for (name, object_id) in refs {
        let name = name.as_ref();
        ref_count = ref_count.saturating_add(1);
        if ref_count > limits.git_refs {
            return Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefCount,
                actual: ref_count,
            });
        }
        ref_name_bytes = ref_name_bytes.saturating_add(name.len() as u64);
        if ref_name_bytes > limits.ref_name_bytes {
            return Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefNameBytes,
                actual: ref_name_bytes,
            });
        }

        let Some(version_bytes) = name.strip_prefix(prefix.as_bytes()) else {
            continue;
        };
        let Ok(version_text) = str::from_utf8(version_bytes) else {
            continue;
        };
        let additional_identifiers = version_text
            .split_once('-')
            .map_or(0, |(_, pre_release)| pre_release.split('.').count() as u64);
        version_identifiers = version_identifiers.saturating_add(additional_identifiers);
        if version_identifiers > limits.version_identifiers {
            return Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::VersionIdentifierCount,
                actual: version_identifiers,
            });
        }
        let Ok(version) = Version::parse(version_text) else {
            continue;
        };
        if version.to_string() != version_text {
            continue;
        }

        // A valid version suffix is ASCII, and the prefix is constructed from
        // an already validated package directory.
        let name = str::from_utf8(name).expect("canonical Git release tag ref is UTF-8");
        let tag = GitReleaseTag {
            name: name.into(),
            version: version.clone(),
            object_id,
        };
        if let Some(existing) = tags.get(&version) {
            if existing.name != tag.name || existing.object_id != tag.object_id {
                return Err(GitReleaseTagListError::ConflictingReleaseTag { name: tag.name });
            }
            continue;
        }
        let actual = tags.len().saturating_add(1);
        if actual > limits.release_tags {
            return Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::ReleaseTagCount,
                actual: actual as u64,
            });
        }
        tags.insert(version, tag);
    }

    Ok(tags.into_values().collect())
}

pub(crate) fn release_tag_prefix(
    directory: Option<&RepositoryPackageDirectory>,
) -> Result<String, GitReleaseTagListError> {
    match directory.map(RepositoryPackageDirectory::as_str) {
        None | Some(".") => Ok("refs/tags/v".to_owned()),
        Some(directory) => {
            let prefix = format!("refs/tags/{directory}/v");
            let probe = format!("{prefix}0.0.0");
            gix::refs::FullName::try_from(probe)
                .map_err(|_| GitReleaseTagListError::InvalidPackageDirectory)?;
            Ok(prefix)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GitReleaseTagListError, GitReleaseTagListLimit, ListingLimits,
        MAX_RELEASE_TAG_IDENTIFIERS_PER_LISTING, MAX_RELEASE_TAGS_PER_LISTING,
        parse_release_tag_refs, parse_release_tag_refs_with_limits,
    };
    use crate::package_path::RepositoryPackageDirectory;

    fn object_id(last_nibble: u8) -> gix::ObjectId {
        let mut hex = vec![b'0'; 40];
        hex[39] = b"0123456789abcdef"[usize::from(last_nibble)];
        gix::ObjectId::from_hex(&hex).expect("valid SHA-1 object ID")
    }

    #[test]
    fn accepts_only_canonical_versions_under_the_exact_package_prefix() {
        let refs = [
            (b"refs/tags/v1.2.3".as_slice(), object_id(1)),
            (b"refs/tags/v1.2".as_slice(), object_id(2)),
            (b"refs/tags/v01.2.3".as_slice(), object_id(3)),
            (b"refs/tags/1.2.3".as_slice(), object_id(4)),
            (b"refs/tags/v1.2.3+build".as_slice(), object_id(5)),
            (b"refs/tags/V1.2.3".as_slice(), object_id(6)),
            (b"refs/tags/other/v1.2.3".as_slice(), object_id(7)),
            (b"refs/tags/v1.2.3^{}".as_slice(), object_id(8)),
        ];

        let tags = parse_release_tag_refs(refs, None).unwrap();

        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name(), "refs/tags/v1.2.3");
        assert_eq!(tags[0].version().to_string(), "1.2.3");
        assert_eq!(tags[0].object_id(), &object_id(1));
    }

    #[test]
    fn filters_and_orders_release_tags_for_a_package_directory() {
        let directory = RepositoryPackageDirectory::parse("packages/core").unwrap();
        let refs = [
            (b"refs/tags/packages/core/v2.0.0".as_slice(), object_id(2)),
            (b"refs/tags/packages/core/v1.0.0".as_slice(), object_id(1)),
            (b"refs/tags/v0.9.0".as_slice(), object_id(9)),
            (b"refs/tags/packages/other/v3.0.0".as_slice(), object_id(3)),
        ];

        let tags = parse_release_tag_refs(refs, Some(&directory)).unwrap();

        assert_eq!(
            tags.iter().map(|tag| tag.name()).collect::<Vec<_>>(),
            [
                "refs/tags/packages/core/v1.0.0",
                "refs/tags/packages/core/v2.0.0",
            ]
        );
    }

    #[test]
    fn treats_dot_directory_as_the_repository_root() {
        let directory = RepositoryPackageDirectory::parse(".").unwrap();
        let tags = parse_release_tag_refs(
            [(b"refs/tags/v1.0.0".as_slice(), object_id(1))],
            Some(&directory),
        )
        .unwrap();

        assert_eq!(tags.len(), 1);
    }

    #[test]
    fn accepts_output_directories_that_form_valid_git_refs() {
        let directory = RepositoryPackageDirectory::parse("out/debug").unwrap();
        let tags = parse_release_tag_refs(
            [(b"refs/tags/out/debug/v1.0.0".as_slice(), object_id(1))],
            Some(&directory),
        )
        .unwrap();
        assert_eq!(tags.len(), 1);
    }

    #[test]
    fn rejects_repository_directories_that_cannot_form_git_refs() {
        for directory in [".sai/cache", "dir with space"] {
            let directory = RepositoryPackageDirectory::parse(directory).unwrap();
            assert_eq!(
                parse_release_tag_refs(
                    [(b"refs/tags/v1.0.0".as_slice(), object_id(1))],
                    Some(&directory),
                ),
                Err(GitReleaseTagListError::InvalidPackageDirectory)
            );
        }
    }

    #[test]
    fn enforces_the_canonical_release_tag_listing_limit() {
        let refs = (0..=MAX_RELEASE_TAGS_PER_LISTING).map(|patch| {
            (
                format!("refs/tags/v1.0.{patch}"),
                object_id((patch % 16) as u8),
            )
        });

        assert_eq!(
            parse_release_tag_refs(refs, None),
            Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::ReleaseTagCount,
                actual: (MAX_RELEASE_TAGS_PER_LISTING + 1) as u64,
            })
        );
    }

    #[test]
    fn bounds_all_advertised_refs_and_name_bytes() {
        let limits = ListingLimits {
            git_refs: 2,
            ref_name_bytes: 100,
            version_identifiers: MAX_RELEASE_TAG_IDENTIFIERS_PER_LISTING,
            release_tags: MAX_RELEASE_TAGS_PER_LISTING,
        };
        let count_exceeded = [
            (b"refs/heads/main".as_slice(), object_id(1)),
            (b"refs/heads/dev".as_slice(), object_id(2)),
            (b"refs/heads/extra".as_slice(), object_id(3)),
        ];
        assert_eq!(
            parse_release_tag_refs_with_limits(count_exceeded, None, limits),
            Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefCount,
                actual: 3,
            })
        );

        let limits = ListingLimits {
            git_refs: 10,
            ref_name_bytes: 20,
            version_identifiers: MAX_RELEASE_TAG_IDENTIFIERS_PER_LISTING,
            release_tags: MAX_RELEASE_TAGS_PER_LISTING,
        };
        let bytes_exceeded = [
            (b"refs/heads/main".as_slice(), object_id(1)),
            (b"refs/heads/development".as_slice(), object_id(2)),
        ];
        assert_eq!(
            parse_release_tag_refs_with_limits(bytes_exceeded, None, limits),
            Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefNameBytes,
                actual: 37,
            })
        );
    }

    #[test]
    fn bounds_prerelease_version_identifier_allocation() {
        let limits = ListingLimits {
            git_refs: 10,
            ref_name_bytes: 100,
            version_identifiers: 2,
            release_tags: MAX_RELEASE_TAGS_PER_LISTING,
        };
        let refs = [(b"refs/tags/v1.2.3-a.b.c".as_slice(), object_id(1))];

        assert_eq!(
            parse_release_tag_refs_with_limits(refs, None, limits),
            Err(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::VersionIdentifierCount,
                actual: 3,
            })
        );
    }

    #[test]
    fn rejects_conflicting_duplicate_release_refs_independent_of_order() {
        let left = (b"refs/tags/v1.2.3".as_slice(), object_id(1));
        let right = (b"refs/tags/v1.2.3".as_slice(), object_id(2));
        let expected = Err(GitReleaseTagListError::ConflictingReleaseTag {
            name: "refs/tags/v1.2.3".into(),
        });

        assert_eq!(parse_release_tag_refs([left, right], None), expected);
        assert_eq!(parse_release_tag_refs([right, left], None), expected);
    }

    #[test]
    fn deduplicates_identical_release_refs() {
        let duplicate = (b"refs/tags/v1.2.3".as_slice(), object_id(1));
        let tags = parse_release_tag_refs([duplicate, duplicate], None).unwrap();

        assert_eq!(tags.len(), 1);
    }
}
