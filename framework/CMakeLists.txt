idf_component_register(
    SRCS
        "crates/claw-sys/csrc/claw_rs_log.c"
    INCLUDE_DIRS
        "crates/claw-cabi/include"
    REQUIRES
        claw_cap
        esp_http_client
        esp-tls
        mbedtls
        freertos
        pthread
    WHOLE_ARCHIVE
)

include("${CMAKE_CURRENT_SOURCE_DIR}/cmake/ClawRustTarget.cmake")
claw_resolve_rust_target(claw_rust_target "${IDF_TARGET}")
claw_resolve_idf_libc_target(claw_idf_libc_target)
set(claw_rust_profile "release")
set(claw_rust_lib "${CMAKE_CURRENT_SOURCE_DIR}/target/${claw_rust_target}/${claw_rust_profile}/libclaw_cabi.a")

if("$ENV{CLAW_DEBUG}" STREQUAL "1")
    set(claw_rust_logging_feature "rich_logging")
    set(claw_rust_build_std_feature_args)
else()
    set(claw_rust_logging_feature "prod_logging")
    set(claw_rust_build_std_feature_args -Z build-std-features=optimize_for_size)
endif()

add_custom_target(claw_rust_build
    BYPRODUCTS "${claw_rust_lib}"
    COMMAND cargo +esp build --release -p claw-cabi
            --features ${claw_rust_logging_feature},claw-agent/reasoning_medium
            --target ${claw_rust_target}
            -Z build-std=std,panic_abort
            ${claw_rust_build_std_feature_args}
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    COMMENT "Building claw-cabi Rust staticlib (${claw_rust_target})"
    VERBATIM
    USES_TERMINAL
)

add_library(claw_cabi_rust STATIC IMPORTED GLOBAL)
set_target_properties(claw_cabi_rust PROPERTIES
    IMPORTED_LOCATION "${claw_rust_lib}"
    INTERFACE_LINK_LIBRARIES "idf::esp_http_client;idf::esp-tls;idf::mbedtls;${claw_idf_libc_target};idf::freertos;idf::pthread"
)
add_dependencies(claw_cabi_rust claw_rust_build)

target_link_libraries(${COMPONENT_LIB} PUBLIC claw_cabi_rust)
add_dependencies(${COMPONENT_LIB} claw_rust_build)
