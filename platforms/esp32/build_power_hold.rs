//! Projects the selected Board's battery power latch (`hardware.power-hold`)
//! into the macro the Platform entry expands right after the chip starts.
//!
//! Shared by the build scripts of every ESP Platform, which include it with
//! `#[path]`; the entries call `__barracuda_power_hold!(peripherals)`.

/// Returns the macro source: one that drives `pin` high, or one that does
/// nothing when the Board has no latch or this Platform is inactive.
pub(crate) fn power_hold_macro(pin: Option<&str>) -> Result<String, String> {
    let body = match pin {
        Some(pin) => {
            if pin.is_empty()
                || !pin
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return Err(format!(
                    "`hardware.power-hold` `{pin}` is not a chip pin name"
                ));
            }
            format!("$crate::application::hold_power($peripherals.{pin})")
        }
        None => String::new(),
    };
    Ok(format!(
        "/// Drives the Board's battery power latch high, when it has one.\n\
         #[doc(hidden)]\n\
         #[macro_export]\n\
         macro_rules! __barracuda_power_hold {{\n    \
         ($peripherals:ident) => {{ {body} }};\n\
         }}\n"
    ))
}
