//! Projects the selected Board's network links into the typed tokens and the
//! macro the ESP32-P4 Platform entry takes them with. Shared by `build.rs`
//! and the Platform's host tests.

use std::error::Error;

use barracuda_board_config::{EthernetDefinition, RmiiReferenceClockSource};

/// Network tokens of a Board that declares no link this Platform drives.
pub const NO_NETWORK_TOKENS: &str =
    "/// Chip resources the selected Board's network links consume: it declares none.
pub struct NetworkTokens;

/// Takes the selected Board's network-link tokens out of the chip peripherals.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_esp32p4_network_tokens {
    ($peripherals:ident) => {
        $crate::application::NetworkTokens
    };
}
";

/// Returns `name` when it is a chip-native token name, such as `GPIO31`, that
/// can be spliced into generated Rust.
fn token(name: &str) -> Result<&str, Box<dyn Error>> {
    let mut characters = name.chars();
    let valid = characters
        .next()
        .is_some_and(|first| first.is_ascii_uppercase())
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        });
    if valid {
        Ok(name)
    } else {
        Err(format!("Board Ethernet pin `{name}` is not an ESP32-P4 pin name").into())
    }
}

/// Projects the Board's RMII Ethernet wiring into the typed tokens and the
/// macro the Platform entry takes them with.
///
/// The EMAC pads are fixed IO_MUX functions; esp-hal's pin traits reject a
/// Board pin that cannot carry its signal when the firmware is compiled.
pub fn ethernet_tokens(ethernet: &EthernetDefinition) -> Result<String, Box<dyn Error>> {
    let rmii = ethernet.rmii();
    let clock = rmii.reference_clock();
    // (parameter, pin) in `NetworkTokens::new` order.
    let mut pins: Vec<(&str, &str)> = vec![("clock_input", token(clock.input())?)];
    let clock_type = match (clock.source(), clock.output()) {
        (RmiiReferenceClockSource::External, _) => format!(
            "::esp_hal::ethernet::clock::ExternalRefClock<::esp_hal::peripherals::{}<'static>>",
            clock.input()
        ),
        (RmiiReferenceClockSource::Internal, Some(output)) => {
            pins.push(("clock_output", token(output)?));
            format!(
                "::esp_hal::ethernet::clock::InternalRefClock<::esp_hal::peripherals::{output}<'static>, ::esp_hal::peripherals::{}<'static>>",
                clock.input()
            )
        }
        (RmiiReferenceClockSource::Internal, None) => {
            return Err("an internal RMII reference clock needs its output pin".into());
        }
    };
    let clock_value = if clock.output().is_some() {
        "::esp_hal::ethernet::clock::InternalRefClock::new(clock_output, clock_input)"
    } else {
        "::esp_hal::ethernet::clock::ExternalRefClock::new(clock_input)"
    };
    let signals = [
        ("rxd0", rmii.rxd0()),
        ("rxd1", rmii.rxd1()),
        ("rx_dv", rmii.crs_dv()),
        ("txd0", rmii.txd0()),
        ("txd1", rmii.txd1()),
        ("tx_en", rmii.tx_en()),
        ("mdc", rmii.mdc()),
        ("mdio", rmii.mdio()),
    ];
    for (parameter, pin) in signals {
        pins.push((parameter, token(pin)?));
    }
    let reset = ethernet.phy().reset().map(token).transpose()?;
    if let Some(reset) = reset {
        pins.push(("phy_reset", reset));
    }

    let bundle_types = signals
        .iter()
        .map(|(_, pin)| format!("::esp_hal::peripherals::{pin}<'static>"))
        .collect::<Vec<_>>()
        .join(", ");
    let parameters = pins
        .iter()
        .map(|(parameter, pin)| format!("{parameter}: ::esp_hal::peripherals::{pin}<'static>"))
        .collect::<Vec<_>>()
        .join(", ");
    let fields = signals
        .iter()
        .map(|(parameter, _)| *parameter)
        .collect::<Vec<_>>()
        .join(", ");
    let reset_value = if reset.is_some() {
        "Some(::esp_hal::gpio::Pin::degrade(phy_reset))"
    } else {
        "None"
    };
    let arguments = pins
        .iter()
        .map(|(_, pin)| format!("$peripherals.{pin}"))
        .collect::<Vec<_>>()
        .join(", ");
    let address = ethernet.phy().address();
    let model = ethernet.phy().model();
    Ok(format!(
        "/// RMII pads the selected Board wires to its `{model}` Ethernet PHY.
type BoardRmiiPins = ::esp_hal::ethernet::RmiiPinBundle<{clock_type}, {bundle_types}>;

/// Chip resources the selected Board's wired Ethernet consumes.
pub struct NetworkTokens(crate::network::ethernet::EthernetTokens<BoardRmiiPins>);

impl NetworkTokens {{
    /// Collects the EMAC and the Board's RMII pins.
    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(eth: ::esp_hal::peripherals::ETH<'static>, {parameters}) -> Self {{
        Self(crate::network::ethernet::EthernetTokens {{
            eth,
            pins: ::esp_hal::ethernet::RmiiPinBundle {{ clock: {clock_value}, {fields} }},
            phy_reset: {reset_value},
            phy_address: {address},
        }})
    }}

    pub(crate) fn into_ethernet(self) -> crate::network::ethernet::EthernetTokens<BoardRmiiPins> {{
        self.0
    }}
}}

/// Takes the selected Board's network-link tokens out of the chip peripherals.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_esp32p4_network_tokens {{
    ($peripherals:ident) => {{
        $crate::application::NetworkTokens::new($peripherals.ETH, {arguments})
    }};
}}
"
    ))
}
