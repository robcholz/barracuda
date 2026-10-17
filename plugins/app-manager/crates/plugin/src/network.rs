//! The `SystemNetwork` capability: the network the system UI shows, when a
//! Plugin shows one.

use alloc::{boxed::Box, rc::Rc, string::String};
use core::{cell::RefCell, future::Future, pin::Pin};

/// The device's network link, as the status bar and the control center show
/// it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NetworkState {
    /// Online, on the network named, if it has a name.
    Connected {
        /// The network's name.
        name: Option<String>,
    },
    /// Joining a network.
    Connecting,
    /// Not online.
    Offline,
}

/// What finishes when the setup hotspot opens or closes; the error is a line
/// for the log.
pub type HotspotFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;

/// A network the system UI shows, with the setup hotspot that lets a phone
/// reach the portal.
pub trait NetworkSource {
    /// The link now.
    fn state(&self) -> NetworkState;
    /// Whether the setup hotspot is open or opening.
    fn hotspot_open(&self) -> bool;
    /// How a phone reaches the portal while the hotspot is open, in one
    /// line.
    fn hotspot_hint(&self) -> String;
    /// Opens the setup hotspot.
    fn open_hotspot(&self) -> HotspotFuture<'_>;
    /// Closes the setup hotspot.
    fn close_hotspot(&self) -> HotspotFuture<'_>;
}

/// The network the system UI shows; published by the `app-manager` Plugin.
/// Without a source, as on a device that never goes online, the system UI
/// shows no network: no Wi-Fi or portal tiles and no offline notice.
#[derive(Default)]
pub struct SystemNetwork {
    source: RefCell<Option<Rc<dyn NetworkSource>>>,
}

impl SystemNetwork {
    /// Has the system UI show `source`. Call it while registering.
    pub fn show(&self, source: Rc<dyn NetworkSource>) {
        self.source.replace(Some(source));
    }

    /// The network shown, if a Plugin shows one.
    pub(crate) fn source(&self) -> Option<Rc<dyn NetworkSource>> {
        self.source.borrow().clone()
    }
}
