/* The ESP32 Platform's top linker script, which `platform.yml` names.

   It is esp-hal 1.2's `linkall.x` over `barracuda-esp32-memory.x` instead of
   esp-hal's `memory.x`: the same memory map, with the instruction-flash
   window the chip maps rather than esp-hal's 3 MiB. */
INCLUDE "barracuda-esp32-memory.x"
INCLUDE "alias.x"
INCLUDE "esp32.x"
INCLUDE "hal-defaults.x"
