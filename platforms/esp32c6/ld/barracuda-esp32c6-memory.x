/* esp-hal 1.2.2's `ld/esp32c6/memory.x` with one change: `ROM` spans the
   16 MiB flash window the chip's flash MMU maps, 256 entries of 64 KiB from
   0x42000000 (ESP-IDF `IDRAM0_2_SEG_SIZE`, `CONFIG_MMU_PAGE_SIZE << 8`, in
   `components/esp_system/ld/esp32c6/memory.ld.in`; `SOC_IROM_LOW`/
   `SOC_IROM_HIGH` in `soc/esp32c6/include/soc/soc.h`). esp-hal stops at
   4 MiB. The ESP-IDF second-stage bootloader maps both of the image's flash
   segments at their full size (`set_cache_and_start_app`). */
MEMORY
{
     /* MEMORY_MAP = [
        [0x00000000, 0x00010000, "PADDING"],
        [0x42800000, 0x43000000, "DROM"],
        [0x40800000, 0x40880000, "RAM"],
        [0x40800000, 0x40880000, "BYTE_ACCESSIBLE"],
        [0x4004AC00, 0x40050000, "DROM_MASK"],
        [0x40000000, 0x4004AC00, "ROM_MASK"],
        [0x42000000, 0x42800000, "ROM"],
        [0x40800000, 0x40880000, "RAM"],
        [0x50000000, 0x50004000, "RTC_RAM"],
        [0x50000000, 0x50004000, "RTC_RAM"],
        [0x600FE000, 0x60100000, "MEM_INTERNAL2"],
    ] */

    /* 512K of on soc RAM, 32K reserved for cache */
    /* Instruction and Data RAM
    0x4086E610 = 2nd stage bootloader iram_loader_seg start address
    see https://github.com/espressif/esp-idf/blob/03414a15508036c8fc0f51642aed7a264e9527df/components/esp_system/ld/esp32c6/memory.ld.in#L26
    */
    RAM : ORIGIN = 0x40800000 , LENGTH = 0x6E610

    /* memory available after the 2nd stage bootloader is finished */
    dram2_seg ( RW )       : ORIGIN = ORIGIN(RAM) + LENGTH(RAM), len = 0x4087e610 - (ORIGIN(RAM) + LENGTH(RAM))

    /* External flash
     The 0x20 offset is a convenience for the app binary image generation.
     Flash cache has 64KB pages. The .bin file which is flashed to the chip
     has a 0x18 byte file header, and each segment has a 0x08 byte segment
     header. Setting this offset makes it simple to meet the flash cache MMU's
     constraint that (paddr % 64KB == vaddr % 64KB).)
    */

    /* Instruction and Data ROM */
    ROM : ORIGIN =   0x42000000 + 0x20, LENGTH = 0x1000000 - 0x20

    /* RTC fast memory (executable). Persists over deep sleep. */
    RTC_FAST : ORIGIN = 0x50000000, LENGTH = 16K /*- ESP_BOOTLOADER_RESERVE_RTC*/
}
