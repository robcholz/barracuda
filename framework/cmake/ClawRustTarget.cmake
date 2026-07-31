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
