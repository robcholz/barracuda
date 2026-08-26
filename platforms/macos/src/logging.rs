//! macOS host logger installation.

pub(crate) fn install() {
    let _already_installed_or_initialized = env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .try_init();
}
