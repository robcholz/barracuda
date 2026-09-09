#![allow(
    missing_docs,
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::panic
)]

mod panel_commands {
    include!("../../build-support/panel_commands.rs");
}

fn main() {
    panel_commands::generate(&[
        (
            "assets/display_init_st7121.h",
            "ST7121_INIT_COMMANDS",
            false,
        ),
        (
            "assets/display_init_st7123.h",
            "ST7123_INIT_COMMANDS",
            false,
        ),
    ]);
}
