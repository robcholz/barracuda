# Audio Plugin

The Audio Plugin owns the Board's primary built-in audio codec and exposes
playback and recording to Lua code as the `audio` Lua package.

- Plugin ID: `audio`
- Dependencies: `vm`
- Settings: none
- Context resources: `hal.peripherals.take_audio_codec()`
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`
- Workflow: none
- Agent Tools: none
- Lua packages: `audio`
- Web: none
- Tasks: none
- Storage: none

This Plugin takes the Board's primary built-in `AudioCodec` capability exactly
once and installs the `audio` Lua package. Codec register control stays in the
ES8311/ES8389 implementation, while I2S controller, DMA, and bounded buffers stay in
the Platform HAL composition.

Lua API:

- `audio.available() -> boolean`
- `audio.open() -> handle`
- `handle:descriptor() -> sample_rate_hz, channels, bits_per_sample`
- `handle:set_volume(percent)`
- `handle:play(pcm_le_bytes)`
- `handle:play_wav(wav_bytes)`
- `handle:record(frames) -> pcm_le_bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

`set_volume` accepts an integer percentage from 0 through 100 and maps it to
the codec capability's portable 0-through-255 output scale. `descriptor`
reports the required stream format.
`play` and `record` use interleaved signed 16-bit little-endian PCM matching
that format. `play_wav` accepts an uncompressed 16-bit integer PCM RIFF/WAVE
file, validates its sample rate and channel count against the codec, skips
unknown chunks, and plays its `data` chunk. Compressed WAV formats are not
decoded. One PCM operation transfers at most 256 KiB, and a WAV file may carry
at most 64 KiB of container overhead.

Boards without a built-in codec report unavailable. The move-only built-in
capability permits one handle per boot; explicit close, lexical `<close>`,
collection, and Plugin revocation prevent further operations and do not
recreate the physical codec token.
