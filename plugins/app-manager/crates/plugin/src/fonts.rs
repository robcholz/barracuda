//! Where the UI kit gets the system fonts.

use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::{cell::RefCell, future::Future, pin::Pin};

/// Loads the system font atlases, each a baked `font-<slot>.bin` image.
pub trait FontAtlases {
    /// Reads every atlas; one that cannot be read is left out.
    fn load(&self) -> Pin<Box<dyn Future<Output = Vec<Vec<u8>>> + '_>>;
}

/// The system fonts' source: a Plugin that ships them, such as
/// `system-fonts`. They are large, so a Board carries them only when it has a
/// display and enables that Plugin; without them, views draw only with the
/// atlases they ship.
#[derive(Default)]
pub struct SystemFonts {
    source: RefCell<Option<Rc<dyn FontAtlases>>>,
}

impl SystemFonts {
    /// Has the UI kit draw with the atlases `source` loads when the display
    /// starts. Call it while registering.
    pub fn load_from(&self, source: Rc<dyn FontAtlases>) {
        self.source.replace(Some(source));
    }

    /// The system font atlases, none when no Plugin ships them.
    pub(crate) async fn load(&self) -> Vec<Vec<u8>> {
        let source = self.source.borrow().clone();
        match source {
            Some(source) => source.load().await,
            None => Vec::new(),
        }
    }
}
