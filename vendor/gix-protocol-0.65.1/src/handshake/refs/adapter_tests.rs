use std::io;

use gix_packetline::{PacketLineRef, decode};

use super::parse::{AdvertisementLimit, AdvertisementLimitExceeded, Error};

const MAX_REFS: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
const NAME_SIZE: usize = 32 * 1024;
fn object_id() -> String {
    format!(
        "{:0width$}",
        1,
        width = gix_hash::Kind::default().len_in_hex()
    )
}

enum End {
    Eof,
    IoError,
    DecodeError,
    Malformed,
}

struct Lines {
    line: Vec<u8>,
    first: Option<Vec<u8>>,
    first_pending: bool,
    remaining: usize,
    end: End,
    reads: usize,
}

impl Lines {
    fn new(name_size: usize, count: usize, end: End) -> Self {
        let mut line = format!("{} ", object_id()).into_bytes();
        line.extend(std::iter::repeat_n(b'r', name_size));
        Self {
            line,
            first: None,
            first_pending: false,
            remaining: count,
            end,
            reads: 0,
        }
    }

    fn next_line(&mut self) -> Option<io::Result<Result<PacketLineRef<'_>, decode::Error>>> {
        self.reads += 1;
        if self.first_pending {
            self.first_pending = false;
            return Some(Ok(Ok(PacketLineRef::Data(self.first.as_deref().unwrap()))));
        }
        if self.remaining > 0 {
            self.remaining -= 1;
            return Some(Ok(Ok(PacketLineRef::Data(&self.line))));
        }
        match self.end {
            End::Eof => None,
            End::IoError => Some(Err(io::Error::new(io::ErrorKind::ConnectionReset, "test"))),
            End::DecodeError => Some(Ok(Err(decode::Error::DataIsEmpty))),
            End::Malformed => Some(Ok(Ok(PacketLineRef::Data(b"malformed")))),
        }
    }
}

impl io::Read for Lines {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        unreachable!()
    }
}

impl io::BufRead for Lines {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        unreachable!()
    }

    fn consume(&mut self, _amount: usize) {
        unreachable!()
    }
}

#[cfg(feature = "blocking-client")]
impl crate::transport::client::blocking_io::ReadlineBufRead for Lines {
    fn readline(&mut self) -> Option<io::Result<Result<PacketLineRef<'_>, decode::Error>>> {
        self.next_line()
    }

    fn readline_str(&mut self, _line: &mut String) -> io::Result<usize> {
        unreachable!()
    }
}

#[cfg(feature = "async-client")]
impl futures_io::AsyncRead for Lines {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
        _buffer: &mut [u8],
    ) -> std::task::Poll<io::Result<usize>> {
        unreachable!()
    }
}

#[cfg(feature = "async-client")]
impl futures_io::AsyncBufRead for Lines {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<&[u8]>> {
        unreachable!()
    }

    fn consume(self: std::pin::Pin<&mut Self>, _amount: usize) {
        unreachable!()
    }
}

#[cfg(feature = "async-client")]
#[async_trait::async_trait(?Send)]
impl crate::transport::client::async_io::ReadlineBufRead for Lines {
    async fn readline(&mut self) -> Option<io::Result<Result<PacketLineRef<'_>, decode::Error>>> {
        self.next_line()
    }

    async fn readline_str(&mut self, _line: &mut String) -> io::Result<usize> {
        unreachable!()
    }
}

fn assert_limit(error: Error, limit: AdvertisementLimit, actual: usize) {
    let Error::Io(error) = error else {
        panic!("expected limit I/O error, got {error:?}");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let detail = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<AdvertisementLimitExceeded>())
        .expect("typed advertisement limit error");
    assert_eq!(detail.limit, limit);
    assert_eq!(detail.actual, actual as u64);
}

#[test]
fn fixture_object_id_uses_the_enabled_dependency_hash() {
    let object_id = object_id();
    let parsed = gix_hash::ObjectId::from_hex(object_id.as_bytes()).expect("supported hash");
    assert_eq!(parsed.kind(), gix_hash::Kind::default());
}

fn check_limits_and_errors(parse: fn(&mut Lines) -> Result<usize, Error>) {
    let mut valid = Lines::new(1, 2, End::Eof);
    assert_eq!(parse(&mut valid).unwrap(), 2);

    let mut count = Lines::new(1, MAX_REFS + 1, End::Malformed);
    assert_limit(
        parse(&mut count).unwrap_err(),
        AdvertisementLimit::RefCount,
        MAX_REFS + 1,
    );
    assert_eq!(count.reads, MAX_REFS + 1);

    let mut bytes = Lines::new(NAME_SIZE, MAX_NAME_BYTES / NAME_SIZE + 1, End::Malformed);
    assert_limit(
        parse(&mut bytes).unwrap_err(),
        AdvertisementLimit::RefNameBytes,
        MAX_NAME_BYTES + NAME_SIZE,
    );
    assert_eq!(bytes.reads, MAX_NAME_BYTES / NAME_SIZE + 1);

    let mut io_error = Lines::new(1, 1, End::IoError);
    assert!(
        matches!(parse(&mut io_error), Err(Error::Io(error)) if error.kind() == io::ErrorKind::ConnectionReset)
    );
    let mut decode_error = Lines::new(1, 1, End::DecodeError);
    assert!(matches!(
        parse(&mut decode_error),
        Err(Error::DecodePacketline(decode::Error::DataIsEmpty))
    ));
    let mut malformed = Lines::new(1, 1, End::Malformed);
    assert!(matches!(
        parse(&mut malformed),
        Err(Error::MalformedV1RefLine(_) | Error::MalformedV2RefLine(_))
    ));
}

macro_rules! adapter_tests {
    ($module:ident, $v1:expr, $v2:expr, $v1_symref:expr) => {
        mod $module {
            use super::*;

            #[test]
            fn v1_and_v2_preserve_limits_and_propagate_errors() {
                for parse in [$v1 as fn(&mut Lines) -> Result<usize, Error>, $v2] {
                    check_limits_and_errors(parse);
                }
            }

            #[test]
            fn v1_capabilities_and_refs_share_one_budget() {
                let symref_bytes = "HEAD".len() + "refs/heads/main".len();
                let mut count = Lines::new(1, MAX_REFS, End::Malformed);
                count.first = Some(format!("{} HEAD", object_id()).into_bytes());
                count.first_pending = true;
                assert_limit(
                    $v1_symref(&mut count).unwrap_err(),
                    AdvertisementLimit::RefCount,
                    MAX_REFS + 1,
                );
                assert_eq!(count.reads, MAX_REFS + 1);

                let mut bytes = Lines::new(NAME_SIZE, MAX_NAME_BYTES / NAME_SIZE, End::Malformed);
                bytes.first = Some(format!("{} HEAD", object_id()).into_bytes());
                bytes.first_pending = true;
                assert_limit(
                    $v1_symref(&mut bytes).unwrap_err(),
                    AdvertisementLimit::RefNameBytes,
                    MAX_NAME_BYTES + symref_bytes,
                );
                assert_eq!(bytes.reads, MAX_NAME_BYTES / NAME_SIZE + 1);
            }
        }
    };
}

#[cfg(feature = "blocking-client")]
fn blocking_v1(lines: &mut Lines) -> Result<usize, Error> {
    super::blocking_io::from_v1_refs_received_as_part_of_handshake_and_capabilities(
        lines,
        std::iter::empty(),
    )
    .map(|(refs, _)| refs.len())
}

#[cfg(feature = "blocking-client")]
fn blocking_v2(lines: &mut Lines) -> Result<usize, Error> {
    super::blocking_io::from_v2_refs(lines).map(|refs| refs.len())
}

#[cfg(feature = "blocking-client")]
fn blocking_v1_symref(lines: &mut Lines) -> Result<usize, Error> {
    let caps = crate::transport::client::Capabilities::from_bytes(b"\0symref=HEAD:refs/heads/main")
        .unwrap()
        .0;
    super::blocking_io::from_v1_refs_received_as_part_of_handshake_and_capabilities(
        lines,
        caps.iter(),
    )
    .map(|(refs, _)| refs.len())
}

#[cfg(feature = "blocking-client")]
adapter_tests!(blocking, blocking_v1, blocking_v2, blocking_v1_symref);

#[cfg(all(feature = "async-client", not(feature = "blocking-client")))]
fn async_v1(lines: &mut Lines) -> Result<usize, Error> {
    futures_lite::future::block_on(
        super::async_io::from_v1_refs_received_as_part_of_handshake_and_capabilities(
            lines,
            std::iter::empty(),
        ),
    )
    .map(|(refs, _)| refs.len())
}

#[cfg(feature = "async-client")]
fn async_v2(lines: &mut Lines) -> Result<usize, Error> {
    futures_lite::future::block_on(super::async_io::from_v2_refs(lines)).map(|refs| refs.len())
}

#[cfg(all(feature = "async-client", not(feature = "blocking-client")))]
fn async_v1_symref(lines: &mut Lines) -> Result<usize, Error> {
    let caps = crate::transport::client::Capabilities::from_bytes(b"\0symref=HEAD:refs/heads/main")
        .unwrap()
        .0;
    futures_lite::future::block_on(
        super::async_io::from_v1_refs_received_as_part_of_handshake_and_capabilities(
            lines,
            caps.iter(),
        ),
    )
    .map(|(refs, _)| refs.len())
}

#[cfg(all(feature = "async-client", not(feature = "blocking-client")))]
adapter_tests!(asynchronous, async_v1, async_v2, async_v1_symref);

#[cfg(all(feature = "async-client", feature = "blocking-client"))]
#[test]
fn async_v2_is_exercised_with_both_clients_enabled() {
    check_limits_and_errors(async_v2);
}
