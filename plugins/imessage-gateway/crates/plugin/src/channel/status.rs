//! A channel's status from its configuration, mode, and receive state.

use crate::channel::http::ChannelControl;
use crate::channel::mode::ChannelMode;
use crate::channel::receive::ReceiveState;

/// How a channel stands overall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelLevel {
    /// Not set up or disabled.
    Off,
    /// Working as configured.
    Ready,
    /// Configured but not working yet, or failing.
    Attention,
}

/// A channel's status in a few words, for a status display such as a portal
/// card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelStatus {
    /// Overall level.
    pub level: ChannelLevel,
    /// Chinese label.
    pub zh: &'static str,
    /// English label.
    pub en: &'static str,
}

/// Maps a channel's state to its status.
///
/// | Channel | Status |
/// | --- | --- |
/// | not configured | `off` 未配置/Not set up |
/// | `disabled` | `off` 已停用/Disabled |
/// | `send` | `ready` 仅发送/Send only |
/// | `send_receive`, receiving | `ready` 收发中/Receiving |
/// | `send_receive`, starting or idle | `attention` 连接中/Connecting |
/// | `send_receive`, no slot | `attention` 名额已满/No slot |
/// | `send_receive`, error | `attention` 连接中断/Disconnected |
pub(crate) fn status_of(
    configured: bool,
    mode: ChannelMode,
    receive: &ReceiveState,
) -> ChannelStatus {
    let (level, zh, en) = if configured {
        match mode {
            ChannelMode::Disabled => (ChannelLevel::Off, "已停用", "Disabled"),
            ChannelMode::Send => (ChannelLevel::Ready, "仅发送", "Send only"),
            ChannelMode::SendReceive => match receive {
                ReceiveState::Receiving => (ChannelLevel::Ready, "收发中", "Receiving"),
                ReceiveState::Idle | ReceiveState::Starting => {
                    (ChannelLevel::Attention, "连接中", "Connecting")
                }
                ReceiveState::NoSlot => (ChannelLevel::Attention, "名额已满", "No slot"),
                ReceiveState::Error(_) => (ChannelLevel::Attention, "连接中断", "Disconnected"),
            },
        }
    } else {
        (ChannelLevel::Off, "未配置", "Not set up")
    };
    ChannelStatus { level, zh, en }
}

/// [`status_of`] a [`ChannelControl`].
pub(crate) fn channel_status<C: ChannelControl + ?Sized>(channel: &C) -> ChannelStatus {
    status_of(
        channel.configured(),
        channel.mode(),
        &channel.receive().state(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(level: ChannelLevel, zh: &'static str, en: &'static str) -> ChannelStatus {
        ChannelStatus { level, zh, en }
    }

    #[test]
    fn maps_every_mode_and_receive_state() {
        let error = ReceiveState::Error("closed".into());
        let cases = [
            (
                false,
                ChannelMode::SendReceive,
                ReceiveState::Receiving,
                status(ChannelLevel::Off, "未配置", "Not set up"),
            ),
            (
                true,
                ChannelMode::Disabled,
                ReceiveState::Idle,
                status(ChannelLevel::Off, "已停用", "Disabled"),
            ),
            (
                true,
                ChannelMode::Send,
                ReceiveState::Idle,
                status(ChannelLevel::Ready, "仅发送", "Send only"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::Receiving,
                status(ChannelLevel::Ready, "收发中", "Receiving"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::Starting,
                status(ChannelLevel::Attention, "连接中", "Connecting"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::Idle,
                status(ChannelLevel::Attention, "连接中", "Connecting"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::NoSlot,
                status(ChannelLevel::Attention, "名额已满", "No slot"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                error,
                status(ChannelLevel::Attention, "连接中断", "Disconnected"),
            ),
        ];
        for (configured, mode, receive, expected) in cases {
            assert_eq!(
                status_of(configured, mode, &receive),
                expected,
                "{configured} {mode:?} {receive:?}"
            );
        }
    }
}
