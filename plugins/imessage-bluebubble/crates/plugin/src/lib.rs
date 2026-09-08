//! IMessage BlueBubbles provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::ClientFactory;
use serde::{Deserialize, Serialize};

/// HTTP path accepting BlueBubbles configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/bluebubbles";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes BlueBubbles configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageBlueBubblePlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageBlueBubblePlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl Plugin for IMessageBlueBubblePlugin {
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
                        id: "imessage-bluebubble",
                        title: "BlueBubbles",
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
        let channel_registration =
            embassy_futures::block_on(load_configuration(context.storage()))?
                .map(|config| {
                    let channel: Rc<dyn MessageChannel> =
                        Rc::new(BlueBubbles::new(self.http_clients.clone(), config.into()));
                    gateway.register(channel).map_err(PluginError::registration)
                })
                .transpose()?;
        let endpoint = ConfigEndpoint {
            gateway,
            http_clients: self.http_clients.clone(),
            channel_registration: Mutex::new(channel_registration),
            storage: context.storage().clone(),
        };
        let registration = webserver
            .serve_http(CONFIG_API_PATH, endpoint)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    server_url: String,
    password: String,
    #[serde(default = "default_true")]
    use_private_api: bool,
    #[serde(default = "default_stream_edit_min_delta_bytes")]
    stream_edit_min_delta_bytes: usize,
    #[serde(default = "default_stream_max_edits")]
    stream_max_edits: usize,
}

impl From<ConfigRequest> for BlueBubblesConfig {
    fn from(value: ConfigRequest) -> Self {
        BlueBubblesConfig {
            server_url: value.server_url,
            password: value.password,
            use_private_api: value.use_private_api,
            stream_edit_min_delta_bytes: value.stream_edit_min_delta_bytes,
            stream_max_edits: value.stream_max_edits,
        }
    }
}

const fn default_true() -> bool {
    true
}
const fn default_stream_edit_min_delta_bytes() -> usize {
    128
}
const fn default_stream_max_edits() -> usize {
    4
}

struct ConfigEndpoint<Storage> {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static>,
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    storage: Storage,
}

impl<Storage> ConfigEndpoint<Storage> {
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            if request.method() != HttpMethod::Post {
                return Self::response(405, br#"{"error":"method_not_allowed"}"#);
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid BlueBubbles gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let mut channel_registration = self.channel_registration.lock().await;
            let previous_configuration =
                match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
                    Ok(configuration) => configuration,
                    Err(error) => {
                        log::error!(
                            "failed to read the previous BlueBubbles gateway configuration: {error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                };
            let Ok(bytes) = encode_configuration(&config) else {
                log::error!("failed to encode BlueBubbles gateway configuration");
                return Self::response(500, br#"{"error":"storage"}"#);
            };
            if let Err(error) = self
                .storage
                .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
                .await
            {
                log::error!("failed to persist BlueBubbles gateway configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            let channel: Rc<dyn MessageChannel> =
                Rc::new(BlueBubbles::new(self.http_clients.clone(), config.into()));
            channel_registration.take();
            match self.gateway.register(channel) {
                Ok(registration) => {
                    channel_registration.replace(registration);
                    log::info!("configured BlueBubbles gateway provider");
                    Self::response(204, b"")
                }
                Err(error) => {
                    let restored = if let Some(previous) = previous_configuration.as_deref() {
                        self.storage.put(CONFIGURATION_STORAGE_KEY, previous).await
                    } else {
                        self.storage.delete(CONFIGURATION_STORAGE_KEY).await
                    };
                    if let Err(storage_error) = restored {
                        log::error!(
                            "failed to roll back rejected BlueBubbles gateway configuration: {storage_error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                    log::warn!("rejected BlueBubbles gateway configuration: {error}");
                    Self::response(422, br#"{"error":"invalid_configuration"}"#)
                }
            }
        })
    }
}

fn encode_configuration(config: &ConfigRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(config)
}

fn decode_configuration(bytes: &[u8]) -> Result<ConfigRequest, serde_json::Error> {
    serde_json::from_slice(bytes)
}

async fn load_configuration<Storage: PluginStorage>(
    storage: &Storage,
) -> PluginResult<Option<ConfigRequest>> {
    storage
        .get_bytes(CONFIGURATION_STORAGE_KEY)
        .await?
        .map(|bytes| decode_configuration(&bytes))
        .transpose()
        .map_err(PluginError::registration)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use core::cell::RefCell;

    use barracuda_imessage_gateway_plugin::{
        ChannelError, ChannelFuture, IMessageGatewayPlugin, Operation, SendMessageRequest,
        SendReceipt,
    };
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack,
    };
    use barracuda_plugin::manager::{PluginDeclaration, PluginManager};
    use barracuda_workflow_plugin::WorkflowPlugin;
    use futures_lite::future::block_on;

    use super::*;

    struct OccupiedImessageChannel;

    impl MessageChannel for OccupiedImessageChannel {
        fn channel(&self) -> &str {
            "imessage"
        }

        fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
            Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
        }
    }

    struct RejectedConfigurationProbe {
        http_clients: ClientFactory<'static>,
        rolled_back: Rc<RefCell<bool>>,
    }

    impl PluginDeclaration for RejectedConfigurationProbe {
        const ID: &'static str = "rejected-configuration-probe";
        const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
    }

    impl Plugin for RejectedConfigurationProbe {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, Storage>,
        ) -> PluginResult<()>
        where
            Storage: PluginStorage,
        {
            let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
            let occupied: Rc<dyn MessageChannel> = Rc::new(OccupiedImessageChannel);
            context.retain(
                gateway
                    .register(occupied)
                    .map_err(PluginError::registration)?,
            );
            let endpoint = ConfigEndpoint {
                gateway,
                http_clients: self.http_clients.clone(),
                channel_registration: Mutex::new(None),
                storage: context.storage().clone(),
            };
            let body = br#"{"server_url":"https://blue.example","password":"secret"}"#;
            let response =
                block_on(endpoint.handle(HttpRequest::new(HttpMethod::Post, body.to_vec())));
            let stored = block_on(context.storage().get_bytes(CONFIGURATION_STORAGE_KEY))?;
            self.rolled_back
                .replace(response.status() == 422 && stored.is_none());
            Ok(())
        }
    }

    fn plugin_context() -> PluginContext {
        let stack = never_embassy_stack();
        PluginContext::new(stack, ClientFactory::plaintext(stack))
    }

    #[test]
    fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
        let config = ConfigRequest {
            server_url: "https://blue.example".into(),
            password: "secret".into(),
            use_private_api: false,
            stream_edit_min_delta_bytes: 64,
            stream_max_edits: 7,
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.server_url, config.server_url);
        assert_eq!(restored.password, config.password);
        assert_eq!(restored.use_private_api, config.use_private_api);
        assert_eq!(
            restored.stream_edit_min_delta_bytes,
            config.stream_edit_min_delta_bytes
        );
        assert_eq!(restored.stream_max_edits, config.stream_max_edits);
        Ok(())
    }

    #[test]
    fn rejected_channel_registration_rolls_back_persisted_configuration() {
        block_on(install_global_memory_vfs()).expect("install test VFS");
        let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
        let mut context = plugin_context();
        let rolled_back = Rc::new(RefCell::new(false));

        manager
            .register(WorkflowPlugin::new(&mut context))
            .expect("register Workflow Plugin");
        manager
            .register(IMessageGatewayPlugin::new(&mut context))
            .expect("register IMessage Gateway Plugin");
        manager
            .register(RejectedConfigurationProbe {
                http_clients: context.http_clients.clone(),
                rolled_back: Rc::clone(&rolled_back),
            })
            .expect("exercise rejected provider configuration");

        assert!(*rolled_back.borrow());
    }
}
