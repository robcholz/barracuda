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
    panel_commands::generate(&[("assets/init.h", "INIT_COMMANDS", true)]);
}
