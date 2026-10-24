//! What every channel Plugin shares, offered through the `IMessageGateway`
//! capability: its mode and storage, the receive-loop scaffold, the HTTP
//! routes over [`ChannelControl`], and its status.

mod http;
mod mode;
mod on_demand;
mod receive;
mod status;

#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod receive_tests;

pub(crate) use http::{sync_receive, ChannelEndpoint};
pub use http::{ChannelControl, ModeError, ModeFuture};
pub use mode::ChannelMode;
pub(crate) use mode::{load_mode, store_mode};
pub use on_demand::OnDemand;
pub(crate) use receive::receive_runtime;
pub use receive::{
    NoSlot, ReceiveChannel, ReceiveControl, ReceiveError, ReceiveFailure, ReceiveFuture,
    ReceiveRuntime, ReceiveSession, ReceiveSlotSource, ReceiveState, ReceiveTiming, SlotWait,
    UnlimitedSlots,
};
pub(crate) use status::channel_status;
pub use status::{ChannelLevel, ChannelStatus};
