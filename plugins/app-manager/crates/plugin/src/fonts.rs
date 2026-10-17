//! Where the UI kit reads the system fonts.

use barracuda_vfs::ScopedVfs;
use core::cell::RefCell;

/// The system fonts' source: the filesystem of the Plugin that ships them in
/// its `/resources/fonts`, such as `system-fonts`. They are large, so a
/// Board carries them only when it has a display and enables that Plugin;
/// without them, views draw only with the atlases they ship.
#[derive(Default)]
pub struct SystemFonts {
    source: RefCell<Option<ScopedVfs>>,
}

impl SystemFonts {
    /// Reads the system fonts from `filesystem`'s `/resources/fonts` when
    /// the display starts. Call it while registering.
    pub fn read_from(&self, filesystem: ScopedVfs) {
        self.source.replace(Some(filesystem));
    }

    /// Where the system fonts are, if a Plugin ships them.
    pub(crate) fn source(&self) -> Option<ScopedVfs> {
        self.source.borrow().clone()
    }
}
