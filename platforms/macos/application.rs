extern crate std;

#[unsafe(no_mangle)]
#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let result = async {
        let resources = barracuda_target::resources(spawner).await?;
        barracuda_system_app::run(spawner, resources)
            .await
            .map_err(ApplicationError::System)
    }
    .await;

    if let Err(error) = result {
        std::eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[derive(Debug)]
enum ApplicationError {
    Target(barracuda_target::Error),
    System(barracuda_system_app::ApplicationError),
}

impl From<barracuda_target::Error> for ApplicationError {
    fn from(error: barracuda_target::Error) -> Self {
        Self::Target(error)
    }
}

impl core::fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Target(error) => write!(
                formatter,
                "failed to construct selected target resources: {error}"
            ),
            Self::System(error) => error.fmt(formatter),
        }
    }
}
