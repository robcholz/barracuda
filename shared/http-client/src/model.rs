use alloc::{boxed::Box, string::String, vec::Vec};
use core::{future::Future, pin::Pin};

use futures_core::Stream;

use crate::Error;

pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, BodyError>> + 'static>>;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BodyError {
    #[error("request body failed: {message}")]
    Failed { message: String },
}

impl BodyError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }
}

#[derive(Default)]
pub enum Body {
    #[default]
    Empty,
    Bytes(Vec<u8>),
    Stream {
        stream: BodyStream,
        content_length: Option<usize>,
    },
}

impl Body {
    pub fn stream(stream: BodyStream) -> Self {
        Self::Stream {
            stream,
            content_length: None,
        }
    }

    pub fn stream_with_length(stream: BodyStream, content_length: usize) -> Self {
        Self::Stream {
            stream,
            content_length: Some(content_length),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<Header>,
    pub body: Body,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::Post, url)
    }

    pub fn put(url: impl Into<String>) -> Self {
        Self::new(Method::Put, url)
    }

    pub fn delete(url: impl Into<String>) -> Self {
        Self::new(Method::Delete, url)
    }

    pub fn patch(url: impl Into<String>) -> Self {
        Self::new(Method::Patch, url)
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(Header::new(name, value));
        self
    }

    pub fn content_type(self, value: impl Into<String>) -> Self {
        self.header("Content-Type", value)
    }

    pub fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    pub fn bytes(self, body: impl Into<Vec<u8>>) -> Self {
        self.body(Body::Bytes(body.into()))
    }

    pub fn json(self, body: impl Into<String>) -> Self {
        self.content_type("application/json")
            .bytes(body.into().into_bytes())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponsePart {
    Head(u16),
    Data(Vec<u8>),
}

pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<Response, Error>> + 'a>>;
pub type ResponseStream<'a> = Pin<Box<dyn Stream<Item = Result<ResponsePart, Error>> + 'a>>;

pub trait HttpClient {
    fn execute(&self, request: Request) -> HttpFuture<'_>;

    /// Executes a request as a response-part stream.
    ///
    /// The default implementation adapts [`Self::execute`] and therefore
    /// buffers the complete body. Streaming transports override this method to
    /// yield body chunks as they arrive.
    fn execute_stream(&self, request: Request) -> ResponseStream<'_> {
        barracuda_runtime_utils::yield_stream::try_yield_stream(|yielder| async move {
            let Response { status, body } = self.execute(request).await?;
            yielder.yield_one(ResponsePart::Head(status)).await;
            if !body.is_empty() {
                yielder.yield_one(ResponsePart::Data(body)).await;
            }
            Ok(())
        })
    }
}
