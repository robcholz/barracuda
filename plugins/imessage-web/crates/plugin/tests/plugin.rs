//! IMessage Web Plugin capability and route registration.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
use barracuda_imessage_web_plugin::{
    IMessageWebPlugin, IMessageWebRoute, WEB_CHANNEL, WEB_CONVERSATION,
};
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, never_embassy_stack};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpRequest, HttpResponse, WebServer, WebServerError,
};
use futures_lite::future::block_on;

struct WebServerDependency {
    server: Rc<WebServer>,
}

impl PluginDeclaration for WebServerDependency {
    const ID: &'static str = "webserver";
}

impl<const M: usize> Plugin<M> for WebServerDependency {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.provide(Rc::clone(&self.server))
    }
}

struct RouteConsumer {
    observed: Rc<RefCell<Option<(String, String)>>>,
}

impl PluginDeclaration for RouteConsumer {
    const ID: &'static str = "route-consumer";
    const DEPENDS_ON: &'static [&'static str] = &["imessage-web"];
}

impl<const M: usize> Plugin<M> for RouteConsumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let route = context.require::<IMessageWebRoute>("imessage-web")?.route();
        *self.observed.borrow_mut() = Some((route.channel, route.conversation_id));
        Ok(())
    }
}

struct UnusedEndpoint;

impl HttpEndpoint for UnusedEndpoint {
    fn handle<'a>(&'a self, _request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async { HttpResponse::new(204, "application/json", Vec::new()) })
    }
}

#[test]
fn plugin_registers_web_channel_root_socket_and_typed_route_capability() {
    block_on(async {
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let partition = memory_partition(64 * 1024)
            .await
            .expect("create Plugin storage");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        let mut router = EventRouter::new(lanes).await.expect("create router");
        let stack = never_embassy_stack();
        let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
        let server = Rc::new(WebServer::new());
        let observed = Rc::new(RefCell::new(None));

        manager
            .register(&mut router, IMessageGatewayPlugin::new(&mut context))
            .expect("register Gateway dependency");
        manager
            .register(
                &mut router,
                WebServerDependency {
                    server: Rc::clone(&server),
                },
            )
            .expect("register WebServer dependency");
        manager
            .register(&mut router, IMessageWebPlugin::new(&mut context))
            .expect("register Web Plugin");
        manager
            .register(
                &mut router,
                RouteConsumer {
                    observed: Rc::clone(&observed),
                },
            )
            .expect("consume Web route capability");
        manager.start(&mut router).expect("start Plugins");

        assert_eq!(
            observed.borrow().as_ref(),
            Some(&(String::from(WEB_CHANNEL), String::from(WEB_CONVERSATION)))
        );
        assert!(matches!(
            server.serve_http("/", UnusedEndpoint),
            Err(WebServerError::DuplicatePath)
        ));
    });
}
