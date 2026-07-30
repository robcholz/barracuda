//! Blocking HTTP transport seam.
//!
//! This namespace holds the synchronous compatibility surface. Agent runtime
//! code should prefer the async [`crate::http::ClawHttp`].

use core::sync::atomic::AtomicBool;

use super::{HttpError, HttpJsonRequest, HttpResponse};

#[cfg(feature = "realhttp")]
pub use super::real::blocking::RealHttp;

/// Networking injection point for blocking LLM clients.
///
/// Thread-safety is **not** a property of this trait: it carries no
/// `Send`/`Sync` supertrait bound. An implementation may hold a single,
/// non-thread-safe connection handle and mutate it through `&mut self`.
/// Concurrency is the *caller's* decision.
pub trait ClawHttp {
    /// Equivalent to `claw_llm_http_post_json`. Returns the body on HTTP 2xx,
    /// otherwise an [`HttpError`]. `abort` is polled cooperatively; when set the
    /// request is cancelled (mirrors the C `volatile bool *abort_flag`).
    fn post_json(
        &mut self,
        request: &HttpJsonRequest,
        abort: &AtomicBool,
    ) -> Result<HttpResponse, HttpError>;
}
