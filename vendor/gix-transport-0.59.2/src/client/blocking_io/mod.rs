use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

///
pub mod connect;

/// Options that bound waiting for output from a blocking subprocess transport.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct SubprocessReadTimeout {
    duration: std::time::Duration,
    transfer_limit: Option<FetchTransferLimit>,
}

impl SubprocessReadTimeout {
    /// Create a read deadline applied by subprocess transports.
    pub const fn new(timeout: std::time::Duration) -> Self {
        Self {
            duration: timeout,
            transfer_limit: None,
        }
    }

    /// Return the maximum duration allowed for subprocess output.
    pub const fn duration(&self) -> std::time::Duration {
        self.duration
    }

    /// Bound bytes delivered by the subprocess for one fetch operation.
    pub fn with_transfer_limit(mut self, transfer_limit: FetchTransferLimit) -> Self {
        self.transfer_limit = Some(transfer_limit);
        self
    }

    /// Return the byte budget, if configured.
    pub fn transfer_limit(&self) -> Option<&FetchTransferLimit> {
        self.transfer_limit.as_ref()
    }
}

/// A shared byte budget for response data read during one fetch operation.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct FetchTransferLimit {
    limit_bytes: u64,
    consumed_bytes: Arc<AtomicU64>,
}

impl FetchTransferLimit {
    /// Create a new transfer budget with the given maximum number of bytes.
    pub fn new(limit_bytes: u64) -> Self {
        Self {
            limit_bytes,
            consumed_bytes: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Return the configured byte limit.
    pub const fn limit_bytes(&self) -> u64 {
        self.limit_bytes
    }

    /// Return the number of response bytes already delivered to the caller.
    pub fn consumed_bytes(&self) -> u64 {
        self.consumed_bytes.load(Ordering::Relaxed)
    }

    /// Return the remaining number of bytes in the budget.
    pub fn remaining_bytes(&self) -> u64 {
        self.limit_bytes
            .saturating_sub(self.consumed_bytes.load(Ordering::Relaxed))
    }

    /// Account for bytes delivered to the caller, failing if the limit is exceeded.
    pub fn consume(&self, bytes: u64) -> std::io::Result<()> {
        let mut current = self.consumed_bytes.load(Ordering::Relaxed);
        loop {
            let Some(next) = current
                .checked_add(bytes)
                .filter(|total| *total <= self.limit_bytes)
            else {
                return Err(self.exceeded_error());
            };
            match self.consumed_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    /// Construct an I/O error that identifies this configured limit.
    pub fn exceeded_error(&self) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            FetchTransferLimitExceeded {
                limit_bytes: self.limit_bytes,
            },
        )
    }
}

/// The response data of one fetch exceeded its configured byte budget.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchTransferLimitExceeded {
    /// The configured byte limit.
    pub limit_bytes: u64,
}

impl std::fmt::Display for FetchTransferLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Git fetch exceeded its {}-byte transfer limit",
            self.limit_bytes
        )
    }
}

impl std::error::Error for FetchTransferLimitExceeded {}

/// A writer that accounts for bytes before forwarding an HTTP response body.
#[cfg(feature = "http-client-reqwest")]
pub(crate) struct LimitedResponseBodyWriter<'a, W> {
    inner: W,
    limit: &'a FetchTransferLimit,
}

#[cfg(feature = "http-client-reqwest")]
impl<'a, W> LimitedResponseBodyWriter<'a, W> {
    pub(crate) fn new(inner: W, limit: &'a FetchTransferLimit) -> Self {
        Self { inner, limit }
    }
}

#[cfg(feature = "http-client-reqwest")]
impl<W: std::io::Write> std::io::Write for LimitedResponseBodyWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self.limit.remaining_bytes();
        if remaining == 0 {
            return Err(self.limit.exceeded_error());
        }
        let permitted = buffer.len().min(remaining.min(usize::MAX as u64) as usize);
        let written = self.inner.write(&buffer[..permitted])?;
        self.limit.consume(written as u64)?;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Copy at most the remaining response body and read one byte beyond the
/// budget to distinguish an exactly-sized response from an oversized one.
#[cfg(feature = "http-client-reqwest")]
pub(crate) fn copy_response_body_with_limit<R, W>(
    reader: &mut R,
    writer: &mut W,
    limit: &FetchTransferLimit,
) -> std::io::Result<u64>
where
    R: std::io::Read,
    W: std::io::Write,
{
    let mut buffer = [0; 8 * 1024];
    let mut copied = 0_u64;
    loop {
        let remaining = limit.remaining_bytes();
        let read_capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..read_capacity])?;
        if read == 0 {
            return Ok(copied);
        }

        if read as u64 > remaining {
            if remaining > 0 {
                let allowed = remaining as usize;
                writer.write_all(&buffer[..allowed])?;
            }
            return Err(limit.exceeded_error());
        }

        writer.write_all(&buffer[..read])?;
        copied += read as u64;
    }
}

#[cfg(all(test, feature = "http-client-reqwest"))]
mod transfer_limit_tests {
    use std::io::{Cursor, Write};

    use super::{FetchTransferLimit, LimitedResponseBodyWriter, copy_response_body_with_limit};

    #[test]
    fn response_writer_stops_before_forwarding_bytes_over_the_budget() {
        let limit = FetchTransferLimit::new(4);
        let mut output = Vec::new();
        let mut writer = LimitedResponseBodyWriter::new(&mut output, &limit);

        let error = writer
            .write_all(b"abcde")
            .expect_err("the fifth byte exceeds the budget");

        assert_eq!(output, b"abcd");
        assert_eq!(limit.consumed_bytes(), 4);
        assert_eq!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<super::FetchTransferLimitExceeded>())
                .map(|source| source.limit_bytes),
            Some(4)
        );
    }

    #[test]
    fn cloned_budgets_account_for_multiple_response_streams() {
        let limit = FetchTransferLimit::new(5);
        let fallback = limit.clone();

        limit.consume(3).unwrap();
        fallback.consume(2).unwrap();

        assert_eq!(limit.consumed_bytes(), 5);
        assert_eq!(fallback.remaining_bytes(), 0);
        assert!(fallback.consume(1).is_err());
    }

    #[test]
    fn limited_copy_reads_only_one_byte_past_the_budget() {
        let limit = FetchTransferLimit::new(4);
        let mut input = Cursor::new(b"abcdef");
        let mut output = Vec::new();
        let mut writer = LimitedResponseBodyWriter::new(&mut output, &limit);

        let error = copy_response_body_with_limit(&mut input, &mut writer, &limit)
            .expect_err("the fifth byte exceeds the budget");

        assert_eq!(input.position(), 5);
        assert_eq!(output, b"abcd");
        assert_eq!(limit.consumed_bytes(), 4);
        assert_eq!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<super::FetchTransferLimitExceeded>())
                .map(|source| source.limit_bytes),
            Some(4)
        );
    }

    #[test]
    fn limited_copy_accepts_a_body_exactly_at_the_budget() {
        let limit = FetchTransferLimit::new(4);
        let mut input = Cursor::new(b"abcd");
        let mut output = Vec::new();
        let mut writer = LimitedResponseBodyWriter::new(&mut output, &limit);

        let copied = copy_response_body_with_limit(&mut input, &mut writer, &limit).unwrap();

        assert_eq!(copied, 4);
        assert_eq!(input.position(), 4);
        assert_eq!(output, b"abcd");
        assert_eq!(limit.consumed_bytes(), 4);
    }
}

///
pub mod file;
///
#[cfg(feature = "http-client")]
pub mod http;

mod bufread_ext;
pub use bufread_ext::{ExtendedBufRead, HandleProgress, ReadlineBufRead};

mod request;
pub use request::RequestWriter;

///
pub mod ssh;

mod traits;
pub use traits::{SetServiceResponse, Transport, TransportV2Ext};
