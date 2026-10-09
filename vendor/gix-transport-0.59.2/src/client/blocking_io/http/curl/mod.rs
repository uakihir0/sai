use std::{
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, SyncSender},
    },
    thread,
};

use gix_features::io;
use parking_lot::Mutex;

use crate::client::blocking_io::FetchTransferLimit;
use crate::client::blocking_io::http::{self, traits::PostBodyDataKind};

mod remote;

/// A curl response body that reports when the fetch byte budget is exceeded.
pub struct CurlResponseBody {
    inner: io::pipe::Reader,
    transfer_limit: Option<FetchTransferLimit>,
    transfer_limit_exceeded: Arc<AtomicBool>,
}

impl CurlResponseBody {
    fn transfer_limit_error(&self) -> Option<std::io::Error> {
        if self
            .transfer_limit_exceeded
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.transfer_limit
                .as_ref()
                .map(FetchTransferLimit::exceeded_error)
        } else {
            None
        }
    }

    fn map_transport_error(&self, error: std::io::Error) -> std::io::Error {
        self.transfer_limit_error().unwrap_or(error)
    }
}

impl std::io::Read for CurlResponseBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        match self.inner.read(buffer) {
            Err(error) => Err(self.map_transport_error(error)),
            Ok(0) => match self.transfer_limit_error() {
                Some(error) => Err(error),
                None => Ok(0),
            },
            Ok(read) => Ok(read),
        }
    }
}

impl std::io::BufRead for CurlResponseBody {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        let transfer_limit_error = self.transfer_limit_error();
        let buffer = match self.inner.fill_buf() {
            Err(error) => return Err(transfer_limit_error.unwrap_or(error)),
            Ok(buffer) => buffer,
        };
        if buffer.is_empty()
            && let Some(error) = transfer_limit_error
        {
            return Err(error);
        }
        Ok(buffer)
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

#[cfg(test)]
mod transfer_limit_tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use crate::client::blocking_io::FetchTransferLimit;

    use super::CurlResponseBody;

    #[test]
    fn curl_transport_errors_are_classified_as_limit_errors_after_overflow() {
        let (_, reader) = gix_features::io::pipe::unidirectional(1);
        let body = CurlResponseBody {
            inner: reader,
            transfer_limit: Some(FetchTransferLimit::new(4)),
            transfer_limit_exceeded: Arc::new(AtomicBool::new(true)),
        };

        let error = body.map_transport_error(std::io::Error::other("curl callback failed"));

        assert_eq!(
            error
                .get_ref()
                .and_then(|source| source
                    .downcast_ref::<crate::client::blocking_io::FetchTransferLimitExceeded>())
                .map(|source| source.limit_bytes),
            Some(4)
        );
    }
}

/// Options to configure the `curl` HTTP handler.
#[derive(Default)]
pub struct Options {
    /// If `true` and runtime configuration is possible for `curl` backends, certificates revocation will be checked.
    ///
    /// This only works on windows apparently. Ignored if `None`.
    pub schannel_check_revoke: Option<bool>,
}

/// The error returned by the 'remote' helper, a purely internal construct to perform http requests.
///
/// It can be used for downcasting errors, which are boxed to hide the actual implementation.
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    Curl(#[from] curl::Error),
    #[error(transparent)]
    Redirect(#[from] http::redirect::Error),
    #[error("Could not finish reading all data to post to the remote")]
    ReadPostBody(#[from] std::io::Error),
    #[error(transparent)]
    Authenticate(#[from] gix_credentials::protocol::Error),
}

impl crate::IsSpuriousError for Error {
    fn is_spurious(&self) -> bool {
        match self {
            Error::Curl(err) => curl_is_spurious(err),
            _ => false,
        }
    }
}

pub(crate) fn curl_is_spurious(err: &curl::Error) -> bool {
    err.is_couldnt_connect()
        || err.is_couldnt_resolve_proxy()
        || err.is_couldnt_resolve_host()
        || err.is_operation_timedout()
        || err.is_recv_error()
        || err.is_send_error()
        || err.is_http2_error()
        || err.is_http2_stream_error()
        || err.is_ssl_connect_error()
        || err.is_partial_file()
}

/// A utility to abstract interactions with curl handles.
pub struct Curl {
    req: SyncSender<remote::Request>,
    res: Receiver<remote::Response>,
    handle: Option<thread::JoinHandle<Result<(), Error>>>,
    config: http::Options,
    redirected_base_url: Arc<Mutex<Option<String>>>,
}

impl Curl {
    fn restore_thread_after_failure(&mut self) -> http::Error {
        let err_that_brought_thread_down = self
            .handle
            .take()
            .expect("thread handle present")
            .join()
            .expect("handler thread should never panic")
            .expect_err("something should have gone wrong with curl (we join on error only)");
        let (handle, req, res, redirected_base_url) = remote::new();
        self.handle = Some(handle);
        self.req = req;
        self.res = res;
        self.redirected_base_url = redirected_base_url;
        err_that_brought_thread_down.into()
    }

    fn make_request(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        upload_body_kind: Option<PostBodyDataKind>,
    ) -> Result<http::PostResponse<io::pipe::Reader, CurlResponseBody, io::pipe::Writer>, http::Error>
    {
        let mut list = curl::easy::List::new();
        for header in headers {
            list.append(header.as_ref())?;
        }
        if self
            .req
            .send(remote::Request {
                url: url.to_owned(),
                base_url: base_url.to_owned(),
                headers: list,
                upload_body_kind,
                config: self.config.clone(),
            })
            .is_err()
        {
            return Err(self.restore_thread_after_failure());
        }
        let remote::Response {
            headers,
            body,
            upload_body,
            transfer_limit_exceeded,
        } = match self.res.recv() {
            Ok(res) => res,
            Err(_) => return Err(self.restore_thread_after_failure()),
        };
        Ok(http::PostResponse {
            post_body: upload_body,
            headers,
            body: CurlResponseBody {
                inner: body,
                transfer_limit: self.config.fetch_transfer_limit.clone(),
                transfer_limit_exceeded,
            },
        })
    }
}

impl Default for Curl {
    fn default() -> Self {
        let (handle, req, res, redirected_base_url) = remote::new();
        Curl {
            handle: Some(handle),
            req,
            res,
            config: http::Options::default(),
            redirected_base_url,
        }
    }
}

impl http::Http for Curl {
    type Headers = io::pipe::Reader;
    type ResponseBody = CurlResponseBody;
    type PostBody = io::pipe::Writer;

    fn get(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<http::GetResponse<Self::Headers, Self::ResponseBody>, http::Error> {
        self.make_request(url, base_url, headers, None)
            .map(Into::into)
    }

    fn post(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        body: PostBodyDataKind,
    ) -> Result<http::PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>, http::Error>
    {
        self.make_request(url, base_url, headers, Some(body))
    }

    fn configure(
        &mut self,
        config: &dyn std::any::Any,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        if let Some(config) = config.downcast_ref::<http::Options>() {
            self.config = config.clone();
        }
        Ok(())
    }

    fn redirected_base_url(&self) -> Option<String> {
        self.redirected_base_url.lock().clone()
    }
}
