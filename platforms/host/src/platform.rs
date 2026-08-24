//! Host Platform initialization from an independently selected Board.

use std::path::Path;

use barracuda_board::Board;
use barracuda_platform::{Platform, PlatformInitResult, PlatformResources};
use embassy_embedded_hal::flash::partition::Partition;
use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

use crate::{
    model_api, DiskFs, FileNorFlash, FileNorFlashError, HostLayout, HostLayoutError, HostTlsError,
    TokioStack,
};

/// Host paths baked from `platforms/host/platform.yml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostSettings {
    state_directory: &'static str,
    flash_image: &'static str,
}

impl HostSettings {
    /// Creates Host Platform settings.
    #[must_use]
    pub const fn new(state_directory: &'static str, flash_image: &'static str) -> Self {
        Self {
            state_directory,
            flash_image,
        }
    }

    /// Returns the root used for host-backed persistent state.
    #[must_use]
    pub const fn state_directory(&self) -> &'static str {
        self.state_directory
    }

    /// Returns the physical NOR image name within the state directory.
    #[must_use]
    pub const fn flash_image(&self) -> &'static str {
        self.flash_image
    }
}

/// Database partition returned by the Host Platform.
pub type HostDatabaseRegion = Partition<'static, NoopRawMutex, FileNorFlash>;

/// Reusable Host Platform implementation.
pub struct HostPlatform;

impl HostPlatform {
    /// Installs the Host OS reactor on the current Embassy executor thread.
    ///
    /// The reactor is process-lifetime Platform state. Calling this inside an
    /// existing Tokio runtime is a no-op, which keeps Host integration tests
    /// composable.
    ///
    /// # Errors
    ///
    /// Returns an error when the Host reactor threads cannot be created.
    pub fn install_reactor() -> Result<(), HostPlatformError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Ok(());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(HostPlatformError::Runtime)?;
        let runtime = Box::leak(Box::new(runtime));
        let guard = runtime.enter();
        core::mem::forget(guard);
        Ok(())
    }

    /// Initializes Host capabilities using explicit generated settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the Board is incompatible, the host flash image
    /// cannot be opened, or its database region cannot be resolved.
    pub async fn initialize_with_settings(
        board: &'static Board,
        settings: &HostSettings,
    ) -> Result<
        PlatformResources<
            TokioStack,
            DiskFs,
            HostDatabaseRegion,
            barracuda_model_api::ModelApiFactory<TokioStack>,
        >,
        HostPlatformError,
    > {
        Self::initialize_with_layout_and_network(
            board,
            settings,
            &crate::BOARD_HOST_LAYOUT,
            Box::leak(Box::new(TokioStack::default())),
        )
        .await
    }

    /// Initializes Host capabilities against an explicit Host-native image layout.
    ///
    /// # Errors
    ///
    /// Returns an error when the image cannot be opened or the Board bindings do not resolve
    /// to valid regions in the supplied Host layout.
    pub async fn initialize_with_layout(
        board: &'static Board,
        settings: &HostSettings,
        layout: &HostLayout,
    ) -> Result<
        PlatformResources<
            TokioStack,
            DiskFs,
            HostDatabaseRegion,
            barracuda_model_api::ModelApiFactory<TokioStack>,
        >,
        HostPlatformError,
    > {
        Self::initialize_with_layout_and_network(
            board,
            settings,
            layout,
            Box::leak(Box::new(TokioStack::default())),
        )
        .await
    }

    async fn initialize_with_layout_and_network(
        board: &'static Board,
        settings: &HostSettings,
        layout: &HostLayout,
        network: &'static TokioStack,
    ) -> Result<
        PlatformResources<
            TokioStack,
            DiskFs,
            HostDatabaseRegion,
            barracuda_model_api::ModelApiFactory<TokioStack>,
        >,
        HostPlatformError,
    > {
        if board.hardware().chip() != "host" {
            return Err(HostPlatformError::IncompatibleChip {
                chip: board.hardware().chip(),
            });
        }
        let model_api_factory = model_api::factory(network).await?;
        let state_directory = Path::new(settings.state_directory());
        let flash_path = state_directory.join(settings.flash_image());
        let physical_flash = FileNorFlash::open(flash_path, layout.capacity()).await?;
        layout.writable_region(&physical_flash, board.storage().filesystem())?;
        if let Some(web_assets) = board.storage().web_assets() {
            layout.read_only_region(&physical_flash, web_assets)?;
        }
        let database = layout.writable_region(&physical_flash, board.storage().database())?;
        let physical_flash = Box::leak(Box::new(Mutex::<NoopRawMutex, _>::new(physical_flash)));
        let database_region = Partition::new(physical_flash, database.offset(), database.size());
        let filesystem = DiskFs::rooted(
            state_directory
                .join("partitions")
                .join(board.storage().filesystem()),
        );

        Ok(PlatformResources {
            network,
            filesystem,
            database_region,
            model_api_factory,
        })
    }
}

impl Platform for HostPlatform {
    type Network = TokioStack;
    type FileSystem = DiskFs;
    type DatabaseRegion = HostDatabaseRegion;
    type ModelApiFactory = barracuda_model_api::ModelApiFactory<TokioStack>;
    type Error = HostPlatformError;

    fn prepare() -> Result<(), Self::Error> {
        Self::install_reactor()
    }

    async fn initialize(spawner: Spawner, board: &'static Board) -> PlatformInitResult<Self> {
        Self::initialize_with_layout_and_network(
            board,
            &crate::PLATFORM_SETTINGS,
            &crate::BOARD_HOST_LAYOUT,
            Box::leak(Box::new(TokioStack::with_spawner(spawner))),
        )
        .await
    }
}

/// Host Platform initialization failure.
#[derive(Debug, thiserror::Error)]
pub enum HostPlatformError {
    /// The independently selected Board names hardware this Platform cannot drive.
    #[error("Host Platform does not support Board chip `{chip}`")]
    IncompatibleChip {
        /// Canonical chip name declared by the Board.
        chip: &'static str,
    },
    /// Host OS reactor initialization failed.
    #[error("failed to initialize Host reactor: {0}")]
    Runtime(std::io::Error),
    /// Host NOR image access failed.
    #[error(transparent)]
    Flash(#[from] FileNorFlashError),
    /// Board binding or Host-native layout is invalid.
    #[error(transparent)]
    Layout(#[from] HostLayoutError),
    /// Host TLS and native trust-store initialization failed.
    #[error(transparent)]
    Tls(#[from] HostTlsError),
}
