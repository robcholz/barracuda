use alloc::boxed::Box;
use alloc::rc::Rc;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, RegisterContext, RunContext,
    UnregisterContext,
};

use crate::{WebServer, WebServerListener, WEB_SERVER_PORT};

pub(crate) struct WebServerComponent<Listener> {
    webserver: Rc<WebServer>,
    listener: Listener,
}

impl<Listener> WebServerComponent<Listener> {
    pub(crate) const fn new(webserver: Rc<WebServer>, listener: Listener) -> Self {
        Self {
            webserver,
            listener,
        }
    }
}

impl<const M: usize, Listener> Component<M> for WebServerComponent<Listener>
where
    Listener: WebServerListener,
{
    fn register(&mut self, _context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        Ok(())
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            self.listener
                .listen(&self.webserver, WEB_SERVER_PORT)
                .await
                .map_err(ComponentError::lifecycle)
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}
