//! Embassy packet-channel construction for the browser virtual NIC.

use std::cell::RefCell;

use barracuda_net_gateway_protocol::{decode, encode, Kind};
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_net_driver_channel::{
    driver::{HardwareAddress, LinkState},
    Device, State, TxRunner,
};
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
    let (runner, device): (_, Device<'static, MTU>) =
        embassy_net_driver_channel::new(state, HardwareAddress::Ip);
    let (link, mut rx, tx) = runner.split();

    let hello_socket = socket.clone();
    let transmit_socket = socket.clone();
    let mut transmit_runner = Some(tx);
    let onopen = Closure::<dyn FnMut()>::new(move || {
        let mut hello = [0; 3];
        let hello_sent = encode(
            Kind::Hello,
            &[barracuda_net_gateway_protocol::VERSION],
            &mut hello,
        )
        .ok()
        .and_then(|length| hello_socket.send_with_u8_array(&hello[..length]).ok())
        .is_some();
        let Some(tx) = transmit_runner.take() else {
            return;
        };
        if !hello_sent
            || spawner
                .spawn(transmit_task(transmit_socket.clone(), link, tx))
                .is_err()
        {
            link.set_link_state(LinkState::Down);
            let _ = hello_socket.close();
            return;
        }
        link.set_link_state(LinkState::Up);
    });
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();

    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
        if let Ok(frame) = decode(&bytes) {
            match frame.kind {
                Kind::Packet if frame.payload.len() <= MTU => {
                    if let Some(target) = rx.try_rx_buf() {
                        target[..frame.payload.len()].copy_from_slice(frame.payload);
                        rx.rx_done(frame.payload.len());
                    }
                }
                Kind::DeviceUrl => {
                    if let Ok(url) = core::str::from_utf8(frame.payload) {
                        report_device_url(url);
                    }
                }
                _ => {}
            }
        }
    });
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    let close_link = link;
    let onclose = Closure::<dyn FnMut()>::new(move || {
        close_link.set_link_state(LinkState::Down);
    });
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();

    let error_link = link;
    let onerror = Closure::<dyn FnMut()>::new(move || {
        error_link.set_link_state(LinkState::Down);
    });
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    onerror.forget();

    let mut dns = heapless::Vec::new();
    dns.push(Ipv4Address::new(10, 0, 2, 3))
        .map_err(|_| JsValue::from_str("DNS capacity is zero"))?;
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24),
        gateway: Some(Ipv4Address::new(10, 0, 2, 2)),
        dns_servers: dns,
    });
    let resources = Box::leak(Box::new(StackResources::<SOCKETS>::new()));
    let (stack, network_runner) = embassy_net::new(device, config, resources, random_seed()?);
    if let Err(error) = spawner.spawn(network_task(network_runner)) {
        let _ = socket.close();
        return Err(JsValue::from_str(&error.to_string()));
    }
    Ok(stack)
}

fn report_device_url(url: &str) {
    let Ok(worker) = js_sys::global().dyn_into::<web_sys::DedicatedWorkerGlobalScope>() else {
        return;
    };
    let event = js_sys::Object::new();
    let _result = js_sys::Reflect::set(
        &event,
        &JsValue::from_str("type"),
        &JsValue::from_str("device-url"),
    );
    let _result = js_sys::Reflect::set(&event, &JsValue::from_str("url"), &JsValue::from_str(url));
    let _result = worker.post_message(&event);
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
async fn transmit_task(
    socket: WebSocket,
    link: embassy_net_driver_channel::StateRunner<'static>,
    mut runner: TxRunner<'static, MTU>,
) {
    loop {
        let packet = runner.tx_buf().await;
        if send_packet(&socket, packet).is_err() {
            runner.tx_done();
            link.set_link_state(LinkState::Down);
            return;
        }
        runner.tx_done();
    }
}
