//! Discovery of Git-relative path packages named by a fetched Git manifest.

use std::collections::BTreeMap;

use crate::git_remote::{
    FetchedGitPackage, GitPackagePathError, GitPackageSourceIdentity, GitPackageSourceView,
};
use crate::manifest::{PackageDeclaration, PackageParseOutput, RequirementSource};
use crate::mvs::{MvsPackageVersion, MvsRequirer};
use crate::package_path::RepositoryPackageDirectory;
use crate::resolver_contract::{ResolverContractContext, ResolverPathKey};

/// A fetched Git path package retained for reuse by source-aware MVS.
#[derive(Debug, Eq, PartialEq)]
struct GitPathPackageSnapshot {
    package: FetchedGitPackage,
    package_version: MvsPackageVersion,
}

/// Git-relative path packages discovered for one requiring package.
///
/// `path_packages` is suitable for `ResolverContractContext::with_path_packages`.
/// Each `ResolverPathKey` also points to the fetched tree at the exact commit
/// and normalized directory named by the requiring package's source identity.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct GitPathRequirementIndex {
    path_packages: BTreeMap<ResolverPathKey, MvsPackageVersion>,
    path_source_keys: BTreeMap<ResolverPathKey, GitPackageSourceIdentityKey>,
    packages: BTreeMap<GitPackageSourceIdentityKey, GitPathPackageSnapshot>,
}

impl GitPathRequirementIndex {
    /// The resolved package versions keyed by requiring package and path spelling.
    pub fn path_packages(&self) -> &BTreeMap<ResolverPathKey, MvsPackageVersion> {
        &self.path_packages
    }

    /// The fetched sibling for one path spelling, when that path was discovered.
    pub fn package(&self, key: &ResolverPathKey) -> Option<&FetchedGitPackage> {
        let source_key = self.path_source_keys.get(key)?;
        self.packages
            .get(source_key)
            .map(|snapshot| &snapshot.package)
    }

    /// The declared package version for one path spelling.
    pub fn package_version(&self, key: &ResolverPathKey) -> Option<&MvsPackageVersion> {
        self.path_packages.get(key)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GitPackageSourceIdentityKey {
    canonical_url: Box<str>,
    commit_id: gix::ObjectId,
    directory: RepositoryPackageDirectory,
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

/// A Git-relative dependency could not be discovered from its fetched source.
#[derive(Debug, Eq, PartialEq)]
pub enum GitPathRequirementDiscoveryError<E> {
    /// The requiring package manifest is syntactically invalid.
    InvalidRequirerManifest(Box<PackageParseOutput>),
    /// The requiring manifest does not have a package header.
    MissingRequirerHeader(Box<PackageParseOutput>),
    /// The requiring manifest declares an invalid package identity.
    InvalidRequirerIdentity {
        manifest: Box<PackageParseOutput>,
        identity: Box<str>,
    },
    /// The requiring manifest does not match the requested package version.
    RequirerVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    /// The root requirer does not match the root bound to the resolver context.
    RootContextMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    /// A sibling path is invalid or escapes the repository.
    DependencyPath {
        path: Box<str>,
        error: GitPackagePathError,
    },
    /// Fetching a sibling at the requiring commit failed.
    FetchSibling { path: Box<str>, source: Box<E> },
    /// The loader returned a package from another repository, commit, or directory.
    SiblingSourceIdentityMismatch {
        path: Box<str>,
        expected: Box<GitPackageSourceIdentity>,
        found: Box<GitPackageSourceIdentity>,
    },
    /// The fetched sibling manifest is syntactically invalid.
    InvalidSiblingManifest {
        path: Box<str>,
        manifest: Box<PackageParseOutput>,
    },
    /// The fetched sibling manifest does not have a package header.
    MissingSiblingHeader {
        path: Box<str>,
        manifest: Box<PackageParseOutput>,
    },
    /// The fetched sibling manifest declares an invalid package identity.
    InvalidSiblingIdentity {
        path: Box<str>,
        manifest: Box<PackageParseOutput>,
        identity: Box<str>,
    },
    /// The sibling's package identity does not match the requirement.
    SiblingIdentityMismatch {
        path: Box<str>,
        expected: Box<str>,
        found: Box<str>,
    },
}

impl<E> GitPathRequirementDiscoveryError<E> {
    /// Return parse diagnostics retained when a Git package manifest is invalid.
    pub fn manifest_parse_output(&self) -> Option<&PackageParseOutput> {
        match self {
            Self::InvalidRequirerManifest(manifest)
            | Self::MissingRequirerHeader(manifest)
            | Self::InvalidRequirerIdentity { manifest, .. }
            | Self::InvalidSiblingManifest { manifest, .. }
            | Self::MissingSiblingHeader { manifest, .. }
            | Self::InvalidSiblingIdentity { manifest, .. } => Some(manifest),
            _ => None,
        }
    }
}

/// Discover target-included Git-relative path requirements in one fetched
/// package, fetching each distinct sibling identity at most once.
///
/// Dependency test edges, requirements excluded by the root's active targets,
/// and requirements replaced by root path or Git overrides are omitted, in
/// line with local path requirement discovery. The fetch callback must return
/// the package from the exact identity it receives; the result is checked
/// before its manifest is used.
pub fn discover_git_path_requirements<E>(
    requiring: &impl GitPackageSourceView,
    requirer: &MvsRequirer,
    root_context: &ResolverContractContext<'_>,
    mut fetch_sibling: impl FnMut(&GitPackageSourceIdentity) -> Result<FetchedGitPackage, E>,
) -> Result<GitPathRequirementIndex, GitPathRequirementDiscoveryError<E>> {
    if let MvsRequirer::Root(root_package) = requirer
        && root_package != root_context.root_package()
    {
        return Err(GitPathRequirementDiscoveryError::RootContextMismatch {
            expected: Box::new(root_context.root_package().clone()),
            found: Box::new(root_package.clone()),
        });
    }
    let expected_requirer = requirer.package_version();
    let manifest = requiring.git_package_manifest();
    let actual_requirer = manifest_package_version(manifest).map_err(|error| match error {
        ManifestPackageVersionError::InvalidSyntax => {
            GitPathRequirementDiscoveryError::InvalidRequirerManifest(Box::new(manifest.clone()))
        }
        ManifestPackageVersionError::MissingHeader => {
            GitPathRequirementDiscoveryError::MissingRequirerHeader(Box::new(manifest.clone()))
        }
        ManifestPackageVersionError::InvalidIdentity(identity) => {
            GitPathRequirementDiscoveryError::InvalidRequirerIdentity {
                manifest: Box::new(manifest.clone()),
                identity,
            }
        }
    })?;
    if &actual_requirer != expected_requirer {
        return Err(GitPathRequirementDiscoveryError::RequirerVersionMismatch {
            expected: Box::new(expected_requirer.clone()),
            found: Box::new(actual_requirer),
        });
    }

    let root_targets = root_context.root_targets();
    let mut index = GitPathRequirementIndex::default();
    for declaration in &manifest.file().declarations {
        let PackageDeclaration::Requires { test, entries } = &declaration.value else {
            continue;
        };
        if *test {
            continue;
        }
        for item in entries {
            let requirement = &item.value;
            let included = requirement.only.as_ref().is_none_or(|only| {
                only.value
                    .iter()
                    .any(|target| root_targets.contains(&target.value))
            });
            let path_override_applies = root_context
                .root_path_override(&requirement.package)
                .is_some_and(|package_version| {
                    package_version.version().compatibility_line()
                        == requirement.version.compatibility_line()
                });
            if !included
                || root_context.has_root_git_override(&requirement.package)
                || path_override_applies
            {
                continue;
            }
            let Some(RequirementSource::Path(path)) =
                requirement.source.as_ref().map(|source| &source.value)
            else {
                continue;
            };

            let path_key = ResolverPathKey::new(requirer.clone(), path.clone());
            if index.path_packages.contains_key(&path_key) {
                continue;
            }
            let sibling_identity = requiring
                .git_source_identity()
                .resolve_dependency_path(path)
                .map_err(|error| GitPathRequirementDiscoveryError::DependencyPath {
                    path: path.clone(),
                    error,
                })?;
            let identity_key = GitPackageSourceIdentityKey::from(&sibling_identity);

            if !index.packages.contains_key(&identity_key) {
                let sibling = fetch_sibling(&sibling_identity).map_err(|source| {
                    GitPathRequirementDiscoveryError::FetchSibling {
                        path: path.clone(),
                        source: Box::new(source),
                    }
                })?;
                if sibling.source_identity() != &sibling_identity {
                    return Err(
                        GitPathRequirementDiscoveryError::SiblingSourceIdentityMismatch {
                            path: path.clone(),
                            expected: Box::new(sibling_identity),
                            found: Box::new(sibling.source_identity().clone()),
                        },
                    );
                }
                let sibling_manifest = sibling.source().package_manifest();
                let package_version =
                    manifest_package_version(sibling_manifest).map_err(|error| match error {
                        ManifestPackageVersionError::InvalidSyntax => {
                            GitPathRequirementDiscoveryError::InvalidSiblingManifest {
                                path: path.clone(),
                                manifest: Box::new(sibling_manifest.clone()),
                            }
                        }
                        ManifestPackageVersionError::MissingHeader => {
                            GitPathRequirementDiscoveryError::MissingSiblingHeader {
                                path: path.clone(),
                                manifest: Box::new(sibling_manifest.clone()),
                            }
                        }
                        ManifestPackageVersionError::InvalidIdentity(identity) => {
                            GitPathRequirementDiscoveryError::InvalidSiblingIdentity {
                                path: path.clone(),
                                manifest: Box::new(sibling_manifest.clone()),
                                identity,
                            }
                        }
                    })?;
                index.packages.insert(
                    identity_key.clone(),
                    GitPathPackageSnapshot {
                        package: sibling,
                        package_version,
                    },
                );
            }

            let package_version = index
                .packages
                .get(&identity_key)
                .expect("a fetched or previously indexed Git identity has a package snapshot")
                .package_version
                .clone();
            if package_version.package() != requirement.package.as_ref() {
                return Err(GitPathRequirementDiscoveryError::SiblingIdentityMismatch {
                    path: path.clone(),
                    expected: requirement.package.clone(),
                    found: package_version.package().into(),
                });
            }
            index
                .path_packages
                .insert(path_key.clone(), package_version);
            index.path_source_keys.insert(path_key, identity_key);
        }
    }
    Ok(index)
}

enum ManifestPackageVersionError {
    InvalidSyntax,
    MissingHeader,
    InvalidIdentity(Box<str>),
}

fn manifest_package_version(
    manifest: &PackageParseOutput,
) -> Result<MvsPackageVersion, ManifestPackageVersionError> {
    if !manifest.is_syntax_valid() {
        return Err(ManifestPackageVersionError::InvalidSyntax);
    }
    let header = manifest
        .file()
        .header
        .as_ref()
        .ok_or(ManifestPackageVersionError::MissingHeader)?;
    let identity = header.value.identity.to_string();
    MvsPackageVersion::new(identity.clone(), header.value.version.clone())
        .map_err(|_| ManifestPackageVersionError::InvalidIdentity(identity.into()))
}
