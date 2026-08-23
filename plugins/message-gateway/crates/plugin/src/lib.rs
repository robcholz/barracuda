//! Plugin that owns the Message Gateway Component and its built-in providers.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use barracuda_message_gateway_component::component::GatewayComponent;
use barracuda_message_gateway_component::gateway_message_received::GatewayInboundMessage;
use barracuda_message_gateway_component::route::GatewayRoute;
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginStartFuture};
use barracuda_webserver_plugin::{WebServer, PLUGIN_ID as WEBSERVER_PLUGIN_ID};
use gateway::{MessageChannel, MessageGateway};
use web::{InboundError, InboundFuture, InboundMessage, InboundMessageSink, Web, WebBridge};

pub use barracuda_message_gateway_component::component::GatewayIngress;

/// Stable identity of the Message Gateway Plugin.
pub const PLUGIN_ID: &str = "message-gateway";

/// Stable name of the built-in Web message channel.
pub const WEB_CHANNEL: &str = "web";
/// Stable conversation bound to the built-in Web endpoint.
pub const WEB_CONVERSATION: &str = "conversation";

const WEB_HISTORY: usize = 64;
const WEB_SUBSCRIBERS: usize = 8;
const GATEWAY_INGRESS_CAPACITY: usize = 16;

type WebChannel = Web<WEB_HISTORY, WEB_SUBSCRIBERS>;

/// Plugin that owns the Message Gateway and its built-in providers.
#[derive(Default)]
pub struct MessageGatewayPlugin;

impl MessageGatewayPlugin {
    /// Creates the self-contained Message Gateway Plugin.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Returns the route shared by the Message Gateway and Gateway-Agent Plugins.
#[must_use]
pub fn default_route() -> GatewayRoute {
    GatewayRoute::new(WEB_CHANNEL, WEB_CONVERSATION)
}

impl<const M: usize> Plugin<M> for MessageGatewayPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[WEBSERVER_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
        Box::pin(async move {
            let webserver = context.require::<WebServer>(WEBSERVER_PLUGIN_ID)?;
            let mut gateway = MessageGateway::new();
            let web = Rc::new(WebChannel::new());
            let channel: Rc<dyn MessageChannel> = web.clone();
            gateway
                .register(channel)
                .map_err(PluginError::registration)?;
            let (component, ingress) = GatewayComponent::new(gateway, GATEWAY_INGRESS_CAPACITY);
            let route = default_route();
            let sink: Rc<dyn InboundMessageSink> = Rc::new(GatewayInboundSink {
                ingress,
                channel: route.channel,
            });
            let web = WebBridge::new(web, sink, route.conversation_id);
            context.load(component)?;
            let web_registration = webserver
                .serve("/", web)
                .map_err(PluginError::registration)?;
            context.retain(web_registration);
            Ok(())
        })
    }
}

struct GatewayInboundSink {
    ingress: GatewayIngress,
    channel: String,
}

impl InboundMessageSink for GatewayInboundSink {
    fn receive_message(&self, request: InboundMessage) -> InboundFuture<'_, ()> {
        Box::pin(async move {
            let message = GatewayInboundMessage {
                route: GatewayRoute {
                    channel: self.channel.clone(),
                    conversation_id: request.conversation_id,
                    thread_id: request.thread_id,
                },
                message_id: request.message_id,
                text: request.text,
            };
            self.ingress
                .publish(message)
                .await
                .map_err(|error| InboundError::Rejected {
                    message: error.to_string(),
                })
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::MessageGatewayPlugin;
    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, MemFs, RpcLaneStorage};
    use barracuda_plugin_manager::{EkvStore, NoopRawMutex, Plugin, PluginId, PluginManager};
    use barracuda_webserver_plugin::{
        WebServer, WebServerListenFuture, WebServerListener, WebServerPlugin,
    };
    use core::convert::Infallible;
    use core::future::pending;
    use ekv::{flash::MemFlash, Config};
    use futures_lite::future::block_on;

    #[derive(Clone, Copy)]
    struct NeverWebServerListener;

    impl WebServerListener for NeverWebServerListener {
        type Error = Infallible;

        fn listen<'a>(
            &'a mut self,
            _server: &'a WebServer,
            _port: u16,
        ) -> WebServerListenFuture<'a, Self::Error> {
            Box::pin(pending())
        }
    }

    #[test]
    fn plugin_loads_its_gateway_component() {
        let store = EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), Config::default());
        block_on(store.format()).expect("format store");
        let mut manager = PluginManager::new(store);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        let id = PluginId::try_from("message-gateway").expect("valid Plugin ID");
        let plugin = MessageGatewayPlugin::new();
        assert_eq!(Plugin::<512>::id(&plugin), "message-gateway");

        block_on(manager.register(&mut router, WebServerPlugin::new(NeverWebServerListener)))
            .expect("register WebServer Plugin");
        block_on(manager.register(&mut router, plugin)).expect("register Message Gateway Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));

        manager
            .unload(&mut router, &id)
            .expect("unload Message Gateway Plugin");
        block_on(manager.register(&mut router, MessageGatewayPlugin::new()))
            .expect("reload Message Gateway Plugin");
        block_on(manager.start(&mut router)).expect("restart Message Gateway Plugin");
    }
}
