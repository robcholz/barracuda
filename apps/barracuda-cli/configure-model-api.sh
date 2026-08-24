#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
env_file="${BARRACUDA_ENV_FILE:-${script_directory}/.env.local}"
endpoint="${BARRACUDA_MODEL_API_ENDPOINT:-http://127.0.0.1:8787/api/model-api}"

if [[ ! -f "${env_file}" ]]; then
    printf 'missing environment file: %s\n' "${env_file}" >&2
    exit 1
fi

for command_name in jq curl; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        printf 'required command is not installed: %s\n' "${command_name}" >&2
        exit 1
    fi
done

set -a
# shellcheck disable=SC1090
source "${env_file}"
set +a

for variable_name in \
    BARRACUDA_LLM_API_KEY \
    BARRACUDA_LLM_MODEL \
    BARRACUDA_LLM_BASE_URL; do
    if [[ -z "${!variable_name:-}" ]]; then
        printf 'missing required value in %s: %s\n' "${env_file}" "${variable_name}" >&2
        exit 1
    fi
done

backend="${BARRACUDA_LLM_BACKEND:-openai_compatible}"
timeout_ms="${BARRACUDA_LLM_TIMEOUT_MS:-30000}"
max_tokens="${BARRACUDA_LLM_MAX_TOKENS:-4096}"
image_max_bytes="${BARRACUDA_LLM_IMAGE_MAX_BYTES:-1048576}"

for purpose in root_agent sub_agent memory compaction; do
    is_default=false
    if [[ "${purpose}" == root_agent ]]; then
        is_default=true
    fi

    status="$({
        jq -nc \
            --arg api_key "${BARRACUDA_LLM_API_KEY}" \
            --arg model "${BARRACUDA_LLM_MODEL}" \
            --arg base_url "${BARRACUDA_LLM_BASE_URL}" \
            --arg backend "${backend}" \
            --arg purpose "${purpose}" \
            --argjson default "${is_default}" \
            --argjson timeout_ms "${timeout_ms}" \
            --argjson max_tokens "${max_tokens}" \
            --argjson image_max_bytes "${image_max_bytes}" \
            '{
                timeout_ms: $timeout_ms,
                max_tokens: $max_tokens,
                image_max_bytes: $image_max_bytes,
                backend: $backend,
                purpose: $purpose,
                default: $default,
                api_key: $api_key,
                model: $model,
                base_url: $base_url
            }'
    } | curl \
        --silent \
        --show-error \
        --output /dev/null \
        --write-out '%{http_code}' \
        --header 'Content-Type: application/json' \
        --data-binary @- \
        "${endpoint}")"

    if [[ "${status}" != 204 ]]; then
        printf 'failed to configure %s: HTTP %s\n' "${purpose}" "${status}" >&2
        exit 1
    fi

    printf 'configured %s from %s\n' "${purpose}" "${env_file}"
done
