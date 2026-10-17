//! The Board's audio codec and the device's output level: one owner that
//! lends the codec to whoever plays or records, and the level the person
//! sets for every sound.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

use alloc::{boxed::Box, rc::Rc};
use barracuda_board_hal::audio::{AudioCodec, AudioCodecPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginRegisterContext, PluginResult};
use core::cell::RefCell;
use portable_atomic::{AtomicBool, AtomicU8, Ordering};
use portable_atomic_util::Arc;

/// The codec while nobody borrows it.
type Shelf<Device> = Arc<critical_section::Mutex<RefCell<Option<Device>>>>;

/// The device's output level, shared by [`Volume`], [`Sound`], and leases.
struct Level {
    /// Percent, 0 through 100.
    volume: AtomicU8,
    muted: AtomicBool,
}

impl Level {
    const fn new() -> Self {
        Self {
            volume: AtomicU8::new(100),
            muted: AtomicBool::new(false),
        }
    }

    /// The codec's `0..=255` output volume for a sound at `own` percent of
    /// the device's level.
    fn codec_volume(&self, own: u8) -> u8 {
        if self.muted.load(Ordering::Acquire) {
            return 0;
        }
        let device = u32::from(self.volume.load(Ordering::Acquire));
        let scaled = (device * u32::from(own.min(100)) * 255 + 5_000) / 10_000;
        u8::try_from(scaled).unwrap_or(u8::MAX)
    }
}

/// The device's output level: what the person sets for every sound.
/// Published by the `sound` Plugin.
pub struct Volume {
    level: Arc<Level>,
    /// Puts the level on the codec while nobody borrows it.
    apply: Box<dyn Fn()>,
}

impl Volume {
    /// Sets the output level to `volume` percent, or silence when `muted`.
    pub fn set(&self, volume: u8, muted: bool) {
        self.level.volume.store(volume.min(100), Ordering::Release);
        self.level.muted.store(muted, Ordering::Release);
        (self.apply)();
    }
}

/// The Board's audio codec, lent to one borrower at a time. Published by
/// the `sound` Plugin. Cloning it is cheap, and a clone may be kept in any
/// task, such as a Lua package's.
pub struct Sound<Device> {
    shelf: Shelf<Device>,
    level: Arc<Level>,
    present: bool,
}

impl<Device> Clone for Sound<Device> {
    fn clone(&self) -> Self {
        Self {
            shelf: Arc::clone(&self.shelf),
            level: Arc::clone(&self.level),
            present: self.present,
        }
    }
}

impl<Device> Sound<Device> {
    /// Lends `codec`; `None` for a Board without one.
    #[must_use]
    pub fn from_codec(codec: Option<Device>) -> Self {
        Self {
            present: codec.is_some(),
            shelf: Arc::new(critical_section::Mutex::new(RefCell::new(codec))),
            level: Arc::new(Level::new()),
        }
    }

    /// Whether the Board has an audio codec.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.present
    }

    /// Whether the codec is there to borrow now.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        critical_section::with(|section| self.shelf.borrow(section).borrow().is_some())
    }

    /// The device's output level, which puts a change on the codec at once
    /// while nobody borrows it.
    #[must_use]
    pub fn volume(&self) -> Volume
    where
        Device: AudioCodec + Send + 'static,
    {
        let idle = self.clone();
        Volume {
            level: Arc::clone(&self.level),
            apply: Box::new(move || {
                let Some(mut lease) = idle.lend() else {
                    return;
                };
                let volume = lease.codec_volume(100);
                if lease
                    .codec_mut()
                    .is_some_and(|codec| codec.set_output_volume(volume).is_err())
                {
                    log::warn!("sound: cannot set the output volume");
                }
            }),
        }
    }

    /// Borrows the codec until the lease drops; `None` without one or while
    /// someone else borrows it.
    #[must_use]
    pub fn lend(&self) -> Option<Lease<Device>> {
        let device =
            critical_section::with(|section| self.shelf.borrow(section).borrow_mut().take())?;
        Some(Lease {
            device: Some(device),
            shelf: Arc::clone(&self.shelf),
            level: Arc::clone(&self.level),
        })
    }
}

/// The codec, borrowed from [`Sound`]; it goes back when the lease drops.
pub struct Lease<Device> {
    device: Option<Device>,
    shelf: Shelf<Device>,
    level: Arc<Level>,
}

impl<Device> Lease<Device> {
    /// The codec's `0..=255` output volume for a sound at `own` percent of
    /// the device's level, or 0 while it is muted. Set it on the codec before
    /// each chunk, so a change of level reaches a sound that is playing.
    #[must_use]
    pub fn codec_volume(&self, own: u8) -> u8 {
        self.level.codec_volume(own)
    }

    /// The codec; `None` only while the lease is being dropped.
    #[must_use]
    pub const fn codec(&self) -> Option<&Device> {
        self.device.as_ref()
    }

    /// The codec, to play or record; `None` only while the lease is being
    /// dropped.
    pub const fn codec_mut(&mut self) -> Option<&mut Device> {
        self.device.as_mut()
    }
}

impl<Device> Drop for Lease<Device> {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            critical_section::with(|section| {
                self.shelf.borrow(section).replace(Some(device));
            });
        }
    }
}

/// Owns the Board's primary audio codec and publishes [`Sound`] and
/// [`Volume`].
#[barracuda_plugin::macros::plugin]
pub struct SoundPlugin<Device> {
    codec: Option<Device>,
}

impl<Device> SoundPlugin<Device> {
    /// Takes the built-in audio codec from the generated Board capability set.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: AudioCodecPeripheral<AudioCodec = Device>,
    {
        Self {
            codec: config::peripherals::AUDIO_CODEC_DEVICE
                .and_then(|instance| context.hal.peripherals.take_audio_codec(instance)),
        }
    }
}

impl<Device> Plugin for SoundPlugin<Device>
where
    Device: AudioCodec + Send + 'static,
{
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let sound = Sound::from_codec(self.codec.take());
        context.provide(Rc::new(sound.volume()))?;
        context.provide(Rc::new(sound))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn the_level_scales_and_mutes_every_sound() {
        let level = Level::new();
        assert_eq!(level.codec_volume(100), 255);
        assert_eq!(level.codec_volume(50), 128);
        level.volume.store(40, Ordering::Release);
        assert_eq!(level.codec_volume(100), 102);
        assert_eq!(level.codec_volume(50), 51);
        level.muted.store(true, Ordering::Release);
        assert_eq!(level.codec_volume(100), 0);
    }

    #[test]
    fn one_borrower_at_a_time_and_the_codec_comes_back() {
        let sound = Sound::from_codec(Some(7_u8));
        let lease = sound.lend();
        assert_eq!(lease.as_ref().and_then(Lease::codec), Some(&7));
        assert!(!sound.is_idle());
        assert!(sound.clone().lend().is_none());
        drop(lease);
        assert!(sound.is_idle() && sound.is_present());
        assert!(Sound::<u8>::from_codec(None).lend().is_none());
    }
}
