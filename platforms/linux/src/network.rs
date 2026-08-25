//! Linux Embassy Net driver backed by a provisioned real layer-three TUN interface.

use embassy_executor::Spawner;
use embassy_net::Stack;

/// Opens a provisioned Linux TUN and starts the common Embassy packet driver.
pub(crate) async fn initialize(
    spawner: Spawner,
    interface: &str,
) -> Result<Stack<'static>, LinuxNetworkError> {
    let mut configuration = tun::Configuration::default();
    configuration
        .tun_name(interface)
        .mtu(tun::DEFAULT_MTU)
        .layer(tun::Layer::L3);
    #[cfg(target_os = "linux")]
    configuration.platform_config(|platform| {
        platform.ensure_root_privileges(false);
    });
    let device =
        tun::create_as_async(&configuration).map_err(|source| LinuxNetworkError::Interface {
            name: interface.to_owned(),
            source,
        })?;
    crate::tun::initialize(spawner, device)
        .await
        .map_err(LinuxNetworkError::Transport)
}

/// Linux network initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum LinuxNetworkError {
    /// The provisioned TUN could not be opened by the application user.
    #[error(
        "failed to open provisioned Linux TUN `{name}`: {source}; run `sudo sh platforms/linux/provision.sh {name}` first"
    )]
    Interface {
        /// Configured Linux interface name.
        name: String,
        /// Native TUN failure.
        source: tun::Error,
    },
    /// The shared Embassy packet transport failed to start.
    #[error(transparent)]
    Transport(#[from] crate::tun::TunNetworkError),
}
