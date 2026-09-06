//! Entry for the selected Barracuda System application.

#![no_std]
#![no_main]

use embassy_executor::Spawner;

#[derive(Debug, thiserror::Error)]
enum EntryError {
    #[error("failed to construct selected target resources: {0}")]
    Target(#[from] barracuda_target::Error),
    #[error(transparent)]
    System(#[from] barracuda_system_app::ApplicationError),
}

async fn application(
    spawner: Spawner,
    bindings: barracuda_target::Bindings,
) -> Result<(), EntryError> {
    let resources = barracuda_target::resources_with_bindings(spawner, bindings).await?;
    barracuda_system_app::run(spawner, resources).await?;
    Ok(())
}

barracuda_target::application_entry!(application);
