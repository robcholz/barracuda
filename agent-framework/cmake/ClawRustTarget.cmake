include_guard(GLOBAL)

# Resolve an ESP-IDF chip target to the Rust target with the matching ISA and
# ESP-IDF ABI. Keep this mapping centralized so every Cargo staticlib linked by
# ESP-IDF is built for the same architecture as the surrounding C firmware.
function(claw_resolve_rust_target output_variable idf_target)
    if(idf_target STREQUAL "esp32s3")
        set(rust_target "xtensa-esp32s3-espidf")
    elseif(idf_target STREQUAL "esp32c5")
        set(rust_target "riscv32imac-esp-espidf")
    elseif(idf_target STREQUAL "esp32p4" OR idf_target STREQUAL "esp32s31")
        set(rust_target "riscv32imafc-esp-espidf")
    else()
        message(FATAL_ERROR
            "Unsupported ESP-IDF target '${idf_target}' for the Rust runtime. "
            "Supported targets: esp32s3, esp32c5, esp32p4, esp32s31")
    endif()

    set(${output_variable} "${rust_target}" PARENT_SCOPE)
endfunction()

# ESP-IDF 5.x exposes its C library component as `newlib`, while ESP-IDF 6.x
# renamed the component to `esp_libc`. Resolve the imported CMake target at
# configure time so the Rust static libraries can be linked by either version.
function(claw_resolve_idf_libc_target output_variable)
    if(TARGET idf::esp_libc)
        set(libc_target "idf::esp_libc")
    elseif(TARGET idf::newlib)
        set(libc_target "idf::newlib")
    else()
        message(FATAL_ERROR
            "Unable to find the ESP-IDF libc component target. "
            "Expected either 'idf::esp_libc' or 'idf::newlib'.")
    endif()

    set(${output_variable} "${libc_target}" PARENT_SCOPE)
endfunction()
