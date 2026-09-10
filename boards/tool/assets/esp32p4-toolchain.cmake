get_filename_component(BARRACUDA_WORKSPACE "${CMAKE_CURRENT_LIST_DIR}/../../.." ABSOLUTE)

set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR riscv32)

set(
    CMAKE_C_COMPILER
    "${BARRACUDA_WORKSPACE}/boards/tool/assets/riscv32-esp-elf-gcc"
    CACHE FILEPATH "ESP32-P4 C compiler" FORCE
)
set(
    CMAKE_CXX_COMPILER
    "${BARRACUDA_WORKSPACE}/boards/tool/assets/riscv32-esp-elf-g++"
    CACHE FILEPATH "ESP32-P4 C++ compiler" FORCE
)
set(
    CMAKE_ASM_COMPILER
    "${BARRACUDA_WORKSPACE}/boards/tool/assets/riscv32-esp-elf-gcc"
    CACHE FILEPATH "ESP32-P4 assembler" FORCE
)

set(
    CMAKE_AR
    "${BARRACUDA_WORKSPACE}/boards/tool/assets/riscv32-esp-elf-ar"
    CACHE FILEPATH "ESP32-P4 archiver" FORCE
)
set(
    CMAKE_RANLIB
    "${BARRACUDA_WORKSPACE}/boards/tool/assets/riscv32-esp-elf-ranlib"
    CACHE FILEPATH "ESP32-P4 archive indexer" FORCE
)
