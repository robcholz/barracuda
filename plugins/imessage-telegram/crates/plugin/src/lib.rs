//! IMessage Telegram provider Plugin.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use barracuda_captive_portal_plugin::{
    CaptivePortal, EntryStatus, ResourceFiles, WebEntry, WebGroup, WebText,
};
use barracuda_imessage_gateway_plugin::IMessageGateway;
use barracuda_imessage_gateway_plugin::{MessageChannel, MessageChannelRegistration};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStorage,
};
use barracuda_webserver_plugin::{
    HttpEndpoint, HttpFuture, HttpMethod, HttpRequest, HttpResponse, WebServer,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use http_client::ClientFactory;
use serde::{Deserialize, Serialize};
use telegram::{Telegram, TelegramConfig};

/// HTTP path accepting Telegram configuration.
pub const CONFIG_API_PATH: &str = "/api/gateway/telegram";

const JSON_CONTENT_TYPE: &str = "application/json";
const CONFIGURATION_STORAGE_KEY: &str = "configuration";

/// Plugin that exposes Telegram configuration and registers the resulting channel.
#[barracuda_plugin::macros::plugin]
pub struct IMessageTelegramPlugin {
    http_clients: ClientFactory<'static>,
}

impl IMessageTelegramPlugin {
    /// Creates an unconfigured provider using Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
        }
    }
}

impl Plugin for IMessageTelegramPlugin {
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
        let configured = Rc::new(Cell::new(false));
        let status = Rc::clone(&configured);
        context.retain(
            portal
                .register_with_status(
                    WebEntry {
                        id: "imessage-telegram",
                        group: WebGroup::Channel,
                        order: 20,
                        title: WebText {
                            zh: "Telegram",
                            en: "Telegram",
                        },
                        summary: WebText {
                            zh: "通过 Bot 收发消息",
                            en: "Send and receive through a bot",
                        },
                        icon: Some("icon.svg"),
                        figure: None,
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                    move || EntryStatus::configured(status.get()),
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
                        Rc::new(Telegram::new(self.http_clients.clone(), config.into()));
                    gateway.register(channel).map_err(PluginError::registration)
                })
                .transpose()?;
        configured.set(channel_registration.is_some());
        let endpoint = ConfigEndpoint {
            gateway,
            http_clients: self.http_clients.clone(),
            channel_registration: Mutex::new(channel_registration),
            configured,
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
    token: String,
    #[serde(default = "default_telegram_api_base")]
    api_base: String,
    #[serde(default = "default_draft_min_delta_bytes")]
    draft_min_delta_bytes: usize,
}

impl From<ConfigRequest> for TelegramConfig {
    fn from(value: ConfigRequest) -> Self {
        TelegramConfig {
            token: value.token,
            api_base: value.api_base,
            draft_min_delta_bytes: value.draft_min_delta_bytes,
        }
    }
}

fn default_telegram_api_base() -> String {
    "https://api.telegram.org".into()
}
const fn default_draft_min_delta_bytes() -> usize {
    24
}

struct ConfigEndpoint<Storage> {
    gateway: Rc<IMessageGateway>,
    http_clients: ClientFactory<'static>,
    channel_registration: Mutex<NoopRawMutex, Option<MessageChannelRegistration>>,
    /// Whether a channel is registered with the Gateway, readable without the lock.
    configured: Rc<Cell<bool>>,
    storage: Storage,
}

impl<Storage> ConfigEndpoint<Storage> {
    fn response(status: u16, body: &'static [u8]) -> HttpResponse {
        HttpResponse::new(status, JSON_CONTENT_TYPE, Vec::from(body))
    }

    /// `GET` body: whether a channel is configured, never its settings.
    fn configured_response(configured: bool) -> HttpResponse {
        if configured {
            Self::response(200, br#"{"configured":true}"#)
        } else {
            Self::response(200, br#"{"configured":false}"#)
        }
    }
}

impl<Storage: PluginStorage> HttpEndpoint for ConfigEndpoint<Storage> {
    fn handle<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(async move {
            match request.method() {
                HttpMethod::Post => {}
                HttpMethod::Get => return Self::configured_response(self.configured.get()),
                _ => return Self::response(405, br#"{"error":"method_not_allowed"}"#),
            }
            let Ok(config) = serde_json::from_slice::<ConfigRequest>(request.body()) else {
                log::warn!("rejected invalid Telegram gateway configuration");
                return Self::response(400, br#"{"error":"invalid_request"}"#);
            };
            let mut channel_registration = self.channel_registration.lock().await;
            let previous_configuration =
                match self.storage.get_bytes(CONFIGURATION_STORAGE_KEY).await {
                    Ok(configuration) => configuration,
                    Err(error) => {
                        log::error!(
                            "failed to read the previous Telegram gateway configuration: {error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                };
            let Ok(bytes) = encode_configuration(&config) else {
                log::error!("failed to encode Telegram gateway configuration");
                return Self::response(500, br#"{"error":"storage"}"#);
            };
            if let Err(error) = self
                .storage
                .put(CONFIGURATION_STORAGE_KEY, bytes.as_slice())
                .await
            {
                log::error!("failed to persist Telegram gateway configuration: {error}");
                return Self::response(500, br#"{"error":"storage"}"#);
            }
            let channel: Rc<dyn MessageChannel> =
                Rc::new(Telegram::new(self.http_clients.clone(), config.into()));
            channel_registration.take();
            self.configured.set(false);
            match self.gateway.register(channel) {
                Ok(registration) => {
                    channel_registration.replace(registration);
                    self.configured.set(true);
                    log::info!("configured Telegram gateway provider");
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
                            "failed to roll back rejected Telegram gateway configuration: {storage_error}"
                        );
                        return Self::response(500, br#"{"error":"storage"}"#);
                    }
                    log::warn!("rejected Telegram gateway configuration: {error}");
                    Self::response(422, br#"{"error":"registration_failed"}"#)
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

    struct OccupiedTelegramChannel;

    impl MessageChannel for OccupiedTelegramChannel {
        fn channel(&self) -> &str {
            "telegram"
        }

        fn send_message(&self, _request: SendMessageRequest) -> ChannelFuture<'_, SendReceipt> {
            Box::pin(async { Err(ChannelError::unsupported(Operation::SendMessage)) })
        }
    }

    /// Exercises the configuration endpoint against the real Gateway, with the
    /// `telegram` channel already taken by another provider when `occupied`.
    struct ConfigurationProbe {
        http_clients: ClientFactory<'static>,
        occupied: bool,
        completed: Rc<RefCell<bool>>,
    }

    impl PluginDeclaration for ConfigurationProbe {
        const ID: &'static str = "configuration-probe";
        const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];
    }

    impl Plugin for ConfigurationProbe {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, Storage>,
        ) -> PluginResult<()>
        where
            Storage: PluginStorage,
        {
            let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
            if self.occupied {
                let occupied: Rc<dyn MessageChannel> = Rc::new(OccupiedTelegramChannel);
                context.retain(
                    gateway
                        .register(occupied)
                        .map_err(PluginError::registration)?,
                );
            }
            let configured = Rc::new(Cell::new(false));
            let endpoint = ConfigEndpoint {
                gateway,
                http_clients: self.http_clients.clone(),
                channel_registration: Mutex::new(None),
                configured: Rc::clone(&configured),
                storage: context.storage().clone(),
            };
            let get = || {
                let response =
                    block_on(endpoint.handle(HttpRequest::new(HttpMethod::Get, Vec::new())));
                assert_eq!(response.status(), 200);
                response.body().map(<[u8]>::to_vec)
            };
            assert_eq!(get().as_deref(), Some(&br#"{"configured":false}"#[..]));
            let body = br#"{"token":"secret"}"#;
            let response =
                block_on(endpoint.handle(HttpRequest::new(HttpMethod::Post, body.to_vec())));
            let stored = block_on(context.storage().get_bytes(CONFIGURATION_STORAGE_KEY))?;
            if self.occupied {
                assert_eq!(response.status(), 422);
                assert_eq!(
                    response.body(),
                    Some(&br#"{"error":"registration_failed"}"#[..])
                );
                assert!(stored.is_none());
                assert!(!configured.get());
                assert_eq!(get().as_deref(), Some(&br#"{"configured":false}"#[..]));
            } else {
                assert_eq!(response.status(), 204);
                assert!(stored.is_some());
                assert!(configured.get());
                let reply = get().expect("buffered body");
                assert_eq!(reply, br#"{"configured":true}"#);
            }
            let response =
                block_on(endpoint.handle(HttpRequest::new(HttpMethod::Delete, Vec::new())));
            assert_eq!(response.status(), 405);
            self.completed.replace(true);
            Ok(())
        }
    }

    fn run(occupied: bool) {
        block_on(install_global_memory_vfs()).expect("install test VFS");
        let partition = block_on(memory_partition(64 * 1024)).expect("create test database region");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        manager.install_vfs(block_on(barracuda_vfs::global_namespace()));
        let stack = never_embassy_stack();
        let info = barracuda_plugin::api::TargetIdentity::new(
            barracuda_plugin::api::PlatformInfo::new("test", "test", "test-arch", "hosted"),
            barracuda_plugin::api::BoardInfo::new(
                "test-board",
                barracuda_plugin::api::Hardware::new("test-chip"),
            ),
        );
        let mut context = PluginContext::new(info, stack, ClientFactory::plaintext(stack));
        let completed = Rc::new(RefCell::new(false));
        manager
            .register(WorkflowPlugin::new(&mut context))
            .expect("register Workflow Plugin");
        manager
            .register(IMessageGatewayPlugin::new(&mut context))
            .expect("register IMessage Gateway Plugin");
        manager
            .register(ConfigurationProbe {
                http_clients: context.http_clients.clone(),
                occupied,
                completed: Rc::clone(&completed),
            })
            .expect("exercise provider configuration");
        assert!(*completed.borrow());
    }

    #[test]
    fn accepted_configuration_is_reported_by_get_and_status() {
        run(false);
    }

    #[test]
    fn rejected_channel_registration_is_registration_failed() {
        run(true);
    }

    #[test]
    fn stored_configuration_round_trips_every_field() -> Result<(), serde_json::Error> {
        let config = ConfigRequest {
            token: "secret".into(),
            api_base: "https://telegram.example".into(),
            draft_min_delta_bytes: 31,
        };

        let bytes = encode_configuration(&config)?;
        let restored = decode_configuration(&bytes)?;

        assert_eq!(restored.token, config.token);
        assert_eq!(restored.api_base, config.api_base);
        assert_eq!(restored.draft_min_delta_bytes, config.draft_min_delta_bytes);
        Ok(())
    }
}
