//! Link-only probe for the selected STM32 Board's native `memory.x` symbols.

#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(target_arch = "arm", no_main)]

#[cfg(target_arch = "arm")]
use panic_halt as _;

#[cfg(target_arch = "arm")]
#[cortex_m_rt::entry]
fn main() -> ! {
    let layout =
        match barracuda_platform_stm32::board_storage_layout(embassy_stm32::flash::FLASH_BASE) {
            Ok(layout) => layout,
            Err(_error) => loop {
                core::hint::spin_loop();
            },
        };
    core::hint::black_box(layout.database().size());
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(target_arch = "arm"))]
fn main() {}
