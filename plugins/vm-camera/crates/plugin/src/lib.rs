//! Lua package for the Board's primary built-in camera.

#![no_std]

extern crate alloc;

use alloc::{string::String, vec, vec::Vec};
use barracuda_board_hal::camera::{Camera, CameraDescriptor, CameraPeripheral, CameraPixelFormat};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

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
            camera: context.hal.peripherals.take_camera(),
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
    camera: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> CameraPackage<Device> {
    fn new(camera: Option<Device>) -> Self {
        Self {
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
        lua.register_lib("camera", move |package| {
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
                    Some(camera) => {
                        lua.create_userdata(CameraHandle::new(camera, Arc::clone(&open_active)))
                    }
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

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct CameraHandle<Device> {
    camera: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> CameraHandle<Device> {
    fn new(camera: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            camera: Some(camera),
            active,
        }
    }

    fn close(&mut self) {
        self.camera.take();
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
) -> Result<Vec<u8>>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    let capacity = usize::try_from(capacity)
        .map_err(|_| Error::runtime("camera capture capacity must be non-negative"))?;
    if capacity == 0 || capacity > MAX_CAPTURE_BYTES {
        return Err(Error::runtime(
            "camera capture capacity must be between 1 and 4194304 bytes",
        ));
    }
    let mut frame = vec![0; capacity];
    let mut handle = handle.borrow_mut()?;
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
}
