//! Experimental Barracuda network-gateway transport.
//!
//! The WebSocket boundary terminates here; each connection gets an isolated
//! packet session. `SlirpSession` is the intended ownership point for a future
//! libslirp NAT, DNS and port-forwarding context; packet routing is not wired.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use barracuda_net_gateway_protocol::{decode, encode, Kind, VERSION};
use clap::Parser;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc;

#[derive(Debug, Parser)]
#[command(about = "Exercise the experimental Barracuda virtual-NIC transport")]
struct Arguments {
    /// WebSocket listen address.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    /// Public base URL used when reporting a device WebServer route.
    #[arg(long, default_value = "http://127.0.0.1:8787/device")]
    public_base_url: String,
    /// Port inside the device to expose through the returned URL.
    #[arg(long, default_value_t = 80)]
    device_web_port: u16,
}

#[derive(Clone)]
struct GatewayState {
    public_base_url: Arc<str>,
    device_web_port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let arguments = Arguments::parse();
    let state = GatewayState {
        public_base_url: arguments.public_base_url.into(),
        device_web_port: arguments.device_web_port,
    };
    let app = Router::new()
        .route("/v1/connect", get(connect))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(arguments.listen).await?;
    tracing::info!(address = %arguments.listen, "network gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn connect(
    upgrade: WebSocketUpgrade,
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| serve_device(socket, state))
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
    let mut slirp = SlirpSession::new(responses_tx, state.device_web_port);

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

    let device_url = format!(
        "{}/{session_id}/",
        state.public_base_url.trim_end_matches('/')
    );
    tracing::info!(%session_id, %device_url, "device network session opened");
    // The URL is emitted through the same response queue before packet traffic.
    slirp.set_device_url(device_url);

    while let Some(Ok(Message::Binary(message))) = receiver.next().await {
        match decode(&message) {
            Ok(frame) if frame.kind == Kind::Packet => slirp.input(frame.payload),
            Ok(_) => break,
            Err(error) => {
                tracing::warn!(%session_id, ?error, "invalid virtual NIC frame");
                break;
            }
        }
    }
    slirp.close();
    send_task.abort();
    tracing::info!(%session_id, "device network session closed");
}

fn is_hello(message: &[u8]) -> bool {
    decode(message).is_ok_and(|frame| frame.kind == Kind::Hello && frame.payload == [VERSION])
}

/// Reserves ownership for one user's future libslirp context.
///
/// This prototype deliberately drops packets. It must not be treated as a NAT
/// implementation until this type owns and polls a real libslirp context.
struct SlirpSession {
    responses: mpsc::Sender<Outbound>,
    device_web_port: u16,
}

impl SlirpSession {
    fn new(responses: mpsc::Sender<Outbound>, device_web_port: u16) -> Self {
        Self {
            responses,
            device_web_port,
        }
    }

    fn input(&mut self, packet: &[u8]) {
        tracing::trace!(bytes = packet.len(), "packet submitted to libslirp session");
        // libslirp's output callback sends response packets through `responses`.
    }

    fn set_device_url(&mut self, url: String) {
        tracing::debug!(port = self.device_web_port, %url, "device WebServer forwarding assigned");
        let _ = self.responses.try_send(Outbound::DeviceUrl(url));
    }

    fn close(self) {}
}

enum Outbound {
    #[allow(dead_code)] // Constructed by the libslirp output callback.
    Packet(Vec<u8>),
    DeviceUrl(String),
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use barracuda_net_gateway_protocol::{encode, encoded_len, Kind, VERSION};

    use super::is_hello;

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
}
