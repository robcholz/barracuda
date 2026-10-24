/* The ESP32-C6 Platform's top linker script, which `platform.yml` names.

   It is esp-hal 1.2's `linkall.x` over `barracuda-esp32c6-memory.x` instead
   of esp-hal's `memory.x`: the same memory map, with the flash window the
   chip maps rather than esp-hal's 4 MiB. */
INCLUDE "barracuda-esp32c6-memory.x"

REGION_ALIAS("ROTEXT", ROM);
REGION_ALIAS("RODATA", ROM);

REGION_ALIAS("RWTEXT", RAM);
REGION_ALIAS("RWDATA", RAM);

REGION_ALIAS("RTC_FAST_RWTEXT", RTC_FAST);
REGION_ALIAS("RTC_FAST_RWDATA", RTC_FAST);

INCLUDE "esp32c6.x"
INCLUDE "hal-defaults.x"
