//! Line-delimited JSON control interface of the virtual peripherals manager.
//!
//! The protocol is documented in this crate's `README.md`.

use std::{
    io::{self, BufRead, BufReader, Write},
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
}

fn default_model() -> String {
    String::from("registers")
}

const fn default_once() -> bool {
    true
}

/// Device models that the control interface can attach by name.
pub const DEVICE_MODELS: &[&str] = &["registers", "bq27220", "ina226", "pi4ioe5v6408", "rx8130ce"];

/// Creates a device model by its control-interface name.
#[must_use]
pub fn device_model(name: &str) -> Option<Box<dyn I2cDevice>> {
    match name {
        "registers" => Some(Box::new(RegisterDevice::new())),
        "bq27220" => Some(Box::new(crate::models::Bq27220Model::new())),
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
    })
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
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let mut response = handle(hardware, &line);
        response.push('\n');
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
    }
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
}
