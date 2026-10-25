//! The fixed lines the Wi-Fi Plugin writes to the console for a setup tool,
//! such as the web installer that flashed the board and still holds its USB
//! serial port.
//!
//! Each line is `BARRACUDA wifi=<state>` followed by `key=value` fields. The
//! SSID, when present, is the last field and runs to the end of the line.

use core::fmt;
use core::net::Ipv4Addr;

use barracuda_webserver_plugin::WEB_SERVER_PORT;

use crate::{SETUP_AP_ADDRESS, WifiControlErrorKind};

/// One Wi-Fi setup event, as a console line.
pub(crate) enum SetupLine<'a> {
    /// The setup access point accepts stations.
    Hotspot { ssid: &'a str },
    /// The station is joining a network.
    Connecting { ssid: &'a str },
    /// The station could not join a network.
    Failed {
        ssid: &'a str,
        reason: WifiControlErrorKind,
    },
    /// The station's network gave it an address; the portal is served there.
    Online { address: Ipv4Addr },
}

impl SetupLine<'_> {
    /// Writes the line through the Platform's console.
    pub(crate) fn emit(&self) {
        log::info!("{self}");
    }
}

impl fmt::Display for SetupLine<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BARRACUDA wifi=")?;
        match self {
            Self::Hotspot { ssid } => write!(
                formatter,
                "hotspot ip={SETUP_AP_ADDRESS} ssid={}",
                OneLine(ssid)
            ),
            Self::Connecting { ssid } => write!(formatter, "connecting ssid={}", OneLine(ssid)),
            Self::Failed { ssid, reason } => write!(
                formatter,
                "failed reason={} ssid={}",
                reason_code(*reason),
                OneLine(ssid)
            ),
            Self::Online { address } => {
                write!(formatter, "online ip={address} port={WEB_SERVER_PORT}")
            }
        }
    }
}

/// The same code the `PUT /api/wifi` error body carries.
const fn reason_code(kind: WifiControlErrorKind) -> &'static str {
    match kind {
        WifiControlErrorKind::InvalidConfiguration => "invalid_configuration",
        WifiControlErrorKind::Storage => "storage",
        WifiControlErrorKind::Device => "connection_failed",
        WifiControlErrorKind::Unsupported => "unsupported",
    }
}

/// An SSID with its control characters dropped, so it cannot break the line.
struct OneLine<'a>(&'a str);

impl fmt::Display for OneLine<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .chars()
            .filter(|character| !character.is_control())
            .try_for_each(|character| fmt::Write::write_char(formatter, character))
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use core::net::Ipv4Addr;

    use super::SetupLine;
    use crate::WifiControlErrorKind;

    #[test]
    fn setup_lines_keep_their_fixed_format() {
        assert_eq!(
            SetupLine::Hotspot {
                ssid: "Barracuda-B2C3"
            }
            .to_string(),
            "BARRACUDA wifi=hotspot ip=192.168.4.1 ssid=Barracuda-B2C3"
        );
        assert_eq!(
            SetupLine::Connecting { ssid: "Home Net" }.to_string(),
            "BARRACUDA wifi=connecting ssid=Home Net"
        );
        assert_eq!(
            SetupLine::Failed {
                ssid: "HomeNet",
                reason: WifiControlErrorKind::Device,
            }
            .to_string(),
            "BARRACUDA wifi=failed reason=connection_failed ssid=HomeNet"
        );
        assert_eq!(
            SetupLine::Online {
                address: Ipv4Addr::new(192, 168, 1, 42)
            }
            .to_string(),
            "BARRACUDA wifi=online ip=192.168.1.42 port=8787"
        );
    }

    #[test]
    fn an_ssid_cannot_break_its_line() {
        assert_eq!(
            SetupLine::Connecting {
                ssid: "Home\nBARRACUDA wifi=online"
            }
            .to_string(),
            "BARRACUDA wifi=connecting ssid=HomeBARRACUDA wifi=online"
        );
    }
}
