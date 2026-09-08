//! Lua package for the Board's primary built-in audio codec.

#![no_std]

extern crate alloc;

use alloc::{format, sync::Arc, vec, vec::Vec};
use barracuda_board_hal::audio::{AudioCodec, AudioDescriptor, BuiltinAudioCodec};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_TRANSFER_BYTES: usize = 256 * 1024;

/// Owns the Board's primary audio codec and publishes playback to Lua.
#[barracuda_plugin::macros::plugin]
pub struct AudioPlugin<Device> {
    codec: Option<Device>,
}

impl<Device> AudioPlugin<Device> {
    /// Takes the built-in audio codec from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: BuiltinAudioCodec<AudioCodec = Device>,
    {
        Self {
            codec: context.hal.builtins.take_audio_codec(),
        }
    }
}

impl<Device> Plugin for AudioPlugin<Device>
where
    Device: AudioCodec + Send + 'static,
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
            .register(AudioPackage::new(self.codec.take()))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct AudioPackage<Device> {
    codec: Arc<critical_section::Mutex<RefCell<Option<Device>>>>,
    active: Arc<AtomicBool>,
}

impl<Device> AudioPackage<Device> {
    fn new(codec: Option<Device>) -> Self {
        Self {
            codec: Arc::new(critical_section::Mutex::new(RefCell::new(codec))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl<Device> Package for AudioPackage<Device>
where
    Device: AudioCodec + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let available_codec = Arc::clone(&self.codec);
        let available_active = Arc::clone(&self.active);
        let open_codec = Arc::clone(&self.codec);
        let open_active = Arc::clone(&self.active);
        lua.register_lib("audio", move |package| {
            package.register("available", move |(): ()| {
                Some(Ok(available_active.load(Ordering::Acquire)
                    && critical_section::with(|section| {
                        available_codec.borrow(section).borrow().is_some()
                    })))
            })?;
            package.register_with("open", move |lua, (): ()| {
                if !open_active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("audio package has been revoked")));
                }
                let codec = critical_section::with(|section| {
                    open_codec.borrow(section).borrow_mut().take()
                });
                Some(match codec {
                    Some(codec) => {
                        lua.create_userdata(AudioHandle::new(codec, Arc::clone(&open_active)))
                    }
                    None => Err(Error::runtime(
                        "built-in audio codec is unavailable or already open",
                    )),
                })
            })
        })
    }
}

impl<Device> LuaPackage for AudioPackage<Device>
where
    Device: AudioCodec + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "audio"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct AudioHandle<Device> {
    codec: Option<Device>,
    active: Arc<AtomicBool>,
}

impl<Device> AudioHandle<Device> {
    fn new(codec: Device, active: Arc<AtomicBool>) -> Self {
        Self {
            codec: Some(codec),
            active,
        }
    }

    fn close(&mut self) {
        self.codec.take();
    }
}

impl<Device> UserData for AudioHandle<Device>
where
    Device: AudioCodec + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("format", |handle, (): ()| {
            Some(handle.descriptor().map(descriptor_values))
        });
        methods.add_method_mut("set_volume", |handle, percent: i64| {
            Some(handle.set_volume(percent))
        });
        methods.add_async_method("play", |handle, bytes: Vec<u8>| async move {
            Some(play(handle, bytes).await)
        });
        methods.add_async_method("record", |handle, frames: i64| async move {
            Some(record(handle, frames).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.codec.is_some()
            ))
        });
        methods.add_method_mut("close", |handle, (): ()| {
            handle.close();
            None::<Result<()>>
        });
        methods.add_meta_method_mut(
            MetaMethod::Close,
            |handle, _error: Option<alloc::string::String>| {
                handle.close();
                None::<Result<()>>
            },
        );
    }
}

impl<Device> AudioHandle<Device>
where
    Device: AudioCodec,
    Device::Error: core::fmt::Debug,
{
    fn codec(&self) -> Result<&Device> {
        ensure_active(&self.active)?;
        self.codec.as_ref().ok_or_else(closed)
    }

    fn codec_mut(&mut self) -> Result<&mut Device> {
        ensure_active(&self.active)?;
        self.codec.as_mut().ok_or_else(closed)
    }

    fn descriptor(&self) -> Result<AudioDescriptor> {
        Ok(self.codec()?.descriptor())
    }

    fn set_volume(&mut self, percent: i64) -> Result<()> {
        let percent = u8::try_from(percent)
            .map_err(|_| Error::runtime("audio volume must be between 0 and 100 percent"))?;
        if percent > 100 {
            return Err(Error::runtime(
                "audio volume must be between 0 and 100 percent",
            ));
        }
        let scaled = (u16::from(percent) * 255 + 50) / 100;
        let volume =
            u8::try_from(scaled).map_err(|_| Error::runtime("audio volume conversion failed"))?;
        self.codec_mut()?
            .set_output_volume(volume)
            .map_err(codec_error)
    }
}

impl<Device> Drop for AudioHandle<Device> {
    fn drop(&mut self) {
        self.close();
    }
}

async fn play<Device>(handle: UserDataHandle<AudioHandle<Device>>, bytes: Vec<u8>) -> Result<()>
where
    Device: AudioCodec + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(Error::runtime("audio playback exceeds 262144 bytes"));
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::runtime(
            "audio PCM must contain complete 16-bit samples",
        ));
    }
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(Error::runtime("audio PCM contains an incomplete sample"));
    }
    let samples: Vec<i16> = pairs.iter().copied().map(i16::from_le_bytes).collect();
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let descriptor = handle.codec.as_ref().ok_or_else(closed)?.descriptor();
    validate_sample_count(samples.len(), descriptor.channels())?;
    handle
        .codec
        .as_mut()
        .ok_or_else(closed)?
        .write(&samples)
        .await
        .map_err(codec_error)
}

async fn record<Device>(handle: UserDataHandle<AudioHandle<Device>>, frames: i64) -> Result<Vec<u8>>
where
    Device: AudioCodec + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let descriptor = handle.codec.as_ref().ok_or_else(closed)?.descriptor();
    let frames = usize::try_from(frames)
        .map_err(|_| Error::runtime("audio frame count must be non-negative"))?;
    let samples_len = frames
        .checked_mul(usize::from(descriptor.channels()))
        .ok_or_else(|| Error::runtime("audio recording size overflows"))?;
    let byte_len = samples_len
        .checked_mul(2)
        .ok_or_else(|| Error::runtime("audio recording size overflows"))?;
    if byte_len > MAX_TRANSFER_BYTES {
        return Err(Error::runtime("audio recording exceeds 262144 bytes"));
    }
    let mut samples = vec![0_i16; samples_len];
    handle
        .codec
        .as_mut()
        .ok_or_else(closed)?
        .read(&mut samples)
        .await
        .map_err(codec_error)?;
    let mut bytes = Vec::with_capacity(byte_len);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

fn descriptor_values(descriptor: AudioDescriptor) -> (i64, i64, i64) {
    (
        i64::from(descriptor.sample_rate_hz()),
        i64::from(descriptor.channels()),
        i64::from(descriptor.bits_per_sample()),
    )
}

fn validate_sample_count(samples: usize, channels: u8) -> Result<()> {
    if channels == 0 || !samples.is_multiple_of(usize::from(channels)) {
        Err(Error::runtime(
            "audio PCM must contain complete interleaved frames",
        ))
    } else {
        Ok(())
    }
}

fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("audio package has been revoked"))
    }
}

fn closed() -> Error {
    Error::runtime("audio handle is closed")
}

fn codec_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("audio codec operation failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use super::*;
    use core::convert::Infallible;

    struct TestCodec {
        volume: u8,
    }

    impl AudioCodec for TestCodec {
        type Error = Infallible;

        fn descriptor(&self) -> AudioDescriptor {
            AudioDescriptor::new(48_000, 2, 16)
        }

        fn set_output_volume(&mut self, volume: u8) -> core::result::Result<(), Self::Error> {
            self.volume = volume;
            Ok(())
        }

        async fn write(&mut self, _samples: &[i16]) -> core::result::Result<(), Self::Error> {
            Ok(())
        }

        async fn read(&mut self, samples: &mut [i16]) -> core::result::Result<(), Self::Error> {
            samples.fill(0x1234);
            Ok(())
        }
    }

    #[test]
    fn lua_application_plays_and_records_pcm() {
        let package = AudioPackage::new(Some(TestCodec { volume: 0 }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install audio package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local audio = require('audio')\nlocal player <close> = audio.open()\nlocal rate, channels, bits = player:format()\nplayer:set_volume(50)\nplayer:play('\\001\\000\\002\\000')\nlocal pcm = player:record(2)\nreturn rate == 48000 and channels == 2 and bits == 16 and #pcm == 8")
                .eval_async(),
        )
        .expect("run audio application");
        assert!(result);
    }

    #[test]
    fn unavailable_board_reports_false_and_rejects_open() {
        let package = AudioPackage::new(None::<TestCodec>);
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install audio package");

        let result: bool = lua
            .load("local audio = require('audio')\nlocal handle, err = audio.open()\nreturn not audio.available() and handle == nil and type(err) == 'string'")
            .eval()
            .expect("query unavailable audio");
        assert!(result);
    }
}
