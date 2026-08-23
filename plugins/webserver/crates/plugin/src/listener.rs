use alloc::boxed::Box;
use core::error::Error;
use core::future::Future;
use core::pin::Pin;

use crate::WebServer;

/// Cooperative future that owns one platform WebServer listener loop.
pub type WebServerListenFuture<'a, E> = Pin<Box<dyn Future<Output = Result<(), E>> + 'a>>;

/// Platform capability that binds and drives the WebServer listener.
///
/// Implementations own socket acceptance and concurrency policy. The future
/// normally runs until the Component is unloaded.
pub trait WebServerListener {
    /// Fatal listener error returned to Event Router.
    type Error: Error + 'static;

    /// Binds `port` and serves connections with the shared portable server.
    fn listen<'a>(
        &'a mut self,
        server: &'a WebServer,
        port: u16,
    ) -> WebServerListenFuture<'a, Self::Error>;
}
