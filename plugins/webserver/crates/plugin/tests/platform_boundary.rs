//! WebServer remains portable above the selected Platform boundary.

#[test]
fn webserver_does_not_expose_a_host_runtime_feature() {
    let manifest = include_str!("../Cargo.toml");

    assert!(!manifest.contains("picoserve/std"));
    assert!(!manifest.contains("\nstd ="));
}
