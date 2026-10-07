/* STM32U5A5ZJ: 4 MiB dual-bank flash (8 KiB pages), 768 KiB SRAM1. */
MEMORY
{
  FLASH (rx)       : ORIGIN = 0x08000000, LENGTH = 2560K /* filesystem: raw */
  SYSTEM (rw)      : ORIGIN = 0x08280000, LENGTH = 1024K /* filesystem: littlefs */
  RESOURCES (r)    : ORIGIN = 0x08380000, LENGTH = 256K /* filesystem: fatfs */
  KV_DATABASE (rw) : ORIGIN = 0x083C0000, LENGTH = 256K /* filesystem: raw */
  RAM (rwx)        : ORIGIN = 0x20000000, LENGTH = 768K
}

__active_start = ORIGIN(FLASH);
__active_end = ORIGIN(FLASH) + LENGTH(FLASH);
__system_start = ORIGIN(SYSTEM);
__system_end = ORIGIN(SYSTEM) + LENGTH(SYSTEM);
__resources_start = ORIGIN(RESOURCES);
__resources_end = ORIGIN(RESOURCES) + LENGTH(RESOURCES);
__kv_database_start = ORIGIN(KV_DATABASE);
__kv_database_end = ORIGIN(KV_DATABASE) + LENGTH(KV_DATABASE);
