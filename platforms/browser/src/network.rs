//! Embassy packet-channel construction for the browser virtual NIC.

use std::cell::RefCell;

use barracuda_net_gateway_protocol::{decode, encode, Kind};
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_net_driver_channel::{driver::HardwareAddress, Device, State, TxRunner};
use wasm_bindgen::{closure::Closure, JsCast as _, JsValue};
use web_sys::{BinaryType, MessageEvent, WebSocket};

const MTU: usize = 1500;
const RX_PACKETS: usize = 8;
const TX_PACKETS: usize = 8;
const SOCKETS: usize = 16;

thread_local! {
    static GATEWAY_URL: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Records the gateway endpoint passed by the worker bootstrap.
pub fn configure_gateway(url: String) -> Result<(), JsValue> {
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        return Err(JsValue::from_str("gateway URL must use ws:// or wss://"));
    }
    GATEWAY_URL.with(|configured| configured.replace(Some(url)));
    Ok(())
}

/// Creates the same Embassy IP stack used on native Platforms.
///
/// The returned socket is intentionally private: Plugins receive only the
/// resulting [`Stack`], never the WebSocket host transport.
pub fn create_stack(spawner: Spawner) -> Result<Stack<'static>, JsValue> {
    let url = GATEWAY_URL
        .with(|configured| configured.borrow().clone())
        .ok_or_else(|| JsValue::from_str("network gateway is not configured"))?;
    let socket = WebSocket::new(&url)?;
    socket.set_binary_type(BinaryType::Arraybuffer);

    let state = Box::leak(Box::new(State::<MTU, RX_PACKETS, TX_PACKETS>::new()));
    let (mut runner, device): (_, Device<'static, MTU>) =
        embassy_net_driver_channel::new(state, HardwareAddress::Ip);
    runner.set_link_state(embassy_net_driver_channel::driver::LinkState::Up);
    let (_state, mut rx, tx) = runner.split();

    let hello_socket = socket.clone();
    let onopen = Closure::<dyn FnMut()>::new(move || {
        let mut hello = [0; 3];
        if let Ok(length) = encode(
            Kind::Hello,
            &[barracuda_net_gateway_protocol::VERSION],
            &mut hello,
        ) {
            let _ = hello_socket.send_with_u8_array(&hello[..length]);
        }
    });
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();

    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
        if let Ok(frame) = decode(&bytes) {
            if frame.kind == Kind::Packet && frame.payload.len() <= MTU {
                if let Some(target) = rx.try_rx_buf() {
                    target[..frame.payload.len()].copy_from_slice(frame.payload);
                    rx.rx_done(frame.payload.len());
                }
            }
        }
    });
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    let mut dns = heapless::Vec::new();
    dns.push(Ipv4Address::new(10, 42, 0, 3))
        .map_err(|_| JsValue::from_str("DNS capacity is zero"))?;
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(10, 42, 0, 2), 24),
        gateway: Some(Ipv4Address::new(10, 42, 0, 1)),
        dns_servers: dns,
    });
    let resources = Box::leak(Box::new(StackResources::<SOCKETS>::new()));
    let (stack, network_runner) = embassy_net::new(device, config, resources, random_seed()?);
    spawner
        .spawn(network_task(network_runner))
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    spawner
        .spawn(transmit_task(socket, tx))
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(stack)
}

/// Sends one packet emitted by a channel [`embassy_net_driver_channel::TxRunner`].
pub fn send_packet(socket: &WebSocket, packet: &[u8]) -> Result<(), JsValue> {
    let mut framed = vec![0; barracuda_net_gateway_protocol::encoded_len(packet.len())];
    let length = encode(Kind::Packet, packet, &mut framed)
        .map_err(|_| JsValue::from_str("packet cannot be framed"))?;
    socket.send_with_u8_array(&framed[..length])
}

fn random_seed() -> Result<u64, JsValue> {
    let worker = js_sys::global()
        .dyn_into::<web_sys::WorkerGlobalScope>()
        .map_err(|_| JsValue::from_str("Web Crypto requires a worker global scope"))?;
    let crypto = worker.crypto()?;
    let mut bytes = [0; 8];
    crypto.get_random_values_with_u8_array(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

type BrowserDevice = Device<'static, MTU>;

#[embassy_executor::task]
async fn network_task(mut runner: Runner<'static, BrowserDevice>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn transmit_task(socket: WebSocket, mut runner: TxRunner<'static, MTU>) {
    loop {
        let packet = runner.tx_buf().await;
        let _ = send_packet(&socket, packet);
        runner.tx_done();
    }
}
