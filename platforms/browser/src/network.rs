//! In-page virtual network used by the Browser Platform.

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{
    tcp::TcpSocket, Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
};
use embassy_net_driver_channel::{
    driver::{HardwareAddress, LinkState},
    Device, RxRunner, State, TxRunner,
};

const MTU: usize = 1500;
const RX_PACKETS: usize = 16;
const TX_PACKETS: usize = 16;
const SOCKETS: usize = 32;
const PORTAL_PORT: u16 = 8787;
const PORTAL_TCP_BUFFER: usize = 8 * 1024;
const WEBSOCKET_BUFFER: usize = 16 * 1024;

/// Address assigned to the System inside the page-local virtual network.
pub(crate) const GUEST_ADDRESS: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);

/// Creates the Embassy IP stack used by the complete Browser System.
///
/// The driver loops traffic addressed to the System back into the same stack,
/// allowing the page bridge to reach the WebServer without a native gateway.
/// The mechanism stays inside this Platform and does not change the System or
/// Plugin network contracts.
pub async fn create_stack(spawner: Spawner) -> Result<Stack<'static>, String> {
    let state = Box::leak(Box::new(State::<MTU, RX_PACKETS, TX_PACKETS>::new()));
    let (runner, device): (_, Device<'static, MTU>) =
        embassy_net_driver_channel::new(state, HardwareAddress::Ip);
    let (link, receive, transmit) = runner.split();

    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(GUEST_ADDRESS, 24),
        gateway: None,
        dns_servers: Default::default(),
    });
    let resources = Box::leak(Box::new(StackResources::<SOCKETS>::new()));
    let (stack, network_runner) = embassy_net::new(device, config, resources, random_seed()?);
    spawner
        .spawn(network_task(network_runner))
        .map_err(|error| error.to_string())?;
    spawner
        .spawn(loopback_task(receive, transmit))
        .map_err(|error| error.to_string())?;
    spawner
        .spawn(portal_task(stack))
        .map_err(|error| error.to_string())?;
    spawner
        .spawn(websocket_bridge_task(stack))
        .map_err(|error| error.to_string())?;
    link.set_link_state(LinkState::Up);
    Ok(stack)
}

#[embassy_executor::task]
async fn websocket_bridge_task(stack: Stack<'static>) -> ! {
    loop {
        match crate::ffi::take_socket_event() {
            Ok(Some(crate::ffi::SocketEvent::Open { id })) => {
                if let Err(error) = run_websocket(stack, id).await {
                    crate::ffi::report_error(&format!("page WebSocket failed: {error}"));
                }
                let _result = crate::ffi::report_socket_closed(id);
            }
            Ok(Some(_event)) => {}
            Ok(None) => embassy_time::Timer::after_millis(5).await,
            Err(error) => {
                crate::ffi::report_error(&error.to_string());
                embassy_time::Timer::after_millis(50).await;
            }
        }
    }
}

async fn run_websocket(stack: Stack<'static>, id: u32) -> Result<(), String> {
    let mut receive_buffer = [0_u8; WEBSOCKET_BUFFER];
    let mut transmit_buffer = [0_u8; WEBSOCKET_BUFFER];
    let mut socket = TcpSocket::new(stack, &mut receive_buffer, &mut transmit_buffer);
    socket
        .connect((GUEST_ADDRESS, PORTAL_PORT))
        .await
        .map_err(|error| format!("connect failed: {error:?}"))?;
    let (mut reader, mut writer) = socket.split();
    write_all(
        &mut writer,
        b"GET / HTTP/1.1\r\nHost: browser.barracuda\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .await?;
    writer
        .flush()
        .await
        .map_err(|error| format!("handshake flush failed: {error:?}"))?;

    let mut incoming = Vec::new();
    let mut chunk = [0_u8; 4 * 1024];
    while !incoming.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|error| format!("handshake read failed: {error:?}"))?;
        if count == 0 {
            return Err(String::from("WebServer closed during WebSocket handshake"));
        }
        incoming.extend_from_slice(&chunk[..count]);
        if incoming.len() > 16 * 1024 {
            return Err(String::from("WebSocket handshake is too large"));
        }
    }
    let header_end = incoming
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|offset| offset + 4)
        .ok_or_else(|| String::from("WebSocket handshake has no header terminator"))?;
    if !incoming.starts_with(b"HTTP/1.1 101") && !incoming.starts_with(b"HTTP/1.0 101") {
        return Err(String::from_utf8_lossy(&incoming[..header_end]).into_owned());
    }
    incoming.drain(..header_end);
    crate::ffi::report_socket_opened(id).map_err(|error| error.to_string())?;

    let mut fragmented_opcode = None;
    let mut fragmented = Vec::new();
    loop {
        while let Some(frame) = take_websocket_frame(&mut incoming)? {
            match frame.opcode {
                0 => {
                    fragmented.extend_from_slice(&frame.payload);
                    if frame.finished {
                        let opcode = fragmented_opcode.take().unwrap_or(2);
                        crate::ffi::report_socket_message(id, &fragmented, opcode == 1)
                            .map_err(|error| error.to_string())?;
                        fragmented.clear();
                    }
                }
                1 | 2 if frame.finished => {
                    crate::ffi::report_socket_message(id, &frame.payload, frame.opcode == 1)
                        .map_err(|error| error.to_string())?;
                }
                1 | 2 => {
                    fragmented_opcode = Some(frame.opcode);
                    fragmented = frame.payload;
                }
                8 => return Ok(()),
                9 => write_websocket_frame(&mut writer, 10, &frame.payload).await?,
                _ => {}
            }
        }

        match select(
            reader.read(&mut chunk),
            embassy_time::Timer::after_millis(5),
        )
        .await
        {
            Either::First(result) => {
                let count = result.map_err(|error| format!("socket read failed: {error:?}"))?;
                if count == 0 {
                    return Ok(());
                }
                incoming.extend_from_slice(&chunk[..count]);
            }
            Either::Second(()) => match crate::ffi::take_socket_event() {
                Ok(Some(crate::ffi::SocketEvent::Send {
                    id: event_id,
                    payload,
                })) if event_id == id => write_websocket_frame(&mut writer, 1, &payload).await?,
                Ok(Some(crate::ffi::SocketEvent::Close { id: event_id })) if event_id == id => {
                    write_websocket_frame(&mut writer, 8, &[]).await?;
                    return Ok(());
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => return Err(error.to_string()),
            },
        }
    }
}

struct WebSocketFrame {
    finished: bool,
    opcode: u8,
    payload: Vec<u8>,
}

fn take_websocket_frame(bytes: &mut Vec<u8>) -> Result<Option<WebSocketFrame>, String> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let finished = bytes[0] & 0x80 != 0;
    let opcode = bytes[0] & 0x0f;
    let masked = bytes[1] & 0x80 != 0;
    let short_length = bytes[1] & 0x7f;
    let (length, mut offset) = match short_length {
        126 if bytes.len() >= 4 => (usize::from(u16::from_be_bytes([bytes[2], bytes[3]])), 4),
        127 if bytes.len() >= 10 => {
            let raw = u64::from_be_bytes(bytes[2..10].try_into().map_err(|_error| "length")?);
            let length = usize::try_from(raw)
                .map_err(|_error| String::from("WebSocket frame is too large"))?;
            (length, 10)
        }
        126 | 127 => return Ok(None),
        value => (usize::from(value), 2),
    };
    let mask = if masked {
        if bytes.len() < offset + 4 {
            return Ok(None);
        }
        let value: [u8; 4] = bytes[offset..offset + 4]
            .try_into()
            .map_err(|_error| String::from("invalid WebSocket mask"))?;
        offset += 4;
        Some(value)
    } else {
        None
    };
    let end = offset
        .checked_add(length)
        .ok_or_else(|| String::from("WebSocket frame length overflow"))?;
    if bytes.len() < end {
        return Ok(None);
    }
    let mut payload = bytes[offset..end].to_vec();
    if let Some(mask) = mask {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    bytes.drain(..end);
    Ok(Some(WebSocketFrame {
        finished,
        opcode,
        payload,
    }))
}

async fn write_websocket_frame(
    writer: &mut embassy_net::tcp::TcpWriter<'_>,
    opcode: u8,
    payload: &[u8],
) -> Result<(), String> {
    let mut header = Vec::with_capacity(14);
    header.push(0x80 | opcode);
    let length = payload.len();
    if length < 126 {
        header.push(0x80 | length as u8);
    } else if let Ok(length) = u16::try_from(length) {
        header.push(0x80 | 126);
        header.extend_from_slice(&length.to_be_bytes());
    } else {
        header.push(0x80 | 127);
        header.extend_from_slice(&(length as u64).to_be_bytes());
    }
    let mut mask = [0; 4];
    crate::ffi::fill_random(&mut mask).map_err(|error| error.to_string())?;
    header.extend_from_slice(&mask);
    write_all(writer, &header).await?;
    let mut masked = payload.to_vec();
    for (index, byte) in masked.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    write_all(writer, &masked).await
}

async fn write_all(
    writer: &mut embassy_net::tcp::TcpWriter<'_>,
    bytes: &[u8],
) -> Result<(), String> {
    let mut written = 0;
    while written < bytes.len() {
        let count = writer
            .write(&bytes[written..])
            .await
            .map_err(|error| format!("socket write failed: {error:?}"))?;
        if count == 0 {
            return Err(String::from("socket write made no progress"));
        }
        written += count;
    }
    Ok(())
}

#[embassy_executor::task]
async fn portal_task(stack: Stack<'static>) -> ! {
    loop {
        match crate::ffi::take_portal_request() {
            Ok(Some(request)) => forward_portal_request(stack, &request).await,
            Ok(None) => embassy_time::Timer::after_millis(5).await,
            Err(error) => {
                crate::ffi::report_error(&error.to_string());
                embassy_time::Timer::after_millis(50).await;
            }
        }
    }
}

async fn forward_portal_request(stack: Stack<'static>, request: &[u8]) {
    if let Err(error) = try_forward_portal_request(stack, request).await {
        let message = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\nBrowser Platform could not reach the System WebServer: {error}"
        );
        let _result = crate::ffi::write_portal_response(message.as_bytes());
    }
    if let Err(error) = crate::ffi::finish_portal_response() {
        crate::ffi::report_error(&error.to_string());
    }
}

async fn try_forward_portal_request(stack: Stack<'static>, request: &[u8]) -> Result<(), String> {
    let mut receive_buffer = [0_u8; PORTAL_TCP_BUFFER];
    let mut transmit_buffer = [0_u8; PORTAL_TCP_BUFFER];
    let mut socket = TcpSocket::new(stack, &mut receive_buffer, &mut transmit_buffer);
    socket
        .connect((GUEST_ADDRESS, PORTAL_PORT))
        .await
        .map_err(|error| format!("connect failed: {error:?}"))?;
    let (mut reader, mut writer) = socket.split();
    let mut written = 0;
    while written < request.len() {
        let count = writer
            .write(&request[written..])
            .await
            .map_err(|error| format!("request write failed: {error:?}"))?;
        if count == 0 {
            return Err(String::from("request write made no progress"));
        }
        written += count;
    }
    writer
        .flush()
        .await
        .map_err(|error| format!("request flush failed: {error:?}"))?;

    let mut chunk = [0_u8; 4 * 1024];
    loop {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|error| format!("response read failed: {error:?}"))?;
        if count == 0 {
            return Ok(());
        }
        crate::ffi::write_portal_response(&chunk[..count]).map_err(|error| error.to_string())?;
    }
}

fn random_seed() -> Result<u64, String> {
    let mut bytes = [0; 8];
    crate::ffi::fill_random(&mut bytes).map_err(|error| error.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}

fn destination(packet: &[u8]) -> Option<[u8; 4]> {
    if packet.first().map(|byte| byte >> 4) != Some(4) {
        return None;
    }
    packet.get(16..20)?.try_into().ok()
}

type BrowserDevice = Device<'static, MTU>;

#[embassy_executor::task]
async fn network_task(mut runner: Runner<'static, BrowserDevice>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn loopback_task(
    mut receive: RxRunner<'static, MTU>,
    mut transmit: TxRunner<'static, MTU>,
) -> ! {
    loop {
        let packet = transmit.tx_buf().await;
        if destination(packet) == Some(GUEST_ADDRESS.octets()) {
            let target = receive.rx_buf().await;
            target[..packet.len()].copy_from_slice(packet);
            receive.rx_done(packet.len());
        }
        transmit.tx_done();
    }
}

#[cfg(test)]
mod tests {
    use super::{destination, GUEST_ADDRESS};

    #[test]
    fn reads_ipv4_destination() {
        let mut packet = [0_u8; 20];
        packet[0] = 0x45;
        packet[16..20].copy_from_slice(&GUEST_ADDRESS.octets());
        assert_eq!(destination(&packet), Some(GUEST_ADDRESS.octets()));
        packet[0] = 0x60;
        assert_eq!(destination(&packet), None);
    }
}
