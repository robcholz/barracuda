//! WebServer capability sharing integration test.

#![allow(clippy::expect_used)]

use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::convert::Infallible;
use std::future::pending;
use std::rc::Rc;

use barracuda_event_router::{EventRouter, MemFs, RpcLaneStorage};
use barracuda_plugin_manager::{
    EkvStore, NoopRawMutex, Plugin, PluginContext, PluginManager, PluginStartFuture,
};
use barracuda_webserver_plugin::{
    WebServer, WebServerListenFuture, WebServerListener, WebServerPlugin, PLUGIN_ID,
    WEB_SERVER_PORT,
};
use ekv::{flash::MemFlash, Config};
use futures_lite::future::block_on;

const FRAME_SIZE: usize = 64;

struct Consumer {
    observed: Rc<RefCell<Option<Rc<WebServer>>>>,
}

#[derive(Clone)]
struct TestListener {
    listening: Rc<Cell<bool>>,
}

impl WebServerListener for TestListener {
    type Error = Infallible;

    fn listen<'a>(
        &'a mut self,
        _server: &'a WebServer,
        port: u16,
    ) -> WebServerListenFuture<'a, Self::Error> {
        Box::pin(async move {
            assert_eq!(port, WEB_SERVER_PORT);
            self.listening.set(true);
            pending().await
        })
    }
}

impl Plugin<FRAME_SIZE> for Consumer {
    const DEPENDS_ON: &'static [&'static str] = &[PLUGIN_ID];

    fn id(&self) -> &'static str {
        "consumer"
    }

    fn start<'a>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE>,
    ) -> PluginStartFuture<'a> {
        Box::pin(async move {
            *self.observed.borrow_mut() = Some(context.require::<WebServer>(PLUGIN_ID)?);
            Ok(())
        })
    }
}

#[test]
fn plugin_provides_webserver_to_dependent_plugins() {
    let store = EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), Config::default());
    block_on(store.format()).expect("format store");
    let mut manager = PluginManager::new(store);
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
    let filesystem = Box::leak(Box::new(MemFs::new()));
    let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
    let observed = Rc::new(RefCell::new(None));
    let listening = Rc::new(Cell::new(false));
    let plugin_id =
        barracuda_plugin_manager::PluginId::try_from(PLUGIN_ID).expect("valid WebServer Plugin ID");

    block_on(manager.register(
        &mut router,
        WebServerPlugin::new(TestListener {
            listening: Rc::clone(&listening),
        }),
    ))
    .expect("register WebServer Plugin");
    block_on(manager.register(
        &mut router,
        Consumer {
            observed: Rc::clone(&observed),
        },
    ))
    .expect("register consumer");
    block_on(manager.start(&mut router)).expect("start Plugins");

    assert!(observed.borrow().is_some());
    assert_eq!(manager.component_ids(&plugin_id).map(<[_]>::len), Some(1));
    assert!(!listening.get());

    assert!(block_on(futures_lite::future::poll_once(&mut router)).is_none());
    assert!(listening.get());
}
