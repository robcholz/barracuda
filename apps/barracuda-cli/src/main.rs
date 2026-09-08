//! `barracuda` — an external terminal Channel for a Barracuda System.
//!
//! The process owns only terminal input, WebSocket transport, and reply
//! rendering. It never constructs or controls a Barracuda System.
//!
//! ```text
//! cargo cli [URL]
//! cargo cli configure [ADDRESS]
//! ```

mod client;
mod command;
mod line_editor;
mod protocol;

use std::{
    net::IpAddr,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context as _, Result};
use dialoguer::{console::Style, theme::ColorfulTheme, Input, Select};

const DEFAULT_ADDRESS: &str = "http://10.42.0.2:8787";
const DEFAULT_REMOTE_PORT: u16 = 8787;
const RUNTIME_ADDRESS_PATH: &str = ".barracuda/address";

#[derive(Debug, PartialEq, Eq)]
enum RunMode<'a> {
    Connect(Option<&'a str>),
    Configure(Option<&'a str>),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let exit_code = match run().await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    };
    std::process::exit(exit_code);
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match mode_from_args(&args.iter().skip(1).map(String::as_str).collect::<Vec<_>>())? {
        RunMode::Connect(explicit_url) => {
            let url = match explicit_url {
                Some(url) => url.to_owned(),
                None => websocket_url(&prompt_for_address()?)?,
            };
            client::run(&url).await
        }
        RunMode::Configure(explicit_address) => {
            let address = match explicit_address {
                Some(address) => address.to_owned(),
                None => prompt_for_address()?,
            };
            open_configuration(&address)
        }
    }
}

fn mode_from_args<'a>(args: &'a [&'a str]) -> Result<RunMode<'a>> {
    match args {
        [] | ["connect"] => Ok(RunMode::Connect(None)),
        ["connect", url] => Ok(RunMode::Connect(Some(url))),
        ["configure"] => Ok(RunMode::Configure(None)),
        ["configure", address] => Ok(RunMode::Configure(Some(address))),
        [url] if url.starts_with("ws://") || url.starts_with("wss://") => {
            Ok(RunMode::Connect(Some(url)))
        }
        [other, ..] => bail!(
            "unknown command `{other}`; use `cargo cli [URL]` or `cargo cli configure [ADDRESS]`"
        ),
    }
}

fn prompt_for_address() -> Result<String> {
    let local_address = default_address();
    let description_style = Style::new().for_stderr().black().bright();
    let local_description = match &local_address {
        Ok(address) => address.as_str(),
        Err(_error) => "no running instance found",
    };
    let choices = [
        format!("Local   {}", description_style.apply_to(local_description)),
        format!(
            "Remote  {}",
            description_style.apply_to("enter a device IP address")
        ),
    ];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select Barracuda")
        .default(0)
        .items(&choices)
        .report(false)
        .interact()
        .context("select Barracuda connection")?;
    match selection {
        0 => {
            let address = local_address?;
            eprintln!("Local Barracuda: {address}");
            Ok(address)
        }
        1 => {
            let address = Input::<String>::with_theme(&ColorfulTheme::default())
                .with_prompt(format!(
                    "Remote Barracuda IP address (port {DEFAULT_REMOTE_PORT})"
                ))
                .validate_with(|value: &String| -> std::result::Result<(), &str> {
                    if value.trim().parse::<IpAddr>().is_ok() {
                        Ok(())
                    } else {
                        Err("enter a valid IPv4 or IPv6 address")
                    }
                })
                .interact_text()
                .context("read remote Barracuda IP address")?;
            let address = address
                .trim()
                .parse::<IpAddr>()
                .context("validate remote Barracuda IP address")?;
            let address = remote_address(address);
            eprintln!("Remote Barracuda: {address}");
            Ok(address)
        }
        index => bail!("invalid connection selection index {index}"),
    }
}

fn default_address() -> Result<String> {
    let current_directory = std::env::current_dir().context("read current directory")?;
    if let Some(address) = runtime_address(&current_directory)? {
        return Ok(address);
    }
    if cfg!(target_os = "macos") {
        bail!(
            "no running Barracuda address found; start `cargo run`, select Remote, or pass an explicit address"
        );
    }
    Ok(DEFAULT_ADDRESS.to_owned())
}

fn runtime_address(start: &Path) -> Result<Option<String>> {
    for directory in start.ancestors() {
        let path = directory.join(RUNTIME_ADDRESS_PATH);
        match std::fs::read_to_string(&path) {
            Ok(address) if !address.trim().is_empty() => {
                return Ok(Some(address.trim().to_owned()));
            }
            Ok(_) => bail!("runtime Barracuda address is empty: {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read runtime Barracuda address `{}`", path.display())
                });
            }
        }
    }
    Ok(None)
}

fn websocket_url(address: &str) -> Result<String> {
    if let Some(authority) = address.strip_prefix("http://") {
        return Ok(format!("ws://{authority}"));
    }
    if let Some(authority) = address.strip_prefix("https://") {
        return Ok(format!("wss://{authority}"));
    }
    bail!("Barracuda address must use http:// or https://")
}

fn remote_address(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => format!("http://{address}:{DEFAULT_REMOTE_PORT}"),
        IpAddr::V6(address) => format!("http://[{address}]:{DEFAULT_REMOTE_PORT}"),
    }
}

fn configuration_url(address: &str) -> Result<String> {
    let address = if let Some(authority) = address.strip_prefix("http://") {
        format!("http://{authority}")
    } else if let Some(authority) = address.strip_prefix("https://") {
        format!("https://{authority}")
    } else if let Some(authority) = address.strip_prefix("ws://") {
        format!("http://{authority}")
    } else if let Some(authority) = address.strip_prefix("wss://") {
        format!("https://{authority}")
    } else {
        bail!("Barracuda address must use http://, https://, ws://, or wss://");
    };
    Ok(format!("{}/portal/", address.trim_end_matches('/')))
}

fn open_configuration(address: &str) -> Result<()> {
    let url = configuration_url(address)?;
    println!("Barracuda configuration: {url}");
    let (program, arguments): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url.as_str()])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url.as_str()])
    } else {
        ("xdg-open", vec![url.as_str()])
    };
    Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("open Barracuda configuration URL `{url}`"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{
        configuration_url, mode_from_args, remote_address, runtime_address, websocket_url, RunMode,
        RUNTIME_ADDRESS_PATH,
    };

    #[test]
    fn default_mode_connects_to_the_default_channel() {
        assert_eq!(
            mode_from_args(&[]).expect("default channel"),
            RunMode::Connect(None)
        );
    }

    #[test]
    fn connect_remains_a_remote_client_mode() {
        assert_eq!(
            mode_from_args(&["connect"]).expect("default remote URL"),
            RunMode::Connect(None)
        );
        assert_eq!(
            mode_from_args(&["connect", "ws://host.example:9000"]).expect("explicit remote URL"),
            RunMode::Connect(Some("ws://host.example:9000"))
        );
        assert_eq!(
            mode_from_args(&["ws://host.example:9000"]).expect("short explicit URL"),
            RunMode::Connect(Some("ws://host.example:9000"))
        );
    }

    #[test]
    fn remote_ip_addresses_use_the_webserver_port() {
        assert_eq!(
            remote_address(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
            "http://192.0.2.10:8787"
        );
        assert_eq!(
            remote_address(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            "http://[::1]:8787"
        );
    }

    #[test]
    fn configuration_mode_opens_the_plugin_portal() {
        assert_eq!(
            mode_from_args(&["configure"]).expect("interactive configuration"),
            RunMode::Configure(None)
        );
        assert_eq!(
            mode_from_args(&["configure", "http://device.local:8787"])
                .expect("explicit configuration address"),
            RunMode::Configure(Some("http://device.local:8787"))
        );
        assert_eq!(
            configuration_url("ws://device.local:8787/").expect("configuration URL"),
            "http://device.local:8787/portal/"
        );
    }

    #[test]
    fn discovers_the_runtime_address_from_a_workspace_parent() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let nested = workspace.path().join("apps/barracuda-cli");
        let address_path = workspace.path().join(RUNTIME_ADDRESS_PATH);
        fs::create_dir_all(&nested).expect("create nested directory");
        fs::create_dir_all(address_path.parent().expect("runtime address parent"))
            .expect("create runtime state directory");
        fs::write(&address_path, "http://127.0.0.1:49152/\n").expect("write runtime address");

        assert_eq!(
            runtime_address(&nested).expect("discover runtime address"),
            Some(String::from("http://127.0.0.1:49152/"))
        );
        assert_eq!(
            websocket_url("http://127.0.0.1:49152/").expect("convert runtime address"),
            "ws://127.0.0.1:49152/"
        );
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        let error = mode_from_args(&["chat"]).expect_err("unknown command");
        assert!(error.to_string().contains("unknown command `chat`"));
    }
}
