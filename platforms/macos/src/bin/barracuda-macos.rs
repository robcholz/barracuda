//! Host launcher for the macOS Platform's user-space network gateway.

use std::{env, path::PathBuf, process::Stdio};

use anyhow::{anyhow, bail, Context as _};
use barracuda_net_gateway::{serve as serve_gateway, GatewayConfig};
use tokio::{net::TcpListener, process::Command};

const GATEWAY_ADDRESS: &str = "127.0.0.1:8787";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _result = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_error| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let mut arguments = env::args_os().skip(1);
    let application = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing macOS application path"))?;
    let workspace = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing workspace path"))?;

    barracuda_system_image::deploy_selected(&workspace).map_err(|error| anyhow!(error))?;
    let listener = TcpListener::bind(GATEWAY_ADDRESS)
        .await
        .with_context(|| format!("bind macOS Platform network gateway at {GATEWAY_ADDRESS}"))?;
    let gateway =
        tokio::spawn(async move { serve_gateway(listener, GatewayConfig::native()).await });

    let mut child = Command::new(&application)
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("start macOS application `{}`", application.display()))?;

    tokio::select! {
        status = child.wait() => {
            let status = status.context("wait for macOS application")?;
            if !status.success() && !interrupted(&status) {
                bail!("macOS application exited with {status}");
            }
        }
        result = gateway => {
            let _result = child.kill().await;
            result.context("macOS network gateway task stopped")??;
            bail!("macOS network gateway stopped unexpectedly");
        }
        signal = tokio::signal::ctrl_c() => {
            signal.context("wait for Ctrl-C")?;
            let _result = child.kill().await;
            let _result = child.wait().await;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn interrupted(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;

    status.signal() == Some(2)
}

#[cfg(not(unix))]
fn interrupted(_status: &std::process::ExitStatus) -> bool {
    false
}
