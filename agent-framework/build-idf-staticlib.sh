#!/usr/bin/env bash
# Build the complete Rust agent runtime as an ESP-IDF-compatible static archive.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
    cat <<'EOF'
Usage: ./build-idf-staticlib.sh [--check]

Builds the complete prebuilt archive set for:
  esp32s3, esp32c5, esp32p4, and esp32s31

The archives are written to:
  crates/claw-cabi/prebuilt/<idf-target>/libclaw_cabi.a

With --check, newly built archives are compared byte-for-byte with the
committed prebuilt set without overwriting it.

Set CLAW_DEBUG=1 to build the rich-logging variant. Production logging is used
otherwise.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

check_only=0
if [[ "${1:-}" == "--check" ]]; then
    check_only=1
    shift
fi

if (( $# > 0 )); then
    usage >&2
    exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo is not available; install Rust before building" >&2
    exit 1
fi

if ! rustc +esp --version >/dev/null 2>&1; then
    echo "ERROR: the Espressif Rust toolchain is unavailable; run 'espup install' first" >&2
    exit 1
fi

cargo_target_dir="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"
if [[ "$cargo_target_dir" != /* ]]; then
    cargo_target_dir="${SCRIPT_DIR}/${cargo_target_dir}"
fi
export CARGO_TARGET_DIR="$cargo_target_dir"

output_root="${SCRIPT_DIR}/crates/claw-cabi/prebuilt"

# Keep release archives reproducible across developer machines and CI checkout
# paths. RUSTFLAGS overrides target rustflags from .cargo/config.toml, so retain
# the ESP-IDF 64-bit time_t cfg explicitly.
esp_sysroot="$(rustc +esp --print sysroot)"
cargo_home="${CARGO_HOME:-}"
if [[ -z "$cargo_home" ]]; then
    cargo_home="$(cd "$(dirname "$(command -v cargo)")/.." && pwd)"
fi
export RUSTFLAGS="--cfg espidf_time64 --remap-path-prefix=${SCRIPT_DIR}=/agent-framework --remap-path-prefix=${esp_sysroot}=/rust-esp --remap-path-prefix=${cargo_home}=/cargo"

logging_feature="prod_logging"
build_std_feature_args=(-Z build-std-features=optimize_for_size)
if [[ "${CLAW_DEBUG:-}" == "1" ]]; then
    logging_feature="rich_logging"
    build_std_feature_args=()
fi

cd "$SCRIPT_DIR"

# esp32p4 and esp32s31 use the same RISC-V ISA and ESP-IDF Rust ABI, so one
# Cargo build supplies both target-specific output directories.
rust_targets=(
    xtensa-esp32s3-espidf
    riscv32imac-esp-espidf
    riscv32imafc-esp-espidf
)

for rust_target in "${rust_targets[@]}"; do
    echo "Building claw-cabi for ${rust_target} (${logging_feature})"
    cargo +esp build \
        --locked \
        --release \
        --package claw-cabi \
        --no-default-features \
        --features "${logging_feature},claw-agent/reasoning_medium" \
        --target "$rust_target" \
        -Z build-std=std,panic_abort \
        "${build_std_feature_args[@]}"

    cargo_archive="${CARGO_TARGET_DIR}/${rust_target}/release/libclaw_cabi.a"
    if [[ ! -s "$cargo_archive" ]]; then
        echo "ERROR: Cargo did not produce the expected archive: $cargo_archive" >&2
        exit 1
    fi
done

idf_targets=(esp32s3 esp32c5 esp32p4 esp32s31)
for idf_target in "${idf_targets[@]}"; do
    case "$idf_target" in
        esp32s3)
            rust_target="xtensa-esp32s3-espidf"
            ;;
        esp32c5)
            rust_target="riscv32imac-esp-espidf"
            ;;
        esp32p4 | esp32s31)
            rust_target="riscv32imafc-esp-espidf"
            ;;
    esac

    cargo_archive="${CARGO_TARGET_DIR}/${rust_target}/release/libclaw_cabi.a"
    output_archive="${output_root}/${idf_target}/libclaw_cabi.a"
    if (( check_only )); then
        if [[ ! -f "$output_archive" ]]; then
            echo "ERROR: committed archive is missing: $output_archive" >&2
            exit 1
        fi
        if ! cmp -s "$cargo_archive" "$output_archive"; then
            echo "ERROR: prebuilt archive is stale: $output_archive" >&2
            echo "Run ./build-idf-staticlib.sh and commit all four archives." >&2
            exit 1
        fi
        echo "Matched: $output_archive"
    else
        output_dir="${output_root}/${idf_target}"
        mkdir -p "$output_dir"
        temporary_archive="$(mktemp "${output_archive}.tmp.XXXXXX")"
        trap 'rm -f "$temporary_archive"' EXIT
        install -m 0644 "$cargo_archive" "$temporary_archive"
        mv -f "$temporary_archive" "$output_archive"
        trap - EXIT
        echo "Built: $output_archive"
    fi
done

if (( check_only )); then
    echo "All committed prebuilt archives match."
else
    echo "ESP-IDF will use these prebuilt archives by default."
fi
