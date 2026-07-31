include("${CMAKE_CURRENT_LIST_DIR}/../ClawRustTarget.cmake")

function(assert_rust_target idf_target expected_target)
    claw_resolve_rust_target(actual_target "${idf_target}")
    if(NOT actual_target STREQUAL expected_target)
        message(FATAL_ERROR
            "IDF target '${idf_target}' resolved to '${actual_target}', "
            "expected '${expected_target}'")
    endif()
endfunction()

assert_rust_target("esp32s3" "xtensa-esp32s3-espidf")
assert_rust_target("esp32c5" "riscv32imac-esp-espidf")
assert_rust_target("esp32p4" "riscv32imafc-esp-espidf")
assert_rust_target("esp32s31" "riscv32imafc-esp-espidf")
