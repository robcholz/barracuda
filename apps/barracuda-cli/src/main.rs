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

use anyhow::{bail, Result};

const DEFAULT_URL: &str = "ws://10.42.0.2:8787";

#[derive(Debug, PartialEq, Eq)]
enum RunMode<'a> {
    Remote(&'a str),
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
        RunMode::Remote(url) => client::run(url).await,
    }
}

fn mode_from_args<'a>(args: &'a [&'a str]) -> Result<RunMode<'a>> {
    match args {
        [] | ["connect"] => Ok(RunMode::Remote(DEFAULT_URL)),
        ["connect", url] => Ok(RunMode::Remote(url)),
        [other, ..] => bail!("unknown subcommand `{other}`; use `connect [url]`"),
    }
}

#[cfg(test)]
mod tests {
    use super::{mode_from_args, RunMode, DEFAULT_URL};

    #[test]
    fn default_mode_connects_to_the_default_channel() {
        assert_eq!(
            mode_from_args(&[]).expect("default channel"),
            RunMode::Remote(DEFAULT_URL)
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
        let error = mode_from_args(&["chat"]).expect_err("unknown command");
        assert!(error.to_string().contains("unknown subcommand `chat`"));
    }
}
