use json_validator::{validator, Validator};

const SCHEMAS: &[Validator] = &[
    validator!("../../components/agent/crates/runtime/resources/tools/conversation_end/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/memory_forget/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/memory_list/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/memory_recall/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/memory_store/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/memory_update/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/permission_resolve_reply/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/plan_clarify/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/plan_enter/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/plan_exit/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/profile_clear/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/profile_read/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/profile_replace/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/skill_list/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/skill_read/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/skill_reload/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_delete/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_interrupt/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_list/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_list_spawnable/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_run/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_spawn/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/subagent_watch/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/tool_load/schema.json"),
    validator!("../../components/agent/crates/runtime/resources/tools/tool_search/schema.json"),
];

#[test]
fn every_current_tool_schema_compiles() {
    assert_eq!(SCHEMAS.len(), 25);
}
