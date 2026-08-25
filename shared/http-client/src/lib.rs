//! Shared executor-neutral HTTP client for `no_std` components.
#![no_std]

extern crate alloc;

mod client;
mod model;
mod multipart;
mod reqwless_client;

pub use client::{Client, ClientFactory, RequestBuilder};
pub use model::{
    Body, BodyError, BodyStream, Header, HttpClient, HttpFuture, Method, Request, Response,
    ResponsePart, ResponseStream,
};
pub use multipart::{Multipart, MultipartError};
#[cfg(any(feature = "embedded-tls", feature = "mbedtls"))]
pub use reqwless::client::TlsConfig;
pub use reqwless_client::Error;
