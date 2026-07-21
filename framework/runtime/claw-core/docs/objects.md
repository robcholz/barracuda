# Objects (Injections)

## BaseAgent

pub(in crate::agent) store: TranscriptStore<F>,
pub(in crate::agent) tools: ToolSet,
pub(in crate::agent) permission_policy: Arc<dyn PermissionPolicy>,
pub(in crate::agent) skills: SkillSet,
pub(in crate::agent) agent_instruction: Block<'static>,
pub(in crate::agent) inherited_context: Vec<Block<'static>>,
Vec<dyn ContextAdapters>
