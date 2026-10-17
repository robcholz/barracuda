//! Line-delimited JSON control interface of the virtual peripherals manager.
//!
//! The protocol is documented in `docs/platforms/virtual-io.mdx`. The same port
//! answers HTTP from a browser: `GET /` serves a viewer of the display panels
//! that also touches the panel through a touch controller model.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    thread,
};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    device::{I2cDevice, RegisterDevice},
    hardware::{hex, unhex, FaultKind, HardwareError, VirtualHardware},
};

/// One control request, selected by its `op` field.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Pins,
    Pin {
        pin: String,
    },
    SetInput {
        pin: String,
        level: Option<bool>,
    },
    Buses,
    AddDevice {
        bus: String,
        address: u64,
        #[serde(default = "default_model")]
        model: String,
        data: Option<String>,
    },
    RemoveDevice {
        bus: String,
        address: u64,
    },
    ReadRegisters {
        bus: String,
        address: u64,
        #[serde(default)]
        offset: usize,
        length: usize,
    },
    WriteRegisters {
        bus: String,
        address: u64,
        #[serde(default)]
        offset: usize,
        data: String,
    },
    InjectFault {
        bus: String,
        address: Option<u64>,
        fault: FaultKind,
        #[serde(default = "default_once")]
        once: bool,
    },
    ClearFaults {
        bus: Option<String>,
    },
    Faults,
    Events {
        #[serde(default)]
        since: u64,
    },
    Violations,
    Reset,
    Displays,
    Display {
        bus: String,
    },
}

fn default_model() -> String {
    String::from("registers")
}

const fn default_once() -> bool {
    true
}

/// Device models that the control interface can attach by name.
pub const DEVICE_MODELS: &[&str] = &[
    "registers",
    "bq27220",
    "cst816t",
    "es7210",
    "es8311",
    "es8389",
    "ina226",
    "pi4ioe5v6408",
    "rx8130ce",
];

/// Creates a device model by its control-interface name.
#[must_use]
pub fn device_model(name: &str) -> Option<Box<dyn I2cDevice>> {
    match name {
        "registers" => Some(Box::new(RegisterDevice::new())),
        "bq27220" => Some(Box::new(crate::models::Bq27220Model::new())),
        "cst816t" => Some(Box::new(crate::models::Cst816tModel::new())),
        "es7210" => Some(Box::new(crate::models::everest::es7210())),
        "es8311" => Some(Box::new(crate::models::everest::es8311())),
        "es8389" => Some(Box::new(crate::models::everest::es8389())),
        "ina226" => Some(Box::new(crate::models::Ina226Model::new())),
        "pi4ioe5v6408" => Some(Box::new(crate::models::Pi4ioe5v6408Model::new())),
        "rx8130ce" => Some(Box::new(crate::models::Rx8130ceModel::new())),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
enum RequestError {
    #[error("invalid request: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Hardware(#[from] HardwareError),
    #[error("`data` must be an even number of hex digits")]
    Hex,
    #[error("unknown device model `{0}`; known models: {models}", models = DEVICE_MODELS.join(", "))]
    UnknownModel(String),
}

/// Answers one request line with one response line (without the newline).
#[must_use]
pub fn handle(hardware: &VirtualHardware, line: &str) -> String {
    let response = match serde_json::from_str::<Request>(line)
        .map_err(RequestError::from)
        .and_then(|request| execute(hardware, request))
    {
        Ok(Value::Object(mut fields)) => {
            fields.insert(String::from("ok"), Value::Bool(true));
            Value::Object(fields)
        }
        Ok(other) => json!({ "ok": true, "result": other }),
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    };
    response.to_string()
}

fn execute(hardware: &VirtualHardware, request: Request) -> Result<Value, RequestError> {
    Ok(match request {
        Request::Pins => json!({ "pins": hardware.pins() }),
        Request::Pin { pin } => json!({ "pin": hardware.pin(&pin)? }),
        Request::SetInput { pin, level } => json!({ "pin": hardware.drive(&pin, level)? }),
        Request::Buses => json!({ "buses": hardware.buses() }),
        Request::AddDevice {
            bus,
            address,
            model,
            data,
        } => {
            let data = data
                .map(|data| unhex(&data).ok_or(RequestError::Hex))
                .transpose()?;
            let mut device = device_model(&model).ok_or(RequestError::UnknownModel(model))?;
            if let Some(data) = data {
                device.poke(0, &data).map_err(HardwareError::from)?;
            }
            hardware.attach(&bus, address, device)?;
            json!({})
        }
        Request::RemoveDevice { bus, address } => {
            hardware.detach(&bus, address)?;
            json!({})
        }
        Request::ReadRegisters {
            bus,
            address,
            offset,
            length,
        } => json!({ "data": hex(&hardware.read_registers(&bus, address, offset, length)?) }),
        Request::WriteRegisters {
            bus,
            address,
            offset,
            data,
        } => {
            let data = unhex(&data).ok_or(RequestError::Hex)?;
            hardware.write_registers(&bus, address, offset, &data)?;
            json!({})
        }
        Request::InjectFault {
            bus,
            address,
            fault,
            once,
        } => json!({ "id": hardware.inject_fault(&bus, address, fault, once)? }),
        Request::ClearFaults { bus } => {
            json!({ "cleared": hardware.clear_faults(bus.as_deref())? })
        }
        Request::Faults => json!({ "faults": hardware.faults() }),
        Request::Events { since } => json!({ "events": hardware.events(since) }),
        Request::Violations => json!({ "violations": hardware.violations() }),
        Request::Reset => {
            hardware.reset();
            json!({})
        }
        Request::Displays => json!({ "displays": hardware.displays() }),
        Request::Display { bus } => {
            let frame = hardware.display(&bus)?;
            json!({
                "width": frame.width,
                "height": frame.height,
                "visible": frame.visible,
                "revision": frame.revision,
                "data": hex(&frame_bytes(&frame.pixels)),
            })
        }
    })
}

/// Row-major RGB565 pixels as little-endian bytes.
fn frame_bytes(pixels: &[u16]) -> Vec<u8> {
    pixels
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .collect()
}

/// Binds the control listener and serves it from background threads.
///
/// Every connection gets its own thread; requests on one connection are
/// answered in order.
///
/// # Errors
///
/// Returns the bind error.
pub fn start(hardware: VirtualHardware, address: SocketAddr) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(address)?;
    let local = listener.local_addr()?;
    thread::Builder::new()
        .name(String::from("virtual-io-control"))
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let hardware = hardware.clone();
                        let spawned = thread::Builder::new()
                            .name(String::from("virtual-io-connection"))
                            .spawn(move || serve_connection(&hardware, stream));
                        if let Err(error) = spawned {
                            log::warn!("virtual I/O control connection rejected: {error}");
                        }
                    }
                    Err(error) => log::warn!("virtual I/O control accept failed: {error}"),
                }
            }
        })?;
    Ok(local)
}

fn serve_connection(hardware: &VirtualHardware, stream: TcpStream) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut writer = stream;
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.starts_with("GET ") || line.starts_with("POST ") {
            serve_http(hardware, &line, &mut reader, &mut writer);
            return;
        }
        if line.trim().is_empty() {
            continue;
        }
        let mut response = handle(hardware, line.trim_end());
        response.push('\n');
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
    }
}

/// The browser viewer of the display panels.
const VIEWER: &str = include_str!("viewer.html");
/// Largest HTTP request body accepted, one control request.
const MAX_HTTP_BODY: usize = 64 * 1024;

/// One HTTP request and its answer on a fresh connection, then close.
///
/// `GET /` is the viewer, `GET /display/<bus>` a panel's pixels, and
/// `POST /control` one control request. Only pages served from this
/// loopback address may post: the request must name a loopback `Host`,
/// carry JSON, and come from no other origin, so other websites the
/// developer visits cannot drive the virtual hardware.
fn serve_http(
    hardware: &VirtualHardware,
    request_line: &str,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
) {
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut host = None;
    let mut origin = None;
    let mut json_body = false;
    let mut length = 0_usize;
    let mut header = String::new();
    loop {
        header.clear();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => host = Some(String::from(value)),
            "origin" => origin = Some(String::from(value)),
            "content-type" => json_body = value.starts_with("application/json"),
            "content-length" => length = value.parse().unwrap_or(usize::MAX),
            _ => {}
        }
    }
    let loopback_host = host.as_deref().is_some_and(is_loopback_host);
    let response = match (method, path) {
        _ if !loopback_host => http(403, "text/plain", &[], b"loopback hosts only".to_vec()),
        ("GET", "/") => http(
            200,
            "text/html; charset=utf-8",
            &[],
            VIEWER.as_bytes().to_vec(),
        ),
        ("GET", path) if path.starts_with("/display/") => {
            match hardware.display(&path["/display/".len()..]) {
                Ok(frame) => http(
                    200,
                    "application/octet-stream",
                    &[
                        ("X-Width", frame.width.to_string()),
                        ("X-Height", frame.height.to_string()),
                        ("X-Visible", frame.visible.to_string()),
                        ("X-Revision", frame.revision.to_string()),
                    ],
                    frame_bytes(&frame.pixels),
                ),
                Err(error) => http(404, "text/plain", &[], error.to_string().into_bytes()),
            }
        }
        ("POST", "/control") => {
            let same_origin = origin.as_deref().is_none_or(|origin| {
                host.as_deref()
                    .is_some_and(|host| origin == format!("http://{host}"))
            });
            if !json_body || !same_origin {
                http(403, "text/plain", &[], b"same-origin JSON only".to_vec())
            } else if length > MAX_HTTP_BODY {
                http(413, "text/plain", &[], b"request too large".to_vec())
            } else {
                let mut body = vec![0; length];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                let line = String::from_utf8_lossy(&body);
                http(
                    200,
                    "application/json",
                    &[],
                    handle(hardware, line.trim()).into_bytes(),
                )
            }
        }
        _ => http(404, "text/plain", &[], b"not found".to_vec()),
    };
    let _closed = writer.write_all(&response);
}

/// Whether an HTTP `Host` names this loopback server, by IP or `localhost`.
fn is_loopback_host(host: &str) -> bool {
    let name = host
        .rsplit_once(':')
        .filter(|(_, port)| port.parse::<u16>().is_ok())
        .map_or(host, |(name, _)| name);
    name == "localhost"
        || name
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn http(status: u16, content_type: &str, headers: &[(&str, String)], body: Vec<u8>) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Payload Too Large",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let mut bytes = response.into_bytes();
    bytes.extend(body);
    bytes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;
    use serde_json::Value;

    use super::*;
    use crate::clock::Clock;

    fn hardware() -> VirtualHardware {
        let hardware = VirtualHardware::new(&["GPIO0", "GPIO1"], &["I2C0"], Clock::manual());
        hardware.name_pin(0, "vio-0");
        hardware
    }

    fn call(hardware: &VirtualHardware, request: &str) -> Value {
        serde_json::from_str(&handle(hardware, request)).expect("JSON response")
    }

    #[test]
    fn pins_can_be_listed_read_and_driven() {
        let hardware = hardware();
        let pins = call(&hardware, r#"{"op":"pins"}"#);
        assert_eq!(pins["ok"], true);
        assert_eq!(pins["pins"][0]["name"], "vio-0");
        assert_eq!(pins["pins"][1]["name"], "GPIO1");
        let driven = call(
            &hardware,
            r#"{"op":"set_input","pin":"vio-0","level":true}"#,
        );
        assert_eq!(driven["pin"]["driven"], true);
        assert_eq!(driven["pin"]["level"], true);
        let pin = call(&hardware, r#"{"op":"pin","pin":"GPIO0"}"#);
        assert_eq!(pin["pin"]["mode"], "disabled");
        let released = call(
            &hardware,
            r#"{"op":"set_input","pin":"vio-0","level":null}"#,
        );
        assert_eq!(released["pin"]["driven"], Value::Null);
    }

    #[test]
    fn devices_registers_and_faults_are_managed() {
        let hardware = hardware();
        let added = call(
            &hardware,
            r#"{"op":"add_device","bus":"I2C0","address":80,"data":"0102"}"#,
        );
        assert_eq!(added, serde_json::json!({ "ok": true }));
        call(
            &hardware,
            r#"{"op":"write_registers","bus":"I2C0","address":80,"offset":16,"data":"aabb"}"#,
        );
        let read = call(
            &hardware,
            r#"{"op":"read_registers","bus":"I2C0","address":80,"length":2}"#,
        );
        assert_eq!(read["data"], "0102");
        let read = call(
            &hardware,
            r#"{"op":"read_registers","bus":"I2C0","address":80,"offset":16,"length":2}"#,
        );
        assert_eq!(read["data"], "aabb");
        let buses = call(&hardware, r#"{"op":"buses"}"#);
        assert_eq!(buses["buses"][0]["devices"][0]["model"], "registers");

        let fault = call(
            &hardware,
            r#"{"op":"inject_fault","bus":"I2C0","address":80,"fault":"bus-error"}"#,
        );
        assert_eq!(fault["id"], 1);
        let mut bus = hardware.i2c_bus("I2C0").expect("bus");
        assert!(bus.write(80, &[0]).is_err());
        assert!(bus.write(80, &[0]).is_ok(), "faults default to once");
        let events = call(&hardware, r#"{"op":"events"}"#);
        assert_eq!(events["events"][0]["kind"], "i2c");
        assert_eq!(events["events"][0]["result"], "bus-error");
        assert_eq!(events["events"][0]["injected"], true);
        assert_eq!(events["events"][1]["operations"][0]["write"], "00");
        let later = call(&hardware, r#"{"op":"events","since":2}"#);
        assert_eq!(later["events"].as_array().map(Vec::len), Some(1));

        call(
            &hardware,
            r#"{"op":"inject_fault","bus":"I2C0","fault":"nack","once":false}"#,
        );
        assert_eq!(
            call(&hardware, r#"{"op":"faults"}"#)["faults"][0]["kind"],
            "nack"
        );
        assert_eq!(call(&hardware, r#"{"op":"clear_faults"}"#)["cleared"], 1);
        assert_eq!(
            call(&hardware, r#"{"op":"violations"}"#)["violations"],
            serde_json::json!([])
        );
        call(
            &hardware,
            r#"{"op":"remove_device","bus":"I2C0","address":80}"#,
        );
        call(&hardware, r#"{"op":"reset"}"#);
        assert_eq!(
            call(&hardware, r#"{"op":"events"}"#)["events"],
            serde_json::json!([])
        );
    }

    #[test]
    fn invalid_requests_answer_with_an_error() {
        let hardware = hardware();
        for request in [
            "not json",
            r#"{"op":"launch"}"#,
            r#"{"op":"pin","pin":"GPIO9"}"#,
            r#"{"op":"add_device","bus":"I2C0","address":80,"model":"flux"}"#,
            r#"{"op":"add_device","bus":"I2C0","address":200}"#,
            r#"{"op":"write_registers","bus":"I2C0","address":80,"data":"abc"}"#,
            r#"{"op":"read_registers","bus":"I2C0","address":80,"length":1}"#,
            r#"{"op":"inject_fault","bus":"I2C0","fault":"gremlins"}"#,
        ] {
            let response = call(&hardware, request);
            assert_eq!(response["ok"], false, "{request}");
            assert!(response["error"].as_str().is_some_and(|e| !e.is_empty()));
        }
    }

    #[test]
    fn the_tcp_server_answers_line_by_line() {
        let hardware = hardware();
        let address = start(hardware, "127.0.0.1:0".parse().expect("address")).expect("start");
        let mut stream = TcpStream::connect(address).expect("connect");
        stream
            .write_all(b"{\"op\":\"pins\"}\n\n{\"op\":\"reset\"}\n")
            .expect("send");
        let mut lines = BufReader::new(stream).lines();
        let first: Value =
            serde_json::from_str(&lines.next().expect("line").expect("read")).expect("JSON");
        assert_eq!(first["pins"][0]["chip"], "GPIO0");
        let second: Value =
            serde_json::from_str(&lines.next().expect("line").expect("read")).expect("JSON");
        assert_eq!(second, serde_json::json!({ "ok": true }));
    }

    fn http_request(address: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(address).expect("connect");
        stream.write_all(request.as_bytes()).expect("send");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("receive");
        String::from_utf8_lossy(&response).into_owned()
    }

    #[test]
    fn displays_are_listed_read_and_served_to_a_same_origin_browser() {
        let hardware = VirtualHardware::new(&["GPIO0"], &[], Clock::manual()).with_spi(&["SPI0"]);
        hardware
            .attach_spi_peripheral(
                "SPI0",
                Box::new(crate::models::MipiDbiPanelModel::new(2, 1, (0, 0), 0)),
                "display",
            )
            .expect("panel");
        let listed = call(&hardware, r#"{"op":"displays"}"#);
        assert_eq!(listed["displays"][0]["peripheral"], "display");
        assert_eq!(listed["displays"][0]["width"], 2);
        let frame = call(&hardware, r#"{"op":"display","bus":"SPI0"}"#);
        assert_eq!(frame["data"], "00000000");
        assert_eq!(frame["visible"], false);

        let address = start(hardware, "127.0.0.1:0".parse().expect("address")).expect("start");
        let page = http_request(address, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("<title>Virtual Displays</title>"));
        let pixels = http_request(
            address,
            "GET /display/SPI0 HTTP/1.1\r\nHost: localhost:1\r\n\r\n",
        );
        assert!(pixels.contains("X-Width: 2\r\n"), "{pixels}");
        let body = r#"{"op":"displays"}"#;
        let post = |headers: &str| {
            http_request(
                address,
                &format!(
                    "POST /control HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
                    address.port(),
                    body.len()
                ),
            )
        };
        let answered = post("Content-Type: application/json\r\n");
        assert!(answered.contains(r#""ok":true"#), "{answered}");
        let same_origin = post(&format!(
            "Content-Type: application/json\r\nOrigin: http://127.0.0.1:{}\r\n",
            address.port()
        ));
        assert!(same_origin.starts_with("HTTP/1.1 200"), "{same_origin}");
        // Other sites the developer visits cannot drive the hardware.
        for refused in [
            post("Content-Type: text/plain\r\n"),
            post("Content-Type: application/json\r\nOrigin: http://example.com\r\n"),
            http_request(address, "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n"),
        ] {
            assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
        }
    }
}
