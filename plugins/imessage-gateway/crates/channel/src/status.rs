//! Portal status of a channel from its configuration, mode, and receive
//! state.

use barracuda_captive_portal_plugin::{EntryState, EntryStatus, WebText};

use crate::http::ChannelControl;
use crate::mode::ChannelMode;
use crate::receive::ReceiveState;

/// Maps a channel's state to its portal status.
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
#[must_use]
pub fn channel_entry_status(
    configured: bool,
    mode: ChannelMode,
    receive: &ReceiveState,
) -> EntryStatus {
    if !configured {
        return EntryStatus::configured(false);
    }
    let (state, zh, en) = match mode {
        ChannelMode::Disabled => (EntryState::Off, "已停用", "Disabled"),
        ChannelMode::Send => (EntryState::Ready, "仅发送", "Send only"),
        ChannelMode::SendReceive => match receive {
            ReceiveState::Receiving => (EntryState::Ready, "收发中", "Receiving"),
            ReceiveState::Idle | ReceiveState::Starting => {
                (EntryState::Attention, "连接中", "Connecting")
            }
            ReceiveState::NoSlot => (EntryState::Attention, "名额已满", "No slot"),
            ReceiveState::Error(_) => (EntryState::Attention, "连接中断", "Disconnected"),
        },
    };
    EntryStatus::new(state, WebText { zh, en })
}

/// [`channel_entry_status`] of a [`ChannelControl`], for
/// `CaptivePortal::register_with_status`.
#[must_use]
pub fn entry_status<C: ChannelControl + ?Sized>(channel: &C) -> EntryStatus {
    channel_entry_status(
        channel.configured(),
        channel.mode(),
        &channel.receive().state(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn status(state: EntryState, zh: &'static str, en: &'static str) -> EntryStatus {
        EntryStatus::new(state, WebText { zh, en })
    }

    #[test]
    fn maps_every_mode_and_receive_state() {
        let error = ReceiveState::Error(String::from("closed"));
        let cases = [
            (
                false,
                ChannelMode::SendReceive,
                ReceiveState::Receiving,
                status(EntryState::Off, "未配置", "Not set up"),
            ),
            (
                true,
                ChannelMode::Disabled,
                ReceiveState::Idle,
                status(EntryState::Off, "已停用", "Disabled"),
            ),
            (
                true,
                ChannelMode::Send,
                ReceiveState::Idle,
                status(EntryState::Ready, "仅发送", "Send only"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::Receiving,
                status(EntryState::Ready, "收发中", "Receiving"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::Starting,
                status(EntryState::Attention, "连接中", "Connecting"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::Idle,
                status(EntryState::Attention, "连接中", "Connecting"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                ReceiveState::NoSlot,
                status(EntryState::Attention, "名额已满", "No slot"),
            ),
            (
                true,
                ChannelMode::SendReceive,
                error,
                status(EntryState::Attention, "连接中断", "Disconnected"),
            ),
        ];
        for (configured, mode, receive, expected) in cases {
            assert_eq!(
                channel_entry_status(configured, mode, &receive),
                expected,
                "{configured} {mode:?} {receive:?}"
            );
        }
    }
}
