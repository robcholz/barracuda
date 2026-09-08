//! Barracuda user-space network gateway for virtual Platform NICs.

mod session;

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header::ORIGIN, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use barracuda_net_gateway_protocol::{decode, encode, Kind, VERSION};
use clap::Parser;
use futures_util::{SinkExt as _, StreamExt as _};
use session::{NetworkSession, Outbound};
use tokio::sync::mpsc;

#[derive(Debug, Parser)]
#[command(about = "Route Barracuda virtual NICs through host TCP and UDP sockets")]
struct Arguments {
    /// WebSocket listen address.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    /// Public base URL used when reporting a device WebServer route.
    #[arg(long, default_value = "http://127.0.0.1")]
    public_base_url: String,
    /// Host address used for per-device WebServer forwarding listeners.
    #[arg(long, default_value = "127.0.0.1")]
    forward_address: std::net::IpAddr,
    /// Port inside the device to expose through the returned URL.
    #[arg(long, default_value_t = 80)]
    device_web_port: u16,
    /// Resolver used for requests sent to the guest-visible DNS address.
    #[arg(long, default_value = "1.1.1.1:53")]
    dns_server: SocketAddr,
    /// Browser origins allowed to open virtual-NIC sessions; repeat for multiple origins.
    #[arg(long = "allowed-origin")]
    allowed_origins: Vec<String>,
}

#[derive(Clone)]
struct GatewayState {
    public_base_url: Arc<str>,
    forward_address: std::net::IpAddr,
    device_web_port: u16,
    dns_server: SocketAddr,
    allowed_origins: Arc<[String]>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let arguments = Arguments::parse();
    let state = GatewayState {
        public_base_url: arguments.public_base_url.into(),
        forward_address: arguments.forward_address,
        device_web_port: arguments.device_web_port,
        dns_server: arguments.dns_server,
        allowed_origins: arguments.allowed_origins.into(),
    };
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(arguments.listen).await?;
    tracing::info!(address = %arguments.listen, "network gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn router(state: GatewayState) -> Router {
    Router::new()
        .route("/v1/connect", get(connect))
        .with_state(state)
}

async fn connect(
    upgrade: WebSocketUpgrade,
    State(state): State<GatewayState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !origin_allowed(&headers, &state.allowed_origins) {
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade
        .on_upgrade(move |socket| serve_device(socket, state))
        .into_response()
}

fn origin_allowed(headers: &HeaderMap, allowed_origins: &[String]) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    origin
        .to_str()
        .is_ok_and(|origin| allowed_origins.iter().any(|allowed| allowed == origin))
}

async fn serve_device(socket: WebSocket, state: GatewayState) {
    let session_id = uuid::Uuid::new_v4();
    let (mut sender, mut receiver) = socket.split();

    let Some(Ok(Message::Binary(message))) = receiver.next().await else {
        tracing::warn!(%session_id, "device disconnected before protocol handshake");
        return;
    };
    if !is_hello(&message) {
        tracing::warn!(%session_id, "device did not begin with a valid hello frame");
        return;
    }

    let (responses_tx, mut responses_rx) = mpsc::channel::<Outbound>(32);
    let session = match NetworkSession::new(
        responses_tx,
        &state.public_base_url,
        state.forward_address,
        state.device_web_port,
        state.dns_server,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%session_id, ?error, "could not start device network session");
            return;
        }
    };

    let send_task = tokio::spawn(async move {
        while let Some(outbound) = responses_rx.recv().await {
            let (kind, payload) = match outbound {
                Outbound::Packet(packet) => (Kind::Packet, packet),
                Outbound::DeviceUrl(url) => (Kind::DeviceUrl, url.into_bytes()),
            };
            let mut message = vec![0; barracuda_net_gateway_protocol::encoded_len(payload.len())];
            let Ok(length) = encode(kind, &payload, &mut message) else {
                break;
            };
            if sender
                .send(Message::Binary(message[..length].to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    tracing::info!(%session_id, "device network session opened");

    while let Some(message) = receiver.next().await {
        let message = match message {
            Ok(Message::Binary(message)) => message,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Close(_) | Message::Text(_)) | Err(_) => break,
        };
        match decode(&message) {
            Ok(frame) if frame.kind == Kind::Packet => {
                if session.input(frame.payload).await.is_err() {
                    break;
                }
            }
            Ok(_) => break,
            Err(error) => {
                tracing::warn!(%session_id, ?error, "invalid virtual NIC frame");
                break;
            }
        }
    }
    session.close().await;
    send_task.abort();
    tracing::info!(%session_id, "device network session closed");
}

fn is_hello(message: &[u8]) -> bool {
    decode(message).is_ok_and(|frame| frame.kind == Kind::Hello && frame.payload == [VERSION])
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};
    use barracuda_net_gateway_protocol::{decode, encode, encoded_len, Kind, VERSION};
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    use super::{is_hello, origin_allowed, router, GatewayState};

    #[test]
    fn session_requires_a_valid_hello_as_its_first_frame() {
        let mut hello = [0; 3];
        let hello_len = encode(Kind::Hello, &[VERSION], &mut hello).unwrap_or_default();
        assert!(is_hello(&hello[..hello_len]));

        let mut packet = vec![0; encoded_len(1)];
        let packet_len = encode(Kind::Packet, &[0x45], &mut packet).unwrap_or_default();
        assert!(!is_hello(&packet[..packet_len]));
        assert!(!is_hello(&[VERSION + 1, Kind::Hello as u8, VERSION]));
    }

    #[test]
    fn browser_origins_require_an_explicit_allowlist_entry() {
        let native_headers = HeaderMap::new();
        assert!(origin_allowed(&native_headers, &[]));

        let mut browser_headers = HeaderMap::new();
        browser_headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:3000"));
        assert!(!origin_allowed(&browser_headers, &[]));
        assert!(origin_allowed(
            &browser_headers,
            &[String::from("http://localhost:3000")]
        ));
    }

    #[tokio::test]
    async fn websocket_handshake_opens_a_routed_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind gateway listener");
        let address = listener.local_addr().expect("gateway listener address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(GatewayState {
                    public_base_url: "http://127.0.0.1".into(),
                    forward_address: "127.0.0.1".parse().expect("forward address"),
                    device_web_port: 80,
                    dns_server: "127.0.0.1:53".parse().expect("DNS address"),
                    allowed_origins: Vec::new().into(),
                }),
            )
            .await
            .expect("serve gateway");
        });

        let (mut socket, _response) = connect_async(format!("ws://{address}/v1/connect"))
            .await
            .expect("connect gateway WebSocket");
        let mut hello = [0; 3];
        let length = encode(Kind::Hello, &[VERSION], &mut hello).expect("encode hello");
        socket
            .send(Message::Binary(hello[..length].to_vec().into()))
            .await
            .expect("send hello");
        let message = socket
            .next()
            .await
            .expect("gateway response")
            .expect("valid WebSocket response")
            .into_data();
        assert!(decode(&message).is_ok_and(|frame| frame.kind == Kind::DeviceUrl));
        socket.close(None).await.expect("close gateway WebSocket");
        server.abort();
    }
}
