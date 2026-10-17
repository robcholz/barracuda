//! Lua package for the Board's primary built-in camera.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::string::String;
use barracuda_board_hal::camera::{Camera, CameraDescriptor, CameraPeripheral, CameraPixelFormat};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Bytes, Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods, Variadic,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Largest frame returned to Lua; it must fit the 1 MiB per-run Lua heap with
/// room to process it (an SVGA JPEG or a full QVGA RGB565 frame).
const MAX_CAPTURE_BYTES: usize = 512 * 1024;
/// The room `camera.snap()` gives a frame unless the app asks for more.
const SNAP_BYTES: i64 = 128 * 1024;

/// The camera while no handle holds it; a handle gives it back when it closes.
type SharedCamera<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// Owns the Board's primary camera capability and publishes it to Lua.
#[barracuda_plugin::macros::plugin]
pub struct CameraPlugin<Device> {
    camera: Option<Device>,
}

impl<Device> CameraPlugin<Device> {
    /// Takes the built-in camera from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: CameraPeripheral<Camera = Device>,
    {
        Self {
            camera: config::peripherals::CAMERA_DEVICE
                .and_then(|instance| context.hal.peripherals.take_camera(instance)),
        }
    }
}

impl<Device> Plugin for CameraPlugin<Device>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let registry = context.require::<LuaPackageRegistry>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
        let registration = registry
            .register(CameraPackage::new(self.camera.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct CameraPackage<Device> {
    /// Whether the Board has the device, which makes the package a capability.
    present: bool,
    camera: SharedCamera<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> CameraPackage<Device> {
    fn new(camera: Option<Device>) -> Self {
        Self {
            present: camera.is_some(),
            camera: Arc::new(critical_section::Mutex::new(RefCell::new(camera))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for CameraPackage<Device>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_camera = Arc::clone(&self.camera);
        let available_active = Arc::clone(&self.active);
        let open_camera = Arc::clone(&self.camera);
        let open_active = Arc::clone(&self.active);
        let snap_camera = Arc::clone(&self.camera);
        let snap_active = Arc::clone(&self.active);
        lua.register_lib("camera", move |package| {
            package.register_async("snap", move |args: Variadic<i64>| {
                let camera = Arc::clone(&snap_camera);
                let active = Arc::clone(&snap_active);
                let capacity = args.first().copied().unwrap_or(SNAP_BYTES);
                async move { Some(snap(camera, active, capacity).await) }
            })?;
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_camera.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("camera package has been revoked")));
                }
                let camera = critical_section::with(|section| {
                    open_camera.borrow(section).borrow_mut().take()
                });
                Some(match camera {
                    Some(camera) => lua.create_userdata(CameraHandle::new(
                        camera,
                        Arc::clone(&open_camera),
                        Arc::clone(&open_active),
                    )),
                    None => Err(Error::runtime(
                        "built-in camera is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Device> LuaPackage for CameraPackage<Device>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "camera"
    }

    fn interface(&self) -> Option<barracuda_vm_plugin::PackageInterface> {
        self.present
            .then_some(barracuda_vm_plugin::PackageInterface::Stable {
                version: 2,
                purpose: "使用相机拍照",
            })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct CameraHandle<Device> {
    camera: Option<Device>,
    /// Where the camera goes back when the handle closes.
    home: SharedCamera<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> CameraHandle<Device> {
    fn new(camera: Device, home: SharedCamera<Device>, active: Arc<AtomicBool>) -> Self {
        Self {
            camera: Some(camera),
            home,
            active,
        }
    }

    fn close(&mut self) {
        if let Some(camera) = self.camera.take() {
            critical_section::with(|section| {
                self.home.borrow(section).replace(Some(camera));
            });
        }
    }
}

impl<Device> UserData for CameraHandle<Device>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("descriptor", |handle, (): ()| {
            Some(handle.descriptor().map(descriptor_values))
        });
        methods.add_async_method("capture", |handle, capacity: i64| async move {
            Some(capture(handle, capacity).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.camera.is_some()
            ))
        });
        methods.add_method_mut("close", |handle, (): ()| {
            handle.close();
            None::<Result<()>>
        });
        methods.add_meta_method_mut(MetaMethod::Close, |handle, _error: Option<String>| {
            handle.close();
            None::<Result<()>>
        });
    }
}

impl<Device> CameraHandle<Device>
where
    Device: Camera,
{
    fn descriptor(&self) -> Result<CameraDescriptor> {
        ensure_active(&self.active)?;
        self.camera
            .as_ref()
            .map(Camera::descriptor)
            .ok_or_else(|| Error::runtime("camera handle is closed"))
    }
}

impl<Device> Drop for CameraHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

async fn capture<Device>(
    handle: UserDataHandle<CameraHandle<Device>>,
    capacity: i64,
) -> Result<Bytes>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    capture_into(&mut handle, capacity).await
}

/// Takes one frame with the camera while no handle holds it, and names its
/// format.
async fn snap<Device>(
    camera: SharedCamera<Device>,
    active: Arc<AtomicBool>,
    capacity: i64,
) -> Result<(Bytes, &'static str)>
where
    Device: Camera,
    Device::Error: core::fmt::Debug,
{
    ensure_active(&active)?;
    let device = critical_section::with(|section| camera.borrow(section).borrow_mut().take())
        .ok_or_else(|| Error::runtime("built-in camera is unavailable or open in a handle"))?;
    // The handle gives the camera back however the capture ends.
    let mut handle = CameraHandle::new(device, camera, active);
    let frame = capture_into(&mut handle, capacity).await?;
    let (_, _, format) = descriptor_values(handle.descriptor()?);
    Ok((frame, format))
}

async fn capture_into<Device>(handle: &mut CameraHandle<Device>, capacity: i64) -> Result<Bytes>
where
    Device: Camera,
    Device::Error: core::fmt::Debug,
{
    let capacity = usize::try_from(capacity)
        .map_err(|_| Error::runtime("camera capture capacity must be non-negative"))?;
    if capacity == 0 || capacity > MAX_CAPTURE_BYTES {
        return Err(Error::runtime(
            "camera capture capacity must be between 1 and 524288 bytes",
        ));
    }
    let mut frame = Bytes::zeroed(capacity)?;
    ensure_active(&handle.active)?;
    let camera = handle
        .camera
        .as_mut()
        .ok_or_else(|| Error::runtime("camera handle is closed"))?;
    let captured = camera
        .capture(&mut frame)
        .await
        .map_err(|error| Error::runtime(alloc::format!("camera capture failed: {error:?}")))?;
    if captured.bytes_used() > frame.len() {
        return Err(Error::runtime(
            "camera peripheral reported an invalid frame length",
        ));
    }
    frame.truncate(captured.bytes_used());
    Ok(frame)
}

fn descriptor_values(descriptor: CameraDescriptor) -> (i64, i64, &'static str) {
    (
        i64::from(descriptor.width()),
        i64::from(descriptor.height()),
        match descriptor.pixel_format() {
            CameraPixelFormat::Jpeg => "jpeg",
            CameraPixelFormat::Rgb565 => "rgb565",
            CameraPixelFormat::Yuv422 => "yuv422",
            CameraPixelFormat::Grayscale => "grayscale",
            _ => "unknown",
        },
    )
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("camera package has been revoked"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use barracuda_board_hal::camera::CapturedFrame;

    struct TestCamera;

    impl Camera for TestCamera {
        type Error = core::convert::Infallible;

        fn descriptor(&self) -> CameraDescriptor {
            CameraDescriptor::new(2, 1, CameraPixelFormat::Jpeg)
        }

        async fn capture<'a>(
            &'a mut self,
            buffer: &'a mut [u8],
        ) -> core::result::Result<CapturedFrame, Self::Error> {
            buffer[..3].copy_from_slice(&[1, 2, 3]);
            Ok(CapturedFrame::new(3, self.descriptor()))
        }
    }

    #[test]
    fn lua_reads_descriptor_and_captures_a_frame() {
        let package = CameraPackage::new(Some(TestCamera));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local cameras = require('camera')\nlocal camera <close> = cameras.open()\nlocal width, height, format = camera:descriptor()\nlocal frame = camera:capture(16)\nreturn width == 2 and height == 1 and format == 'jpeg' and #frame == 3")
                .eval_async(),
        )
        .expect("capture frame");
        assert!(result);
    }

    #[test]
    fn snap_captures_while_no_handle_holds_the_camera() {
        let package = CameraPackage::new(Some(TestCamera));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local camera = require('camera')\n\
                 local frame, format = camera.snap()\n\
                 assert(#frame == 3 and format == 'jpeg')\n\
                 local held = camera.open()\n\
                 local busy, err = camera.snap()\n\
                 assert(busy == nil and err:find('open in a handle'))\n\
                 held:close()\n\
                 local small = camera.snap(8)\n\
                 return #small == 3 and camera.available()",
            )
            .eval_async(),
        )
        .expect("snap");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = CameraPackage::new(Some(TestCamera));
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
