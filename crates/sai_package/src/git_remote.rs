//! Bounded, no-object-fetch Git release-tag listing.

use std::error::Error as StdError;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gix::bstr::{BStr, BString, ByteSlice};

use crate::git_tags::{
    GitReleaseTag, GitReleaseTagListError, GitReleaseTagListLimit, parse_release_tag_refs,
};
use crate::lock_validation::canonical_git_url;
use crate::package_path::DependencyPackagePath;

const SSH_TAG_LIST_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Errors while listing package release tags from a Git remote.
///
/// Transport details are intentionally not retained: underlying Git, proxy,
/// and credential errors can contain private URLs or authentication material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitRemoteTagListError {
    /// The URL is not a supported secure SAI Git URL.
    InvalidGitUrl,
    /// The package directory does not map to a release-tag namespace.
    InvalidPackageDirectory,
    /// A temporary repository could not be created or initialized.
    TemporaryRepository,
    /// The remote URL or refspec could not be configured.
    RemoteConfiguration,
    /// The remote did not provide a usable ref advertisement.
    FetchFailed,
    /// The ref advertisement contained invalid or over-limit release tags.
    TagList(GitReleaseTagListError),
}

impl GitRemoteTagListError {
    /// Return the user-facing diagnostic code for this failure.
    pub const fn diagnostic_code(&self) -> sai_diagnostics::Code {
        match self {
            Self::InvalidGitUrl => sai_diagnostics::Code::UnsupportedGitUrl,
            Self::InvalidPackageDirectory => sai_diagnostics::Code::InvalidPackagePath,
            Self::TemporaryRepository | Self::RemoteConfiguration | Self::FetchFailed => {
                sai_diagnostics::Code::FetchFailed
            }
            Self::TagList(GitReleaseTagListError::LimitExceeded { .. }) => {
                sai_diagnostics::Code::PackageTooLarge
            }
            Self::TagList(GitReleaseTagListError::InvalidPackageDirectory) => {
                sai_diagnostics::Code::InvalidPackagePath
            }
            Self::TagList(GitReleaseTagListError::ConflictingReleaseTag { .. }) => {
                sai_diagnostics::Code::FetchFailed
            }
        }
    }
}

impl fmt::Display for GitRemoteTagListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGitUrl => formatter.write_str("Git URL is not supported"),
            Self::InvalidPackageDirectory => {
                formatter.write_str("Git package directory is not normalized")
            }
            Self::TemporaryRepository => {
                formatter.write_str("Could not prepare the temporary Git repository")
            }
            Self::RemoteConfiguration => formatter.write_str("Could not configure the Git remote"),
            Self::FetchFailed => formatter.write_str("Could not list Git release tags"),
            Self::TagList(error) => error.fmt(formatter),
        }
    }
}

impl StdError for GitRemoteTagListError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::TagList(error) => Some(error),
            _ => None,
        }
    }
}

/// List canonical release tags for a secure Git URL without fetching objects.
///
/// The remote URL is canonicalized before use. Only credential-helper settings
/// from trusted user-level Git configuration and the documented HTTPS proxy
/// variables are passed to the isolated transport. Other Git configuration
/// and environment-based Git overrides are excluded.
pub fn list_remote_release_tags(
    url: &str,
    directory: Option<&DependencyPackagePath>,
) -> Result<Vec<GitReleaseTag>, GitRemoteTagListError> {
    let refspec = release_tag_refspec(directory)?;
    let canonical_url =
        canonical_git_url(url).map_err(|()| GitRemoteTagListError::InvalidGitUrl)?;
    let config_overrides = transport_config_overrides()?;
    let scratch =
        TemporaryGitDirectory::create().map_err(|()| GitRemoteTagListError::TemporaryRepository)?;
    let open_options = gix::open::Options::isolated().config_overrides(config_overrides);
    let repository = gix::ThreadSafeRepository::init_opts(
        &scratch.path,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        open_options,
    )
    .map_err(|_| GitRemoteTagListError::TemporaryRepository)?;
    let repository = repository.to_thread_local();

    list_release_tags_in_repository(&repository, &canonical_url, &refspec, directory)
}

fn list_release_tags_in_repository(
    repository: &gix::Repository,
    url: &str,
    refspec: &str,
    directory: Option<&DependencyPackagePath>,
) -> Result<Vec<GitReleaseTag>, GitRemoteTagListError> {
    let remote = repository
        .remote_at_without_url_rewrite(url)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
        .with_refspecs([BString::from(refspec)], gix::remote::Direction::Fetch)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
        .with_fetch_tags(gix::remote::fetch::Tags::None);
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(map_transport_error)?
        .with_transport_options(Box::new(
            gix_transport::client::blocking_io::SubprocessReadTimeout::new(
                SSH_TAG_LIST_READ_TIMEOUT,
            ),
        ));
    let (ref_map, _) = connection
        .ref_map(gix::progress::Discard, Default::default())
        .map_err(map_transport_error)?;

    let refs = ref_map.remote_refs.into_iter().filter_map(|reference| {
        let (name, object_id, _peeled_id) = reference.unpack();
        Some((name.to_owned(), object_id?.to_owned()))
    });
    parse_release_tag_refs(refs, directory).map_err(GitRemoteTagListError::TagList)
}

fn map_transport_error(error: impl StdError + 'static) -> GitRemoteTagListError {
    let mut source = Some(&error as &(dyn StdError + 'static));
    while let Some(current) = source {
        let limit = current
            .downcast_ref::<gix::protocol::handshake::refs::parse::AdvertisementLimitExceeded>()
            .or_else(|| {
                current
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::get_ref)
                    .and_then(|source| {
                        source.downcast_ref::<
                            gix::protocol::handshake::refs::parse::AdvertisementLimitExceeded,
                        >()
                    })
            });
        if let Some(limit) = limit {
            let actual = limit.actual;
            let limit = match limit.limit {
                gix::protocol::handshake::refs::parse::AdvertisementLimit::RefCount => {
                    GitReleaseTagListLimit::GitRefCount
                }
                gix::protocol::handshake::refs::parse::AdvertisementLimit::RefNameBytes => {
                    GitReleaseTagListLimit::GitRefNameBytes
                }
            };
            return GitRemoteTagListError::TagList(GitReleaseTagListError::LimitExceeded {
                limit,
                actual,
            });
        }
        source = current.source();
    }
    GitRemoteTagListError::FetchFailed
}

fn release_tag_refspec(
    directory: Option<&DependencyPackagePath>,
) -> Result<String, GitRemoteTagListError> {
    let prefix = match directory.map(DependencyPackagePath::as_str) {
        None | Some(".") => "refs/tags/v".to_owned(),
        Some(directory) if directory.split('/').any(|component| component == "..") => {
            return Err(GitRemoteTagListError::InvalidPackageDirectory);
        }
        Some(directory) => format!("refs/tags/{directory}/v"),
    };
    Ok(format!("{prefix}*:{prefix}*"))
}

fn transport_config_overrides() -> Result<Vec<BString>, GitRemoteTagListError> {
    let mut overrides = vec![
        config_override("protocol.version", "2"),
        config_override("http.sslVerify", "true"),
        config_override("gitoxide.https.sslNoVerify", "false"),
        config_override("gitoxide.credentials.terminalPrompt", "false"),
        config_override("gitoxide.credentials.helperStderr", "false"),
        config_override("ssh.variant", "ssh"),
        config_override("http.followRedirects", "initial"),
        config_override("gitoxide.credentials.useHttpPath", "false"),
        config_override("core.sshCommand", ssh_command()),
    ];
    overrides.extend(proxy_environment_overrides(|name| std::env::var_os(name))?);
    overrides.extend(user_credential_overrides()?);
    Ok(overrides)
}

fn ssh_command() -> &'static str {
    #[cfg(windows)]
    {
        "ssh -F NUL -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
    }
    #[cfg(not(windows))]
    {
        "ssh -F /dev/null -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=2"
    }
}

fn config_override(key: &str, value: &str) -> BString {
    let mut bytes = Vec::with_capacity(key.len() + value.len() + 1);
    bytes.extend_from_slice(key.as_bytes());
    bytes.push(b'=');
    bytes.extend_from_slice(value.as_bytes());
    bytes.into()
}

fn proxy_environment_overrides(
    mut get: impl FnMut(&str) -> Option<OsString>,
) -> Result<Vec<BString>, GitRemoteTagListError> {
    let mut overrides = Vec::new();
    if let Some(proxy) = first_environment_value(&mut get, "HTTPS_PROXY", "https_proxy")? {
        if !proxy.is_empty() {
            overrides.push(config_override("http.proxy", &proxy));
        }
    }
    if let Some(no_proxy) = first_environment_value(&mut get, "NO_PROXY", "no_proxy")? {
        if !no_proxy.is_empty() {
            overrides.push(config_override("gitoxide.http.noProxy", &no_proxy));
        }
    }
    Ok(overrides)
}

fn first_environment_value(
    get: &mut impl FnMut(&str) -> Option<OsString>,
    upper: &str,
    lower: &str,
) -> Result<Option<String>, GitRemoteTagListError> {
    get(upper)
        .or_else(|| get(lower))
        .map(|value| {
            value
                .into_string()
                .map_err(|_| GitRemoteTagListError::RemoteConfiguration)
        })
        .transpose()
}

fn user_credential_overrides() -> Result<Vec<BString>, GitRemoteTagListError> {
    let mut environment = |name: &str| std::env::var_os(name);
    let home = gix::path::env::home_dir();
    let paths = user_credential_config_paths(&mut environment, home.as_deref());
    let mut overrides = Vec::new();
    for (path, source) in paths {
        if matches!(fs::metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        overrides.extend(credential_overrides_from_path(
            path,
            source,
            home.as_deref(),
        )?);
    }
    Ok(overrides)
}

fn credential_overrides_from_path(
    path: PathBuf,
    source: gix::config::Source,
    home: Option<&Path>,
) -> Result<Vec<BString>, GitRemoteTagListError> {
    let metadata = gix::config::file::Metadata::try_from_path(path, source)
        .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?;
    let config = gix::config::File::from_paths_metadata(
        [metadata],
        gix::config::file::init::Options {
            includes: gix::config::file::includes::Options::follow_without_conditional(home),
            ..Default::default()
        },
    )
    .map_err(|_| GitRemoteTagListError::RemoteConfiguration)?
    .ok_or(GitRemoteTagListError::RemoteConfiguration)?;
    Ok(credential_overrides_from_file(&config))
}

fn user_credential_config_paths(
    environment: &mut impl FnMut(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Vec<(PathBuf, gix::config::Source)> {
    let mut paths = Vec::new();
    if let Some(path) = gix::path::env::xdg_config("config", environment) {
        paths.push((path, gix::config::Source::Git));
    }
    if let Some(home) = home {
        let path = home.join(".gitconfig");
        if !paths.iter().any(|(existing, _)| existing == &path) {
            paths.push((path, gix::config::Source::User));
        }
    }
    paths
}

fn credential_overrides_from_file(config: &gix::config::File) -> Vec<BString> {
    let mut overrides = Vec::new();
    for section in config.sections() {
        if section.meta().trust != gix::sec::Trust::Full
            || !section
                .header()
                .name()
                .as_bytes()
                .eq_ignore_ascii_case(b"credential")
        {
            continue;
        }

        let subsection = section.header().subsection_name();
        for name in ["helper", "username", "useHttpPath", "protectProtocol"] {
            let key = credential_key(subsection, name.as_bytes());
            for value in section.values(name) {
                let mut setting = key.clone();
                setting.push(b'=');
                setting.extend_from_slice(&value);
                overrides.push(setting);
            }
        }
    }
    overrides
}

fn credential_key(subsection: Option<&BStr>, value_name: &[u8]) -> BString {
    let mut key = b"credential".to_vec();
    if let Some(subsection) = subsection {
        key.push(b'.');
        key.extend_from_slice(subsection.as_bytes());
    }
    key.push(b'.');
    key.extend_from_slice(value_name);
    key.into()
}

struct TemporaryGitDirectory {
    path: PathBuf,
}

impl TemporaryGitDirectory {
    fn create() -> Result<Self, ()> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let base = std::env::temp_dir();
        for _ in 0..128 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("sai-git-remote-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(()),
            }
        }
        Err(())
    }
}

impl Drop for TemporaryGitDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{
        GitRemoteTagListError, TemporaryGitDirectory, credential_overrides_from_file,
        credential_overrides_from_path, list_release_tags_in_repository, map_transport_error,
        proxy_environment_overrides, release_tag_refspec, user_credential_config_paths,
    };
    use crate::git_tags::{GitReleaseTagListError, GitReleaseTagListLimit};
    use crate::package_path::DependencyPackagePath;

    #[test]
    fn maps_advertisement_limit_failures_to_package_too_large() {
        let error = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            gix::protocol::handshake::refs::parse::AdvertisementLimitExceeded {
                limit: gix::protocol::handshake::refs::parse::AdvertisementLimit::RefCount,
                actual: 1_000_001,
            },
        );

        assert_eq!(
            map_transport_error(error),
            GitRemoteTagListError::TagList(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefCount,
                actual: 1_000_001,
            })
        );
        assert_eq!(
            GitRemoteTagListError::TagList(GitReleaseTagListError::LimitExceeded {
                limit: GitReleaseTagListLimit::GitRefCount,
                actual: 1_000_001,
            })
            .diagnostic_code(),
            sai_diagnostics::Code::PackageTooLarge
        );
    }

    #[test]
    fn refspec_requests_only_the_package_release_tag_prefix() {
        assert_eq!(
            release_tag_refspec(None).unwrap(),
            "refs/tags/v*:refs/tags/v*"
        );
        let directory = DependencyPackagePath::parse("packages/text").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&directory)).unwrap(),
            "refs/tags/packages/text/v*:refs/tags/packages/text/v*"
        );
    }

    #[test]
    fn rejects_dependency_directories_that_escape_the_repository() {
        let directory = DependencyPackagePath::parse("../shared").unwrap();
        assert_eq!(
            release_tag_refspec(Some(&directory)),
            Err(GitRemoteTagListError::InvalidPackageDirectory)
        );
    }

    #[test]
    fn only_trusted_credential_keys_are_copied_from_user_configuration() {
        let config = gix::config::File::try_from(
            r#"
                [credential]
                    helper = store
                    helper =
                    useHttpPath = true
                [credential "https://example.com/team/repo"]
                    helper = "!command with spaces"
                    username = build-user
                [http]
                    proxy = http://ignored.example
                    extraHeader = Authorization: ignored
                [core]
                    sshCommand = ignored
                [url "https://mirror.example/"]
                    insteadOf = https://example.com/
            "#,
        )
        .unwrap();

        let values = credential_overrides_from_file(&config)
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                "credential.helper=store",
                "credential.helper=",
                "credential.useHttpPath=true",
                "credential.https://example.com/team/repo.helper=!command with spaces",
                "credential.https://example.com/team/repo.username=build-user",
            ]
        );
    }

    #[test]
    fn finds_the_standard_xdg_git_config_path() {
        let mut environment = |name: &str| match name {
            "XDG_CONFIG_HOME" => Some(OsString::from("/home/build/.config")),
            _ => None,
        };
        let paths = user_credential_config_paths(&mut environment, Some(Path::new("/home/build")));

        assert_eq!(
            paths.into_iter().map(|(path, _)| path).collect::<Vec<_>>(),
            [
                PathBuf::from("/home/build/.config/git/config"),
                PathBuf::from("/home/build/.gitconfig"),
            ]
        );
    }

    #[test]
    fn extracts_only_credential_settings_from_trusted_user_includes() {
        let directory = TemporaryGitDirectory::create().unwrap();
        let user_config = directory.path.join(".gitconfig");
        std::fs::write(
            &user_config,
            "[include]\n\tpath = credentials.inc\n[http]\n\tsslVerify = false\n",
        )
        .unwrap();
        std::fs::write(
            directory.path.join("credentials.inc"),
            "[credential]\n\thelper = manager\n[core]\n\tsshCommand = ignored\n",
        )
        .unwrap();

        let values = credential_overrides_from_path(
            user_config,
            gix::config::Source::User,
            Some(&directory.path),
        )
        .unwrap()
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();

        assert_eq!(values, ["credential.helper=manager"]);
    }

    #[test]
    fn honors_only_the_documented_https_proxy_environment_variables() {
        let values = proxy_environment_overrides(|name| match name {
            "HTTPS_PROXY" => Some(OsString::from("proxy.internal:8080")),
            "https_proxy" => Some(OsString::from("ignored-lowercase:8080")),
            "NO_PROXY" => Some(OsString::from("localhost,.internal")),
            "HTTP_PROXY" | "ALL_PROXY" => Some(OsString::from("ignored")),
            _ => None,
        })
        .unwrap()
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                "http.proxy=proxy.internal:8080",
                "gitoxide.http.noProxy=localhost,.internal",
            ]
        );
    }

    #[test]
    fn ref_advertisements_do_not_fetch_tag_objects() {
        let id = "1111111111111111111111111111111111111111";
        let peeled = "2222222222222222222222222222222222222222";
        let test_dir = super::TemporaryGitDirectory::create().unwrap();
        let test_dir_path = &test_dir.path;
        std::fs::create_dir_all(test_dir_path.join("refs/tags")).unwrap();
        std::fs::write(
            test_dir_path.join("config"),
            "[core]\n\tbare = true\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        std::fs::write(test_dir_path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            test_dir_path.join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled\n{id} refs/tags/v1.2.3\n^{peeled}\n"),
        )
        .unwrap();
        let object_dir = test_dir_path.join("objects");
        std::fs::create_dir_all(object_dir.join("info")).unwrap();
        std::fs::create_dir_all(object_dir.join("pack")).unwrap();

        let scratch = super::TemporaryGitDirectory::create().unwrap();
        let repository = gix::ThreadSafeRepository::init_opts(
            &scratch.path,
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated(),
        )
        .unwrap()
        .to_thread_local();
        let remote = format!("file://{}", test_dir_path.display());
        let refspec = "refs/tags/v*:refs/tags/v*";
        let tags = list_release_tags_in_repository(&repository, &remote, refspec, None).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name(), "refs/tags/v1.2.3");
        assert_eq!(tags[0].object_id().to_string(), id);
        for entry in std::fs::read_dir(scratch.path.join("objects")).unwrap() {
            let entry = entry.unwrap();
            assert!(matches!(entry.file_name().to_str(), Some("info" | "pack")));
            assert!(entry.file_type().unwrap().is_dir());
            assert!(entry.path().read_dir().unwrap().next().is_none());
        }
    }
}
