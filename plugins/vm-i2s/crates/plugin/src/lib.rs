//! Lua package for I2S PCM streams on Board-exposed pins.

#![no_std]

extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use barracuda_board_hal::{
    ExposedIo, I2sProvider, I2sRequest,
    audio::{PcmFormat, PcmStream},
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaPackageRegistry, MetaMethod, Package, Result, UserData,
    UserDataHandle, UserDataMethods,
};
use core::sync::atomic::{AtomicBool, Ordering};

const MAX_TRANSFER_BYTES: usize = 256 * 1024;

/// Shares the unified exposed-I/O owner with the `i2s` Lua package.
#[barracuda_plugin::macros::plugin]
pub struct I2sPlugin<Io> {
    io: Arc<Io>,
}

impl<Io> I2sPlugin<Io> {
    /// Acquires a shared handle to the Board's single runtime I/O owner.
    #[must_use]
    pub fn new<Builtins>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Io: ExposedIo + I2sProvider,
    {
        Self {
            io: Arc::clone(&context.hal.io),
        }
    }
}

impl<Io> Plugin for I2sPlugin<Io>
where
    Io: ExposedIo + I2sProvider,
    Io::Error: core::fmt::Display,
    <Io::Stream as PcmStream>::Error: core::fmt::Debug,
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
            .register(I2sPackage::new(Arc::clone(&self.io)))
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}

struct I2sPackage<Io> {
    io: Arc<Io>,
    active: Arc<AtomicBool>,
}

impl<Io> I2sPackage<Io> {
    fn new(io: Arc<Io>) -> Self {
        Self {
            io,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

type OpenArgs = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
);

impl<Io> Package for I2sPackage<Io>
where
    Io: I2sProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Stream as PcmStream>::Error: core::fmt::Debug,
{
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let io = Arc::clone(&self.io);
        let active = Arc::clone(&self.active);
        lua.register_lib("i2s", move |package| {
            package.register_with("open", move |lua, args: OpenArgs| {
                Some((|| {
                    ensure_active(&active)?;
                    let (bclk, ws, dout, din, mclk, rate, channels, bits) = args;
                    validate_pins(&bclk, &ws, dout.as_deref(), din.as_deref(), mclk.as_deref())?;
                    let format = parse_format(rate, channels, bits, mclk.is_some())?;
                    let stream = io
                        .open_i2s(I2sRequest {
                            bclk: &bclk,
                            ws: &ws,
                            dout: dout.as_deref(),
                            din: din.as_deref(),
                            mclk: mclk.as_deref(),
                            format,
                        })
                        .map_err(|error| Error::runtime(error.to_string()))?;
                    lua.create_userdata(I2sHandle::new(stream, Arc::clone(&active)))
                })())
            })
        })
    }
}

impl<Io> LuaPackage for I2sPackage<Io>
where
    Io: I2sProvider + Send + Sync + 'static,
    Io::Error: core::fmt::Display,
    <Io::Stream as PcmStream>::Error: core::fmt::Debug,
{
    fn name(&self) -> &'static str {
        "i2s"
    }
    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
}

struct I2sHandle<Stream> {
    stream: Option<Stream>,
    active: Arc<AtomicBool>,
}

impl<Stream> I2sHandle<Stream> {
    fn new(stream: Stream, active: Arc<AtomicBool>) -> Self {
        Self {
            stream: Some(stream),
            active,
        }
    }
    fn close(&mut self) {
        self.stream.take();
    }
}

impl<Stream> UserData for I2sHandle<Stream>
where
    Stream: PcmStream + Send + 'static,
    Stream::Error: core::fmt::Debug,
{
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("format", |handle, (): ()| {
            Some((|| {
                let format = handle.stream()?.format();
                Ok((
                    i64::from(format.sample_rate_hz),
                    i64::from(format.channels),
                    i64::from(format.bits_per_sample),
                    format.master_clock_hz.map(i64::from),
                ))
            })())
        });
        methods.add_async_method("write", |handle, bytes: Vec<u8>| async move {
            Some(i2s_write(handle, bytes).await)
        });
        methods.add_async_method("read", |handle, frames: i64| async move {
            Some(i2s_read(handle, frames).await)
        });
        methods.add_method("is_open", |handle, (): ()| {
            Some(Ok(
                handle.active.load(Ordering::Acquire) && handle.stream.is_some()
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

impl<Stream: PcmStream> I2sHandle<Stream> {
    fn stream(&self) -> Result<&Stream> {
        ensure_active(&self.active)?;
        self.stream.as_ref().ok_or_else(closed)
    }
}

async fn i2s_write<Stream>(handle: UserDataHandle<I2sHandle<Stream>>, bytes: Vec<u8>) -> Result<()>
where
    Stream: PcmStream + Send + 'static,
    Stream::Error: core::fmt::Debug,
{
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(Error::runtime("I2S write exceeds 262144 bytes"));
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::runtime(
            "I2S PCM bytes must contain complete 16-bit samples",
        ));
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    let samples: Vec<i16> = pairs.iter().copied().map(i16::from_le_bytes).collect();
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let format = handle.stream.as_ref().ok_or_else(closed)?.format();
    validate_sample_count(samples.len(), format.channels)?;
    handle
        .stream
        .as_mut()
        .ok_or_else(closed)?
        .write(&samples)
        .await
        .map_err(stream_error)
}

async fn i2s_read<Stream>(handle: UserDataHandle<I2sHandle<Stream>>, frames: i64) -> Result<Vec<u8>>
where
    Stream: PcmStream + Send + 'static,
    Stream::Error: core::fmt::Debug,
{
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let format = handle.stream.as_ref().ok_or_else(closed)?.format();
    let frames = usize::try_from(frames)
        .map_err(|_| Error::runtime("I2S frame count must be non-negative"))?;
    let samples_len = frames
        .checked_mul(usize::from(format.channels))
        .ok_or_else(|| Error::runtime("I2S read size overflows"))?;
    let byte_len = samples_len
        .checked_mul(2)
        .ok_or_else(|| Error::runtime("I2S read size overflows"))?;
    if byte_len > MAX_TRANSFER_BYTES {
        return Err(Error::runtime("I2S read exceeds 262144 bytes"));
    }
    let mut samples = vec![0_i16; samples_len];
    handle
        .stream
        .as_mut()
        .ok_or_else(closed)?
        .read(&mut samples)
        .await
        .map_err(stream_error)?;
    let mut bytes = Vec::with_capacity(byte_len);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

fn validate_pins(
    bclk: &str,
    ws: &str,
    dout: Option<&str>,
    din: Option<&str>,
    mclk: Option<&str>,
) -> Result<()> {
    if dout.is_none() && din.is_none() {
        return Err(Error::runtime("I2S requires DOUT or DIN"));
    }
    let roles = [Some(bclk), Some(ws), dout, din, mclk];
    for (index, role) in roles.iter().enumerate() {
        if role.is_some() && roles[..index].contains(role) {
            return Err(Error::runtime("I2S signal roles must use different pins"));
        }
    }
    Ok(())
}
fn parse_format(rate: i64, channels: i64, bits: i64, has_mclk: bool) -> Result<PcmFormat> {
    let sample_rate_hz = u32::try_from(rate)
        .map_err(|_| Error::runtime("I2S sample rate must be a positive 32-bit integer"))?;
    if sample_rate_hz == 0 {
        return Err(Error::runtime("I2S sample rate must be greater than zero"));
    }
    let channels =
        u8::try_from(channels).map_err(|_| Error::runtime("I2S channels must be 1 or 2"))?;
    if !matches!(channels, 1 | 2) {
        return Err(Error::runtime("I2S channels must be 1 or 2"));
    }
    if bits != 16 {
        return Err(Error::runtime("I2S bits per sample must be 16"));
    }
    Ok(PcmFormat {
        sample_rate_hz,
        channels,
        bits_per_sample: 16,
        master_clock_hz: has_mclk.then_some(sample_rate_hz.saturating_mul(256)),
    })
}
fn validate_sample_count(samples: usize, channels: u8) -> Result<()> {
    if channels == 0 || !samples.is_multiple_of(usize::from(channels)) {
        Err(Error::runtime(
            "I2S PCM data must contain complete interleaved frames",
        ))
    } else {
        Ok(())
    }
}
fn ensure_active(active: &AtomicBool) -> Result<()> {
    if active.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(Error::runtime("I2S package has been revoked"))
    }
}
fn closed() -> Error {
    Error::runtime("I2S handle is closed")
}
fn stream_error(error: impl core::fmt::Debug) -> Error {
    Error::runtime(format!("I2S transfer failed: {error:?}"))
}

impl<Stream> Drop for I2sHandle<Stream> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    extern crate std;
    use super::*;
    use core::convert::Infallible;

    struct TestStream(PcmFormat);
    impl PcmStream for TestStream {
        type Error = Infallible;
        fn format(&self) -> PcmFormat {
            self.0
        }
        async fn write(&mut self, _samples: &[i16]) -> core::result::Result<(), Self::Error> {
            Ok(())
        }
        async fn read(&mut self, samples: &mut [i16]) -> core::result::Result<(), Self::Error> {
            samples.fill(0x1234);
            Ok(())
        }
    }
    #[derive(Debug)]
    struct OpenError;
    impl core::fmt::Display for OpenError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("unavailable")
        }
    }
    impl core::error::Error for OpenError {}
    struct Provider;
    impl I2sProvider for Provider {
        type Stream = TestStream;
        type Error = OpenError;
        fn open_i2s(
            &self,
            request: I2sRequest<'_>,
        ) -> core::result::Result<Self::Stream, Self::Error> {
            if request.bclk == "D1" && request.ws == "D2" && request.dout == Some("D3") {
                Ok(TestStream(request.format))
            } else {
                Err(OpenError)
            }
        }
    }

    #[test]
    fn lua_streams_pcm_through_the_audio_contract() {
        let package = I2sPackage::new(Arc::new(Provider));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install I2S package");
        let result: bool = futures_lite::future::block_on(lua.load("local i2s = require('i2s')\nlocal stream <close> = i2s.open('D1', 'D2', 'D3', nil, nil, 48000, 2, 16)\nstream:write('\\001\\000\\002\\000')\nlocal pcm = stream:read(2)\nlocal rate, channels, bits = stream:format()\nreturn #pcm == 8 and rate == 48000 and channels == 2 and bits == 16").eval_async()).expect("use I2S");
        assert!(result);
    }
}
