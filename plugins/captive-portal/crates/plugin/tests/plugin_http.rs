#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(missing_docs)]
#![recursion_limit = "256"]

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use barracuda_agent_plugin::{AgentSetApi, ApiPurpose, ModelApiConfig};
use barracuda_captive_portal_plugin::{CaptivePortalPlugin, SET_API_PATH};
use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_platform_test::{
    install_global_memory_vfs, loopback_network, memory_partition, never_embassy_stack,
};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_webserver_plugin::WebServer;
use embassy_net::{tcp::TcpSocket, Ipv4Address};
use embedded_io_async::Write as _;
use picoserve::time::EmbassyTimer;

const FRAME_SIZE: usize = 512;

struct AgentProvider {
    capability: Option<AgentSetApi>,
}

impl PluginDeclaration for AgentProvider {
    const ID: &'static str = "agent";
}

impl Plugin<FRAME_SIZE> for AgentProvider {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let capability = self.capability.take().expect("capability starts once");
        context.provide(Rc::new(capability))?;
        Ok(())
    }
}

struct WebServerProvider {
    server: Rc<WebServer>,
}

impl PluginDeclaration for WebServerProvider {
    const ID: &'static str = "webserver";
}

impl Plugin<FRAME_SIZE> for WebServerProvider {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.provide(Rc::clone(&self.server))?;
        Ok(())
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
    install_global_memory_vfs()
        .await
        .expect("install global test VFS");
    let mut router = EventRouter::new(lanes).await.expect("create router");
    let observed = Rc::new(RefCell::new(
        Vec::<(ModelApiConfig, ApiPurpose, bool)>::new(),
    ));
    let target = Rc::clone(&observed);
    let server = Rc::new(WebServer::new());

    manager
        .register(
            &mut router,
            AgentProvider {
                capability: Some(AgentSetApi::new(move |api, purpose, default| {
                    target.borrow_mut().push((api, purpose, default));
                    Ok(())
                })),
            },
        )
        .expect("register Agent provider");
    manager
        .register(
            &mut router,
            WebServerProvider {
                server: Rc::clone(&server),
            },
        )
        .expect("register WebServer provider");
    let construction_stack = never_embassy_stack();
    let mut context = PluginContext::new(
        construction_stack,
        ClientFactory::plaintext(construction_stack),
    );
    manager
        .register(&mut router, CaptivePortalPlugin::new(&mut context))
        .expect("register Captive Portal Plugin");
    manager.start(&mut router).expect("start Plugins");

    let network = loopback_network();
    let stack = network.stack();
    let port = 8787;
    let listener = async {
        let mut tcp_rx = [0_u8; 4096];
        let mut tcp_tx = [0_u8; 4096];
        let mut http = [0_u8; 8192];
        let mut socket = TcpSocket::new(stack, &mut tcp_rx, &mut tcp_tx);
        socket.accept(port).await.expect("accept HTTP connection");
        server
            .serve_connection(EmbassyTimer, &mut http, socket)
            .await
            .expect("serve HTTP connection");
    };
    let client = async move {
        let mut tcp_rx = [0_u8; 4096];
        let mut tcp_tx = [0_u8; 4096];
        let mut socket = TcpSocket::new(stack, &mut tcp_rx, &mut tcp_tx);
        socket
            .connect((Ipv4Address::new(10, 0, 0, 1), port))
            .await
            .expect("connect to WebServer");
        let body = br#"[{"timeout_ms":30000,"max_tokens":4096,"image_max_bytes":1048576,"backend":"openai_compatible","purpose":"root_agent","default":true,"api_key":"secret","model":"test-model","base_url":"https://example.invalid/v1"},{"timeout_ms":30000,"max_tokens":4096,"image_max_bytes":1048576,"backend":"openai_compatible","purpose":"memory","default":false,"api_key":"secret","model":"test-model","base_url":"https://example.invalid/v1"}]"#;
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
            () = network.run() => panic!("network runner stopped"),
            result = listener => panic!("listener stopped before client completed: {result:?}"),
            () = client => {}
        }
    })
    .await
    .expect("HTTP request completes");

    let observed = observed.borrow();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0.model, "test-model");
    assert_eq!(observed[0].1, ApiPurpose::RootAgent);
    assert!(observed[0].2);
    assert_eq!(observed[1].1, ApiPurpose::Memory);
    assert!(!observed[1].2);
}
