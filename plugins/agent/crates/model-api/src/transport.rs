//! Model API adapter over the workspace-wide `http-client` transport.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::pin::Pin;

use futures_core::Stream;
use futures_lite::StreamExt as _;
use http_client::{Client, ResponsePart as HttpResponsePart};

use crate::StatusCode;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: StatusCode,
    pub(crate) body: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResponsePart {
    Head(StatusCode),
    Data(Vec<u8>),
}

pub(crate) type ResponseStream<'a> = Pin<Box<dyn Stream<Item = Result<ResponsePart, Error>> + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("request was cancelled")]
    Cancelled,
    #[error("response body is not UTF-8")]
    InvalidUtf8,
    #[error("HTTP stream protocol error")]
    Codec,
    #[error(transparent)]
    Http(#[from] http_client::Error),
}

impl Error {
    /// Whether retrying the request may recover from this transport failure.
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Http(error) if error.retryable())
    }
}

/// Thin LLM-specific request adapter over the shared HTTP client.
pub(crate) struct HttpTransport<'net> {
    client: Client<'net>,
}

impl<'net> HttpTransport<'net> {
    #[must_use]
    pub(crate) const fn new(client: Client<'net>) -> Self {
        Self { client }
    }

    pub(crate) async fn post_json(
        &self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let request = headers.iter().fold(
            self.client.post(url).json(body),
            |request, (name, value)| request.header(*name, *value),
        );
        let response = request.send().await?;
        let body = String::from_utf8(response.body).map_err(|_error| Error::InvalidUtf8)?;
        Ok(Response {
            status: StatusCode(response.status),
            body,
        })
    }

    pub(crate) fn post_json_stream<'a>(
        &'a self,
        url: String,
        body: String,
        headers: Vec<(String, String)>,
    ) -> ResponseStream<'a> {
        let request = headers.iter().fold(
            self.client.post(url).json(body),
            |request, (name, value)| request.header(name, value),
        );
        let stream = request.send_stream().map(|part| {
            part.map(|part| match part {
                HttpResponsePart::Head(status) => ResponsePart::Head(StatusCode(status)),
                HttpResponsePart::Data(bytes) => ResponsePart::Data(bytes),
            })
            .map_err(Error::from)
        });
        Box::pin(stream)
    }
}
