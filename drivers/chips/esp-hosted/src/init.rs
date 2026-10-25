//! The start-up handshake on the private interface.
//!
//! Once the host opens the data path, the co-processor sends one private
//! packet of type `ESP_PACKET_TYPE_EVENT` (0x33) carrying an
//! `ESP_PRIV_EVENT_INIT` (0x22) event: a TLV list of its capabilities, chip,
//! and, from ESP-Hosted 3.x, its firmware and RPC versions and SDIO buffer
//! layout. The host answers with its own INIT event (tags 0x44-0x48, and the
//! RPC version 0x1A only when the co-processor advertised one, as the 3.x
//! host does), after which RPC and data traffic may flow.
//!
//! Tags and values follow ESP-Hosted-MCU `esp_hosted_transport_init.h`
//! (0.0.6 to 2.x) and `eh_tlv_tags.h` (3.x).

use core::fmt;

/// Event type of the INIT event in both directions.
pub const EVENT_INIT: u8 = 0x22;
/// Private packet type of an event sent by the co-processor.
pub const PACKET_TYPE_EVENT: u8 = 0x33;

const TAG_CAPABILITY: u8 = 0x11;
const TAG_CHIP_ID: u8 = 0x12;
const TAG_RAW_THROUGHPUT: u8 = 0x13;
const TAG_RX_QUEUE_SIZE: u8 = 0x14;
const TAG_TX_QUEUE_SIZE: u8 = 0x15;
const TAG_EXTENDED_CAPABILITY: u8 = 0x16;
const TAG_FIRMWARE_VERSION: u8 = 0x17;
const TAG_SDIO_MODE: u8 = 0x18;
const TAG_RPC_VERSION: u8 = 0x1a;
const TAG_SDIO_BUFFER_CONFIG: u8 = 0x1b;

const TAG_HOST_CAPABILITIES: u8 = 0x44;
const TAG_HOST_CHIP_ID: u8 = 0x45;
const TAG_HOST_RAW_THROUGHPUT: u8 = 0x46;
const TAG_HOST_THROTTLE_HIGH: u8 = 0x47;
const TAG_HOST_THROTTLE_LOW: u8 = 0x48;

/// `ESP_WLAN_SDIO_SUPPORT`: the co-processor carries Wi-Fi over SDIO.
pub const CAPABILITY_WLAN_SDIO: u8 = 1 << 0;
/// `ESP_CHECKSUM_ENABLED`: packets from the co-processor carry checksums.
pub const CAPABILITY_CHECKSUM: u8 = 1 << 7;
/// `ESP_WLAN_SUPPORT` in the extended capabilities.
pub const EXTENDED_CAPABILITY_WLAN: u32 = 1 << 4;

/// The only RPC dialect this host speaks: protobuf `Rpc` messages on the
/// `RPCRsp`/`RPCEvt` endpoints (`ESP_HOSTED_RPC_VERSION_V2`, the MCU RPC of
/// every ESP-Hosted-MCU release).
pub const RPC_VERSION: u8 = 2;

/// Chip identifiers the co-processor reports (`ESP_PRIV_FIRMWARE_CHIP_*`).
pub const CHIP_ESP32C6: u8 = 0x0d;

/// Wi-Fi flow-control thresholds the host asks for, in percent of the
/// co-processor's queue: ESP-Hosted's SDIO Kconfig defaults
/// (`ESP_HOSTED_PRIV_WIFI_TX_SDIO_HIGH_THRESHOLD`, `..._LOW_THRESHOLD`).
pub const THROTTLE_HIGH_PERCENT: u8 = 80;
/// See [`THROTTLE_HIGH_PERCENT`].
pub const THROTTLE_LOW_PERCENT: u8 = 60;

/// How the co-processor moves frames from itself to the host over SDIO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdioMode {
    /// One packet per transfer or a stream of packets back to back.
    Packets,
    /// ESP-Hosted 3.x software aggregation: 4-byte aligned packets in one
    /// transfer whose size is at most `host_to_device_bytes`, credited per
    /// transfer.
    Aggregated {
        /// The co-processor's receive buffer, the credit unit for host
        /// transfers.
        host_to_device_bytes: u32,
        /// The largest transfer the co-processor sends.
        device_to_host_bytes: u32,
    },
}

/// The co-processor firmware version from a 3.x INIT event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareVersion {
    /// Major version.
    pub major: u8,
    /// Minor version.
    pub minor: u8,
    /// Patch version.
    pub patch: u8,
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What the co-processor announced in its INIT event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlaveInit {
    capabilities: u8,
    extended_capabilities: Option<u32>,
    chip_id: Option<u8>,
    firmware_version: Option<u32>,
    rpc_version: Option<u8>,
    sdio_streaming: Option<u8>,
    sdio_buffer_config: Option<[u8; 5]>,
    rx_queue_size: Option<u8>,
    tx_queue_size: Option<u8>,
    raw_throughput: Option<u8>,
}

impl SlaveInit {
    /// Parses the payload of a private packet.
    ///
    /// # Errors
    ///
    /// Returns an error for a payload that is not an INIT event or whose TLV
    /// list is malformed.
    pub fn parse(payload: &[u8]) -> Result<Self, InitError> {
        let (&event, rest) = payload.split_first().ok_or(InitError::Truncated)?;
        if event != EVENT_INIT {
            return Err(InitError::NotInit(event));
        }
        let (&length, rest) = rest.split_first().ok_or(InitError::Truncated)?;
        let mut tlvs = rest
            .get(..usize::from(length))
            .ok_or(InitError::Truncated)?;
        let mut init = Self::default();
        while let [tag, length, rest @ ..] = tlvs {
            let value = rest
                .get(..usize::from(*length))
                .ok_or(InitError::Truncated)?;
            let byte = value.first().copied();
            let word = value
                .get(..4)
                .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
            match *tag {
                TAG_CAPABILITY => init.capabilities = byte.unwrap_or_default(),
                TAG_CHIP_ID => init.chip_id = byte,
                TAG_RAW_THROUGHPUT => init.raw_throughput = byte,
                TAG_RX_QUEUE_SIZE => init.rx_queue_size = byte,
                TAG_TX_QUEUE_SIZE => init.tx_queue_size = byte,
                TAG_EXTENDED_CAPABILITY => init.extended_capabilities = word,
                TAG_FIRMWARE_VERSION => init.firmware_version = word,
                TAG_SDIO_MODE => init.sdio_streaming = byte,
                TAG_RPC_VERSION => init.rpc_version = byte,
                TAG_SDIO_BUFFER_CONFIG => {
                    init.sdio_buffer_config = Some(
                        value
                            .try_into()
                            .map_err(|_| InitError::MalformedSdioBufferConfig)?,
                    );
                }
                // Unknown tags are skipped, as every ESP-Hosted host does.
                _ => {}
            }
            tlvs = &rest[usize::from(*length)..];
        }
        if !tlvs.is_empty() {
            return Err(InitError::Truncated);
        }
        Ok(init)
    }

    /// Returns the basic capability bits (`ESP_CAPABILITIES`).
    #[must_use]
    pub const fn capabilities(&self) -> u8 {
        self.capabilities
    }

    /// Returns the extended capability bits, sent from ESP-Hosted 2.x.
    #[must_use]
    pub const fn extended_capabilities(&self) -> Option<u32> {
        self.extended_capabilities
    }

    /// Returns the co-processor chip identifier.
    #[must_use]
    pub const fn chip_id(&self) -> Option<u8> {
        self.chip_id
    }

    /// Returns the co-processor's queue depths toward and from the host.
    #[must_use]
    pub const fn queue_sizes(&self) -> (Option<u8>, Option<u8>) {
        (self.rx_queue_size, self.tx_queue_size)
    }

    /// Returns whether the co-processor runs a raw-throughput test.
    #[must_use]
    pub const fn raw_throughput(&self) -> Option<u8> {
        self.raw_throughput
    }

    /// Returns whether the co-processor firmware carries Wi-Fi.
    #[must_use]
    pub const fn wlan(&self) -> bool {
        self.capabilities & CAPABILITY_WLAN_SDIO != 0
            || match self.extended_capabilities {
                Some(extended) => extended & EXTENDED_CAPABILITY_WLAN != 0,
                None => false,
            }
    }

    /// Returns whether packets from the co-processor carry checksums.
    #[must_use]
    pub const fn checksum(&self) -> bool {
        self.capabilities & CAPABILITY_CHECKSUM != 0
    }

    /// Returns the firmware version a 3.x co-processor announces; earlier
    /// releases announce none.
    #[must_use]
    pub const fn firmware_version(&self) -> Option<FirmwareVersion> {
        match self.firmware_version {
            Some(value) => Some(FirmwareVersion {
                major: (value >> 16) as u8,
                minor: (value >> 8) as u8,
                patch: value as u8,
            }),
            None => None,
        }
    }

    /// Returns the RPC version the co-processor advertised, if any.
    #[must_use]
    pub const fn rpc_version(&self) -> Option<u8> {
        self.rpc_version
    }

    /// Returns how SDIO transfers are laid out.
    ///
    /// # Errors
    ///
    /// Returns an error for an SDIO buffer configuration this host cannot
    /// follow.
    pub fn sdio_mode(&self) -> Result<SdioMode, Incompatible> {
        let Some(
            [
                transport,
                to_host_mode,
                to_host_blocks,
                to_device_mode,
                to_device_blocks,
            ],
        ) = self.sdio_buffer_config
        else {
            return Ok(SdioMode::Packets);
        };
        // `eh_common_sdio_cfg.h`: transport 0 is SDIO; modes 0 software
        // aggregation, 1 stream, 2 packet; sizes in 512-byte units.
        if transport != 0 || to_host_blocks == 0 || to_device_blocks == 0 {
            return Err(Incompatible::SdioBufferConfig);
        }
        match (to_host_mode, to_device_mode) {
            (0, 0) => Ok(SdioMode::Aggregated {
                host_to_device_bytes: u32::from(to_device_blocks) * 512,
                device_to_host_bytes: u32::from(to_host_blocks) * 512,
            }),
            (1 | 2, 1 | 2) => Ok(SdioMode::Packets),
            _ => Err(Incompatible::SdioBufferConfig),
        }
    }

    /// Checks that this host can drive the co-processor that sent the event.
    ///
    /// # Errors
    ///
    /// Returns what is incompatible: a co-processor without Wi-Fi, another
    /// chip than the Board declares, an RPC dialect other than
    /// [`RPC_VERSION`], or an SDIO layout this host does not implement.
    pub fn check(&self, expected_chip: u8) -> Result<(), Incompatible> {
        match self.chip_id {
            Some(chip) if chip != expected_chip => {
                return Err(Incompatible::Chip {
                    expected: expected_chip,
                    reported: chip,
                });
            }
            None => return Err(Incompatible::NoChipId),
            Some(_) => {}
        }
        if let Some(version) = self.rpc_version
            && version != RPC_VERSION
        {
            return Err(Incompatible::RpcVersion(version));
        }
        if !self.wlan() {
            return Err(Incompatible::NoWlan);
        }
        self.sdio_mode().map(|_| ())
    }

    /// Writes the host's INIT event answering this one into `out` and returns
    /// its length.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Truncated`] when `out` is too small.
    pub fn host_reply(&self, out: &mut [u8]) -> Result<usize, InitError> {
        let tlvs = [
            (TAG_HOST_CAPABILITIES, 0),
            (TAG_HOST_CHIP_ID, self.chip_id.unwrap_or(0xff)),
            (TAG_HOST_RAW_THROUGHPUT, 0),
            (TAG_HOST_THROTTLE_HIGH, THROTTLE_HIGH_PERCENT),
            (TAG_HOST_THROTTLE_LOW, THROTTLE_LOW_PERCENT),
            (TAG_RPC_VERSION, RPC_VERSION),
        ];
        // The RPC version goes back only to a co-processor that announced
        // one: earlier co-processor parsers may not skip unknown tags.
        let count = if self.rpc_version.is_some() {
            tlvs.len()
        } else {
            tlvs.len() - 1
        };
        let event_length = count * 3;
        let out = out
            .get_mut(..event_length + 2)
            .ok_or(InitError::Truncated)?;
        out[0] = EVENT_INIT;
        out[1] = event_length as u8;
        for (chunk, (tag, value)) in out[2..].as_chunks_mut::<3>().0.iter_mut().zip(tlvs) {
            chunk.copy_from_slice(&[tag, 1, value]);
        }
        Ok(event_length + 2)
    }
}

/// A private packet is not a well-formed INIT event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// The event or one of its TLVs ends early.
    Truncated,
    /// The private packet carries another event type.
    NotInit(u8),
    /// The SDIO buffer configuration TLV is not 5 bytes.
    MalformedSdioBufferConfig,
}

impl fmt::Display for InitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("co-processor INIT event is truncated"),
            Self::NotInit(event) => {
                write!(
                    formatter,
                    "co-processor sent private event {event:#04x}, not INIT"
                )
            }
            Self::MalformedSdioBufferConfig => {
                formatter.write_str("co-processor SDIO buffer configuration is malformed")
            }
        }
    }
}

impl core::error::Error for InitError {}

/// The co-processor runs firmware this host cannot drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Incompatible {
    /// The INIT event names no chip.
    NoChipId,
    /// Another chip than the Board declares answered.
    Chip {
        /// The chip the Board declares.
        expected: u8,
        /// The chip the co-processor reported.
        reported: u8,
    },
    /// The co-processor speaks another RPC dialect.
    RpcVersion(u8),
    /// The co-processor firmware was built without Wi-Fi.
    NoWlan,
    /// The co-processor's SDIO buffer layout is unknown to this host.
    SdioBufferConfig,
}

impl fmt::Display for Incompatible {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoChipId => formatter.write_str("co-processor did not report its chip"),
            Self::Chip { expected, reported } => write!(
                formatter,
                "co-processor is chip {reported:#04x}, the Board declares {expected:#04x}"
            ),
            Self::RpcVersion(version) => write!(
                formatter,
                "co-processor speaks ESP-Hosted RPC version {version}; this host speaks version {RPC_VERSION}"
            ),
            Self::NoWlan => formatter.write_str("co-processor firmware has no Wi-Fi"),
            Self::SdioBufferConfig => formatter.write_str(
                "co-processor uses an SDIO transfer layout this host does not implement",
            ),
        }
    }
}

impl core::error::Error for Incompatible {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The INIT event of an ESP-Hosted-MCU 2.x C6 over SDIO, built as
    /// `generate_startup_event` in `slave/main/sdio_slave_api.c` builds it:
    /// capability (WLAN over SDIO), chip ID 0x0d, raw throughput off, queue
    /// sizes 20, extended capabilities (WLAN).
    const MCU_2X_INIT: [u8; 23] = [
        0x22, 21, //
        0x11, 1, 0x01, //
        0x12, 1, 0x0d, //
        0x13, 1, 0x00, //
        0x14, 1, 20, //
        0x15, 1, 20, //
        0x16, 4, 0x10, 0x00, 0x00, 0x00, //
    ];

    /// The INIT event of an ESP-Hosted 3.x C6 in software-aggregation mode,
    /// in `eh_tlv_pack_v2` order, then the SDIO mode and buffer TLVs
    /// (`eh_cp_transport_sdio.c`): firmware 3.0.9, RPC version 2, 4 KiB
    /// buffers both ways.
    const MCU_3X_INIT: [u8; 42] = [
        0x22, 40, //
        0x12, 1, 0x0d, //
        0x11, 1, 0x81, //
        0x13, 1, 0x00, //
        0x14, 1, 20, //
        0x15, 1, 20, //
        0x17, 4, 0x09, 0x00, 0x03, 0x00, //
        0x16, 4, 0x10, 0x00, 0x00, 0x00, //
        0x1a, 1, 0x02, //
        0x18, 1, 0x01, //
        0x1b, 5, 0x00, 0x00, 0x08, 0x00, 0x08, //
    ];

    #[test]
    fn a_2x_init_event_announces_wifi_on_a_c6_without_versions() {
        let init = SlaveInit::parse(&MCU_2X_INIT).unwrap();
        assert!(init.wlan());
        assert!(!init.checksum());
        assert_eq!(init.chip_id(), Some(CHIP_ESP32C6));
        assert_eq!(init.firmware_version(), None);
        assert_eq!(init.rpc_version(), None);
        assert_eq!(init.sdio_mode(), Ok(SdioMode::Packets));
        assert_eq!(init.check(CHIP_ESP32C6), Ok(()));
    }

    #[test]
    fn a_3x_init_event_announces_its_version_and_aggregation() {
        let init = SlaveInit::parse(&MCU_3X_INIT).unwrap();
        assert!(init.checksum());
        assert_eq!(
            init.firmware_version(),
            Some(FirmwareVersion {
                major: 3,
                minor: 0,
                patch: 9
            })
        );
        assert_eq!(init.rpc_version(), Some(2));
        assert_eq!(
            init.sdio_mode(),
            Ok(SdioMode::Aggregated {
                host_to_device_bytes: 4096,
                device_to_host_bytes: 4096,
            })
        );
        assert_eq!(init.check(CHIP_ESP32C6), Ok(()));
    }

    #[test]
    fn the_host_reply_matches_the_2x_send_slave_config_layout() {
        let init = SlaveInit::parse(&MCU_2X_INIT).unwrap();
        let mut out = [0; 32];
        let length = init.host_reply(&mut out).unwrap();
        assert_eq!(
            &out[..length],
            &[
                0x22, 15, //
                0x44, 1, 0, //
                0x45, 1, 0x0d, //
                0x46, 1, 0, //
                0x47, 1, 80, //
                0x48, 1, 60,
            ]
        );
    }

    #[test]
    fn the_host_echoes_the_rpc_version_only_to_a_co_processor_that_sent_one() {
        let init = SlaveInit::parse(&MCU_3X_INIT).unwrap();
        let mut out = [0; 32];
        let length = init.host_reply(&mut out).unwrap();
        assert_eq!(length, 20);
        assert_eq!(&out[17..20], &[0x1a, 1, 2]);
        assert_eq!(out[1], 18);
    }

    #[test]
    fn mismatches_are_reported_precisely() {
        let mut other_chip = MCU_2X_INIT;
        other_chip[7] = 0x17;
        assert_eq!(
            SlaveInit::parse(&other_chip).unwrap().check(CHIP_ESP32C6),
            Err(Incompatible::Chip {
                expected: 0x0d,
                reported: 0x17
            })
        );

        let mut other_rpc = MCU_3X_INIT;
        other_rpc[31] = 3;
        assert_eq!(
            SlaveInit::parse(&other_rpc).unwrap().check(CHIP_ESP32C6),
            Err(Incompatible::RpcVersion(3))
        );

        let mut no_wifi = MCU_2X_INIT;
        no_wifi[4] = 0x04;
        no_wifi[19] = 0x20;
        assert_eq!(
            SlaveInit::parse(&no_wifi).unwrap().check(CHIP_ESP32C6),
            Err(Incompatible::NoWlan)
        );
    }

    #[test]
    fn truncated_and_foreign_events_are_refused() {
        assert_eq!(
            SlaveInit::parse(&MCU_2X_INIT[..10]),
            Err(InitError::Truncated)
        );
        assert_eq!(SlaveInit::parse(&[0x23, 0]), Err(InitError::NotInit(0x23)));
    }
}
