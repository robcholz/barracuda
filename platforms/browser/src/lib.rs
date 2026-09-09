//! Experimental Browser Platform transport support.
//!
//! Barracuda runs inside a dedicated worker. The browser-facing JavaScript
//! bootstrap downloads the System image before invoking [`start`], while the
//! Rust side retains ownership of the Embassy stack and its packet queues.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

/// Browser Platform marker reserved for future static target composition.
///
/// This type does not implement `barracuda_platform::Platform` yet.
pub struct BrowserPlatform;

/// Virtual network implementation backed by the gateway WebSocket.
#[cfg(target_arch = "wasm32")]
pub mod network;

/// Validates the inputs for the experimental worker transport.
///
/// This does not start the Barracuda System yet. The guard deliberately rejects
/// main-window execution so later device work cannot monopolize the UI event
/// loop.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start(
    gateway_url: String,
    system_image: js_sys::Uint8Array,
) -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast as _;

    let global = js_sys::global();
    if !global.is_instance_of::<web_sys::DedicatedWorkerGlobalScope>() {
        return Err(wasm_bindgen::JsValue::from_str(
            "Barracuda must be started in a dedicated Web Worker",
        ));
    }
    if system_image.length() == 0 {
        return Err(wasm_bindgen::JsValue::from_str("system image is empty"));
    }
    network::configure_gateway(gateway_url)?;
    Ok(())
}
