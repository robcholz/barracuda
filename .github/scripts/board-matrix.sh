#!/usr/bin/env bash
# Prints every device Board, grouped by compilation target, as a GitHub
# Actions matrix. CI checks and builds each group; the release workflow
# builds and packages it. Boards sharing a target run on one runner, or a few
# when there are many, so the expensive dependency graph is reused.
set -euo pipefail

entries='[]'
while IFS= read -r board_file; do
  board="$(basename "$(dirname "$board_file")")"

  # Host Boards flash no firmware; CI tests them in its host jobs.
  if [[ "$board" == "local-linux" || "$board" == "local-macos" ]]; then
    continue
  fi

  target="$(cargo board target "$board")"
  os=ubuntu-latest
  toolchain=rust
  chip=''
  c_toolchain=none
  key="$target"

  if [[ -z "$target" ]]; then
    key="$board"
    toolchain=host
    if [[ "$board" == "local-macos" ]]; then
      os=macos-latest
    fi
  elif [[ "$target" == xtensa-* ]]; then
    toolchain=esp
    chip="${target#xtensa-}"
    chip="${chip%-none-elf}"
  elif [[ "$target" == riscv32imafc-* ]]; then
    # The prebuilt riscv32imafc standard libraries mix float ABIs, so
    # ESP32-P4 builds core and alloc with espup's esp toolchain.
    toolchain=esp
    c_toolchain=esp-riscv
  elif [[ "$target" == riscv32* ]]; then
    c_toolchain=esp-riscv
  elif [[ "$target" == thumb* ]]; then
    c_toolchain=arm
  fi

  entry="$(jq -cn \
    --arg board "$board" \
    --arg key "$key" \
    --arg os "$os" \
    --arg toolchain "$toolchain" \
    --arg chip "$chip" \
    --arg c_toolchain "$c_toolchain" \
    '{board: $board, key: $key, os: $os, toolchain: $toolchain,
      chip: $chip, c_toolchain: $c_toolchain}')"
  entries="$(jq -cn \
    --argjson entries "$entries" \
    --argjson entry "$entry" \
    '$entries + [$entry]')"
done < <(find boards/configs -mindepth 2 -maxdepth 2 -type f -name board.yml | sort)

# A target with more Boards than this is split across runners, so no group
# outlasts the Boards job's timeout; every part shares the target's cache.
group_size=12

matrix="$(jq -cn --argjson entries "$entries" --argjson size "$group_size" '
  {include: ($entries
    | sort_by(.key, .board)
    | group_by(.key)
    | map(. as $target
      | [range(0; $target | length; $size)]
      | map($target[.:(. + $size)] as $part | {
          key: $part[0].key,
          group: (if ($target | length) > $size
            then "\($part[0].key)-\(. / $size + 1)"
            else $part[0].key
            end),
          label: (if ($part | length) == 1 then $part[0].board
            else "\($part[0].board) + \(($part | length) - 1) more"
            end),
          os: $part[0].os,
          toolchain: $part[0].toolchain,
          chip: $part[0].chip,
          c_toolchain: $part[0].c_toolchain,
          boards: ($part | map(.board))
        }))
    | add)}')"
jq -c . <<< "$matrix"
