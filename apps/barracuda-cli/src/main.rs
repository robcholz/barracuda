//! `barracuda` — a terminal chat application for the Barracuda agent framework.
//!
//! The default mode owns a local System and connects the terminal to its Web
//! gateway. `connect` keeps the terminal-only mode for an external host.
//!
//! ```text
//! cargo run -p barracuda-cli                 # start a local host and chat
//! cargo run -p barracuda-cli -- connect URL  # terminal client only
//! ```

mod client;
mod command;
mod line_editor;
mod local_host;
mod protocol;

use anyhow::{anyhow, bail, Result};

const DEFAULT_URL: &str = "ws://127.0.0.1:8787";

#[derive(Debug, PartialEq, Eq)]
enum RunMode<'a> {
    Local,
    Remote(&'a str),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let local = tokio::task::LocalSet::new();
    if let Err(error) = local.run_until(run()).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match mode_from_args(&args.iter().skip(1).map(String::as_str).collect::<Vec<_>>())? {
        RunMode::Local => run_local().await,
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

async fn run_local() -> Result<()> {
    let system = local_host::build().await?;
    tokio::pin!(system);

    tokio::select! {
        result = &mut system => match result {
            Ok(()) => bail!("local Barracuda System stopped"),
            Err(error) => Err(anyhow!("local Barracuda System stopped: {error}")),
        },
        result = client::run_when_available(DEFAULT_URL) => result,
    }
}

#[cfg(test)]
mod tests {
    use super::{mode_from_args, RunMode, DEFAULT_URL};

    #[test]
    fn default_and_chat_own_a_local_host() {
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
