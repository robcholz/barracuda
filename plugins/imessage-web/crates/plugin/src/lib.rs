//! IMessage Web provider Plugin.

#![no_std]

extern crate alloc;

mod api;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry, WebGroup, WebText};
use barracuda_imessage_gateway_plugin::{
    AttachmentMeta, BinaryStream, ChannelError, GatewayControlKind, GatewayInboundControl,
    GatewayInboundMessage, GatewayRoute, IMessageGateway, MediaKind, StoreError,
};
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::{PluginContext, SharedEntropy};
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_webserver_plugin::WebServer;
use web::{
    InboundControl, InboundError, InboundFuture, InboundMessage, InboundMessageSink, MediaSpool,
    SpoolFuture, Web, WebBridge, WebControl,
};

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
pub struct IMessageWebPlugin {
    entropy: SharedEntropy,
}

impl IMessageWebPlugin {
    /// Creates the IMessage Web Plugin, minting media ids from the Platform's
    /// entropy.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            entropy: context.entropy.clone(),
        }
    }
}

impl Plugin for IMessageWebPlugin {
    const REQUIREMENTS: barracuda_plugin::manager::PluginRequirements =
        barracuda_plugin::manager::PluginRequirements::new()
            .with_filesystem(barracuda_plugin::manager::PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        context.retain(
            portal
                .register(
                    WebEntry {
                        id: "imessage-web",
                        group: WebGroup::Channel,
                        order: 10,
                        title: WebText {
                            zh: "Web 聊天",
                            en: "Web chat",
                        },
                        summary: WebText {
                            zh: "在浏览器里直接和设备对话",
                            en: "Talk to the device right in the browser",
                        },
                        icon: Some("icon.svg"),
                        figure: Some("figure.js"),
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                )
                .map_err(PluginError::registration)?,
        );
        let gateway = context.require::<IMessageGateway>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[1],
        )?;
        let spool: Rc<dyn MediaSpool> = Rc::new(GatewaySpool(Rc::clone(&gateway)));
        let web = Rc::new(WebChannel::new(spool, self.entropy.clone()));
        let channel: Rc<dyn MessageChannel> = web.clone();
        let channel_registration: MessageChannelRegistration = gateway
            .register(channel)
            .map_err(PluginError::registration)?;
        let route = GatewayRoute::new(WEB_CHANNEL, WEB_CONVERSATION);
        let sink: Rc<dyn InboundMessageSink> = Rc::new(GatewayInboundSink {
            gateway: Rc::clone(&gateway),
            channel: route.channel.clone(),
        });
        let bridge = WebBridge::new(Rc::clone(&web), sink, route.conversation_id.clone());
        let web_registration = webserver
            .serve_websocket("/ws/message", bridge)
            .map_err(PluginError::registration)?;
        let api_registrations =
            api::register(&webserver, web, gateway).map_err(PluginError::registration)?;
        log::info!(
            "registered IMessage Web channel `{WEB_CHANNEL}` for conversation `{WEB_CONVERSATION}` at WebSocket route `/ws/message`"
        );

        context.retain(channel_registration);
        context.retain(web_registration);
        context.retain(api_registrations);
        context.provide(Rc::new(IMessageWebRoute::new(route)))?;
        Ok(())
    }
}

/// Writes media sent as bytes or a stream to the Gateway's attachment
/// storage, so the channel keeps only a file reference.
struct GatewaySpool(Rc<IMessageGateway>);

impl MediaSpool for GatewaySpool {
    fn spool(
        &self,
        kind: MediaKind,
        mime_type: String,
        filename: Option<String>,
        body: BinaryStream,
    ) -> SpoolFuture<'_> {
        Box::pin(async move {
            let meta = AttachmentMeta {
                kind,
                mime_type: Some(mime_type),
                filename,
            };
            let stored = self
                .0
                .store_inbound(WEB_CHANNEL, meta, body)
                .await
                .map_err(store_error)?;
            self.0.open_source(&stored.path).await.map_err(store_error)
        })
    }
}

fn store_error(error: StoreError) -> ChannelError {
    match error {
        StoreError::TooLarge { .. } => ChannelError::InvalidRequest {
            message: error.to_string(),
        },
        StoreError::Stream(error) => ChannelError::Stream(error),
        error => ChannelError::Transport {
            message: error.to_string(),
        },
    }
}

struct GatewayInboundSink {
    gateway: Rc<IMessageGateway>,
    channel: String,
}

impl InboundMessageSink for GatewayInboundSink {
    fn receive_control(&self, request: InboundControl) -> InboundFuture<'_, ()> {
        Box::pin(async move {
            let control = GatewayInboundControl {
                temporary: request.temporary,
                session: request.session,
                index: None,
                title: request.title,
                confirm: request.confirm,
                ..GatewayInboundControl::new(
                    GatewayRoute {
                        channel: self.channel.clone(),
                        conversation_id: request.conversation_id,
                        thread_id: request.thread_id,
                    },
                    match request.control {
                        WebControl::Interrupt => GatewayControlKind::Interrupt,
                        WebControl::Cancel => GatewayControlKind::Cancel,
                        WebControl::Sessions => GatewayControlKind::Sessions,
                        WebControl::New => GatewayControlKind::New,
                        WebControl::Switch => GatewayControlKind::Switch,
                        WebControl::Rename => GatewayControlKind::Rename,
                        WebControl::Delete => GatewayControlKind::Delete,
                    },
                )
            };
            self.gateway
                .publish_control(control)
                .map_err(|error| InboundError::Rejected {
                    message: error.to_string(),
                })
        })
    }

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
                attachments: request.attachments,
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
