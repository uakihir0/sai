//! Source identity lookup for packages fetched from Git.

use std::collections::BTreeMap;

use crate::git_remote::{GitPackagePathError, GitPackageSourceIdentity};
use crate::mvs::{MvsPackageVersion, MvsRequirer};
use crate::resolver_source::{ResolverPackageSource, ResolverPathPinSource};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GitPackageSourceRequirer {
    Root(MvsPackageVersion),
    Package(MvsPackageVersion),
}

impl From<&MvsRequirer> for GitPackageSourceRequirer {
    fn from(requirer: &MvsRequirer) -> Self {
        match requirer {
            MvsRequirer::Root(package_version) => Self::Root(package_version.clone()),
            MvsRequirer::Package(package_version) => Self::Package(package_version.clone()),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GitPackageSourceIdentityKey {
    canonical_url: Box<str>,
    commit_id: gix::ObjectId,
    directory: crate::package_path::RepositoryPackageDirectory,
}

impl From<&GitPackageSourceIdentity> for GitPackageSourceIdentityKey {
    fn from(identity: &GitPackageSourceIdentity) -> Self {
        Self {
            canonical_url: identity.canonical_url().into(),
            commit_id: *identity.commit_id(),
            directory: identity.directory().clone(),
        }
    }
}

/// A Git path pin could not be resolved from the identities loaded so far.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitPackageSourceIdentityIndexError {
    /// A root or dependency package version was already associated with
    /// another Git source.
    ConflictingSourceIdentity(MvsRequirer),
    /// A Git source was already associated with another package version.
    ConflictingPackageVersion {
        existing: Box<MvsPackageVersion>,
        discovered: Box<MvsPackageVersion>,
    },
    /// The exact root or dependency requirer has not yet been loaded from Git.
    MissingRequirer(MvsRequirer),
    /// The path source's base differs from the edge's requiring package.
    RequirerMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    /// The selection is not a path source.
    NotPathSource,
    /// The dependency path is invalid or leaves the repository.
    Path(GitPackagePathError),
}

/// Git source identities for the packages loaded during one resolution.
///
/// The index records the exact repository, commit, and directory that supplied
/// each requiring package. It can then canonicalize a Git-relative path pin
/// without consulting a release tag or reopening mutable package metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitPackageSourceIdentityIndex {
    identities: BTreeMap<GitPackageSourceRequirer, GitPackageSourceIdentity>,
    source_versions: BTreeMap<GitPackageSourceIdentityKey, MvsPackageVersion>,
}

impl GitPackageSourceIdentityIndex {
    /// Record the source identity paired with a successfully loaded root or
    /// dependency package.
    ///
    /// Re-inserting an identical pair is idempotent. A package version cannot
    /// silently switch repositories, commits, or package directories during
    /// one resolution.
    pub fn insert(
        &mut self,
        requirer: MvsRequirer,
        identity: GitPackageSourceIdentity,
    ) -> Result<(), GitPackageSourceIdentityIndexError> {
        let key = GitPackageSourceRequirer::from(&requirer);
        match self.identities.get(&key) {
            Some(existing) if existing == &identity => Ok(()),
            Some(_) => Err(GitPackageSourceIdentityIndexError::ConflictingSourceIdentity(requirer)),
            None => {
                let source_key = GitPackageSourceIdentityKey::from(&identity);
                if let Some(existing) = self.source_versions.get(&source_key) {
                    if existing != requirer.package_version() {
                        return Err(
                            GitPackageSourceIdentityIndexError::ConflictingPackageVersion {
                                existing: Box::new(existing.clone()),
                                discovered: Box::new(requirer.package_version().clone()),
                            },
                        );
                    }
                }
                self.source_versions
                    .entry(source_key)
                    .or_insert_with(|| requirer.package_version().clone());
                self.identities.insert(key, identity);
                Ok(())
            }
        }
    }

    /// Return the source identity recorded for one exact root or dependency
    /// requirer. The same version can have separate root and dependency
    /// identities during source-aware MVS.
    pub fn get(&self, requirer: &MvsRequirer) -> Option<&GitPackageSourceIdentity> {
        self.identities
            .get(&GitPackageSourceRequirer::from(requirer))
    }

    /// Resolve one path pin against the exact Git source of its requirer.
    ///
    /// This returns the complete path-package identity used to compare
    /// multiple pins before the selected sibling is fetched.
    pub fn canonicalize_path_pin(
        &self,
        pin: &ResolverPathPinSource,
    ) -> Result<GitPackageSourceIdentity, GitPackageSourceIdentityIndexError> {
        let ResolverPackageSource::Path { relative_to, path } = &pin.source else {
            return Err(GitPackageSourceIdentityIndexError::NotPathSource);
        };
        let requirer = pin.requirer.package_version();
        if relative_to != requirer {
            return Err(GitPackageSourceIdentityIndexError::RequirerMismatch {
                expected: Box::new(requirer.clone()),
                found: Box::new(relative_to.clone()),
            });
        }
        let identity = self.get(&pin.requirer).ok_or_else(|| {
            GitPackageSourceIdentityIndexError::MissingRequirer(pin.requirer.clone())
        })?;
        identity
            .resolve_dependency_path(path)
            .map_err(GitPackageSourceIdentityIndexError::Path)
    }
}
