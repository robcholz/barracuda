//! Architecture boundary tests for the portable device application.

#[test]
fn portable_application_has_no_concrete_target_or_board_code() {
    let source = include_str!("../src/lib.rs").to_ascii_lowercase();
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
            "portable device source contains `{}`",
            forbidden,
        );
        assert!(
            !manifest.contains(forbidden),
            "portable device manifest contains `{}`",
            forbidden,
        );
    }

    assert!(
        source.contains("system::new"),
        "portable device application must construct core/system"
    );
}
