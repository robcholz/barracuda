# CMake toolchain for C libraries built for ESP32-P4 firmware. The compilers
# and flags come from the cc-style environment that `cargo board select`
# writes, through the wrappers in this directory.
set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR riscv32)
