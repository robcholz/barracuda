# I2S Plugin

- Plugin ID: `i2s`
- Direct Plugin dependencies: `vm`
- Required typed capability: `barracuda_vm_package_api::LuaPackageRegistry` from `vm`
- Provided typed capabilities: none

This Plugin shares the Board's unified exposed-I/O owner and installs the
`i2s` Lua package. It reuses the same `audio::PcmStream` contract implemented
by Platform I2S adapters and consumed by built-in audio codec Drivers.

Lua API:

- `i2s.open(bclk, ws, dout, din, mclk, sample_rate_hz, channels, bits_per_sample) -> handle`
- `handle:format() -> sample_rate_hz, channels, bits_per_sample, master_clock_hz`
- `handle:write(pcm_le_bytes)`
- `handle:read(frames) -> pcm_le_bytes`
- `handle:is_open() -> boolean`
- `handle:close()`

`dout`, `din`, and `mclk` may be `nil`; at least one data direction is
required. PCM bytes are interleaved signed 16-bit little-endian samples. The
current portable stream contract therefore accepts `bits_per_sample = 16`.
Opening atomically consumes every signal pin from the same owner used by all
other exposed protocols.
