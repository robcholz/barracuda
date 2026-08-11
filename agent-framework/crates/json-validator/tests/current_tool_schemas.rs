use json_validator::{validator, Validator};

const SCHEMAS: &[Validator] = &[
    validator!("../claw-core/resources/tools/conversation_end/schema.json"),
    validator!("../claw-core/resources/tools/memory_forget/schema.json"),
    validator!("../claw-core/resources/tools/memory_list/schema.json"),
    validator!("../claw-core/resources/tools/memory_recall/schema.json"),
    validator!("../claw-core/resources/tools/memory_store/schema.json"),
    validator!("../claw-core/resources/tools/memory_update/schema.json"),
    validator!("../claw-core/resources/tools/permission_resolve_reply/schema.json"),
    validator!("../claw-core/resources/tools/plan_clarify/schema.json"),
    validator!("../claw-core/resources/tools/plan_enter/schema.json"),
    validator!("../claw-core/resources/tools/plan_exit/schema.json"),
    validator!("../claw-core/resources/tools/profile_clear/schema.json"),
    validator!("../claw-core/resources/tools/profile_read/schema.json"),
    validator!("../claw-core/resources/tools/profile_replace/schema.json"),
    validator!("../claw-core/resources/tools/skill_list/schema.json"),
    validator!("../claw-core/resources/tools/skill_read/schema.json"),
    validator!("../claw-core/resources/tools/skill_reload/schema.json"),
    validator!("../claw-core/resources/tools/subagent_delete/schema.json"),
    validator!("../claw-core/resources/tools/subagent_interrupt/schema.json"),
    validator!("../claw-core/resources/tools/subagent_list/schema.json"),
    validator!("../claw-core/resources/tools/subagent_list_spawnable/schema.json"),
    validator!("../claw-core/resources/tools/subagent_run/schema.json"),
    validator!("../claw-core/resources/tools/subagent_spawn/schema.json"),
    validator!("../claw-core/resources/tools/subagent_watch/schema.json"),
    validator!("../claw-core/resources/tools/tool_load/schema.json"),
    validator!("../claw-core/resources/tools/tool_search/schema.json"),
];

#[test]
fn every_current_tool_schema_compiles() {
    assert_eq!(SCHEMAS.len(), 25);
}
