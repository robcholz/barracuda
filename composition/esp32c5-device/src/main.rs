//! ESP32-C5 image entry for the portable Barracuda device application.

#![no_std]
#![no_main]

use esp_hal::ram;

esp_bootloader_esp_idf::esp_app_desc!();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    esp_println::println!("Barracuda ESP32-C5 Target panic: {info}");
    esp_println::println!("{}", esp_alloc::HEAP.stats());
    loop {
        core::hint::spin_loop();
    }
}

// Rust's abort-only embedded target does not ship an unwinder, but generated
// debug metadata can still reference the personality symbol. No unwinding can
// reach this function because the workspace release profile is panic=abort.
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    // Keep dynamic memory below the C5's internal-RAM budget. The complete
    // System and its Embassy task storage are statically allocated as well.
    esp_alloc::heap_allocator!(size: 47 * 1024);
    // The C5 bootloader releases a separate 64 KiB RAM window before entering
    // the application. Add it as a second internal-capability heap region.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64 * 1024);
    // The selected C5 application does not retain state across deep sleep, so
    // RTC-fast RAM is available as a final internal heap region.
    esp_alloc::heap_allocator!(#[ram(unstable(rtc_fast))] size: 16 * 1024);

    let bindings = match barracuda_target::bindings_from_peripherals(peripherals) {
        Ok(bindings) => bindings,
        Err(error) => {
            esp_println::println!("failed to split selected Target bindings: {error}");
            core::future::pending::<()>().await;
            return;
        }
    };
    let resources = match barracuda_target::resources_with_bindings(spawner, bindings).await {
        Ok(resources) => resources,
        Err(error) => {
            log::error!("failed to initialize selected Target: {error}");
            core::future::pending::<()>().await;
            return;
        }
    };

    if let Err(error) = barracuda_device::run(spawner, resources).await {
        log::error!("portable Barracuda device application stopped: {error}");
    }
    core::future::pending::<()>().await;
}
