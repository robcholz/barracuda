//! Per-channel operating mode and its storage.

use barracuda_plugin::manager::{PluginStorage, StorageError};
use serde::{Deserialize, Serialize};

/// Plugin storage key holding the channel's mode as a JSON string.
pub const MODE_STORAGE_KEY: &str = "mode";

/// How an external channel takes part in the Gateway.
///
/// Serialized as `"disabled"`, `"send"`, or `"send_receive"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelMode {
    /// Not registered with the Gateway: no task work and no connections.
    Disabled,
    /// Registered and sends only; it never holds a receive connection.
    Send,
    /// Registered, plus a receive loop holding one receive slot. The default
    /// for a newly configured channel.
    #[default]
    SendReceive,
}

impl ChannelMode {
    /// Every mode, in portal order.
    pub const ALL: &'static [Self] = &[Self::Disabled, Self::Send, Self::SendReceive];

    /// Modes of a channel that cannot send without receiving (WeChat).
    pub const WITHOUT_SEND_ONLY: &'static [Self] = &[Self::Disabled, Self::SendReceive];

    /// Mode of a configuration stored before modes existed: send only,
    /// today's behaviour. WeChat passes [`Self::SendReceive`] instead.
    ///
    /// Usable as `#[serde(default = "ChannelMode::legacy")]` on a mode field
    /// stored inside a channel's configuration.
    #[must_use]
    pub const fn legacy() -> Self {
        Self::Send
    }

    /// Whether the channel is registered with the Gateway.
    #[must_use]
    pub const fn registers(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Whether the channel runs its receive loop.
    #[must_use]
    pub const fn receives(self) -> bool {
        matches!(self, Self::SendReceive)
    }

    /// The JSON name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Send => "send",
            Self::SendReceive => "send_receive",
        }
    }
}

/// Reads the stored mode, migrating a channel that has none.
///
/// Without a stored mode, a channel that is already `configured` was set up
/// before modes existed and gets `legacy`; an unconfigured one gets the
/// default, [`ChannelMode::SendReceive`]. Either way the result is stored, so
/// a later configuration keeps it. An unreadable stored value is replaced the
/// same way.
///
/// # Errors
///
/// Returns [`StorageError`] when reading or writing the mode fails.
pub async fn load_mode<Storage: PluginStorage>(
    storage: &Storage,
    configured: bool,
    legacy: ChannelMode,
) -> Result<ChannelMode, StorageError> {
    if let Some(bytes) = storage.get_bytes(MODE_STORAGE_KEY).await? {
        match serde_json::from_slice(&bytes) {
            Ok(mode) => return Ok(mode),
            Err(_error) => log::warn!("replacing an unreadable stored channel mode"),
        }
    }
    let mode = if configured {
        legacy
    } else {
        ChannelMode::default()
    };
    store_mode(storage, mode).await?;
    Ok(mode)
}

/// Stores `mode`.
///
/// # Errors
///
/// Returns [`StorageError`] when the write fails.
pub async fn store_mode<Storage: PluginStorage>(
    storage: &Storage,
    mode: ChannelMode,
) -> Result<(), StorageError> {
    let encoded = alloc::format!("\"{}\"", mode.as_str());
    storage.put(MODE_STORAGE_KEY, encoded.as_bytes()).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn modes_use_snake_case_json() {
        for (mode, json) in [
            (ChannelMode::Disabled, r#""disabled""#),
            (ChannelMode::Send, r#""send""#),
            (ChannelMode::SendReceive, r#""send_receive""#),
        ] {
            assert_eq!(serde_json::to_string(&mode).expect("encode"), json);
            assert_eq!(
                serde_json::from_str::<ChannelMode>(json).expect("decode"),
                mode
            );
        }
        assert!(serde_json::from_str::<ChannelMode>(r#""receive""#).is_err());
    }

    #[test]
    fn defaults_split_new_and_legacy_configurations() {
        assert_eq!(ChannelMode::default(), ChannelMode::SendReceive);
        assert_eq!(ChannelMode::legacy(), ChannelMode::Send);

        #[derive(serde::Deserialize)]
        struct Stored {
            #[serde(default = "ChannelMode::legacy")]
            mode: ChannelMode,
        }
        let stored: Stored = serde_json::from_str("{}").expect("old configuration");
        assert_eq!(stored.mode, ChannelMode::Send);
    }

    #[test]
    fn registration_and_receive_follow_the_mode() {
        assert!(!ChannelMode::Disabled.registers());
        assert!(ChannelMode::Send.registers() && !ChannelMode::Send.receives());
        assert!(ChannelMode::SendReceive.registers() && ChannelMode::SendReceive.receives());
    }
}
