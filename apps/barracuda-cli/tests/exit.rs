#[test]
fn terminal_entry_exits_the_process_after_run_completes() -> Result<(), std::io::Error> {
    let main = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )?;

    assert!(main.contains("std::process::exit(exit_code);"));
    Ok(())
}
