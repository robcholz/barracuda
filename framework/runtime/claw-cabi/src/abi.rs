use core::ffi::{c_char, c_int};

pub type EspErr = c_int;

pub const ESP_OK: EspErr = 0;
pub const ESP_FAIL: EspErr = -1;
pub const ESP_ERR_INVALID_ARG: EspErr = 0x102;
pub const ESP_ERR_INVALID_STATE: EspErr = 0x103;
pub const ESP_ERR_INVALID_SIZE: EspErr = 0x104;
pub const ESP_ERR_NOT_FOUND: EspErr = 0x105;
pub const ESP_ERR_TIMEOUT: EspErr = 0x107;

pub const CLAW_CAP_KIND_CALLABLE: c_int = 0;
pub const CLAW_CAP_KIND_HYBRID: c_int = 2;

pub const CLAW_CAP_CALLER_AGENT: c_int = 1;

pub const CLAW_CAP_FLAG_CALLABLE_BY_LLM: u32 = 1 << 0;
pub const CLAW_CAP_FLAG_ROOT_AGENT_ONLY: u32 = 1 << 4;

pub const TOOL_OUTPUT_CAPACITY: usize = 16 * 1024;

#[repr(C)]
pub struct ClawAgentConfig {
    pub api_key: *const c_char,
    pub backend_type: *const c_char,
    pub model: *const c_char,
    pub base_url: *const c_char,
    pub persistence_dir: *const c_char,
    pub skills_root_dir: *const c_char,
    pub system_skills_root_dir: *const c_char,
}

/// One LLM API configuration linked to an agent usage.
#[repr(C)]
pub struct ClawAgentApiConfig {
    pub api_key: *const c_char,
    pub backend_type: *const c_char,
    pub model: *const c_char,
    pub base_url: *const c_char,
}

pub const CLAW_AGENT_API_USAGE_ROOT_AGENT: c_int = 0;
pub const CLAW_AGENT_API_USAGE_SUBAGENT: c_int = 1;
pub const CLAW_AGENT_API_USAGE_MEMORY: c_int = 2;
pub const CLAW_AGENT_API_USAGE_COMPACTION: c_int = 3;

pub const CLAW_AGENT_SESSION_PERSISTENCE_PERSISTENT: c_int = 0;
pub const CLAW_AGENT_SESSION_PERSISTENCE_EPHEMERAL: c_int = 1;

/// Event kinds delivered by `claw_agent_session_receive`, one event per call.
/// The event payload union member is selected by this kind.
pub const CLAW_AGENT_EVENT_KIND_TURN_STARTED: c_int = 0;
pub const CLAW_AGENT_EVENT_KIND_INPUT_REQUESTED: c_int = 1;
pub const CLAW_AGENT_EVENT_KIND_ITERATION_STARTED: c_int = 2;
pub const CLAW_AGENT_EVENT_KIND_REASONING_DELTA: c_int = 3;
pub const CLAW_AGENT_EVENT_KIND_REASONING_END: c_int = 4;
pub const CLAW_AGENT_EVENT_KIND_OUTPUT_DELTA: c_int = 5;
pub const CLAW_AGENT_EVENT_KIND_OUTPUT_END: c_int = 6;
pub const CLAW_AGENT_EVENT_KIND_TOOL_CALL: c_int = 7;
pub const CLAW_AGENT_EVENT_KIND_TOOL_CALLS_END: c_int = 8;
pub const CLAW_AGENT_EVENT_KIND_ITERATION_ENDED: c_int = 9;
pub const CLAW_AGENT_EVENT_KIND_TURN_ENDED: c_int = 10;
pub const CLAW_AGENT_EVENT_KIND_ERROR: c_int = 11;
pub const CLAW_AGENT_EVENT_KIND_CLOSED: c_int = 12;

pub const CLAW_AGENT_TURN_ORIGIN_USER: c_int = 0;
pub const CLAW_AGENT_TURN_ORIGIN_SUBAGENT: c_int = 1;

pub const CLAW_AGENT_INPUT_REQUEST_KIND_PERMISSION_APPROVAL: c_int = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentTurnStartedEvent {
    pub turn_id: u32,
    pub origin: c_int,
    pub agent_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentInputRequestedEvent {
    pub request_id: u32,
    pub kind: c_int,
    pub summary: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentIterationEvent {
    pub iteration_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentTextDeltaEvent {
    pub text: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentToolCallEvent {
    pub id: *mut c_char,
    pub name: *mut c_char,
    pub arguments_json: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentTurnEndedEvent {
    pub turn_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClawAgentErrorEvent {
    pub message: *mut c_char,
}

#[repr(C)]
pub union ClawAgentEventData {
    pub turn_started: ClawAgentTurnStartedEvent,
    pub input_requested: ClawAgentInputRequestedEvent,
    pub iteration: ClawAgentIterationEvent,
    pub text_delta: ClawAgentTextDeltaEvent,
    pub tool_call: ClawAgentToolCallEvent,
    pub turn_ended: ClawAgentTurnEndedEvent,
    pub error: ClawAgentErrorEvent,
    pub reserved: u32,
}

#[repr(C)]
pub struct ClawAgentEvent {
    pub kind: c_int,
    pub data: ClawAgentEventData,
}

#[repr(C)]
pub struct ClawCapCallContext {
    pub request_id: u32,
    pub session_id: *const c_char,
    pub agent_id: *const c_char,
    pub agent_type: *const c_char,
    pub parent_agent_id: *const c_char,
    pub parent_session_id: *const c_char,
    pub channel: *const c_char,
    pub chat_id: *const c_char,
    pub target_channel: *const c_char,
    pub target_chat_id: *const c_char,
    pub source_cap: *const c_char,
    pub correlation_id: *const c_char,
    pub caller: c_int,
}

impl Default for ClawCapCallContext {
    fn default() -> Self {
        Self {
            request_id: 0,
            session_id: core::ptr::null(),
            agent_id: core::ptr::null(),
            agent_type: core::ptr::null(),
            parent_agent_id: core::ptr::null(),
            parent_session_id: core::ptr::null(),
            channel: core::ptr::null(),
            chat_id: core::ptr::null(),
            target_channel: core::ptr::null(),
            target_chat_id: core::ptr::null(),
            source_cap: core::ptr::null(),
            correlation_id: core::ptr::null(),
            caller: CLAW_CAP_CALLER_AGENT,
        }
    }
}

pub type ClawCapLifecycleFn = Option<unsafe extern "C" fn() -> EspErr>;

pub type ClawCapExecuteFn = Option<
    unsafe extern "C" fn(
        input_json: *const c_char,
        ctx: *const ClawCapCallContext,
        output: *mut c_char,
        output_size: usize,
    ) -> EspErr,
>;

#[repr(C)]
pub struct ClawCapDescriptor {
    pub id: *const c_char,
    pub name: *const c_char,
    pub family: *const c_char,
    pub description: *const c_char,
    pub kind: c_int,
    pub cap_flags: u32,
    pub input_schema_json: *const c_char,
    pub init: ClawCapLifecycleFn,
    pub start: ClawCapLifecycleFn,
    pub stop: ClawCapLifecycleFn,
    pub execute: ClawCapExecuteFn,
}

#[repr(C)]
pub struct ClawCapList {
    pub items: *const ClawCapDescriptor,
    pub count: usize,
}

#[repr(C)]
pub struct ClawCapDescriptorInfo {
    pub id: *const c_char,
    pub name: *const c_char,
    pub group_id: *const c_char,
    pub state: c_int,
    pub active_calls: u32,
}

extern "C" {
    pub fn claw_cap_list() -> ClawCapList;
    pub fn claw_cap_get_descriptor_state(
        id_or_name: *const c_char,
        info: *mut ClawCapDescriptorInfo,
    ) -> EspErr;
    pub fn claw_cap_is_llm_tool_available(
        id_or_name: *const c_char,
        ctx: *const ClawCapCallContext,
    ) -> bool;
    pub fn claw_cap_call(
        id_or_name: *const c_char,
        input_json: *const c_char,
        ctx: *const ClawCapCallContext,
        output: *mut c_char,
        output_size: usize,
    ) -> EspErr;
}
