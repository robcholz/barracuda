//! Host launcher for the macOS Platform's user-space network gateway.

use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{anyhow, bail, Context as _};
use barracuda_platform_macos_network_gateway::{serve as serve_gateway, GatewayConfig};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
};

const GATEWAY_ADDRESS: &str = "127.0.0.1:8787";
const RUNTIME_ADDRESS_PATH: &str = ".barracuda/address";

struct RuntimeAddressFile(PathBuf);

impl RuntimeAddressFile {
    fn prepare(workspace: &Path) -> anyhow::Result<Self> {
        let path = workspace.join(RUNTIME_ADDRESS_PATH);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove stale runtime address `{}`", path.display()));
            }
        }
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for RuntimeAddressFile {
    fn drop(&mut self) {
        let _result = fs::remove_file(&self.0);
    }
}

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
    let runtime_address = RuntimeAddressFile::prepare(&workspace)?;
    let gateway_config =
        GatewayConfig::native().with_runtime_address_file(runtime_address.path().to_path_buf());
    let gateway = tokio::spawn(async move { serve_gateway(listener, gateway_config).await });

    let mut command = Command::new(&application);
    command
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("start macOS application `{}`", application.display()))?;

    tokio::select! {
        status = child.wait() => {
            if status.is_err() {
                let _result = terminate_child(&mut child).await;
            }
            let status = status.context("wait for macOS application")?;
            if !status.success() && !interrupted(&status) {
                bail!("macOS application exited with {status}");
            }
        }
        result = gateway => {
            let termination = terminate_child(&mut child).await;
            result.context("macOS network gateway task stopped")??;
            termination?;
            bail!("macOS network gateway stopped unexpectedly");
        }
        signal = shutdown_signal() => {
            let termination = terminate_child(&mut child).await;
            signal.context("wait for launcher shutdown signal")?;
            termination?;
        }
    }
    Ok(())
}

async fn terminate_child(child: &mut Child) -> anyhow::Result<()> {
    match child.try_wait() {
        Ok(Some(_status)) => Ok(()),
        Ok(None) => child.kill().await.context("terminate macOS application"),
        Err(error) => {
            let _result = child.start_kill();
            let _result = child.wait().await;
            Err(error).context("check macOS application status")
        }
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _signal = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fs;

    use tokio::process::Command;

    use super::{terminate_child, RuntimeAddressFile, RUNTIME_ADDRESS_PATH};

    #[test]
    fn runtime_address_file_removes_stale_state_and_cleans_up() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let path = workspace.path().join(RUNTIME_ADDRESS_PATH);
        fs::create_dir_all(path.parent().expect("runtime address parent"))
            .expect("create runtime state directory");
        fs::write(&path, "http://127.0.0.1:49152/\n").expect("write stale runtime address");

        let runtime_address =
            RuntimeAddressFile::prepare(workspace.path()).expect("prepare runtime address");
        assert!(!path.exists());
        fs::write(runtime_address.path(), "http://127.0.0.1:49153/\n")
            .expect("write active runtime address");
        drop(runtime_address);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminating_a_running_application_also_reaps_it() {
        let mut command = Command::new("sleep");
        command.arg("30").kill_on_drop(true);
        let mut child = command.spawn().expect("start test application");

        terminate_child(&mut child)
            .await
            .expect("terminate test application");

        assert!(child
            .try_wait()
            .expect("check terminated test application")
            .is_some());
    }
}
