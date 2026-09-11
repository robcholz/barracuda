//! Concrete Board hardware identity and native-layout selection generated from YAML.

#![no_std]

/// One concrete product's fixed, platform-neutral settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Board {
    info: BoardInfo,
    native_layout: NativeLayout,
}

impl Board {
    /// Creates a static Board description generated from YAML.
    #[must_use]
    pub const fn new(name: &'static str, hardware: Hardware, native_layout: NativeLayout) -> Self {
        Self {
            info: BoardInfo::new(name, hardware),
            native_layout,
        }
    }

    /// Returns the fixed identity exported to runtime observers.
    #[must_use]
    pub const fn info(&self) -> BoardInfo {
        self.info
    }

    /// Returns the stable Board name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.info.name()
    }

    /// Returns the concrete hardware identity declared by this Board.
    #[must_use]
    pub const fn hardware(&self) -> &Hardware {
        self.info.hardware()
    }

    /// Returns the native physical-layout artifact bundled with this Board.
    #[must_use]
    pub const fn native_layout(&self) -> &NativeLayout {
        &self.native_layout
    }
}

/// Fixed identity of one compiled Board matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardInfo {
    name: &'static str,
    hardware: Hardware,
}

impl BoardInfo {
    /// Creates one static Board descriptor.
    #[must_use]
    pub const fn new(name: &'static str, hardware: Hardware) -> Self {
        Self { name, hardware }
    }

    /// Returns the stable Board bundle name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the Board's concrete hardware identity.
    #[must_use]
    pub const fn hardware(&self) -> &Hardware {
        &self.hardware
    }
}

/// Hardware facts needed to check a Board against an independently selected Platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hardware {
    chip: &'static str,
}

impl Hardware {
    /// Creates a hardware identity from the canonical HAL chip name.
    #[must_use]
    pub const fn new(chip: &'static str) -> Self {
        Self { chip }
    }

    /// Returns the canonical HAL chip name.
    #[must_use]
    pub const fn chip(&self) -> &'static str {
        self.chip
    }
}

/// Board-bundled native physical-layout artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeLayout {
    artifact: &'static str,
}

impl NativeLayout {
    /// Creates a native-layout reference relative to the Board bundle.
    #[must_use]
    pub const fn new(artifact: &'static str) -> Self {
        Self { artifact }
    }

    /// Returns the artifact path relative to the Board bundle.
    #[must_use]
    pub const fn artifact(&self) -> &'static str {
        self.artifact
    }
}
