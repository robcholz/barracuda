#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use barracuda_e2e::resources;
use barracuda_event_router::RpcLaneStorage;
use barracuda_system::System;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{select, Either};
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Address, Stack};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embedded_io_async::{Read as _, Write as _};
use serde::Deserialize;
use serde_json::Value;

const COMPONENTS: usize = 32;
const RPC_MESSAGE_BYTES: usize = 512;
const RPC_QUEUE_DEPTH: usize = 8;
const WEB_PORT: u16 = 8787;
const MODEL_PORT: u16 = 8000;
const HTTP_FIXTURE_PORT: u16 = 9000;
const TAVILY_FIXTURE_PORT: u16 = 9001;
const TELEGRAM_FIXTURE_PORT: u16 = 9002;
const WECHAT_FIXTURE_PORT: u16 = 9003;
const BLUEBUBBLES_FIXTURE_PORT: u16 = 9004;
const QQ_FIXTURE_PORT: u16 = 9005;
const INKBOX_FIXTURE_PORT: u16 = 9006;
const FIXTURE_PORTS: [u16; 7] = [
    HTTP_FIXTURE_PORT,
    TAVILY_FIXTURE_PORT,
    TELEGRAM_FIXTURE_PORT,
    WECHAT_FIXTURE_PORT,
    BLUEBUBBLES_FIXTURE_PORT,
    QQ_FIXTURE_PORT,
    INKBOX_FIXTURE_PORT,
];
const FIXED_UNIX_SECONDS: u64 = 1_800_001_800;
const USER_INPUT: &str = "barracuda deterministic e2e input";
const EXPECTED_REPLY_FRAGMENT: &str = "AGENT_DYNAMIC_RPC_E2E_OK";
const PRIMARY_TAPE: &str = include_str!("../tapes/agent-dynamic-rpc.jsonl");

#[derive(Deserialize)]
struct TapeEvent {
    kind: String,
    #[serde(default)]
    interaction_id: Option<String>,
    #[serde(default)]
    call_index: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    at_us: Option<u64>,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    data_b64: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
}

struct TimedChunk {
    sequence: u64,
    at_us: u64,
    bytes: Vec<u8>,
}

struct ReplayInteraction {
    call_index: u64,
    method: String,
    path: String,
    response_at_us: u64,
    status: u16,
    reason: String,
    chunks: Vec<TimedChunk>,
    end_at_us: u64,
}

fn recorded_interactions(tape: &str) -> Result<Vec<ReplayInteraction>, String> {
    let events = tape
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<TapeEvent>(line).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut requests = events
        .iter()
        .filter(|event| event.kind == "request")
        .collect::<Vec<_>>();
    requests.sort_by_key(|event| event.call_index);
    requests
        .into_iter()
        .map(|request| recorded_interaction(&events, request))
        .collect()
}

fn recorded_interaction(
    events: &[TapeEvent],
    request: &TapeEvent,
) -> Result<ReplayInteraction, String> {
    let interaction_id = request
        .interaction_id
        .clone()
        .ok_or_else(|| "recorded request has no interaction id".to_string())?;
    let response = events
        .iter()
        .find(|event| {
            event.kind == "response_start"
                && event.interaction_id.as_deref() == Some(interaction_id.as_str())
        })
        .ok_or_else(|| "recorded interaction has no response start".to_string())?;
    let end = events
        .iter()
        .find(|event| {
            event.kind == "response_end"
                && event.interaction_id.as_deref() == Some(interaction_id.as_str())
        })
        .ok_or_else(|| "recorded interaction has no response end".to_string())?;
    if end.outcome.as_deref() != Some("eof") {
        return Err("recorded interaction did not end at EOF".into());
    }

    let mut chunks = events
        .iter()
        .filter(|event| {
            event.kind == "response_chunk"
                && event.interaction_id.as_deref() == Some(interaction_id.as_str())
        })
        .map(|event| {
            let encoded = event
                .data_b64
                .as_deref()
                .ok_or_else(|| "recorded response chunk has no data".to_string())?;
            Ok(TimedChunk {
                sequence: event
                    .seq
                    .ok_or_else(|| "recorded response chunk has no sequence".to_string())?,
                at_us: event
                    .at_us
                    .ok_or_else(|| "recorded response chunk has no timestamp".to_string())?,
                bytes: STANDARD
                    .decode(encoded)
                    .map_err(|error| error.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    chunks.sort_by_key(|chunk| chunk.sequence);
    for (expected, chunk) in chunks.iter().enumerate() {
        let expected = u64::try_from(expected).map_err(|error| error.to_string())?;
        if chunk.sequence != expected {
            return Err(format!(
                "recorded response chunk sequence jumped from {expected} to {}",
                chunk.sequence
            ));
        }
    }

    Ok(ReplayInteraction {
        call_index: request
            .call_index
            .ok_or_else(|| "recorded request has no call index".to_string())?,
        method: request
            .method
            .clone()
            .ok_or_else(|| "recorded request has no method".to_string())?,
        path: request
            .path
            .clone()
            .ok_or_else(|| "recorded request has no path".to_string())?,
        response_at_us: response
            .at_us
            .ok_or_else(|| "recorded response has no timestamp".to_string())?,
        status: response
            .status
            .ok_or_else(|| "recorded response has no status".to_string())?,
        reason: response.reason.clone().unwrap_or_else(|| "OK".into()),
        chunks,
        end_at_us: end
            .at_us
            .ok_or_else(|| "recorded response end has no timestamp".to_string())?,
    })
}

#[embassy_executor::task]
async fn replay_model(
    stack: Stack<'static>,
    interactions: &'static [ReplayInteraction],
    prompt_seen: Arc<AtomicBool>,
) {
    for interaction in interactions {
        if let Err(error) = serve_recorded_interaction(stack, interaction, &prompt_seen).await {
            panic!("LLM tape replay failed: {error}");
        }
    }
}

async fn serve_recorded_interaction(
    stack: Stack<'static>,
    interaction: &ReplayInteraction,
    prompt_seen: &AtomicBool,
) -> Result<(), String> {
    let mut rx = vec![0_u8; 64 * 1024];
    let mut tx = vec![0_u8; 8 * 1024];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket
        .accept(MODEL_PORT)
        .await
        .map_err(|error| format!("accept model request: {error:?}"))?;
    let request = read_http_request(&mut socket).await?;
    let expected_line = format!("{} {} HTTP/1.1", interaction.method, interaction.path);
    if !request.starts_with(expected_line.as_bytes()) {
        return Err(format!(
            "request mismatch: expected {expected_line}, got {}",
            String::from_utf8_lossy(
                request
                    .split(|byte| *byte == b'\n')
                    .next()
                    .unwrap_or(&request)
            )
        ));
    }
    if request
        .windows(USER_INPUT.len())
        .any(|window| window == USER_INPUT.as_bytes())
    {
        prompt_seen.store(true, Ordering::SeqCst);
    }
    assert_tool_results(interaction.call_index, &request)?;

    let content_length = interaction
        .chunks
        .iter()
        .try_fold(0_usize, |total, chunk| total.checked_add(chunk.bytes.len()))
        .ok_or_else(|| "recorded response length overflowed usize".to_string())?;
    let started = Instant::now();
    wait_until(started, interaction.response_at_us).await;
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/event-stream; charset=utf-8\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n",
        interaction.status, interaction.reason
    );
    socket
        .write_all(head.as_bytes())
        .await
        .map_err(|error| format!("write model response head: {error:?}"))?;
    for chunk in &interaction.chunks {
        wait_until(started, chunk.at_us).await;
        socket
            .write_all(&chunk.bytes)
            .await
            .map_err(|error| format!("write model response chunk: {error:?}"))?;
    }
    wait_until(started, interaction.end_at_us).await;
    socket
        .flush()
        .await
        .map_err(|error| format!("flush model response: {error:?}"))?;
    Ok(())
}

fn assert_tool_results(call_index: u64, request: &[u8]) -> Result<(), String> {
    let body_start = find_bytes(request, b"\r\n\r\n")
        .and_then(|end| end.checked_add(4))
        .ok_or_else(|| "model request had no complete HTTP headers".to_string())?;
    let body: Value = serde_json::from_slice(
        request
            .get(body_start..)
            .ok_or_else(|| "model request body offset was invalid".to_string())?,
    )
    .map_err(|error| format!("parse model request body: {error}"))?;
    let expectations: &[(&str, &str)] = match call_index {
        0 => return assert_discovery_tools(&body),
        1 => &[
            ("call-load-http", r#""loaded":true"#),
            ("call-load-time", r#""loaded":true"#),
        ],
        2 => &[
            ("call-http", "http-plugin-ok"),
            ("call-time", r#""ok":true"#),
        ],
        other => return Err(format!("unexpected replay call index {other}")),
    };
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "model request has no messages array".to_string())?;
    for (tool_call_id, marker) in expectations {
        let content = messages
            .iter()
            .rev()
            .find(|message| {
                message.get("tool_call_id").and_then(Value::as_str) == Some(*tool_call_id)
            })
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("model request omitted result for {tool_call_id}"))?;
        if !content.contains(marker) {
            return Err(format!(
                "result for {tool_call_id} did not contain {marker:?}: {content}"
            ));
        }
    }
    if call_index == 1 {
        assert_dynamic_rpc_tools(&body)?;
    }
    Ok(())
}

fn assert_discovery_tools(body: &Value) -> Result<(), String> {
    let names = tool_names(body)?;
    for expected in ["tool_search", "tool_load"] {
        if !names.contains(&expected) {
            return Err(format!("Agent discovery tool {expected} was not visible"));
        }
    }
    for hidden in ["rpc_4_http_request", "rpc_4_time_now"] {
        if names.contains(&hidden) {
            return Err(format!("unloaded Event Router tool {hidden} was visible"));
        }
    }
    Ok(())
}

fn assert_dynamic_rpc_tools(body: &Value) -> Result<(), String> {
    let names = tool_names(body)?;
    let deferred = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        })
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| "model request has no system prompt".to_string())?;
    for expected in ["rpc_4_http_request", "rpc_4_time_now"] {
        if !deferred.contains(expected) {
            return Err(format!(
                "loaded dynamic RPC tool {expected} was absent from deferred tool context"
            ));
        }
        if names.contains(&expected) {
            return Err(format!(
                "loaded dynamic RPC tool {expected} leaked into the default tool surface"
            ));
        }
    }
    for hidden in [
        "barracuda_rpc",
        "barracuda_search",
        "barracuda_lua",
        "barracuda_gateway_media",
        "file_read",
        "gateway_send",
        "scheduler_schedule",
        "vm_run",
        "rpc_10_web_search_search",
    ] {
        if names.contains(&hidden) || deferred.contains(hidden) {
            return Err(format!("non-qualifying Agent tool {hidden} was visible"));
        }
    }
    if !deferred.contains(r#""url""#) {
        return Err("rpc_4_http_request did not carry its baked request schema".into());
    }
    Ok(())
}

fn tool_names(body: &Value) -> Result<Vec<&str>, String> {
    Ok(body
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "model request has no tools array".to_string())?
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect())
}

async fn wait_until(started: Instant, at_us: u64) {
    Timer::at(started.saturating_add(Duration::from_micros(at_us))).await;
}

async fn read_http_request(socket: &mut TcpSocket<'_>) -> Result<Vec<u8>, String> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|error| format!("read HTTP request: {error:?}"))?;
        if count == 0 {
            return Err("HTTP request closed before its body completed".into());
        }
        request.extend_from_slice(&chunk[..count]);
        let Some(head_end) = find_bytes(&request, b"\r\n\r\n") else {
            continue;
        };
        let body_start = head_end
            .checked_add(4)
            .ok_or_else(|| "HTTP header length overflowed".to_string())?;
        let content_length = content_length(&request[..head_end])?;
        let complete = body_start
            .checked_add(content_length)
            .ok_or_else(|| "HTTP body length overflowed".to_string())?;
        if request.len() >= complete {
            request.truncate(complete);
            return Ok(request);
        }
    }
}

fn content_length(head: &[u8]) -> Result<usize, String> {
    let text = String::from_utf8_lossy(head);
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .transpose()
        .map_err(|error| format!("invalid Content-Length: {error}"))?
        .ok_or_else(|| "HTTP request has no Content-Length".to_string())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[embassy_executor::task(pool_size = 2)]
async fn dns_fixture(stack: Stack<'static>) {
    if let Err(error) = serve_dns(stack).await {
        panic!("DNS fixture failed: {error}");
    }
}

async fn serve_dns(stack: Stack<'static>) -> Result<(), String> {
    let mut rx_metadata = [PacketMetadata::EMPTY; 2];
    let mut tx_metadata = [PacketMetadata::EMPTY; 2];
    let mut rx_buffer = [0_u8; 1024];
    let mut tx_buffer = [0_u8; 1024];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_metadata,
        &mut rx_buffer,
        &mut tx_metadata,
        &mut tx_buffer,
    );
    socket
        .bind(53)
        .map_err(|error| format!("bind DNS fixture: {error:?}"))?;
    let mut request = [0_u8; 512];
    let mut response = [0_u8; 512];
    loop {
        let (length, remote) = socket
            .recv_from(&mut request)
            .await
            .map_err(|error| format!("receive DNS query: {error:?}"))?;
        let response_length = dns_response(&request[..length], &mut response)?;
        socket
            .send_to(&response[..response_length], remote.endpoint)
            .await
            .map_err(|error| format!("send DNS response: {error:?}"))?;
    }
}

fn dns_response(request: &[u8], response: &mut [u8]) -> Result<usize, String> {
    if request.len() < 17 {
        return Err("DNS query was too short".into());
    }
    let mut cursor = 12_usize;
    loop {
        let length = usize::from(*request.get(cursor).ok_or("DNS name was truncated")?);
        cursor = cursor
            .checked_add(1)
            .ok_or_else(|| "DNS cursor overflowed".to_string())?;
        if length == 0 {
            break;
        }
        cursor = cursor
            .checked_add(length)
            .filter(|cursor| *cursor < request.len())
            .ok_or_else(|| "DNS label was truncated".to_string())?;
    }
    let question_end = cursor
        .checked_add(4)
        .filter(|end| *end <= request.len())
        .ok_or_else(|| "DNS question was truncated".to_string())?;
    let response_length = question_end
        .checked_add(16)
        .filter(|end| *end <= response.len())
        .ok_or_else(|| "DNS response buffer was too small".to_string())?;
    response[..question_end].copy_from_slice(&request[..question_end]);
    response[2..12].copy_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
    response[question_end..response_length]
        .copy_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 10, 0, 0, 1]);
    Ok(response_length)
}

#[embassy_executor::task(pool_size = 2)]
async fn sntp_fixture(stack: Stack<'static>) {
    if let Err(error) = serve_sntp(stack).await {
        panic!("SNTP fixture failed: {error}");
    }
}

async fn serve_sntp(stack: Stack<'static>) -> Result<(), String> {
    let mut rx_metadata = [PacketMetadata::EMPTY; 2];
    let mut tx_metadata = [PacketMetadata::EMPTY; 2];
    let mut rx_buffer = [0_u8; 256];
    let mut tx_buffer = [0_u8; 256];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_metadata,
        &mut rx_buffer,
        &mut tx_metadata,
        &mut tx_buffer,
    );
    socket
        .bind(123)
        .map_err(|error| format!("bind SNTP fixture: {error:?}"))?;
    let mut request = [0_u8; 64];
    loop {
        let (length, remote) = socket
            .recv_from(&mut request)
            .await
            .map_err(|error| format!("receive SNTP request: {error:?}"))?;
        if length < 48 {
            continue;
        }
        let mut response = [0_u8; 48];
        response[0] = 0x24;
        response[1] = 1;
        response[2] = 6;
        response[3] = 0xec;
        response[12..16].copy_from_slice(b"GPS\0");
        response[24..32].copy_from_slice(&request[40..48]);
        let ntp_seconds = FIXED_UNIX_SECONDS
            .checked_add(2_208_988_800)
            .ok_or_else(|| "NTP timestamp overflowed".to_string())?;
        let ntp_seconds = u32::try_from(ntp_seconds)
            .map_err(|error| format!("NTP timestamp did not fit: {error}"))?;
        response[32..36].copy_from_slice(&ntp_seconds.to_be_bytes());
        response[40..44].copy_from_slice(&ntp_seconds.to_be_bytes());
        socket
            .send_to(&response, remote.endpoint)
            .await
            .map_err(|error| format!("send SNTP response: {error:?}"))?;
    }
}

#[embassy_executor::task(pool_size = 14)]
async fn http_fixture(stack: Stack<'static>, port: u16) {
    if let Err(error) = serve_http_fixture(stack, port).await {
        panic!("HTTP fixture failed: {error}");
    }
}

async fn serve_http_fixture(stack: Stack<'static>, port: u16) -> Result<(), String> {
    loop {
        let mut rx = vec![0_u8; 16 * 1024];
        let mut tx = vec![0_u8; 8 * 1024];
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket
            .accept(port)
            .await
            .map_err(|error| format!("accept fixture request: {error:?}"))?;
        let request = read_http_request(&mut socket).await?;
        let first_line = request
            .split(|byte| *byte == b'\n')
            .next()
            .ok_or_else(|| "HTTP fixture request had no request line".to_string())?;
        let (status, content_type, body) = fixture_response(&String::from_utf8_lossy(first_line));
        let head = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(head.as_bytes())
            .await
            .map_err(|error| format!("write fixture response head: {error:?}"))?;
        socket
            .write_all(body.as_bytes())
            .await
            .map_err(|error| format!("write fixture response body: {error:?}"))?;
        socket
            .flush()
            .await
            .map_err(|error| format!("flush fixture response: {error:?}"))?;
    }
}

fn fixture_response(request_line: &str) -> (u16, &'static str, &'static str) {
    if request_line.contains(" /search ") {
        return (
            200,
            "application/json",
            r#"{"results":[{"title":"Barracuda","url":"https://example.test/result","content":"deterministic search result","score":0.99}]}"#,
        );
    }
    if request_line.contains("/sendMessage ") {
        return (
            200,
            "application/json",
            r#"{"ok":true,"result":{"message_id":7}}"#,
        );
    }
    if request_line.contains("/ilink/bot/sendmessage ") {
        return (200, "application/json", r#"{"ret":0}"#);
    }
    if request_line.contains("/api/v1/message/text?") {
        return (
            200,
            "application/json",
            r#"{"status":200,"data":{"guid":"bb-e2e"}}"#,
        );
    }
    if request_line.contains("/v2/users/") {
        return (200, "application/json", r#"{"id":"qq-e2e"}"#);
    }
    if request_line.contains("/api/v1/imessage/messages?") {
        return (200, "application/json", r#"{"message":{"id":"ink-e2e"}}"#);
    }
    if request_line.contains(" /echo ") {
        return (201, "text/plain", "http-plugin-ok");
    }
    (
        404,
        "application/json",
        r#"{"error":"unexpected_fixture_request"}"#,
    )
}

#[embassy_executor::task]
async fn e2e_task(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = run_e2e(spawner).await;
    let _ignored = completed.send(result);
}

async fn run_e2e(spawner: Spawner) -> Result<(), String> {
    let interactions = Box::leak(recorded_interactions(PRIMARY_TAPE)?.into_boxed_slice());
    let prompt_seen = Arc::new(AtomicBool::new(false));
    let target = resources(spawner)
        .await
        .map_err(|error| format!("construct e2e target: {error}"))?;
    let stack = target.platform.ip_stack;
    spawner
        .spawn(dns_fixture(stack))
        .map_err(|error| format!("spawn DNS fixture: {error}"))?;
    spawner
        .spawn(sntp_fixture(stack))
        .map_err(|error| format!("spawn SNTP fixture: {error}"))?;
    for port in FIXTURE_PORTS {
        spawner
            .spawn(http_fixture(stack, port))
            .map_err(|error| format!("spawn HTTP fixture on port {port}: {error}"))?;
    }
    spawner
        .spawn(replay_model(stack, interactions, Arc::clone(&prompt_seen)))
        .map_err(|error| format!("spawn replay server: {error}"))?;

    let lanes = Box::leak(Box::new(RpcLaneStorage::<
        COMPONENTS,
        RPC_MESSAGE_BYTES,
        RPC_QUEUE_DEPTH,
    >::new()));
    let mut system = System::new(lanes, target, spawner)
        .await
        .map_err(|error| format!("construct System: {error}"))?;
    let scenario = with_timeout(Duration::from_secs(20), web_gateway_scenario(stack));

    let scenario_result = match select(&mut system, scenario).await {
        Either::First(system_result) => {
            return Err(format!(
                "System stopped before the e2e scenario completed: {system_result:?}"
            ));
        }
        Either::Second(Ok(result)) => result,
        Either::Second(Err(_timeout)) => Err("e2e scenario timed out".into()),
    };
    scenario_result?;
    if !prompt_seen.load(Ordering::SeqCst) {
        return Err("the recorded model replay did not observe the injected Web input".into());
    }

    with_timeout(Duration::from_secs(5), system.shutdown())
        .await
        .map_err(|_timeout| "System shutdown timed out".to_string())?
        .map_err(|error| format!("System shutdown failed: {error}"))?;
    barracuda_vfs::unmount("/")
        .await
        .map_err(|error| format!("unmount e2e System filesystem: {error}"))?;
    Ok(())
}

async fn web_gateway_scenario(stack: Stack<'static>) -> Result<(), String> {
    configure_model_api(stack).await?;
    configure_plugin_routes(stack, false).await?;
    let mut rx = [0_u8; 8192];
    let mut tx = [0_u8; 8192];
    let mut socket = connect_with_retry(stack, WEB_PORT, &mut rx, &mut tx).await?;
    websocket_handshake(&mut socket).await?;
    let input = serde_json::json!({ "text": USER_INPUT, "reply_to": null }).to_string();
    send_websocket_text(&mut socket, input.as_bytes()).await?;

    let mut reply = String::new();
    loop {
        let (opcode, payload) = read_websocket_frame(&mut socket).await?;
        match opcode {
            0x1 => {
                let frame = String::from_utf8(payload)
                    .map_err(|error| format!("WebSocket text was not UTF-8: {error}"))?;
                let fields = parse_sse(&frame);
                match fields.get("event").map(String::as_str) {
                    Some("message.delta") => {
                        let data = fields
                            .get("data")
                            .ok_or_else(|| "message.delta has no data".to_string())?;
                        let value: Value = serde_json::from_str(data)
                            .map_err(|error| format!("parse message.delta: {error}"))?;
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            reply.push_str(delta);
                        }
                    }
                    Some("message.end") => {
                        let data = fields
                            .get("data")
                            .ok_or_else(|| "message.end has no data".to_string())?;
                        let value: Value = serde_json::from_str(data)
                            .map_err(|error| format!("parse message.end: {error}"))?;
                        if let Some(error) = value.get("error").and_then(Value::as_str) {
                            return Err(format!("gateway ended the reply with an error: {error}"));
                        }
                        if !reply.contains(EXPECTED_REPLY_FRAGMENT) {
                            return Err(format!("unexpected recorded model reply: {reply}"));
                        }
                        return Ok(());
                    }
                    _ => {}
                }
            }
            0x8 => return Err("WebSocket closed before message.end".into()),
            _ => {}
        }
    }
}

fn fixture_url(port: u16) -> String {
    format!("http://10.0.0.1:{port}")
}

async fn configure_plugin_routes(stack: Stack<'static>, inkbox: bool) -> Result<(), String> {
    if inkbox {
        return expect_http_status(
            stack,
            "POST",
            "/api/gateway/inkbox",
            &serde_json::json!({
                "api_key": "e2e-key",
                "identity_id": "identity",
                "api_base": fixture_url(INKBOX_FIXTURE_PORT),
            })
            .to_string(),
            204,
        )
        .await;
    }
    let configurations = [
        (
            "/api/gateway/telegram",
            serde_json::json!({ "token": "e2e-token", "api_base": fixture_url(TELEGRAM_FIXTURE_PORT) }),
        ),
        (
            "/api/gateway/wechat",
            serde_json::json!({ "token": "e2e-token", "api_base": fixture_url(WECHAT_FIXTURE_PORT) }),
        ),
        (
            "/api/gateway/bluebubbles",
            serde_json::json!({ "server_url": fixture_url(BLUEBUBBLES_FIXTURE_PORT), "password": "e2e-password" }),
        ),
        (
            "/api/gateway/qq",
            serde_json::json!({ "app_id": "app", "access_token": "e2e-token", "api_base": fixture_url(QQ_FIXTURE_PORT) }),
        ),
        (
            "/api/tavily",
            serde_json::json!({ "api_key": "tvly-e2e", "api_base": fixture_url(TAVILY_FIXTURE_PORT) }),
        ),
    ];
    for (path, body) in configurations {
        expect_http_status(stack, "POST", path, &body.to_string(), 204).await?;
    }
    Ok(())
}

async fn expect_http_status(
    stack: Stack<'static>,
    method: &str,
    path: &str,
    body: &str,
    expected: u16,
) -> Result<(), String> {
    let (status, response) = request_http(stack, method, path, body).await?;
    if status != expected {
        return Err(format!(
            "{method} {path} returned {status}, expected {expected}: {}",
            String::from_utf8_lossy(&response)
        ));
    }
    Ok(())
}

async fn request_http(
    stack: Stack<'static>,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(u16, Vec<u8>), String> {
    let mut rx = [0_u8; 8192];
    let mut tx = [0_u8; 8192];
    let mut socket = connect_with_retry(stack, WEB_PORT, &mut rx, &mut tx).await?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 10.0.0.1:{WEB_PORT}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write HTTP route request: {error:?}"))?;
    socket.close();
    let mut response = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|error| format!("read HTTP route response: {error:?}"))?;
        if count == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..count]);
    }
    let head_end = find_bytes(&response, b"\r\n\r\n")
        .ok_or_else(|| "HTTP route response had no complete headers".to_string())?;
    let head = String::from_utf8_lossy(&response[..head_end]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("HTTP route response had invalid status: {head}"))?;
    let body_start = head_end
        .checked_add(4)
        .ok_or_else(|| "HTTP response header length overflowed".to_string())?;
    let body = response
        .get(body_start..)
        .ok_or_else(|| "HTTP response body offset was invalid".to_string())?;
    Ok((status, body.to_vec()))
}

async fn configure_model_api(stack: Stack<'static>) -> Result<(), String> {
    let body = format!(
        "[{{\"timeout_ms\":30000,\"max_tokens\":4096,\"image_max_bytes\":1048576,\"backend\":\"openai_compatible\",\"purpose\":\"root_agent\",\"default\":true,\"api_key\":\"e2e-key\",\"model\":\"recorded-model\",\"base_url\":\"http://10.0.0.1:{MODEL_PORT}\"}}]"
    );
    let mut rx = [0_u8; 4096];
    let mut tx = [0_u8; 4096];
    let mut socket = connect_with_retry(stack, WEB_PORT, &mut rx, &mut tx).await?;
    let request = format!(
        "POST /api/model-api HTTP/1.1\r\nHost: 10.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write model configuration: {error:?}"))?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 256];
    while find_bytes(&response, b"\r\n\r\n").is_none() {
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|error| format!("read model configuration response: {error:?}"))?;
        if count == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..count]);
    }
    if !response.starts_with(b"HTTP/1.1 204 ") {
        return Err(format!(
            "model configuration failed: {}",
            String::from_utf8_lossy(&response)
        ));
    }
    Ok(())
}

async fn connect_with_retry<'a>(
    stack: Stack<'static>,
    port: u16,
    rx: &'a mut [u8],
    tx: &'a mut [u8],
) -> Result<TcpSocket<'a>, String> {
    let mut socket = TcpSocket::new(stack, rx, tx);
    for _attempt in 0..200 {
        match socket.connect((Ipv4Address::new(10, 0, 0, 1), port)).await {
            Ok(()) => return Ok(socket),
            Err(_error) => Timer::after_millis(10).await,
        }
    }
    Err(format!("TCP port {port} did not become ready"))
}

async fn websocket_handshake(socket: &mut TcpSocket<'_>) -> Result<(), String> {
    let request = concat!(
        "GET / HTTP/1.1\r\n",
        "Host: 10.0.0.1:8787\r\n",
        "Connection: Upgrade\r\n",
        "Upgrade: websocket\r\n",
        "Sec-WebSocket-Version: 13\r\n",
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
        "\r\n"
    );
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write WebSocket handshake: {error:?}"))?;
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while find_bytes(&response, b"\r\n\r\n").is_none() {
        socket
            .read_exact(&mut byte)
            .await
            .map_err(|error| format!("read WebSocket handshake: {error:?}"))?;
        response.push(byte[0]);
        if response.len() > 4096 {
            return Err("WebSocket handshake exceeded 4096 bytes".into());
        }
    }
    if !response.starts_with(b"HTTP/1.1 101 ") {
        return Err(format!(
            "WebSocket upgrade failed: {}",
            String::from_utf8_lossy(&response)
        ));
    }
    Ok(())
}

async fn send_websocket_text(socket: &mut TcpSocket<'_>, payload: &[u8]) -> Result<(), String> {
    if payload.len() >= 126 {
        return Err("test WebSocket payload exceeds the short-frame limit".into());
    }
    let length = u8::try_from(payload.len()).map_err(|error| error.to_string())?;
    let mask = [0x12_u8, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(
        payload
            .len()
            .checked_add(6)
            .ok_or_else(|| "WebSocket frame length overflowed".to_string())?,
    );
    frame.extend_from_slice(&[0x81, 0x80 | length]);
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .zip(mask.iter().cycle())
            .map(|(byte, mask_byte)| byte ^ mask_byte),
    );
    socket
        .write_all(&frame)
        .await
        .map_err(|error| format!("write WebSocket input: {error:?}"))
}

async fn read_websocket_frame(socket: &mut TcpSocket<'_>) -> Result<(u8, Vec<u8>), String> {
    let mut head = [0_u8; 2];
    socket
        .read_exact(&mut head)
        .await
        .map_err(|error| format!("read WebSocket frame head: {error:?}"))?;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    let short_length = head[1] & 0x7f;
    let length = match short_length {
        126 => {
            let mut extended = [0_u8; 2];
            socket
                .read_exact(&mut extended)
                .await
                .map_err(|error| format!("read WebSocket 16-bit length: {error:?}"))?;
            usize::from(u16::from_be_bytes(extended))
        }
        127 => {
            let mut extended = [0_u8; 8];
            socket
                .read_exact(&mut extended)
                .await
                .map_err(|error| format!("read WebSocket 64-bit length: {error:?}"))?;
            usize::try_from(u64::from_be_bytes(extended)).map_err(|error| error.to_string())?
        }
        value => usize::from(value),
    };
    let mut mask = [0_u8; 4];
    if masked {
        socket
            .read_exact(&mut mask)
            .await
            .map_err(|error| format!("read WebSocket mask: {error:?}"))?;
    }
    let mut payload = vec![0_u8; length];
    socket
        .read_exact(&mut payload)
        .await
        .map_err(|error| format!("read WebSocket payload: {error:?}"))?;
    if masked {
        for (byte, mask_byte) in payload.iter_mut().zip(mask.iter().cycle()) {
            *byte ^= mask_byte;
        }
    }
    Ok((opcode, payload))
}

fn parse_sse(frame: &str) -> BTreeMap<String, String> {
    frame
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.to_string(), value.trim_start().to_string()))
        })
        .collect()
}

#[test]
fn agent_discovers_and_calls_schema_baked_dynamic_rpcs_through_the_web_gateway() {
    let (completed, result) = sync_channel(1);
    std::thread::Builder::new()
        .name("barracuda-e2e".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(e2e_task(spawner, completed))
                    .expect("spawn Barracuda e2e task");
            });
        })
        .expect("spawn Barracuda e2e executor thread");

    result
        .recv_timeout(StdDuration::from_secs(30))
        .expect("Barracuda e2e task timed out")
        .expect("Barracuda e2e scenario failed");
}
