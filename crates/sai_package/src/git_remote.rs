//! Bounded collection of refs from a Git remote advertisement.
//!
//! This module is the consumer side of the bounded parser boundary. It does
//! not open a transport. In particular, do not implement [`BoundedRefParser`]
//! by calling gix's high-level `Connection::ref_map()`: that API materializes
//! the full advertisement before returning it.
//!
//! A production parser must stream one wire ref at a time, preserve the raw
//! advertised ref-name bytes, and enforce the supplied limits while parsing.
//! The collector below independently checks those limits before copying a
//! matching ref or growing its result `Vec`. Until a bounded gix-protocol
//! parser and transport adapter are integrated, this module alone does not
//! provide a remote reader or establish package/04 §4.4.4 compliance.

use std::error::Error;
use std::fmt;

use crate::git_tags::{MAX_GIT_REF_NAME_BYTES_PER_LISTING, MAX_GIT_REFS_PER_LISTING};
use crate::package_path::DependencyPackagePath;

/// One release-tag ref and its directly advertised object ID.
///
/// For an annotated tag this is the tag object ID. No object is fetched or
/// peeled here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdvertisedRef {
    name: Box<[u8]>,
    object_id: gix::ObjectId,
}

impl AdvertisedRef {
    /// The complete advertised ref-name spelling.
    pub(crate) fn name(&self) -> &[u8] {
        &self.name
    }

    /// The object ID advertised directly for this ref.
    pub(crate) fn object_id(&self) -> &gix::ObjectId {
        &self.object_id
    }
}

/// Fixed limits for one remote ref advertisement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RefAdvertisementLimits {
    /// Maximum number of wire refs.
    pub(crate) max_refs: u64,
    /// Maximum cumulative byte length of wire ref names.
    pub(crate) max_ref_name_bytes: u64,
}

impl RefAdvertisementLimits {
    const PACKAGE: Self = Self {
        max_refs: MAX_GIT_REFS_PER_LISTING,
        max_ref_name_bytes: MAX_GIT_REF_NAME_BYTES_PER_LISTING,
    };
}

/// The kind of one ref record in the server's advertisement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdvertisedRefKind {
    /// A ref with its own advertised object ID.
    Direct,
    /// A protocol-v1 peeled pseudo-ref, such as `refs/tags/v1.2.3^{}`.
    Peeled,
    /// A symbolic ref record.
    Symbolic,
    /// An unborn symbolic ref record with no object ID.
    Unborn,
}

/// A single ref record borrowed from the parser's current bounded input frame.
///
/// `name` is the wire ref name, before normalization. In protocol v1, a peeled
/// pseudo-ref therefore includes its `^{}` suffix and is reported as
/// [`AdvertisedRefKind::Peeled`]. Protocol-v2 peeled metadata remains attached
/// to its direct ref and must not replace that ref's direct object ID.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawAdvertisedRef<'a> {
    pub(crate) name: &'a [u8],
    pub(crate) kind: AdvertisedRefKind,
    pub(crate) object_id: Option<&'a gix::ObjectId>,
}

/// Receives each ref from a streaming advertisement parser.
pub(crate) trait RefAdvertisementVisitor {
    /// Consume one borrowed ref record.
    fn visit(&mut self, reference: RawAdvertisedRef<'_>) -> Result<(), GitRemoteError>;
}

/// Internal seam for a bounded, streaming Git ref-advertisement parser.
///
/// Implementations must:
///
/// - parse directly from the transport's framed response, without first
///   collecting the advertisement in a `Vec`;
/// - process no more than one bounded protocol frame/ref record at a time;
/// - report every wire ref exactly once with its raw name bytes (including
///   protocol-v1 peeled pseudo-refs), and call the visitor before advancing to
///   the next ref;
/// - enforce `limits` before any parser-owned ref/name collection can grow;
/// - stop immediately when the visitor returns an error.
///
/// The transport adapter may request a `ref-prefix` in protocol v2 as an
/// optimization, but it must still enforce these limits and filter locally:
/// a server is allowed to ignore the prefix request. The existing gix
/// `Connection::ref_map()` is not a valid implementation of this trait because
/// it returns only after building an unbounded ref vector.
pub(crate) trait BoundedRefParser {
    /// Stream the advertisement into `visitor`.
    fn visit_refs(
        &mut self,
        limits: RefAdvertisementLimits,
        visitor: &mut dyn RefAdvertisementVisitor,
    ) -> Result<(), GitRemoteError>;
}

/// An error while parsing or collecting one remote ref advertisement.
#[derive(Debug)]
pub(crate) enum GitRemoteError {
    /// The package directory cannot be used to form a release-tag prefix.
    InvalidPackageDirectory,
    /// The advertisement exceeded one of its fixed limits.
    LimitExceeded {
        /// Which limit was exceeded.
        limit: GitRemoteLimit,
        /// The value observed when it was exceeded.
        actual: u64,
    },
    /// A bounded allocation for retained release-tag data failed.
    AllocationFailed(&'static str),
    /// A transport or bounded parser error.
    Parser(Box<dyn Error + Send + Sync>),
}

impl GitRemoteError {
    /// Wrap an error returned by the transport or the bounded parser.
    pub(crate) fn parser(source: impl Error + Send + Sync + 'static) -> Self {
        Self::Parser(Box::new(source))
    }
}

impl fmt::Display for GitRemoteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackageDirectory => {
                formatter.write_str("Git package directory is not normalized within the repository")
            }
            Self::LimitExceeded { limit, actual } => {
                write!(
                    formatter,
                    "{} limit exceeded (reached {actual})",
                    limit.name()
                )
            }
            Self::AllocationFailed(resource) => {
                write!(formatter, "could not allocate bounded {resource}")
            }
            Self::Parser(source) => write!(formatter, "Git ref advertisement failed: {source}"),
        }
    }
}

impl Error for GitRemoteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parser(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// A fixed resource limit for one remote ref advertisement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitRemoteLimit {
    /// Maximum number of wire refs.
    RefCount,
    /// Maximum cumulative byte length of wire ref names.
    RefNameBytes,
}

impl GitRemoteLimit {
    fn name(self) -> &'static str {
        match self {
            Self::RefCount => "advertised Git ref count",
            Self::RefNameBytes => "advertised Git ref-name bytes",
        }
    }
}

/// Collect release-tag refs from a bounded advertisement parser.
///
/// The parser is responsible for streaming and enforcing the same supplied
/// limits before its own storage can grow. This function independently counts
/// all advertised refs and their raw name bytes, including unrelated refs,
/// before copying any selected name or growing the result vector.
pub(crate) fn list_release_tag_refs(
    parser: &mut dyn BoundedRefParser,
    directory: Option<&DependencyPackagePath>,
) -> Result<Vec<AdvertisedRef>, GitRemoteError> {
    let prefix = release_tag_prefix(directory)?;
    let mut collector = RefCollector::new(prefix.as_bytes(), RefAdvertisementLimits::PACKAGE);
    parser.visit_refs(RefAdvertisementLimits::PACKAGE, &mut collector)?;
    Ok(collector.refs)
}

fn release_tag_prefix(
    directory: Option<&DependencyPackagePath>,
) -> Result<Box<str>, GitRemoteError> {
    match directory.map(DependencyPackagePath::as_str) {
        None | Some(".") => Ok("refs/tags/v".into()),
        Some(directory)
            if directory.starts_with('/')
                || directory.contains('\\')
                || directory
                    .split('/')
                    .any(|component| matches!(component, "" | "." | "..")) =>
        {
            Err(GitRemoteError::InvalidPackageDirectory)
        }
        Some(directory) => Ok(format!("refs/tags/{directory}/v").into_boxed_str()),
    }
}

struct RefCollector<'a> {
    prefix: &'a [u8],
    limits: RefAdvertisementLimits,
    ref_count: u64,
    ref_name_bytes: u64,
    refs: Vec<AdvertisedRef>,
}

impl<'a> RefCollector<'a> {
    fn new(prefix: &'a [u8], limits: RefAdvertisementLimits) -> Self {
        Self {
            prefix,
            limits,
            ref_count: 0,
            ref_name_bytes: 0,
            refs: Vec::new(),
        }
    }

    fn account_ref(&mut self, name: &[u8]) -> Result<(), GitRemoteError> {
        let ref_count = self.ref_count.checked_add(1).unwrap_or(u64::MAX);
        if ref_count > self.limits.max_refs {
            return Err(GitRemoteError::LimitExceeded {
                limit: GitRemoteLimit::RefCount,
                actual: ref_count,
            });
        }

        let name_bytes = u64::try_from(name.len()).unwrap_or(u64::MAX);
        let ref_name_bytes = self
            .ref_name_bytes
            .checked_add(name_bytes)
            .unwrap_or(u64::MAX);
        if ref_name_bytes > self.limits.max_ref_name_bytes {
            return Err(GitRemoteError::LimitExceeded {
                limit: GitRemoteLimit::RefNameBytes,
                actual: ref_name_bytes,
            });
        }

        self.ref_count = ref_count;
        self.ref_name_bytes = ref_name_bytes;
        Ok(())
    }
}

impl RefAdvertisementVisitor for RefCollector<'_> {
    fn visit(&mut self, reference: RawAdvertisedRef<'_>) -> Result<(), GitRemoteError> {
        // Account for every wire ref before filtering. The limit checks happen
        // before either selected-name copying or result-Vec growth.
        self.account_ref(reference.name)?;

        if reference.kind != AdvertisedRefKind::Direct || !reference.name.starts_with(self.prefix) {
            return Ok(());
        }
        let Some(object_id) = reference.object_id else {
            return Ok(());
        };

        self.refs
            .try_reserve_exact(1)
            .map_err(|_| GitRemoteError::AllocationFailed("release-tag ref list"))?;
        let mut name = Vec::new();
        name.try_reserve_exact(reference.name.len())
            .map_err(|_| GitRemoteError::AllocationFailed("release-tag ref name"))?;
        name.extend_from_slice(reference.name);
        self.refs.push(AdvertisedRef {
            name: name.into_boxed_slice(),
            object_id: object_id.clone(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdvertisedRefKind, BoundedRefParser, GitRemoteError, GitRemoteLimit, RawAdvertisedRef,
        RefAdvertisementLimits, RefAdvertisementVisitor, RefCollector, list_release_tag_refs,
    };

    struct TestRef {
        name: Vec<u8>,
        kind: AdvertisedRefKind,
        object_id: Option<gix::ObjectId>,
    }

    struct TestParser {
        refs: Vec<TestRef>,
    }

    impl BoundedRefParser for TestParser {
        fn visit_refs(
            &mut self,
            _limits: RefAdvertisementLimits,
            visitor: &mut dyn RefAdvertisementVisitor,
        ) -> Result<(), GitRemoteError> {
            for reference in &self.refs {
                visitor.visit(RawAdvertisedRef {
                    name: &reference.name,
                    kind: reference.kind,
                    object_id: reference.object_id.as_ref(),
                })?;
            }
            Ok(())
        }
    }

    fn object_id(last_nibble: u8) -> gix::ObjectId {
        let mut hex = vec![b'0'; 40];
        hex[39] = b"0123456789abcdef"[usize::from(last_nibble)];
        gix::ObjectId::from_hex(&hex).expect("valid SHA-1 object ID")
    }

    fn direct(name: &[u8], id: u8) -> TestRef {
        TestRef {
            name: name.to_vec(),
            kind: AdvertisedRefKind::Direct,
            object_id: Some(object_id(id)),
        }
    }

    fn limits(max_refs: u64, max_ref_name_bytes: u64) -> RefAdvertisementLimits {
        RefAdvertisementLimits {
            max_refs,
            max_ref_name_bytes,
        }
    }

    #[test]
    fn filters_locally_and_returns_the_direct_advertised_id() {
        let mut parser = TestParser {
            refs: vec![
                direct(b"refs/heads/main", 1),
                direct(b"refs/tags/v1.2.3", 2),
                direct(b"refs/tags/v01.2.3", 3),
                direct(b"refs/tags/sub/v1.2.3", 4),
            ],
        };
        let tags = list_release_tag_refs(&mut parser, None).unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].name(), b"refs/tags/v1.2.3");
        assert_eq!(tags[0].object_id(), &object_id(2));
        assert_eq!(tags[1].name(), b"refs/tags/v01.2.3");
    }

    #[test]
    fn ignores_peeled_pseudo_refs_instead_of_returning_the_peeled_id() {
        let mut parser = TestParser {
            refs: vec![
                direct(b"refs/tags/v1.2.3", 1),
                TestRef {
                    name: b"refs/tags/v1.2.3^{}".to_vec(),
                    kind: AdvertisedRefKind::Peeled,
                    object_id: Some(object_id(2)),
                },
            ],
        };
        let tags = list_release_tag_refs(&mut parser, None).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].object_id(), &object_id(1));
    }

    #[test]
    fn enforces_ref_count_before_growing_the_result_vec() {
        let mut collector = RefCollector::new(b"refs/tags/v", limits(1, 100));
        collector
            .visit(RawAdvertisedRef {
                name: b"refs/tags/v1.0.0",
                kind: AdvertisedRefKind::Direct,
                object_id: Some(&object_id(1)),
            })
            .unwrap();
        let old_capacity = collector.refs.capacity();

        let error = collector
            .visit(RawAdvertisedRef {
                name: b"refs/tags/v2.0.0",
                kind: AdvertisedRefKind::Direct,
                object_id: Some(&object_id(2)),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            GitRemoteError::LimitExceeded {
                limit: GitRemoteLimit::RefCount,
                actual: 2,
            }
        ));
        assert_eq!(collector.refs.len(), 1);
        assert_eq!(collector.refs.capacity(), old_capacity);
    }

    #[test]
    fn enforces_cumulative_name_bytes_before_copying_a_matching_name() {
        let mut collector = RefCollector::new(b"refs/tags/v", limits(10, 20));
        collector
            .visit(RawAdvertisedRef {
                name: b"refs/heads/main",
                kind: AdvertisedRefKind::Direct,
                object_id: Some(&object_id(1)),
            })
            .unwrap();
        assert!(collector.refs.is_empty());

        let error = collector
            .visit(RawAdvertisedRef {
                name: b"refs/tags/v1.0.0",
                kind: AdvertisedRefKind::Direct,
                object_id: Some(&object_id(2)),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            GitRemoteError::LimitExceeded {
                limit: GitRemoteLimit::RefNameBytes,
                ..
            }
        ));
        assert!(collector.refs.is_empty());
    }

    #[test]
    fn scopes_the_release_prefix_to_the_package_directory() {
        let package = crate::package_path::DependencyPackagePath::parse("sub/dep")
            .expect("normalized package path");
        let mut parser = TestParser {
            refs: vec![
                direct(b"refs/tags/v1.2.3", 1),
                direct(b"refs/tags/sub/dep/v1.2.3", 2),
            ],
        };
        let tags = list_release_tag_refs(&mut parser, Some(&package)).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name(), b"refs/tags/sub/dep/v1.2.3");
    }
}
