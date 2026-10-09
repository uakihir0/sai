///
pub mod connect;

/// Options that bound waiting for output from a blocking subprocess transport.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct SubprocessReadTimeout(std::time::Duration);

impl SubprocessReadTimeout {
    /// Create a read deadline applied by subprocess transports.
    pub const fn new(timeout: std::time::Duration) -> Self {
        Self(timeout)
    }

    /// Return the maximum duration allowed for subprocess output.
    pub const fn duration(self) -> std::time::Duration {
        self.0
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
