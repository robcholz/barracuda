#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(missing_docs)]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use barracuda_agent_plugin::{
    AgentSetApi, ApiPurpose, ModelApiConfig, PLUGIN_ID as AGENT_PLUGIN_ID,
};
use barracuda_captive_portal_plugin::{CaptivePortalPlugin, SET_API_PATH};
use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_host::TokioStack;
use barracuda_platform_test::{memory_partition, MemFs};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginManager, PluginStartFuture};
use barracuda_webserver_plugin::{
    WebServer, WebServerListenFuture, WebServerListener, PLUGIN_ID as WEBSERVER_PLUGIN_ID,
    WEB_SERVER_CONNECTION_SLOTS,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const FRAME_SIZE: usize = 512;

struct AgentProvider {
    capability: Option<AgentSetApi>,
}

impl Plugin<FRAME_SIZE> for AgentProvider {
    fn id(&self) -> &'static str {
        AGENT_PLUGIN_ID
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            let capability = self.capability.take().expect("capability starts once");
            context.provide(Rc::new(capability))?;
            Ok(())
        })
    }
}

struct WebServerProvider {
    server: Rc<WebServer>,
}

impl Plugin<FRAME_SIZE> for WebServerProvider {
    fn id(&self) -> &'static str {
        WEBSERVER_PLUGIN_ID
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            context.provide(Rc::clone(&self.server))?;
            Ok(())
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_exposes_agent_set_api_over_http() {
    let partition = memory_partition(64 * 1024)
        .await
        .expect("create database partition");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    let lanes = Box::leak(Box::new(RpcLaneStorage::<4, FRAME_SIZE, 4>::new()));
    let mut router = EventRouter::new(lanes, MemFs::new(), "workflows").expect("create router");
    let observed = Rc::new(RefCell::new(None::<(ModelApiConfig, ApiPurpose, bool)>));
    let target = Rc::clone(&observed);
    let server = Rc::new(WebServer::new());

    manager
        .register(
            &mut router,
            AgentProvider {
                capability: Some(AgentSetApi::new(move |api, purpose, default| {
                    *target.borrow_mut() = Some((api, purpose, default));
                    Ok(())
                })),
            },
        )
        .await
        .expect("register Agent provider");
    manager
        .register(
            &mut router,
            WebServerProvider {
                server: Rc::clone(&server),
            },
        )
        .await
        .expect("register WebServer provider");
    manager
        .register(&mut router, CaptivePortalPlugin::new())
        .await
        .expect("register Captive Portal Plugin");
    manager.start(&mut router).await.expect("start Plugins");

    let available =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("reserve test port");
    let port = available.local_addr().expect("read test address").port();
    drop(available);
    let mut network = TokioStack::default();
    let listener: WebServerListenFuture<'_, std::io::Error> =
        network.listen(Rc::clone(&server), port, WEB_SERVER_CONNECTION_SLOTS);
    let client = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect to WebServer");
        let body = br#"{"timeout_ms":30000,"max_tokens":4096,"image_max_bytes":1048576,"backend":"openai_compatible","purpose":"root_agent","default":true,"api_key":"secret","model":"test-model","base_url":"https://example.invalid/v1"}"#;
        let head = format!(
            "POST {SET_API_PATH} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(head.as_bytes())
            .await
            .expect("write request head");
        socket.write_all(body).await.expect("write request body");
        let mut response = Vec::new();
        let mut chunk = [0_u8; 128];
        loop {
            let length = socket.read(&mut chunk).await.expect("read response");
            response.extend_from_slice(&chunk[..length]);
            if length == 0 || response.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            response.starts_with(b"HTTP/1.1 204 \r\n"),
            "{}",
            String::from_utf8_lossy(&response),
        );
    };

    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            result = listener => panic!("listener stopped unexpectedly: {result:?}"),
            () = client => {}
        }
    })
    .await
    .expect("HTTP request completes");

    let observed = observed.borrow();
    let (api, purpose, default) = observed.as_ref().expect("Agent set API invoked");
    assert_eq!(api.model, "test-model");
    assert_eq!(*purpose, ApiPurpose::RootAgent);
    assert!(*default);
}
