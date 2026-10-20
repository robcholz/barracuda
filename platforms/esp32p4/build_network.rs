//! Projects the selected Board's network links into the typed tokens and the
//! macros the ESP32-P4 Platform entry takes them with. Shared by `build.rs`
//! and the Platform's host tests.

use std::error::Error;

use barracuda_board_config::{
    CoprocessorPowerExpander, EthernetDefinition, RmiiReferenceClockSource,
    WifiCoprocessorDefinition, WifiCoprocessorFirmware,
};

/// The macro that powers the selected Board's Wi-Fi co-processor before the
/// Board takes its buses: a Board without a switched co-processor does
/// nothing.
const NO_POWER: &str = "
/// Powers the selected Board's network co-processor: it has none to switch.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_esp32p4_network_power {
    ($peripherals:ident) => {};
}
";

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

/// Powers the selected Board's network co-processor: it has none to switch.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_esp32p4_network_power {
    ($peripherals:ident) => {};
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
        Err(format!("Board network pin `{name}` is not an ESP32-P4 pin name").into())
    }
}

/// The `ESP_PRIV_FIRMWARE_CHIP_*` value an ESP-Hosted co-processor of `chip`
/// reports (ESP-Hosted-MCU `transport_drv.h`).
fn hosted_chip_id(chip: &str) -> Result<u8, Box<dyn Error>> {
    Ok(match chip {
        "esp32" => 0x00,
        "esp32s2" => 0x02,
        "esp32c3" => 0x05,
        "esp32s3" => 0x09,
        "esp32c2" => 0x0c,
        "esp32c6" => 0x0d,
        "esp32c5" => 0x17,
        other => {
            return Err(format!("ESP-Hosted has no co-processor build for `{other}`").into());
        }
    })
}

/// Generated Rust for one link: the constructor parameters, the struct
/// field and its initializer, the peripheral each parameter takes, and the
/// items the link needs.
#[derive(Default)]
struct Projection {
    parameters: Vec<String>,
    fields: Vec<String>,
    initializers: Vec<String>,
    arguments: Vec<String>,
    items: String,
}

fn project_ethernet(ethernet: &EthernetDefinition) -> Result<Projection, Box<dyn Error>> {
    let rmii = ethernet.rmii();
    let clock = rmii.reference_clock();
    // (parameter, pin) in constructor order.
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
    // In esp-hal's `RmiiPinBundle` field order.
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
    let bundle_fields = signals
        .iter()
        .map(|(parameter, _)| *parameter)
        .collect::<Vec<_>>()
        .join(", ");
    let reset_value = if reset.is_some() {
        "Some(::esp_hal::gpio::Pin::degrade(phy_reset))"
    } else {
        "None"
    };
    let mut projection = Projection {
        parameters: vec![String::from("eth: ::esp_hal::peripherals::ETH<'static>")],
        arguments: vec![String::from("$peripherals.ETH")],
        ..Projection::default()
    };
    for (parameter, pin) in &pins {
        projection.parameters.push(format!(
            "{parameter}: ::esp_hal::peripherals::{pin}<'static>"
        ));
        projection.arguments.push(format!("$peripherals.{pin}"));
    }
    projection.fields.push(String::from(
        "    /// The wired Ethernet.\n    pub(crate) ethernet: crate::network::ethernet::EthernetTokens<BoardRmiiPins>,",
    ));
    projection.initializers.push(format!(
        "ethernet: crate::network::ethernet::EthernetTokens {{
                eth,
                pins: ::esp_hal::ethernet::RmiiPinBundle {{ clock: {clock_value}, {bundle_fields} }},
                phy_reset: {reset_value},
                phy_address: {address},
            }}",
        address = ethernet.phy().address(),
    ));
    projection.items = format!(
        "/// RMII pads the selected Board wires to its `{model}` Ethernet PHY.
type BoardRmiiPins = ::esp_hal::ethernet::RmiiPinBundle<{clock_type}, {bundle_types}>;
",
        model = ethernet.phy().model(),
    );
    Ok(projection)
}

fn project_coprocessor(
    coprocessor: &WifiCoprocessorDefinition,
) -> Result<(Projection, String), Box<dyn Error>> {
    match coprocessor.firmware() {
        WifiCoprocessorFirmware::EspHostedMcu => {}
    }
    let chip = hosted_chip_id(coprocessor.chip())?;
    let sdio = coprocessor.sdio();
    // Slot 0 of the ESP32-P4 SD host is wired to fixed IO_MUX pads for SD
    // cards; slot 1 reaches any pin through the GPIO matrix, which is where
    // Boards put their co-processor.
    if sdio.slot() != 1 {
        return Err(format!(
            "the ESP32-P4 Platform drives a Wi-Fi co-processor on SD host slot 1, not slot {}",
            sdio.slot()
        )
        .into());
    }
    let [d0, d1, d2, d3] = sdio.data();
    let pins = [
        ("sdio_clk", token(sdio.clk())?),
        ("sdio_cmd", token(sdio.cmd())?),
        ("sdio_d0", token(d0)?),
        ("sdio_d1", token(d1)?),
        ("sdio_d2", token(d2)?),
        ("sdio_d3", token(d3)?),
        ("coprocessor_reset", token(coprocessor.reset())?),
    ];
    let mut projection = Projection {
        parameters: vec![String::from(
            "sdhost: ::esp_hal::peripherals::SDHOST<'static>",
        )],
        arguments: vec![String::from("$peripherals.SDHOST")],
        ..Projection::default()
    };
    for (parameter, pin) in pins {
        projection.parameters.push(format!(
            "{parameter}: ::esp_hal::peripherals::{pin}<'static>"
        ));
        projection.arguments.push(format!("$peripherals.{pin}"));
    }
    projection.fields.push(format!(
        "    /// The `{}` Wi-Fi co-processor on SDIO.\n    pub(crate) coprocessor: crate::network::coprocessor::CoprocessorTokens,",
        coprocessor.chip()
    ));
    projection.initializers.push(format!(
        "coprocessor: crate::network::coprocessor::CoprocessorTokens {{
                sdhost,
                clk: ::esp_hal::gpio::Pin::degrade(sdio_clk),
                cmd: ::esp_hal::gpio::Pin::degrade(sdio_cmd),
                data: [
                    ::esp_hal::gpio::Pin::degrade(sdio_d0),
                    ::esp_hal::gpio::Pin::degrade(sdio_d1),
                    ::esp_hal::gpio::Pin::degrade(sdio_d2),
                    ::esp_hal::gpio::Pin::degrade(sdio_d3),
                ],
                reset: ::esp_hal::gpio::Pin::degrade(coprocessor_reset),
                frequency_hz: {frequency},
                chip: {chip:#04x},
            }}",
        frequency = sdio.frequency_hz(),
    ));

    let power = match coprocessor.power_enable() {
        None => String::from(NO_POWER),
        Some(power) => {
            match power.expander() {
                CoprocessorPowerExpander::Pi4ioe5v6408 => {}
            }
            format!(
                "
/// Powers the selected Board's Wi-Fi co-processor through output {pin} of
/// the PI4IOE5V6408 at {address:#04x}, before the Board takes the bus.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_esp32p4_network_power {{
    ($peripherals:ident) => {{
        $crate::application::power_coprocessor_through_pi4ioe5v6408(
            $peripherals.{i2c}.reborrow(),
            $peripherals.{scl}.reborrow(),
            $peripherals.{sda}.reborrow(),
            {address},
            {pin},
        );
    }};
}}
",
                i2c = token(power.i2c_peripheral())?,
                scl = token(power.scl())?,
                sda = token(power.sda())?,
                address = power.address(),
                pin = power.pin(),
            )
        }
    };
    Ok((projection, power))
}

/// Projects the Board's network links into the typed tokens and the macros
/// the Platform entry takes them with.
///
/// The EMAC pads are fixed IO_MUX functions; esp-hal's pin traits reject a
/// Board pin that cannot carry its signal when the firmware is compiled.
pub fn network_tokens(
    ethernet: Option<&EthernetDefinition>,
    coprocessor: Option<&WifiCoprocessorDefinition>,
) -> Result<String, Box<dyn Error>> {
    if ethernet.is_none() && coprocessor.is_none() {
        return Ok(String::from(NO_NETWORK_TOKENS));
    }
    if let (Some(ethernet), Some(coprocessor)) = (ethernet, coprocessor) {
        let shared = ethernet
            .pins()
            .map(|(_, pin)| pin)
            .find(|pin| coprocessor.pins().any(|(_, other)| other == *pin));
        if let Some(pin) = shared {
            return Err(format!(
                "`{pin}` is wired to both the Ethernet PHY and the Wi-Fi co-processor"
            )
            .into());
        }
    }
    let mut links = Vec::new();
    let mut power = String::from(NO_POWER);
    if let Some(ethernet) = ethernet {
        links.push(project_ethernet(ethernet)?);
    }
    if let Some(coprocessor) = coprocessor {
        let (projection, power_macro) = project_coprocessor(coprocessor)?;
        links.push(projection);
        power = power_macro;
    }
    let items = links
        .iter()
        .map(|link| link.items.as_str())
        .collect::<String>();
    let fields = links
        .iter()
        .flat_map(|link| link.fields.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    let parameters = links
        .iter()
        .flat_map(|link| link.parameters.iter().cloned())
        .collect::<Vec<_>>()
        .join(", ");
    let initializers = links
        .iter()
        .flat_map(|link| link.initializers.iter().cloned())
        .collect::<Vec<_>>()
        .join(",\n            ");
    let arguments = links
        .iter()
        .flat_map(|link| link.arguments.iter().cloned())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "{items}
/// Chip resources the selected Board's network links consume.
pub struct NetworkTokens {{
{fields}
}}

impl NetworkTokens {{
    /// Collects the chip resources of the Board's links.
    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new({parameters}) -> Self {{
        Self {{
            {initializers}
        }}
    }}
}}

/// Takes the selected Board's network-link tokens out of the chip peripherals.
#[doc(hidden)]
#[macro_export]
macro_rules! __barracuda_esp32p4_network_tokens {{
    ($peripherals:ident) => {{
        $crate::application::NetworkTokens::new({arguments})
    }};
}}
{power}"
    ))
}
