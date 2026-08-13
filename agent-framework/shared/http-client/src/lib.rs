//! Shared executor-neutral HTTP client for `no_std` components.
#![no_std]

extern crate alloc;

mod model;
mod multipart;
mod reqwless_client;

pub use model::{
    Body, BodyError, BodyStream, Header, HttpClient, HttpFuture, Method, Request, Response,
};
pub use multipart::{Multipart, MultipartError};
pub use reqwless_client::{Error, ReqwlessClient};
