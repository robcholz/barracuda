# Audio Plugin

- Plugin ID: `audio`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none
- Workflow Actions: none
- Events: none
- Agent Tools: none
- Owned long-running tasks: none

This Plugin takes the Board's primary built-in `AudioCodec` capability exactly
once and installs the `audio` Lua package. Codec register control stays in the
ES8311/ES8389 Driver, while I2S controller, DMA, and bounded buffers stay in
the Platform HAL composition.

Lua API:

- `audio.available() -> boolean`
- `audio.open() -> handle`
- `handle:format() -> sample_rate_hz, channels, bits_per_sample`
- `handle:set_volume(percent)`
- `handle:play(pcm_le_bytes)`
- `handle:record(frames) -> pcm_le_bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

`set_volume` accepts an integer percentage from 0 through 100 and maps it to
the codec capability's portable 0-through-255 output scale. `play` and
`record` use interleaved signed 16-bit little-endian PCM matching the format
reported by `format`; the Plugin does not decode container or compressed audio
formats. One operation transfers at most 256 KiB.

Boards without a built-in codec report unavailable. The move-only built-in
capability permits one handle per boot; explicit close, lexical `<close>`,
collection, and Plugin revocation prevent further operations and do not
recreate the physical codec token.
