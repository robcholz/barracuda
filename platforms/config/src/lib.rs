//! Shared host-side resolution of Barracuda Platform identities.

use core::fmt;

/// Cargo target properties used to select a concrete Platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformTarget<'a> {
    triple: &'a str,
    os: &'a str,
    arch: &'a str,
}

impl<'a> PlatformTarget<'a> {
    /// Creates a target description from Cargo's `TARGET` and `CARGO_CFG_*` values.
    #[must_use]
    pub const fn new(triple: &'a str, os: &'a str, arch: &'a str) -> Self {
        Self { triple, os, arch }
    }
}

/// Failure while resolving a concrete Platform.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveError {
    /// No registered Platform supports this Cargo target.
    UnsupportedTarget {
        /// Cargo target triple.
        triple: String,
        /// Cargo target operating system.
        os: String,
        /// Cargo target architecture.
        arch: String,
    },
    /// A requested Platform name is unsafe or malformed.
    InvalidPlatformName(String),
    /// The requested Platform cannot compile for the Cargo target.
    IncompatibleTarget {
        /// Requested Platform name.
        platform: String,
        /// Cargo target triple.
        triple: String,
        /// Cargo target operating system.
        os: String,
        /// Cargo target architecture.
        arch: String,
    },
    /// No registered Platform supports this Board chip.
    UnsupportedBoardChip(String),
    /// The Board chip and its toolchain target select different Platforms.
    IncompatibleBoardTarget {
        /// Board chip identifier.
        chip: String,
        /// Platform registered for the Board chip.
        chip_platform: String,
        /// Board toolchain target.
        target: String,
        /// Platform registered for the toolchain target.
        target_platform: String,
    },
    /// A device Board does not declare the target needed to resolve its Platform.
    MissingBoardTarget(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget { triple, os, arch } => write!(
                formatter,
                "no Barracuda Platform is registered for target `{triple}` (OS `{os}`, architecture `{arch}`)"
            ),
            Self::InvalidPlatformName(name) => {
                write!(formatter, "invalid Platform name `{name}`")
            }
            Self::IncompatibleTarget {
                platform,
                triple,
                os,
                arch,
            } => write!(
                formatter,
                "Platform `{platform}` cannot be built for target `{triple}` (OS `{os}`, architecture `{arch}`)"
            ),
            Self::UnsupportedBoardChip(chip) => {
                write!(formatter, "no Barracuda Platform supports Board chip `{chip}`")
            }
            Self::IncompatibleBoardTarget {
                chip,
                chip_platform,
                target,
                target_platform,
            } => write!(
                formatter,
                "Board chip `{chip}` selects Platform `{chip_platform}`, but target `{target}` selects Platform `{target_platform}`"
            ),
            Self::MissingBoardTarget(chip) => write!(
                formatter,
                "Board chip `{chip}` requires a toolchain target to select its Platform"
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolves the concrete Platform for one Cargo target.
///
/// `requested` carries the optional existing `BARRACUDA_PLATFORM` override.
/// The override is validated against the same target as the default.
///
/// # Errors
///
/// Returns [`ResolveError`] when the target is unsupported, the override name
/// is invalid, or the override is incompatible with the target.
pub fn resolve_platform(
    target: PlatformTarget<'_>,
    requested: Option<&str>,
) -> Result<String, ResolveError> {
    let default = default_platform(target)?;
    let platform = requested.unwrap_or(default);
    validate_platform_name(platform)?;
    if platform_supports_target(platform, target) {
        Ok(platform.to_owned())
    } else {
        Err(ResolveError::IncompatibleTarget {
            platform: platform.to_owned(),
            triple: target.triple.to_owned(),
            os: target.os.to_owned(),
            arch: target.arch.to_owned(),
        })
    }
}

/// Resolves a Platform from the selected Board's hardware and toolchain fields.
///
/// Host Boards (`macos` and `linux`) do not require an explicit toolchain.
/// Device Boards must declare a target, and its Platform must agree with the
/// Platform registered for the Board chip.
///
/// # Errors
///
/// Returns [`ResolveError`] when the Board chip or target is unsupported, a
/// device target is missing, or the two fields resolve to different Platforms.
pub fn resolve_board_platform(
    chip: &str,
    toolchain_target: Option<&str>,
) -> Result<String, ResolveError> {
    let chip_platform = platform_for_chip(chip)?;
    let Some(target) = toolchain_target else {
        return if matches!(chip_platform, "macos" | "linux") {
            Ok(chip_platform.to_owned())
        } else {
            Err(ResolveError::MissingBoardTarget(chip.to_owned()))
        };
    };
    let target_platform =
        platform_for_embedded_target(target).ok_or_else(|| ResolveError::UnsupportedTarget {
            triple: target.to_owned(),
            os: String::from("none"),
            arch: embedded_arch(target).unwrap_or("unknown").to_owned(),
        })?;
    if chip_platform == target_platform {
        Ok(chip_platform.to_owned())
    } else {
        Err(ResolveError::IncompatibleBoardTarget {
            chip: chip.to_owned(),
            chip_platform: chip_platform.to_owned(),
            target: target.to_owned(),
            target_platform: target_platform.to_owned(),
        })
    }
}

fn default_platform(target: PlatformTarget<'_>) -> Result<&'static str, ResolveError> {
    match (target.os, target.arch) {
        ("macos", _) => Ok("macos"),
        ("linux", _) => Ok("linux"),
        _ => platform_for_embedded_target(target.triple).ok_or_else(|| {
            ResolveError::UnsupportedTarget {
                triple: target.triple.to_owned(),
                os: target.os.to_owned(),
                arch: target.arch.to_owned(),
            }
        }),
    }
}

fn platform_supports_target(platform: &str, target: PlatformTarget<'_>) -> bool {
    match platform {
        "macos" => target.os == "macos",
        "linux" => target.os == "linux",
        platform => platform_for_embedded_target(target.triple) == Some(platform),
    }
}

fn platform_for_embedded_target(target: &str) -> Option<&'static str> {
    match target {
        "xtensa-esp32-none-elf" => Some("esp32"),
        "xtensa-esp32s2-none-elf" => Some("esp32s2"),
        "xtensa-esp32s3-none-elf" => Some("esp32s3"),
        "riscv32imc-unknown-none-elf" => Some("esp32c3"),
        "riscv32imac-unknown-none-elf" => Some("esp32c6"),
        "riscv32imafc-unknown-none-elf" => Some("esp32p4"),
        target if target.starts_with("thumb") && target.ends_with("-none-eabi") => Some("stm32"),
        target if target.starts_with("thumb") && target.ends_with("-none-eabihf") => Some("stm32"),
        _ => None,
    }
}

fn embedded_arch(target: &str) -> Option<&'static str> {
    match target {
        target if target.starts_with("xtensa-") => Some("xtensa"),
        target if target.starts_with("riscv32") => Some("riscv32"),
        target if target.starts_with("thumb") => Some("arm"),
        _ => None,
    }
}

fn platform_for_chip(chip: &str) -> Result<&'static str, ResolveError> {
    match chip {
        "macos" => Ok("macos"),
        "linux" => Ok("linux"),
        "esp32" => Ok("esp32"),
        "esp32s2" => Ok("esp32s2"),
        "esp32s3" => Ok("esp32s3"),
        "esp32c3" => Ok("esp32c3"),
        "esp32c6" => Ok("esp32c6"),
        "esp32p4" => Ok("esp32p4"),
        chip if chip.starts_with("stm32") => Ok("stm32"),
        _ => Err(ResolveError::UnsupportedBoardChip(chip.to_owned())),
    }
}

fn validate_platform_name(value: &str) -> Result<(), ResolveError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(ResolveError::InvalidPlatformName(value.to_owned()))
    }
}
