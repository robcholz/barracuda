//! Lua package for the Board's primary built-in audio codec.

#![no_std]

extern crate alloc;

use alloc::{format, sync::Arc, vec, vec::Vec};
use barracuda_board_hal::audio::{AudioCodec, AudioCodecPeripheral, AudioDescriptor};
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
const MAX_WAV_HEADER_BYTES: usize = 64 * 1024;
const MAX_WAV_BYTES: usize = MAX_TRANSFER_BYTES + MAX_WAV_HEADER_BYTES;

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
        Builtins: AudioCodecPeripheral<AudioCodec = Device>,
    {
        Self {
            codec: context.hal.peripherals.take_audio_codec(),
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
        methods.add_method("descriptor", |handle, (): ()| {
            Some(handle.descriptor().map(descriptor_values))
        });
        methods.add_method("format", |handle, (): ()| {
            Some(handle.descriptor().map(descriptor_values))
        });
        methods.add_method_mut("set_volume", |handle, percent: i64| {
            Some(handle.set_volume(percent))
        });
        methods.add_async_method("play", |handle, bytes: Vec<u8>| async move {
            Some(play(handle, bytes).await)
        });
        methods.add_async_method("play_wav", |handle, bytes: Vec<u8>| async move {
            Some(play_wav(handle, bytes).await)
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
    let samples = decode_pcm_le(&bytes)?;
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let descriptor = handle.codec.as_ref().ok_or_else(closed)?.descriptor();
    validate_sample_count(samples.len(), descriptor.channels())?;
    write_samples(&mut handle, &samples).await
}

async fn play_wav<Device>(handle: UserDataHandle<AudioHandle<Device>>, bytes: Vec<u8>) -> Result<()>
where
    Device: AudioCodec + Send + 'static,
    Device::Error: core::fmt::Debug,
{
    if bytes.len() > MAX_WAV_BYTES {
        return Err(Error::runtime("audio WAV file exceeds 327680 bytes"));
    }
    let wav = parse_pcm_wav(&bytes)?;
    if wav.data_len > MAX_TRANSFER_BYTES {
        return Err(Error::runtime("audio WAV payload exceeds 262144 bytes"));
    }
    let samples = decode_pcm_le(&bytes[wav.data_start..wav.data_start + wav.data_len])?;
    let mut handle = handle.borrow_mut()?;
    ensure_active(&handle.active)?;
    let descriptor = handle.codec.as_ref().ok_or_else(closed)?.descriptor();
    validate_wav_format(wav, descriptor)?;
    validate_sample_count(samples.len(), descriptor.channels())?;
    write_samples(&mut handle, &samples).await
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

#[derive(Clone, Copy)]
struct PcmWav {
    sample_rate_hz: u32,
    channels: u16,
    bits_per_sample: u16,
    block_align: u16,
    data_start: usize,
    data_len: usize,
}

#[derive(Clone, Copy)]
struct WavFormat {
    sample_rate_hz: u32,
    channels: u16,
    bits_per_sample: u16,
    block_align: u16,
}

fn parse_pcm_wav(bytes: &[u8]) -> Result<PcmWav> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::runtime("audio WAV must be a RIFF/WAVE file"));
    }
    let riff_size = usize::try_from(read_u32_le(bytes, 4)?)
        .map_err(|_| Error::runtime("audio WAV size is unsupported"))?;
    let riff_end = riff_size
        .checked_add(8)
        .ok_or_else(|| Error::runtime("audio WAV size overflows"))?;
    if riff_end > bytes.len() {
        return Err(Error::runtime("audio WAV is truncated"));
    }

    let mut offset = 12_usize;
    let mut format = None;
    let mut data = None;
    while offset < riff_end {
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| Error::runtime("audio WAV chunk offset overflows"))?;
        if header_end > riff_end {
            return Err(Error::runtime("audio WAV has a truncated chunk header"));
        }
        let chunk_size = usize::try_from(read_u32_le(bytes, offset + 4)?)
            .map_err(|_| Error::runtime("audio WAV chunk size is unsupported"))?;
        let chunk_start = header_end;
        let chunk_end = chunk_start
            .checked_add(chunk_size)
            .ok_or_else(|| Error::runtime("audio WAV chunk size overflows"))?;
        if chunk_end > riff_end {
            return Err(Error::runtime("audio WAV has a truncated chunk"));
        }
        match &bytes[offset..offset + 4] {
            b"fmt " if format.is_none() => {
                format = Some(parse_wav_format(&bytes[chunk_start..chunk_end])?);
            }
            b"data" if data.is_none() => data = Some((chunk_start, chunk_size)),
            _ => {}
        }
        offset = chunk_end
            .checked_add(chunk_size % 2)
            .ok_or_else(|| Error::runtime("audio WAV chunk offset overflows"))?;
        if offset > riff_end {
            return Err(Error::runtime("audio WAV chunk padding is truncated"));
        }
    }

    let format = format.ok_or_else(|| Error::runtime("audio WAV has no fmt chunk"))?;
    let (data_start, data_len) =
        data.ok_or_else(|| Error::runtime("audio WAV has no data chunk"))?;
    if data_len % usize::from(format.block_align) != 0 {
        return Err(Error::runtime(
            "audio WAV data must contain complete interleaved frames",
        ));
    }
    Ok(PcmWav {
        sample_rate_hz: format.sample_rate_hz,
        channels: format.channels,
        bits_per_sample: format.bits_per_sample,
        block_align: format.block_align,
        data_start,
        data_len,
    })
}

fn parse_wav_format(bytes: &[u8]) -> Result<WavFormat> {
    if bytes.len() < 16 {
        return Err(Error::runtime("audio WAV fmt chunk is truncated"));
    }
    if read_u16_le(bytes, 0)? != 1 {
        return Err(Error::runtime(
            "audio WAV must use uncompressed integer PCM",
        ));
    }
    let channels = read_u16_le(bytes, 2)?;
    let sample_rate_hz = read_u32_le(bytes, 4)?;
    let byte_rate = read_u32_le(bytes, 8)?;
    let block_align = read_u16_le(bytes, 12)?;
    let bits_per_sample = read_u16_le(bytes, 14)?;
    if channels == 0 || sample_rate_hz == 0 {
        return Err(Error::runtime(
            "audio WAV sample rate and channel count must be non-zero",
        ));
    }
    if bits_per_sample != 16 {
        return Err(Error::runtime("audio WAV must use signed 16-bit PCM"));
    }
    let expected_block_align = channels
        .checked_mul(2)
        .ok_or_else(|| Error::runtime("audio WAV channel count overflows"))?;
    let expected_byte_rate = sample_rate_hz
        .checked_mul(u32::from(expected_block_align))
        .ok_or_else(|| Error::runtime("audio WAV byte rate overflows"))?;
    if block_align != expected_block_align || byte_rate != expected_byte_rate {
        return Err(Error::runtime("audio WAV has inconsistent PCM framing"));
    }
    Ok(WavFormat {
        sample_rate_hz,
        channels,
        bits_per_sample,
        block_align,
    })
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| Error::runtime("audio WAV is truncated"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| Error::runtime("audio WAV is truncated"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn decode_pcm_le(bytes: &[u8]) -> Result<Vec<i16>> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::runtime(
            "audio PCM must contain complete 16-bit samples",
        ));
    }
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(Error::runtime("audio PCM contains an incomplete sample"));
    }
    Ok(pairs.iter().copied().map(i16::from_le_bytes).collect())
}

fn validate_wav_format(wav: PcmWav, descriptor: AudioDescriptor) -> Result<()> {
    if wav.sample_rate_hz != descriptor.sample_rate_hz()
        || wav.channels != u16::from(descriptor.channels())
        || wav.bits_per_sample != u16::from(descriptor.bits_per_sample())
        || wav.block_align != u16::from(descriptor.channels()) * 2
    {
        return Err(Error::runtime(
            "audio WAV format does not match the built-in codec",
        ));
    }
    Ok(())
}

async fn write_samples<Device>(handle: &mut AudioHandle<Device>, samples: &[i16]) -> Result<()>
where
    Device: AudioCodec,
    Device::Error: core::fmt::Debug,
{
    handle
        .codec
        .as_mut()
        .ok_or_else(closed)?
        .write(samples)
        .await
        .map_err(codec_error)
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
    fn lua_application_plays_pcm_and_wav_then_records() {
        let package = AudioPackage::new(Some(TestCodec { volume: 0 }));
        let mut lua = Lua::new().expect("create Lua");
        package.install(&mut lua).expect("install audio package");

        let result: bool = futures_lite::future::block_on(
            lua.load("local audio = require('audio')\nlocal player <close> = audio.open()\nlocal rate, channels, bits = player:descriptor()\nlocal format_rate = player:format()\nlocal wav = 'RIFF\\040\\000\\000\\000WAVEfmt \\016\\000\\000\\000\\001\\000\\002\\000\\128\\187\\000\\000\\000\\238\\002\\000\\004\\000\\016\\000data\\004\\000\\000\\000\\001\\000\\002\\000'\nplayer:set_volume(50)\nplayer:play('\\001\\000\\002\\000')\nplayer:play_wav(wav)\nlocal pcm = player:record(2)\nreturn rate == 48000 and format_rate == rate and channels == 2 and bits == 16 and #pcm == 8")
                .eval_async(),
        )
        .expect("run audio application");
        assert!(result);
    }

    #[test]
    fn wav_parser_handles_unknown_padded_chunks() {
        let wav = test_wav(48_000, 2, 16, &[1, 0, 2, 0], Some((b"JUNK", &[7])));
        let parsed = parse_pcm_wav(&wav).expect("parse PCM WAV");

        assert_eq!(parsed.sample_rate_hz, 48_000);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.bits_per_sample, 16);
        assert_eq!(
            &wav[parsed.data_start..parsed.data_start + parsed.data_len],
            &[1, 0, 2, 0]
        );
    }

    #[test]
    fn wav_parser_rejects_unsupported_and_mismatched_formats() {
        let eight_bit = test_wav(48_000, 2, 8, &[1, 2], None);
        assert!(parse_pcm_wav(&eight_bit).is_err());

        let mono = test_wav(48_000, 1, 16, &[1, 0], None);
        let parsed = parse_pcm_wav(&mono).expect("parse mono PCM WAV");
        assert!(validate_wav_format(parsed, AudioDescriptor::new(48_000, 2, 16)).is_err());
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

    fn test_wav(
        sample_rate_hz: u32,
        channels: u16,
        bits_per_sample: u16,
        pcm: &[u8],
        extra: Option<(&[u8; 4], &[u8])>,
    ) -> Vec<u8> {
        let block_align = channels * (bits_per_sample / 8);
        let byte_rate = sample_rate_hz * u32::from(block_align);
        let extra_len = extra.map_or(0, |(_, data)| 8 + data.len() + data.len() % 2);
        let riff_size = 4 + 8 + 16 + extra_len + 8 + pcm.len();
        let mut wav = Vec::with_capacity(riff_size + 8);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(
            &u32::try_from(riff_size)
                .expect("RIFF size fits")
                .to_le_bytes(),
        );
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate_hz.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits_per_sample.to_le_bytes());
        if let Some((id, data)) = extra {
            wav.extend_from_slice(id);
            wav.extend_from_slice(
                &u32::try_from(data.len())
                    .expect("extra chunk size fits")
                    .to_le_bytes(),
            );
            wav.extend_from_slice(data);
            if !data.len().is_multiple_of(2) {
                wav.push(0);
            }
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(
            &u32::try_from(pcm.len())
                .expect("PCM size fits")
                .to_le_bytes(),
        );
        wav.extend_from_slice(pcm);
        wav
    }
}
