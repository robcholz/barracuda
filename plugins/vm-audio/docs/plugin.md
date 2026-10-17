# Audio Plugin

The Audio Plugin owns the Board's primary audio codec, exposes playback and
recording to Lua code as the `audio` Lua package, and publishes the device's
output level to other Plugins as `AudioOutput`.

- Plugin ID: `audio`
- Dependencies: `vm`
- Settings: none
- Context resources: peripheral slot `audio-codec.device` (`primary`, optional)
- Provided capabilities: `AudioOutput`
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `audio`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary `AudioCodec` capability exactly
once and installs the `audio` Lua package. Codec register control stays in the
ES8311/ES8389 implementation, while I2S controller, DMA, and bounded buffers stay in
the Platform HAL composition.

Lua API:

- `audio.available() -> boolean`
- `audio.open() -> handle`
- `audio.beep([hz [, ms]]) -> true` plays a square-wave tone, 880 Hz for
  150 ms unless given, 20 to 20000 Hz for at most 2000 ms
- `handle:descriptor() -> sample_rate_hz, channels, bits_per_sample`
- `handle:set_volume(percent)`
- `handle:play(pcm_le_bytes)`
- `handle:play_wav(wav_bytes)`
- `handle:record(frames) -> pcm_le_bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

`set_volume` accepts an integer percentage from 0 through 100: the app's own
volume, which scales the device's level. `AudioOutput::set(volume, muted)`
sets that level, as the system UI does from the person's volume and mute, and
applies to every app: the codec's portable 0-through-255 output is the
device's percent times the app's percent, or 0 while muted. A handle puts the
level on the codec when its app changes its volume and before each chunk it
plays, so a change reaches a sound that is playing. `descriptor`
reports the required stream format.
`play` and `record` use interleaved signed 16-bit little-endian PCM matching
that format. `play_wav` accepts an uncompressed 16-bit integer PCM RIFF/WAVE
file, validates its sample rate and channel count against the codec, skips
unknown chunks, and plays its `data` chunk. Compressed WAV formats are not
decoded. One PCM operation transfers at most 256 KiB, and a WAV file may carry
at most 64 KiB of container overhead.

While the Board has the device, the package declares a stable interface
(`PackageInterface::Stable`), version 2; version 2 added `audio.beep`.

`beep`, from version 2, plays at the device's level while no handle holds
the codec and returns `nil, error` while one does.

Boards without a built-in codec report unavailable. One handle is open at a
time: explicit close, lexical `<close>`, and collection prevent further
operations on it and give the codec back, so `open` works again; Plugin
revocation prevents every further operation.
