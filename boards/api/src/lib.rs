//! Platform-neutral Board identity and native-storage bindings generated from YAML.

#![no_std]

/// One concrete product's fixed, platform-neutral settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Board {
    name: &'static str,
    hardware: Hardware,
    storage: Storage,
}

impl Board {
    /// Creates a static Board description generated from YAML.
    #[must_use]
    pub const fn new(name: &'static str, hardware: Hardware, storage: Storage) -> Self {
        Self {
            name,
            hardware,
            storage,
        }
    }

    /// Returns the stable Board name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the concrete hardware identity declared by this Board.
    #[must_use]
    pub const fn hardware(&self) -> &Hardware {
        &self.hardware
    }

    /// Returns logical roles bound to names in the selected Platform's native layout.
    #[must_use]
    pub const fn storage(&self) -> &Storage {
        &self.storage
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

/// Logical storage roles mapped to labels in a Platform-native layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storage {
    filesystem: &'static str,
    web_assets: Option<&'static str>,
    database: &'static str,
}

impl Storage {
    /// Creates a logical storage mapping.
    #[must_use]
    pub const fn new(
        filesystem: &'static str,
        web_assets: Option<&'static str>,
        database: &'static str,
    ) -> Self {
        Self {
            filesystem,
            web_assets,
            database,
        }
    }

    /// Returns the mutable filesystem partition name.
    #[must_use]
    pub const fn filesystem(&self) -> &'static str {
        self.filesystem
    }

    /// Returns the optional read-only Web asset partition name.
    #[must_use]
    pub const fn web_assets(&self) -> Option<&'static str> {
        self.web_assets
    }

    /// Returns the system database partition name.
    #[must_use]
    pub const fn database(&self) -> &'static str {
        self.database
    }
}
