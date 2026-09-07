MEMORY
{
  FLASH (rx)       : ORIGIN = 0x08000000, LENGTH = 512K /* filesystem: raw */
  DFU (rw)         : ORIGIN = 0x08080000, LENGTH = 512K /* filesystem: raw */
  BOOT_STATE (rw)  : ORIGIN = 0x08100000, LENGTH = 128K /* filesystem: raw */
  SYSTEM (rw)      : ORIGIN = 0x08120000, LENGTH = 256K /* filesystem: littlefs */
  RESOURCES (r)    : ORIGIN = 0x08160000, LENGTH = 256K /* filesystem: fatfs */
  KV_DATABASE (rw) : ORIGIN = 0x081A0000, LENGTH = 384K /* filesystem: raw */
  RAM (rwx)        : ORIGIN = 0x20000000, LENGTH = 128K
}

__active_start = ORIGIN(FLASH);
__active_end = ORIGIN(FLASH) + LENGTH(FLASH);
__dfu_start = ORIGIN(DFU);
__dfu_end = ORIGIN(DFU) + LENGTH(DFU);
__bootloader_state_start = ORIGIN(BOOT_STATE);
__bootloader_state_end = ORIGIN(BOOT_STATE) + LENGTH(BOOT_STATE);
__system_start = ORIGIN(SYSTEM);
__system_end = ORIGIN(SYSTEM) + LENGTH(SYSTEM);
__resources_start = ORIGIN(RESOURCES);
__resources_end = ORIGIN(RESOURCES) + LENGTH(RESOURCES);
__kv_database_start = ORIGIN(KV_DATABASE);
__kv_database_end = ORIGIN(KV_DATABASE) + LENGTH(KV_DATABASE);
