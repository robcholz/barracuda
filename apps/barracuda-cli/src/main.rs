//! `barracuda` — a terminal chat application for the Barracuda agent framework.
//!
//! The default mode owns a local System and connects the terminal to its Web
//! gateway. `connect` keeps the terminal-only mode for an external endpoint.
//!
//! ```text
//! cargo run -p barracuda-cli                 # start the local native target and chat
//! cargo run -p barracuda-cli -- connect URL  # terminal client only
//! ```

mod client;
mod command;
mod line_editor;
mod local_native;
mod protocol;

use anyhow::{bail, Result};
use embassy_executor::Spawner;

const DEFAULT_URL: &str = "ws://10.42.0.2:8787";

#[derive(Debug, PartialEq, Eq)]
enum RunMode<'a> {
    Local,
    Remote(&'a str),
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let exit_code = match run(spawner).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    };
    std::process::exit(exit_code);
}

async fn run(spawner: Spawner) -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match mode_from_args(&args.iter().skip(1).map(String::as_str).collect::<Vec<_>>())? {
        RunMode::Local => run_local(spawner).await,
        RunMode::Remote(url) => client::run(url).await,
    }
}

fn mode_from_args<'a>(args: &'a [&'a str]) -> Result<RunMode<'a>> {
    match args {
        [] | ["chat"] => Ok(RunMode::Local),
        ["connect"] => Ok(RunMode::Remote(DEFAULT_URL)),
        ["connect", url] => Ok(RunMode::Remote(url)),
        [other, ..] => {
            bail!("unknown subcommand `{other}`; use `connect <url>` or `chat`")
        }
    }
}

async fn run_local(spawner: Spawner) -> Result<()> {
    local_native::run(spawner).await
}

#[cfg(test)]
mod tests {
    use super::{mode_from_args, RunMode, DEFAULT_URL};

    #[test]
    fn default_and_chat_own_a_local_native_system() {
        assert_eq!(mode_from_args(&[]).expect("default mode"), RunMode::Local);
        assert_eq!(
            mode_from_args(&["chat"]).expect("chat mode"),
            RunMode::Local
        );
    }

    #[test]
    fn connect_remains_a_remote_client_mode() {
        assert_eq!(
            mode_from_args(&["connect"]).expect("default remote URL"),
            RunMode::Remote(DEFAULT_URL)
        );
        assert_eq!(
            mode_from_args(&["connect", "ws://host.example:9000"]).expect("explicit remote URL"),
            RunMode::Remote("ws://host.example:9000")
        );
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        let error = mode_from_args(&["serve"]).expect_err("unknown command");
        assert!(error.to_string().contains("unknown subcommand `serve`"));
    }
}
