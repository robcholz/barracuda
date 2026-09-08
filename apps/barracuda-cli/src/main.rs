//! `barracuda` — an external terminal Channel for a Barracuda System.
//!
//! The process owns only terminal input, WebSocket transport, and reply
//! rendering. It never constructs or controls a Barracuda System.
//!
//! ```text
//! cargo cli URL
//! ```

mod client;
mod command;
mod line_editor;
mod protocol;

use std::path::Path;

use anyhow::{bail, Context as _, Result};

const DEFAULT_URL: &str = "ws://10.42.0.2:8787";
const RUNTIME_ADDRESS_PATH: &str = ".barracuda/address";

#[derive(Debug, PartialEq, Eq)]
enum RunMode<'a> {
    Remote(Option<&'a str>),
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
        RunMode::Remote(explicit_url) => {
            let url = match explicit_url {
                Some(url) => url.to_owned(),
                None => default_url()?,
            };
            client::run(&url).await
        }
    }
}

fn mode_from_args<'a>(args: &'a [&'a str]) -> Result<RunMode<'a>> {
    match args {
        [] | ["connect"] => Ok(RunMode::Remote(None)),
        ["connect", url] => Ok(RunMode::Remote(Some(url))),
        [other, ..] => bail!("unknown subcommand `{other}`; use `connect [url]`"),
    }
}

fn default_url() -> Result<String> {
    let current_directory = std::env::current_dir().context("read current directory")?;
    if let Some(address) = runtime_address(&current_directory)? {
        return websocket_url(&address);
    }
    if cfg!(target_os = "macos") {
        bail!(
            "no running Barracuda address found; start `cargo run` first or pass `cargo cli URL`"
        );
    }
    Ok(DEFAULT_URL.to_owned())
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{mode_from_args, runtime_address, websocket_url, RunMode, RUNTIME_ADDRESS_PATH};

    #[test]
    fn default_mode_connects_to_the_default_channel() {
        assert_eq!(
            mode_from_args(&[]).expect("default channel"),
            RunMode::Remote(None)
        );
    }

    #[test]
    fn connect_remains_a_remote_client_mode() {
        assert_eq!(
            mode_from_args(&["connect"]).expect("default remote URL"),
            RunMode::Remote(None)
        );
        assert_eq!(
            mode_from_args(&["connect", "ws://host.example:9000"]).expect("explicit remote URL"),
            RunMode::Remote(Some("ws://host.example:9000"))
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
        assert!(error.to_string().contains("unknown subcommand `chat`"));
    }
}
