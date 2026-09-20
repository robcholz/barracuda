# ESP32-P4 pre-v3 patch

`esp32p4-rev1.patch` is a three-commit patch series based on
`esp-hal-v1.2.0-rc.0` (`160b10794`). It adds the experimental
`esp32p4-rev1` feature for ESP32-P4 revisions below v3.0.

The patch currently covers the minimum runtime layer needed by Barracuda:

- the ECO0-ECO4 ROM linker profile;
- the non-contiguous pre-v3 SRAM layout;
- the 360/180/90 MHz CPU clock presets;
- removal of the v3.2-only CLIC/Zcmp lock workaround from the pre-v3 path;
- conservative defaults that disable PMP, stack watchpoints, and the v3
  peripheral-gate sweep until revision-specific implementations are added.

It was tested on an M5Stack Tab5 with ESP32-P4 revision v1.3. The unmodified
`hello_world` example completed `esp_hal::init()` and produced repeated output
through `Instant`-based timing.

Apply the series to the matching upstream tag with:

```sh
git checkout esp-hal-v1.2.0-rc.0
git am /path/to/barracuda/patches/esp-hal/esp32p4-rev1.patch
```

Then enable both `esp32p4` and `esp32p4-rev1`. This is a bootstrap patch, not
yet a claim of complete pre-v3 peripheral support. Flash/PSRAM, split-bank heap
placement, PMP, stack watchpoints, and the revision-specific peripheral clock
gate table still need dedicated validation before Barracuda can use its full
Tab5 configuration.
