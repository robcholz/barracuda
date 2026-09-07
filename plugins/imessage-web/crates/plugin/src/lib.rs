//! IMessage Web provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use barracuda_imessage_gateway_plugin::{GatewayInboundMessage, GatewayRoute, IMessageGateway};
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::WebServer;
use web::{InboundError, InboundFuture, InboundMessage, InboundMessageSink, Web, WebBridge};

pub use web::WebClientFrame;

/// Stable name of the Web message channel.
pub const WEB_CHANNEL: &str = "web";
/// Stable conversation mounted by the Web provider.
pub const WEB_CONVERSATION: &str = "conversation";

const WEB_HISTORY: usize = 64;
const WEB_SUBSCRIBERS: usize = 8;

type WebChannel = Web<WEB_HISTORY, WEB_SUBSCRIBERS>;

/// Route capability published for consumers of the built-in Web conversation.
pub struct IMessageWebRoute {
    route: GatewayRoute,
}

impl IMessageWebRoute {
    /// Creates a Web route capability.
    #[must_use]
    pub fn new(route: GatewayRoute) -> Self {
        Self { route }
    }

    /// Returns the Web provider's configured Gateway route.
    #[must_use]
    pub fn route(&self) -> GatewayRoute {
        self.route.clone()
    }
}

/// Plugin that registers the Web channel with the IMessage Gateway.
#[barracuda_plugin::macros::plugin]
pub struct IMessageWebPlugin;

impl IMessageWebPlugin {
    /// Creates the IMessage Web Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for IMessageWebPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let web = Rc::new(WebChannel::new());
        let channel: Rc<dyn MessageChannel> = web.clone();
        let channel_registration: MessageChannelRegistration = gateway
            .register(channel)
            .map_err(PluginError::registration)?;
        let route = GatewayRoute::new(WEB_CHANNEL, WEB_CONVERSATION);
        let sink: Rc<dyn InboundMessageSink> = Rc::new(GatewayInboundSink {
            gateway,
            channel: route.channel.clone(),
        });
        let bridge = WebBridge::new(web, sink, route.conversation_id.clone());
        let web_registration = webserver
            .serve("/", bridge)
            .map_err(PluginError::registration)?;
        log::info!(
            "registered IMessage Web channel `{WEB_CHANNEL}` for conversation `{WEB_CONVERSATION}` at WebSocket route `/`"
        );

        context.retain(channel_registration);
        context.retain(web_registration);
        context.provide(Rc::new(IMessageWebRoute::new(route)))?;
        Ok(())
    }
}

struct GatewayInboundSink {
    gateway: Rc<IMessageGateway>,
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
            self.gateway
                .publish(message)
                .await
                .map_err(|error| InboundError::Rejected {
                    message: error.to_string(),
                })
        })
    }
}
