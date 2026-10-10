use std::fmt;

/// A validated source, WIT, or artifact path written in a package contract.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentPackagePath {
    value: Box<str>,
}

/// A validated `from path` or `use path` dependency path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DependencyPackagePath {
    value: Box<str>,
}

/// A normalized package directory inside a Git repository.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RepositoryPackageDirectory {
    value: Box<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackagePathError {
    InvalidPackagePath,
    PathEscapesRepository,
}

impl ContentPackagePath {
    pub fn parse(value: &str) -> Result<Self, PackagePathError> {
        validate_path(value, PackagePathUse::Content)?;
        Ok(Self {
            value: value.into(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl DependencyPackagePath {
    pub fn parse(value: &str) -> Result<Self, PackagePathError> {
        validate_path(value, PackagePathUse::Dependency)?;
        Ok(Self {
            value: value.into(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl RepositoryPackageDirectory {
    pub fn parse(value: &str) -> Result<Self, PackagePathError> {
        validate_path(value, PackagePathUse::RepositoryDirectory)?;
        Ok(Self {
            value: value.into(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Resolve a path dependency inside a Git package to its repository-relative
/// package directory.
///
/// `package_directory` is the requiring package's directory in the repository;
/// `None` denotes the repository root. The result is normalized lexically and
/// cannot escape the repository.
pub fn normalize_git_relative_package_directory(
    package_directory: Option<&RepositoryPackageDirectory>,
    dependency_path: &DependencyPackagePath,
) -> Result<RepositoryPackageDirectory, PackagePathError> {
    let mut components = package_directory
        .filter(|directory| directory.as_str() != ".")
        .map_or_else(Vec::new, |directory| {
            directory.as_str().split('/').collect()
        });

    for component in dependency_path.as_str().split('/') {
        match component {
            "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(PackagePathError::PathEscapesRepository);
                }
            }
            component => components.push(component),
        }
    }

    let normalized = if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    };
    RepositoryPackageDirectory::parse(&normalized)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackagePathUse {
    Content,
    Dependency,
    RepositoryDirectory,
}

fn validate_path(value: &str, usage: PackagePathUse) -> Result<(), PackagePathError> {
    if value.is_empty()
        || value.len() > 4096
        || value
            .bytes()
            .any(|byte| !(b' '..=b'~').contains(&byte) || b"<>:\"|?*\\".contains(&byte))
    {
        return Err(PackagePathError::InvalidPackagePath);
    }

    if value == "." {
        return Ok(());
    }

    if value.starts_with('/')
        || value.ends_with('/')
        || value.as_bytes().windows(2).any(|pair| pair == b"//")
    {
        return Err(PackagePathError::InvalidPackagePath);
    }

    let components = value.split('/').collect::<Vec<_>>();
    if components.iter().any(|component| {
        component.len() > 255
            || *component == "."
            || (*component != ".." && (component.ends_with('.') || component.ends_with(' ')))
            || component.eq_ignore_ascii_case(".git")
            || is_windows_device_name(component)
            || (*component == ".." && usage != PackagePathUse::Dependency)
    }) {
        return Err(PackagePathError::InvalidPackagePath);
    }
    if usage == PackagePathUse::Content
        && components.first().is_some_and(|component| {
            component.eq_ignore_ascii_case(".sai") || component.eq_ignore_ascii_case("out")
        })
    {
        return Err(PackagePathError::InvalidPackagePath);
    }

    Ok(())
}

fn is_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ');
    let bytes = stem.as_bytes();
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        || (bytes.len() == 4
            && (bytes[..3].eq_ignore_ascii_case(b"COM") || bytes[..3].eq_ignore_ascii_case(b"LPT"))
            && matches!(bytes[3], b'1'..=b'9'))
}

impl fmt::Display for ContentPackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

impl fmt::Display for DependencyPackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

impl fmt::Display for RepositoryPackageDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

impl fmt::Display for PackagePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackagePath => formatter.write_str("invalid path in `sai.package`"),
            Self::PathEscapesRepository => {
                formatter.write_str("Git-relative package path escapes the repository")
            }
        }
    }
}

impl std::error::Error for PackagePathError {}

#[cfg(test)]
mod tests {
    use super::{
        ContentPackagePath, DependencyPackagePath, PackagePathError, RepositoryPackageDirectory,
        normalize_git_relative_package_directory,
    };

    #[test]
    fn accepts_relative_content_and_dependency_paths() {
        for source in [".", "src", "src/main.sai", "assets/images/logo-2.svg"] {
            assert_eq!(
                ContentPackagePath::parse(source)
                    .expect("valid content path")
                    .as_str(),
                source
            );
        }
        assert_eq!(DependencyPackagePath::parse(".").unwrap().as_str(), ".");

        for source in ["../shared", "../../packages/core", "vendor/../local"] {
            assert_eq!(
                DependencyPackagePath::parse(source)
                    .expect("valid dependency path")
                    .as_str(),
                source
            );
        }
    }

    #[test]
    fn accepts_reserved_repository_directories_but_requires_normalization() {
        for directory in [".", ".sai/dependency", "out/sibling"] {
            assert_eq!(
                RepositoryPackageDirectory::parse(directory)
                    .expect("valid Git repository package directory")
                    .as_str(),
                directory
            );
        }
        for directory in ["../sibling", "packages/../sibling"] {
            assert_eq!(
                RepositoryPackageDirectory::parse(directory),
                Err(PackagePathError::InvalidPackagePath)
            );
        }
        assert_eq!(
            RepositoryPackageDirectory::parse("nested/../out"),
            Err(PackagePathError::InvalidPackagePath)
        );
        assert_eq!(
            ContentPackagePath::parse("out/sibling"),
            Err(PackagePathError::InvalidPackagePath)
        );
    }

    #[test]
    fn rejects_invalid_components_and_host_specific_paths() {
        for source in [
            "",
            "/absolute",
            "C:/absolute",
            "src\\main.sai",
            "src//main.sai",
            "/",
            "src/",
            "./src",
            "src/.",
            "../src",
            "src/../other",
            "src/.git/config",
            "a< b",
            "a>b",
            "name:stream",
            "name\"quote",
            "name|pipe",
            "name?query",
            "name*glob",
            "line\nbreak",
            "café",
            "src/.. /outside",
            "name.",
            "name ",
            "src/name.",
            "src/name ",
            "src/CON",
            "src/prn.txt",
            "src/Aux.log",
            "src/nul.data",
            "src/CoM1.txt",
            "src/CON .txt",
            "src/lpt9.out",
        ] {
            assert_eq!(
                ContentPackagePath::parse(source),
                Err(PackagePathError::InvalidPackagePath),
                "unexpectedly accepted {source:?}"
            );
        }
        assert!(DependencyPackagePath::parse("../src").is_ok());
        assert_eq!(
            DependencyPackagePath::parse(".git/config"),
            Err(PackagePathError::InvalidPackagePath)
        );
        assert_eq!(
            DependencyPackagePath::parse(".GIT/config"),
            Err(PackagePathError::InvalidPackagePath)
        );
        assert_eq!(
            DependencyPackagePath::parse("../shared "),
            Err(PackagePathError::InvalidPackagePath)
        );
    }

    #[test]
    fn content_paths_cannot_name_tool_state_or_output_roots() {
        for source in [
            ".sai",
            ".sai/cache",
            ".SAI/cache",
            "out",
            "out/debug",
            "OUT/cache",
        ] {
            assert_eq!(
                ContentPackagePath::parse(source),
                Err(PackagePathError::InvalidPackagePath)
            );
        }
        assert!(DependencyPackagePath::parse(".sai").is_ok());
        assert!(DependencyPackagePath::parse("out/cache").is_ok());
    }

    #[test]
    fn normalizes_git_relative_package_directories_within_the_repository() {
        let base = RepositoryPackageDirectory::parse("packages/client").unwrap();
        let sibling = DependencyPackagePath::parse("../shared").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(Some(&base), &sibling)
                .unwrap()
                .as_str(),
            "packages/shared"
        );

        let nested = DependencyPackagePath::parse("vendor/../local").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(Some(&base), &nested)
                .unwrap()
                .as_str(),
            "packages/client/local"
        );

        let root_base = RepositoryPackageDirectory::parse(".").unwrap();
        let root_relative = DependencyPackagePath::parse("packages/shared").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(None, &root_relative)
                .unwrap()
                .as_str(),
            "packages/shared"
        );
        let parent = DependencyPackagePath::parse("..").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(Some(&base), &parent)
                .unwrap()
                .as_str(),
            "packages"
        );
        let to_root = DependencyPackagePath::parse("../..").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(Some(&base), &to_root)
                .unwrap()
                .as_str(),
            "."
        );
        let current = DependencyPackagePath::parse(".").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(Some(&root_base), &current)
                .unwrap()
                .as_str(),
            "."
        );
    }

    #[test]
    fn rejects_git_relative_paths_that_escape_the_repository() {
        let base = RepositoryPackageDirectory::parse(".").unwrap();
        let parent = DependencyPackagePath::parse("..").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(None, &parent),
            Err(PackagePathError::PathEscapesRepository)
        );

        let over_parent = DependencyPackagePath::parse("../../shared").unwrap();
        assert_eq!(
            normalize_git_relative_package_directory(Some(&base), &over_parent),
            Err(PackagePathError::PathEscapesRepository)
        );

        for reserved in [".sai/cache", "out/debug", "nested/../out"] {
            let path = DependencyPackagePath::parse(reserved).unwrap();
            assert!(normalize_git_relative_package_directory(None, &path).is_ok());
        }

        let base = RepositoryPackageDirectory::parse("packages/client").unwrap();
        for (path, expected) in [
            ("../../out/sibling", "out/sibling"),
            ("../../.sai/sibling", ".sai/sibling"),
        ] {
            let path = DependencyPackagePath::parse(path).unwrap();
            assert_eq!(
                normalize_git_relative_package_directory(Some(&base), &path)
                    .unwrap()
                    .as_str(),
                expected
            );
        }
    }

    #[test]
    fn enforces_portable_path_length_limits() {
        assert!(ContentPackagePath::parse(&"x".repeat(255)).is_ok());
        assert_eq!(
            ContentPackagePath::parse(&"x".repeat(256)),
            Err(PackagePathError::InvalidPackagePath)
        );

        let mut components = vec!["x".repeat(255); 15];
        components.push("x".repeat(254));
        components.push("x".into());
        let at_limit = components.join("/");
        assert_eq!(at_limit.len(), 4096);
        assert!(ContentPackagePath::parse(&at_limit).is_ok());

        components[15].push('x');
        let over_limit = components.join("/");
        assert_eq!(over_limit.len(), 4097);
        assert_eq!(
            ContentPackagePath::parse(&over_limit),
            Err(PackagePathError::InvalidPackagePath)
        );
    }
}
