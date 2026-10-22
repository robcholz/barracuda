//! Lua package for the Board's built-in cameras.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{string::String, vec::Vec};
use barracuda_board_hal::camera::{Camera, CameraDescriptor, CameraPeripheral, CameraPixelFormat};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    BoardDevices, Bytes, Error, LentDevice, Lua, LuaPackage, LuaPackageRegistry, LuaValue,
    MetaMethod, Package, Result, UserData, UserDataHandle, UserDataMethods, Variadic,
};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

/// Largest frame returned to Lua; it must fit the 1 MiB per-run Lua heap with
/// room to process it (an SVGA JPEG or a full QVGA RGB565 frame).
const MAX_CAPTURE_BYTES: usize = 512 * 1024;
/// The room `camera.snap()` gives a frame unless the app asks for more.
const SNAP_BYTES: i64 = 128 * 1024;

/// Owns the Board's cameras and publishes them to Lua.
#[barracuda_plugin::macros::plugin]
pub struct CameraPlugin<Device> {
    cameras: Vec<(&'static str, Device)>,
}

impl<Device> CameraPlugin<Device> {
    /// Takes the built-in cameras from the generated Board capability set,
    /// the primary one first.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: CameraPeripheral<Camera = Device>,
    {
        Self {
            cameras: config::peripherals::CAMERA_DEVICES
                .iter()
                .filter_map(|(name, instance)| {
                    context
                        .hal
                        .peripherals
                        .take_camera(*instance)
                        .map(|camera| (*name, camera))
                })
                .collect(),
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
            .register(CameraPackage::new(core::mem::take(&mut self.cameras)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct CameraPackage<Device> {
    cameras: BoardDevices<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> CameraPackage<Device> {
    fn new(cameras: Vec<(&'static str, Device)>) -> Self {
        Self {
            cameras: BoardDevices::new(cameras),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// Reads `snap`'s optional capacity and camera name.
fn snap_arguments(values: Variadic<LuaValue>) -> Result<(i64, Option<String>)> {
    let values: Vec<LuaValue> = values.into();
    let mut values = values.into_iter();
    let capacity = match values.next() {
        None | Some(LuaValue::Nil) => SNAP_BYTES,
        Some(LuaValue::Integer(capacity)) => capacity,
        Some(_) => return Err(Error::runtime("a capture capacity is an integer")),
    };
    let name = match values.next() {
        None | Some(LuaValue::Nil) => None,
        Some(LuaValue::String(name)) => Some(name),
        Some(_) => return Err(Error::runtime("expected a camera name")),
    };
    if values.next().is_some() {
        return Err(Error::runtime(
            "expected at most a capacity and a camera name",
        ));
    }
    Ok((capacity, name))
}

impl<Device> Package for CameraPackage<Device>
where
    Device: Camera + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available = (self.cameras.clone(), Arc::clone(&self.active));
        let list = self.cameras.clone();
        let open = (self.cameras.clone(), Arc::clone(&self.active));
        let snapping = (self.cameras.clone(), Arc::clone(&self.active));
        lua.register_lib("camera", move |package| {
            package.register_async("snap", move |values: Variadic<LuaValue>| {
                let (cameras, active) = (snapping.0.clone(), Arc::clone(&snapping.1));
                let parsed = snap_arguments(values);
                async move {
                    Some(
                        async {
                            let (capacity, name) = parsed?;
                            snap(&cameras, active, capacity, name.as_deref()).await
                        }
                        .await,
                    )
                }
            })?;
            package.register("available", move |name: Variadic<String>| {
                let (cameras, active) = &available;
                Some(Ok(active.load(Ordering::Acquire)
                    && cameras.is_available(name.first().map(String::as_str))))
            })?;
            package.register_with("list", move |lua, (): ()| Some(list.list(lua)))?;
            package.register_with("open", move |lua, name: Variadic<String>| {
                let (cameras, active) = &open;
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("camera package has been revoked")));
                }
                let name = name.first().map(String::as_str);
                Some(match cameras.lend(name) {
                    Some(camera) => {
                        lua.create_userdata(CameraHandle::new(camera, Arc::clone(active)))
                    }
                    None => Err(Error::runtime(match name {
                        Some(name) => {
                            alloc::format!("camera `{name}` is unavailable or already open")
                        }
                        None => String::from("built-in camera is unavailable or already open"),
                    })),
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
        (!self.cameras.is_empty()).then_some(barracuda_vm_plugin::PackageInterface::Stable {
            version: 3,
            purpose: "使用相机拍照",
        })
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct CameraHandle<Device> {
    /// The camera, which goes back to the package when the handle closes.
    camera: Option<LentDevice<Device>>,
    active: Arc<AtomicBool>,
}

impl<Device> CameraHandle<Device> {
    fn new(camera: LentDevice<Device>, active: Arc<AtomicBool>) -> Self {
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
            .as_deref()
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

/// Takes one frame with the camera `name`, or the primary one, while no
/// handle holds it, and names its format.
async fn snap<Device>(
    cameras: &BoardDevices<Device>,
    active: Arc<AtomicBool>,
    capacity: i64,
    name: Option<&str>,
) -> Result<(Bytes, &'static str)>
where
    Device: Camera,
    Device::Error: core::fmt::Debug,
{
    ensure_active(&active)?;
    let device = cameras.lend(name).ok_or_else(|| {
        Error::runtime(match name {
            Some(name) => alloc::format!("camera `{name}` is unavailable or open in a handle"),
            None => String::from("built-in camera is unavailable or open in a handle"),
        })
    })?;
    // The handle gives the camera back however the capture ends.
    let mut handle = CameraHandle::new(device, active);
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
        .as_deref_mut()
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
        let package = CameraPackage::new(std::vec![("main", TestCamera)]);
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
        let package = CameraPackage::new(std::vec![("main", TestCamera)]);
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
    fn lua_names_a_camera_and_defaults_to_the_primary() {
        let package = CameraPackage::new(std::vec![("main", TestCamera), ("rear", TestCamera)]);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install package");

        let result: bool = futures_lite::future::block_on(
            lua.load(
                "local camera = require('camera')\n\
                 local names = camera.list()\n\
                 assert(#names == 2 and names[1] == 'main' and names[2] == 'rear')\n\
                 local main <close> = assert(camera.open())\n\
                 local frame = assert(camera.snap(nil, 'rear'))\n\
                 local rear <close> = assert(camera.open('rear'))\n\
                 local busy, err = camera.snap()\n\
                 return #frame == 3 and busy == nil and err:find('open in a handle') ~= nil and not camera.available('rear')",
            )
            .eval_async(),
        )
        .expect("use both cameras");
        assert!(result);
    }

    #[test]
    fn package_fits_the_vm_package_budget() {
        let package = CameraPackage::new(std::vec![("main", TestCamera)]);
        assert!(barracuda_vm_runtime::fits_package_budget(&package));
    }
}
