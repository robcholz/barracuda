/*
 * SPDX-FileCopyrightText: 2026 Espressif Systems (Shanghai) CO LTD
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    /* Initial root API fields. All four may be NULL/empty to initialize the
     * runtime without an API and bind one later with claw_agent_link_api(). */
    const char *api_key;
    /* UTF-8 C string, e.g. "openai_compatible". */
    const char *backend_type;
    /* UTF-8 C string. */
    const char *model;
    /* UTF-8 C string. */
    const char *base_url;
    /* Required non-null UTF-8 C string. */
    const char *persistence_dir;
    /* Optional UTF-8 C string; may be NULL. Writable DATA skills root, e.g.
     * "<DATA>/skills". Scanned first so it takes priority over the system root. */
    const char *skills_root_dir;
    /* Optional UTF-8 C string; may be NULL. Read-only firmware skills root,
     * e.g. "/system/skills". Scanned after the DATA root. */
    const char *system_skills_root_dir;
} claw_agent_config_t;

typedef struct {
    /* Required non-null UTF-8 C string. */
    const char *api_key;
    /* Required non-null UTF-8 C string, e.g. "openai_compatible". */
    const char *backend_type;
    /* Required non-null UTF-8 C string. */
    const char *model;
    /* Required non-null UTF-8 C string. */
    const char *base_url;
} claw_agent_api_config_t;

typedef enum {
    CLAW_AGENT_API_PURPOSE_ROOT_AGENT = 0,
    CLAW_AGENT_API_PURPOSE_SUBAGENT = 1,
    CLAW_AGENT_API_PURPOSE_MEMORY = 2,
    CLAW_AGENT_API_PURPOSE_COMPACTION = 3,
} claw_agent_api_purpose_t;

typedef enum {
    CLAW_AGENT_SESSION_PERSISTENCE_PERSISTENT = 0,
    CLAW_AGENT_SESSION_PERSISTENCE_EPHEMERAL = 1,
} claw_agent_session_persistence_t;

typedef enum {
    CLAW_AGENT_EVENT_KIND_TURN_STARTED = 0,
    CLAW_AGENT_EVENT_KIND_INPUT_REQUESTED = 1,
    CLAW_AGENT_EVENT_KIND_ITERATION_STARTED = 2,
    CLAW_AGENT_EVENT_KIND_REASONING_DELTA = 3,
    CLAW_AGENT_EVENT_KIND_REASONING_END = 4,
    CLAW_AGENT_EVENT_KIND_OUTPUT_DELTA = 5,
    CLAW_AGENT_EVENT_KIND_OUTPUT_END = 6,
    CLAW_AGENT_EVENT_KIND_TOOL_CALL = 7,
    CLAW_AGENT_EVENT_KIND_TOOL_CALLS_END = 8,
    CLAW_AGENT_EVENT_KIND_ITERATION_ENDED = 9,
    CLAW_AGENT_EVENT_KIND_TURN_ENDED = 10,
    CLAW_AGENT_EVENT_KIND_ERROR = 11,
    CLAW_AGENT_EVENT_KIND_CLOSED = 12,
} claw_agent_event_kind_t;

typedef enum {
    CLAW_AGENT_TURN_ORIGIN_USER = 0,
    CLAW_AGENT_TURN_ORIGIN_SUBAGENT = 1,
} claw_agent_turn_origin_t;

typedef enum {
    CLAW_AGENT_INPUT_REQUEST_KIND_PERMISSION_APPROVAL = 0,
} claw_agent_input_request_kind_t;

typedef struct {
    uint32_t turn_id;
    claw_agent_turn_origin_t origin;
    /* Non-zero only for SUBAGENT origin. */
    uint32_t agent_id;
} claw_agent_turn_started_event_t;

typedef struct {
    /* All three strings are owned and contain one complete tool call. */
    char *id;
    char *name;
    char *arguments_json;
} claw_agent_tool_call_event_t;

typedef struct {
    uint32_t request_id;
    claw_agent_input_request_kind_t kind;
    claw_agent_tool_call_event_t tool_call;
    /* Owned UTF-8 reason supplied by the permission policy. */
    char *reason;
} claw_agent_input_requested_event_t;

typedef struct {
    uint32_t iteration_id;
} claw_agent_iteration_event_t;

typedef struct {
    /* Owned UTF-8 append fragment. */
    char *text;
} claw_agent_text_delta_event_t;

typedef struct {
    uint32_t turn_id;
} claw_agent_turn_ended_event_t;

typedef struct {
    /* Owned UTF-8 error message. */
    char *message;
} claw_agent_error_event_t;

typedef union {
    claw_agent_turn_started_event_t turn_started;
    claw_agent_input_requested_event_t input_requested;
    /* Used by ITERATION_STARTED. ITERATION_ENDED has no payload. */
    claw_agent_iteration_event_t iteration;
    /* Used by REASONING_DELTA and OUTPUT_DELTA. */
    claw_agent_text_delta_event_t text_delta;
    claw_agent_tool_call_event_t tool_call;
    claw_agent_turn_ended_event_t turn_ended;
    claw_agent_error_event_t error;
    uint32_t reserved;
} claw_agent_event_data_t;

typedef struct {
    claw_agent_event_kind_t kind;
    /* Read only the union member selected by kind. Owned strings in that
     * member remain valid until claw_agent_event_free(). */
    claw_agent_event_data_t data;
} claw_agent_event_t;

/*
 * Construct the agent runtime in the stopped state.
 *
 * This creates and retains AgentSystem, restores persistent state, and
 * registers capability tools. If all four initial API fields are configured,
 * it also links them as the default ROOT_AGENT API. If all four are empty, the
 * runtime starts unbound and may be configured later with claw_agent_link_api().
 * Session operations remain disabled until claw_agent_start().
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG if config is NULL, the initial API is only partially
 *   configured, a required runtime string is invalid, or backend_type is unknown.
 * - ESP_ERR_INVALID_STATE if the runtime is already initialized.
 */
esp_err_t claw_agent_init(const claw_agent_config_t *config);

/*
 * Link or replace an LLM API config for one purpose after claw_agent_init(). It
 * may be called before or after claw_agent_start(). A running agent observes
 * the change at the next turn boundary without disturbing an in-flight turn;
 * the stopped runtime retains the same AgentSystem and therefore the same
 * binding. Bindings survive a claw_agent_stop()/claw_agent_start() cycle and
 * are released only by claw_agent_deinit(). If is_default is true, this model
 * is also the fallback for purposes without an explicit binding.
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG if config/purpose is invalid or a required string is
 *   missing, non-UTF-8, or empty.
 * - ESP_ERR_INVALID_STATE if the runtime is not initialized.
 */
esp_err_t claw_agent_link_api(const claw_agent_api_config_t *config,
                              claw_agent_api_purpose_t purpose,
                              bool is_default);

/*
 * Transition an initialized runtime from stopped to running.
 *
 * This activates the registered tool set and enables session operations. It
 * does not reconstruct AgentSystem; the instance created by init is retained.
 *
 * Returns:
 * - ESP_OK on success or if already started.
 * - ESP_ERR_INVALID_STATE if the runtime was not initialized.
 * - ESP_FAIL for tool activation failures.
 */
esp_err_t claw_agent_start(void);

/*
 * Transition a running runtime back to the initialized/stopped state.
 *
 * This deactivates the registered tool set and disables session operations.
 * AgentSystem, API bindings, session state, and open session connections are
 * retained for a later start. It does not perform deinitialization.
 *
 * Returns:
 * - ESP_OK on success or if initialized but not running.
 * - ESP_ERR_INVALID_STATE if the runtime was not initialized.
 * - ESP_FAIL for tool deactivation failures.
 */
esp_err_t claw_agent_stop(void);

/*
 * Stop if needed, then release the runtime and its AgentSystem.
 *
 * This is the only lifecycle call that destroys API bindings, in-memory
 * sessions, open streams, and the underlying orchestrator worker.
 *
 * Returns:
 * - ESP_OK on success or if already deinitialized.
 * - ESP_FAIL if tool deactivation failed; runtime state is still released.
 */
esp_err_t claw_agent_deinit(void);

/*
 * Open a numeric session's event stream.
 *
 * session_id must be non-zero and refer to a live session returned by
 * claw_agent_session_create().
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG for invalid session arguments.
 * - ESP_ERR_INVALID_STATE if the runtime is not started or the session is
 *   already open.
 * - ESP_ERR_NOT_FOUND if session_id is not live.
 */
esp_err_t claw_agent_session_open(uint32_t session_id);

/*
 * Submit input for an open numeric session id.
 *
 * session_id must be non-zero and already opened with claw_agent_session_open().
 *
 * text must be a non-NULL UTF-8 string.
 *
 * Returns:
 * - ESP_OK after the worker accepts the input.
 * - ESP_ERR_INVALID_ARG for invalid text/session arguments.
 * - ESP_ERR_INVALID_STATE if the runtime is not started, is stopping, or the
 *   session already has an active foreground submit.
 * - ESP_ERR_NOT_FOUND if session_id is not open.
 * - ESP_FAIL for unexpected scheduling failures.
 */
esp_err_t claw_agent_session_submit(uint32_t session_id, const char *text);

/*
 * Respond to INPUT_REQUESTED inside the current turn.
 *
 * request_id must match the id delivered by the latest INPUT_REQUESTED event.
 * Unlike claw_agent_session_submit(), this resumes the existing turn instead
 * of starting a new one.
 *
 * Returns:
 * - ESP_OK after the worker accepts the response.
 * - ESP_ERR_INVALID_ARG for a zero id or invalid text.
 * - ESP_ERR_INVALID_STATE if the request is stale, the session is not waiting
 *   for input, or the runtime is not running.
 * - ESP_ERR_NOT_FOUND if session_id is not open.
 */
esp_err_t claw_agent_session_respond(uint32_t session_id,
                                     uint32_t request_id,
                                     const char *text);

/*
 * Request graceful interruption of the active foreground turn.
 *
 * The stream may not emit TURN_ENDED immediately; keep receiving events.
 *
 * Returns:
 * - ESP_OK if the request was accepted or already unnecessary.
 * - ESP_ERR_INVALID_ARG for invalid session id.
 * - ESP_ERR_INVALID_STATE if the runtime is not started.
 * - ESP_ERR_NOT_FOUND if session_id is not open.
 */
esp_err_t claw_agent_session_interrupt(uint32_t session_id);

/*
 * Request hard cancellation of foreground and background work in a session.
 *
 * The stream may not emit TURN_ENDED/CLOSED immediately; keep receiving
 * session events.
 *
 * Returns:
 * - ESP_OK if the request was accepted or already unnecessary.
 * - ESP_ERR_INVALID_ARG for invalid session id.
 * - ESP_ERR_INVALID_STATE if the runtime is not started.
 * - ESP_ERR_NOT_FOUND if session_id is not open.
 */
esp_err_t claw_agent_session_cancel(uint32_t session_id);

/*
 * Create a new numeric session id with caller-selected persistence.
 *
 * PERSISTENT sessions are checkpointed and survive a runtime restart.
 * EPHEMERAL sessions remain in memory for this process only.
 * out_session_id must be non-NULL.
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG if persistence is unknown or out_session_id is NULL.
 * - ESP_ERR_INVALID_STATE if the runtime is not started or is stopping.
 */
esp_err_t claw_agent_session_create(claw_agent_session_persistence_t persistence,
                                    uint32_t *out_session_id);

/*
 * List live numeric session ids.
 *
 * out_count must be non-NULL. On every successful or ESP_ERR_INVALID_SIZE
 * return, out_count receives the total live session count. out_session_ids may
 * be NULL only when capacity is 0.
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG if out_count is NULL, or out_session_ids is NULL while
 *   capacity is non-zero.
 * - ESP_ERR_INVALID_STATE if the runtime is not started or is stopping.
 * - ESP_ERR_INVALID_SIZE if capacity is smaller than the live session count.
 */
esp_err_t claw_agent_session_list(uint32_t *out_session_ids,
                                  size_t capacity,
                                  size_t *out_count);

/*
 * Close an open numeric session stream.
 *
 * session_id must be non-zero and open. Closing cancels live work associated
 * with the open stream and eventually yields CLAW_AGENT_EVENT_KIND_CLOSED. The
 * session id remains live and may be opened again.
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG if session_id is 0.
 * - ESP_ERR_INVALID_STATE if the runtime is not started or is stopping.
 * - ESP_ERR_NOT_FOUND if session_id is not open.
 */
esp_err_t claw_agent_session_close(uint32_t session_id);

/*
 * Delete a live numeric session id.
 *
 * session_id must be non-zero and live. If the session has an open stream,
 * deletion cancels live work and eventually yields CLAW_AGENT_EVENT_KIND_CLOSED.
 *
 * Returns:
 * - ESP_OK on success.
 * - ESP_ERR_INVALID_ARG if session_id is 0.
 * - ESP_ERR_INVALID_STATE if the runtime is not started or is stopping.
 * - ESP_ERR_NOT_FOUND if session_id is not live.
 */
esp_err_t claw_agent_session_delete(uint32_t session_id);

/*
 * Receive the next event from an open session, one event per call.
 *
 * A session is consumed incrementally: call this in a loop, handling each event
 * as it arrives. TURN_ENDED closes one turn but the stream remains open for
 * future user submits and detached-subagent turns. CLOSED alone is terminal.
 *
 * session_id must be non-zero and open. out_event must be non-NULL. On ESP_OK,
 * the payload union member selected by out_event->kind is valid. Any owned
 * strings in that member remain valid until claw_agent_event_free().
 *
 * timeout_ms == 0 performs a non-blocking poll (returns the next buffered event
 * or ESP_ERR_TIMEOUT immediately). Otherwise it waits up to timeout_ms for the
 * next event; on timeout the session stream is retained and a later call
 * resumes it.
 *
 * Returns:
 * - ESP_OK with out_event populated (inspect out_event->kind).
 * - ESP_ERR_INVALID_ARG if session_id is 0 or out_event is NULL.
 * - ESP_ERR_INVALID_STATE if the runtime is not started.
 * - ESP_ERR_NOT_FOUND if session_id is not open.
 * - ESP_ERR_TIMEOUT if no event is available before timeout_ms.
 * - ESP_FAIL for unexpected event allocation failures.
 */
esp_err_t claw_agent_session_receive(uint32_t session_id,
                                     claw_agent_event_t *out_event,
                                     uint32_t timeout_ms);

/*
 * Free owned strings returned by claw_agent_session_receive.
 *
 * event may be NULL. After return, the event is reset to CLOSED with no owned
 * payload.
 */
void claw_agent_event_free(claw_agent_event_t *event);

#ifdef __cplusplus
}
#endif
