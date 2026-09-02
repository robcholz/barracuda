//! Provider Plugin routes exercised through the real portable WebServer.

#![allow(clippy::expect_used, clippy::panic)]
#![recursion_limit = "256"]

use std::rc::Rc;
use std::time::Duration;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_imessage_bluebubble_plugin::{
    IMessageBlueBubblePlugin, CONFIG_API_PATH as BLUEBUBBLES_PATH,
};
use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
use barracuda_imessage_inkbox_plugin::{IMessageInkboxPlugin, CONFIG_API_PATH as INKBOX_PATH};
use barracuda_imessage_qq_plugin::{IMessageQQPlugin, CONFIG_API_PATH as QQ_PATH};
use barracuda_imessage_telegram_plugin::{
    IMessageTelegramPlugin, CONFIG_API_PATH as TELEGRAM_PATH,
};
use barracuda_imessage_wechat_plugin::{IMessageWechatPlugin, CONFIG_API_PATH as WECHAT_PATH};
use barracuda_platform_test::{install_global_memory_vfs, loopback_network, memory_partition};
use barracuda_plugin_api::{ClientFactory, PluginContext};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_webserver_plugin::WebServer;
use embassy_net::{tcp::TcpSocket, Ipv4Address, Stack};
use embedded_io_async::Write as _;
use picoserve::time::EmbassyTimer;

const PORT: u16 = 8787;

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

async fn serve_once(server: Rc<WebServer>, stack: Stack<'static>) -> Result<(), String> {
    let mut tcp_rx = [0_u8; 4096];
    let mut tcp_tx = [0_u8; 4096];
    let mut http = [0_u8; 8192];
    let mut socket = TcpSocket::new(stack, &mut tcp_rx, &mut tcp_tx);
    socket
        .accept(PORT)
        .await
        .map_err(|error| format!("accept failed: {error:?}"))?;
    server
        .serve_connection(EmbassyTimer, &mut http, socket)
        .await
        .map_err(|error| format!("serve failed: {error}"))?;
    Ok(())
}

async fn request(stack: Stack<'static>, method: &str, path: &str, body: &str) -> Vec<u8> {
    let rx = Box::leak(Box::new([0_u8; 4096]));
    let tx = Box::leak(Box::new([0_u8; 4096]));
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket
        .connect((Ipv4Address::new(10, 0, 0, 1), PORT))
        .await
        .expect("connect to provider WebServer");
    let wire = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(wire.as_bytes())
        .await
        .expect("write provider request");

    let mut response = Vec::new();
    let mut chunk = [0_u8; 256];
    loop {
        let length = socket
            .read(&mut chunk)
            .await
            .expect("read provider response");
        response.extend_from_slice(&chunk[..length]);
        if length == 0 {
            return response;
        }
    }
}

async fn exchange(
    server: &Rc<WebServer>,
    stack: Stack<'static>,
    method: &str,
    path: &str,
    body: &str,
) -> Vec<u8> {
    let serving = serve_once(Rc::clone(server), stack);
    let client = request(stack, method, path, body);
    tokio::select! {
        result = serving => panic!("WebServer stopped before returning the provider response: {result:?}"),
        response = client => response,
    }
}

fn assert_status(response: &[u8], status: u16) {
    let prefix = format!("HTTP/1.1 {status} ");
    assert!(
        response.starts_with(prefix.as_bytes()),
        "unexpected response: {}",
        String::from_utf8_lossy(response)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_routes_reject_bad_requests_and_replace_valid_configuration() {
    let network = loopback_network();
    let stack = network.stack();
    let test = async {
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let cases = [
            (TELEGRAM_PATH, r#"{"token":"telegram-token"}"#),
            (
                BLUEBUBBLES_PATH,
                r#"{"server_url":"http://blue.test","password":"password"}"#,
            ),
            (WECHAT_PATH, r#"{"token":"wechat-token"}"#),
            (QQ_PATH, r#"{"app_id":"qq-app","access_token":"qq-token"}"#),
            (
                INKBOX_PATH,
                r#"{"api_key":"inkbox-key","identity_id":"identity"}"#,
            ),
        ];

        for (path, valid_body) in cases {
            // BlueBubbles and Inkbox intentionally own the same stable
            // `imessage` channel, so each provider scenario gets a fresh
            // Gateway registry. This also proves that Plugin teardown releases
            // every retained route and channel registration.
            let server = Rc::new(WebServer::new());
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create Plugin storage");
            let mut manager = PluginManager::open(partition)
                .await
                .expect("open Plugin storage");
            let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 8>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create router");
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));

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
                .register(&mut router, IMessageTelegramPlugin::new(&mut context))
                .expect("register Telegram Plugin");
            manager
                .register(&mut router, IMessageBlueBubblePlugin::new(&mut context))
                .expect("register BlueBubbles Plugin");
            manager
                .register(&mut router, IMessageWechatPlugin::new(&mut context))
                .expect("register Wechat Plugin");
            manager
                .register(&mut router, IMessageQQPlugin::new(&mut context))
                .expect("register QQ Plugin");
            manager
                .register(&mut router, IMessageInkboxPlugin::new(&mut context))
                .expect("register Inkbox Plugin");
            manager.start(&mut router).expect("start provider Plugins");

            assert_status(&exchange(&server, stack, "GET", path, "").await, 405);
            assert_status(
                &exchange(&server, stack, "POST", path, "not-json").await,
                400,
            );
            assert_status(
                &exchange(&server, stack, "POST", path, valid_body).await,
                204,
            );
            assert_status(
                &exchange(&server, stack, "POST", path, valid_body).await,
                204,
            );
            drop(manager);
        }
    };

    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            () = network.run() => panic!("network runner stopped"),
            () = test => {}
        }
    })
    .await
    .expect("provider route scenarios complete");
}
