//! IMessage WeChat provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;

use barracuda_imessage_gateway_plugin::{IMessageGateway, PLUGIN_ID as IMESSAGE_GATEWAY_PLUGIN_ID};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginStartFuture};
use gateway::MessageChannel;
use http_client::HttpClient;
use wechat::{Wechat, WechatConfig};

/// Stable identity of the IMessage WeChat Plugin.
pub const PLUGIN_ID: &str = "imessage-wechat";

/// Plugin that registers a WeChat channel with the IMessage Gateway.
pub struct IMessageWechatPlugin {
    channel: Rc<Wechat>,
}

impl IMessageWechatPlugin {
    /// Creates a WeChat provider from its portable HTTP client and settings.
    #[must_use]
    pub fn new(http: Rc<dyn HttpClient>, config: WechatConfig) -> Self {
        Self {
            channel: Rc::new(Wechat::new(http, config)),
        }
    }
}

impl<const M: usize> Plugin<M> for IMessageWechatPlugin {
    const DEPENDS_ON: &'static [&'static str] = &[IMESSAGE_GATEWAY_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, M, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            let gateway = context.require::<IMessageGateway>(IMESSAGE_GATEWAY_PLUGIN_ID)?;
            let channel: Rc<dyn MessageChannel> = self.channel.clone();
            let registration = gateway
                .register(channel)
                .map_err(PluginError::registration)?;
            context.retain(registration);
            Ok(())
        })
    }
}
