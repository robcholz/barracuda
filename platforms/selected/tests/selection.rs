//! Selected Platform behavior for the current native target.

#[test]
fn selected_native_platform_matches_the_target_operating_system() {
    assert_eq!(
        barracuda_platform_selected::PLATFORM_NAME,
        std::env::consts::OS
    );
}
