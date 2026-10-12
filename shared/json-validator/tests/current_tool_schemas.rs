use json_validator::{validator, Validator};

const SCHEMAS: &[Validator] = &[
    validator!("../../plugins/agent/crates/agent/resources/tools/background_cancel/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/background_input/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/background_list/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/background_wait/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/conversation_end/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/memory_forget/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/memory_list/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/memory_recall/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/memory_store/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/memory_update/schema.json"),
    validator!(
        "../../plugins/agent/crates/session/resources/tools/permission_resolve_reply/schema.json"
    ),
    validator!("../../plugins/agent/crates/agent/resources/tools/plan_clarify/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/plan_enter/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/plan_exit/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/profile_clear/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/profile_read/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/profile_replace/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/skill_list/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/skill_read/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/skill_resource_read/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/skill_reload/schema.json"),
    validator!("../../plugins/agent/crates/multiagent/resources/tools/subagent_delete/schema.json"),
    validator!(
        "../../plugins/agent/crates/multiagent/resources/tools/subagent_interrupt/schema.json"
    ),
    validator!("../../plugins/agent/crates/multiagent/resources/tools/subagent_list/schema.json"),
    validator!(
        "../../plugins/agent/crates/multiagent/resources/tools/subagent_list_spawnable/schema.json"
    ),
    validator!("../../plugins/agent/crates/multiagent/resources/tools/subagent_run/schema.json"),
    validator!("../../plugins/agent/crates/multiagent/resources/tools/subagent_spawn/schema.json"),
    validator!("../../plugins/agent/crates/multiagent/resources/tools/subagent_watch/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/tool_load/schema.json"),
    validator!("../../plugins/agent/crates/agent/resources/tools/tool_search/schema.json"),
];

#[test]
fn every_current_tool_schema_compiles() {
    assert_eq!(SCHEMAS.len(), 30);
}
