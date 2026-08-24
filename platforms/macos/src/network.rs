//! macOS Embassy Net driver backed by a real layer-three UTUN interface.

use embassy_executor::Spawner;
use embassy_net::Stack;

/// Inherited UTUN file descriptor supplied by a privileged macOS launcher.
pub const NETWORK_FD_ENV: &str = "BARRACUDA_MACOS_UTUN_FD";

/// Creates the macOS UTUN interface and starts the common Embassy packet driver.
pub(crate) async fn initialize(spawner: Spawner) -> Result<Stack<'static>, MacosNetworkError> {
    let mut configuration = tun::Configuration::default();
    let raw_fd = inherited_fd()?.ok_or(MacosNetworkError::MissingInheritedDescriptor)?;
    configuration
        .mtu(tun::DEFAULT_MTU)
        .layer(tun::Layer::L3)
        .raw_fd(raw_fd)
        .close_fd_on_drop(true);
    #[cfg(target_os = "macos")]
    configuration.platform_config(|platform| {
        platform.packet_information(true).enable_routing(false);
    });

    let device = tun::create_as_async(&configuration).map_err(MacosNetworkError::Interface)?;
    crate::tun::initialize(spawner, device)
        .await
        .map_err(MacosNetworkError::Transport)
}

fn inherited_fd() -> Result<Option<i32>, MacosNetworkError> {
    let Some(value) = std::env::var_os(NETWORK_FD_ENV) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_value| MacosNetworkError::InvalidInheritedDescriptor)?;
    let descriptor = value
        .parse::<i32>()
        .map_err(|_error| MacosNetworkError::InvalidInheritedDescriptor)?;
    if descriptor < 0 {
        return Err(MacosNetworkError::InvalidInheritedDescriptor);
    }
    Ok(Some(descriptor))
}

/// macOS network initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum MacosNetworkError {
    /// The process was not started by the privileged network launcher.
    #[error(
        "macOS real networking requires an inherited UTUN; run `sudo target/debug/barracuda-macos-network target/debug/barracuda`"
    )]
    MissingInheritedDescriptor,
    /// UTUN creation or configuration failed.
    #[error("failed to adopt the inherited macOS UTUN: {0}")]
    Interface(tun::Error),
    /// The inherited descriptor environment value was malformed.
    #[error("BARRACUDA_MACOS_UTUN_FD must contain a non-negative file descriptor")]
    InvalidInheritedDescriptor,
    /// The shared Embassy packet transport failed to start.
    #[error(transparent)]
    Transport(#[from] crate::tun::TunNetworkError),
}

#[cfg(test)]
mod tests {
    use super::NETWORK_FD_ENV;

    #[test]
    fn inherited_descriptor_has_a_platform_specific_name() {
        assert_eq!(NETWORK_FD_ENV, "BARRACUDA_MACOS_UTUN_FD");
    }
}
