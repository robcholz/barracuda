//! Architecture boundaries for the portable System application.

#[test]
fn portable_application_has_no_concrete_platform_or_board_code() {
    let source = include_str!("../src/lib.rs").to_ascii_lowercase();
    let entry = include_str!("../src/main.rs").to_ascii_lowercase();
    let manifest = include_str!("../Cargo.toml").to_ascii_lowercase();

    for forbidden in [
        "esp32",
        "esp_hal",
        "stm32",
        "target_arch",
        "barracuda-platform",
        "barracuda-board",
    ] {
        assert!(
            !source.contains(forbidden),
            "portable System source contains `{forbidden}`",
        );
        assert!(
            !manifest.contains(forbidden),
            "portable System manifest contains `{forbidden}`",
        );
    }

    assert!(source.contains("system::new"));
    assert!(entry.contains("#![no_std]"));
    assert!(entry.contains("barracuda_target::application_entry!()"));
    assert!(!entry.contains("target_os"));
    assert!(!entry.contains("embassy_executor::main"));
    assert!(!entry.contains("std::process"));
}
