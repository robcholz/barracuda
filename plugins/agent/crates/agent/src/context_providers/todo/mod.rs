//! Agent-owned execution checklist and its model-callable update tool.

use alloc::vec::Vec;
use alloc::{
    borrow::Cow,
    string::{String, ToString},
};

use barracuda_agent_context::{Band, BlockKind, ContextSink, Scope};
use barracuda_agent_tool::{Tool, ToolGroup};
use serde::{Deserialize, Serialize};

use crate::engine::{AgentStorage, ContextProvider, ContextProviderResult};

use self::tools::TodoUpdateTool;

mod tools;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[-]",
            Self::Completed => "[x]",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct TodoItem {
    content: String,
    status: TodoStatus,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct TodoList {
    items: Vec<TodoItem>,
}

pub(crate) struct TodoContextProvider;

impl TodoContextProvider {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl ContextProvider for TodoContextProvider {
    fn id(&self) -> &'static str {
        "todo"
    }

    fn contribute(
        &mut self,
        storage: &AgentStorage,
        output: &mut ContextSink<'_>,
    ) -> ContextProviderResult {
        let todos = load_todos(storage)?;
        let reminder = render_reminder(&todos);
        output.reminder(todo_reminder_kind(), reminder.as_deref());
        Ok(())
    }

    fn tools(&self, storage: &AgentStorage) -> Option<ToolGroup> {
        Some(ToolGroup::new(
            self.id(),
            true,
            [Tool::new(TodoUpdateTool {
                storage: storage.clone(),
            })],
        ))
    }
}

fn todo_reminder_kind() -> BlockKind {
    BlockKind::Custom {
        band: Band::Volatile,
        scope: Scope::Agent,
        order: 3,
        label: Cow::Borrowed("todo"),
    }
}

fn load_todos(storage: &AgentStorage) -> Result<TodoList, serde_json::Error> {
    storage
        .load()
        .map(serde_json::from_value)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn store_todos(storage: &AgentStorage, todos: &TodoList) -> Result<(), serde_json::Error> {
    let value = serde_json::to_value(todos)?;
    storage.store(value);
    Ok(())
}

fn render_reminder(todos: &TodoList) -> Option<String> {
    if todos.items.is_empty()
        || todos
            .items
            .iter()
            .all(|item| item.status == TodoStatus::Completed)
    {
        return None;
    }

    let mut reminder =
        "Current todo list. Keep it current with todo_update as work progresses:".to_string();
    for item in &todos.items {
        reminder.push('\n');
        reminder.push_str(item.status.marker());
        reminder.push(' ');
        reminder.push_str(&item.content);
    }
    Some(reminder)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use barracuda_agent_context::{Band, BlockKind, Context, Scope};
    use barracuda_agent_persistence::DurableState;
    use barracuda_agent_tool::{ToolInvocation, ToolRunner, ToolSet};
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;
    use serde_json::Value;

    use super::{load_todos, todo_reminder_kind, TodoContextProvider, TodoStatus};
    use crate::engine::{AgentStorage, ContextProvider};
    use crate::{AgentEngineState, AgentKind};

    fn provider() -> (TodoContextProvider, AgentStorage) {
        let state = DurableState::new(AgentEngineState::new(&AgentKind::from_static(
            "conversation",
        )));
        let provider = TodoContextProvider::new();
        let storage = AgentStorage::new(&state, provider.id());
        (provider, storage)
    }

    fn update(provider: &TodoContextProvider, storage: &AgentStorage, arguments: &str) -> bool {
        let invocation = ToolInvocation::try_new(Some("call-test"), "todo_update", arguments)
            .expect("valid invocation");
        let mut tools = ToolSet::empty();
        tools
            .add_group(provider.tools(storage).expect("todo tools exist"))
            .expect("todo tools register");
        let tools = tools.begin().expect("tool set begins");
        let joined = ToolRunner::new(&tools).run(vec![invocation]);
        block_on(joined.collect::<Vec<_>>())
            .pop()
            .expect("tool result")
            .1
            .ok
    }

    fn reminder(
        provider: &mut TodoContextProvider,
        storage: &AgentStorage,
        context: &mut Context,
    ) -> Option<String> {
        let history = {
            let mut sink = context.sink();
            assert!(provider.contribute(storage, &mut sink).is_ok());
            sink.into_history()
        };
        context
            .request(&history)
            .reminders()
            .first()
            .and_then(|message| {
                message
                    .to_value()
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
    }

    #[test]
    fn update_replaces_the_complete_list_and_normalizes_content() {
        let (provider, storage) = provider();
        assert!(update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"  inspect code  ","status":"completed"},{"content":"implement provider","status":"in_progress"}]}"#,
        ));

        let todos = load_todos(&storage).expect("stored todo list");
        assert_eq!(todos.items.len(), 2);
        assert_eq!(todos.items[0].content, "inspect code");
        assert_eq!(todos.items[1].status, TodoStatus::InProgress);

        assert!(update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"run tests","status":"pending"}]}"#,
        ));
        let todos = load_todos(&storage).expect("replaced todo list");
        assert_eq!(todos.items.len(), 1);
        assert_eq!(todos.items[0].content, "run tests");
    }

    #[test]
    fn reminder_uses_a_custom_tool_owned_block() {
        assert_eq!(
            todo_reminder_kind(),
            BlockKind::Custom {
                band: Band::Volatile,
                scope: Scope::Agent,
                order: 3,
                label: "todo".into(),
            }
        );
    }

    #[test]
    fn invalid_update_preserves_the_previous_list() {
        let (provider, storage) = provider();
        assert!(update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"first","status":"in_progress"}]}"#,
        ));
        assert!(!update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"first","status":"in_progress"},{"content":"second","status":"in_progress"}]}"#,
        ));

        let todos = load_todos(&storage).expect("original todo list");
        assert_eq!(todos.items.len(), 1);
        assert_eq!(todos.items[0].content, "first");
    }

    #[test]
    fn unfinished_list_is_reinjected_without_changing_the_system_prefix() {
        let (mut provider, storage) = provider();
        assert!(update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"inspect code","status":"completed"},{"content":"implement provider","status":"in_progress"},{"content":"run tests","status":"pending"}]}"#,
        ));
        let mut context = Context::new();
        let version = context.version();

        let reminder = reminder(&mut provider, &storage, &mut context).expect("todo reminder");

        assert_eq!(context.version(), version);
        assert!(reminder.contains("[x] inspect code"));
        assert!(reminder.contains("[-] implement provider"));
        assert!(reminder.contains("[ ] run tests"));
    }

    #[test]
    fn empty_or_completed_list_clears_the_reminder() {
        let (mut provider, storage) = provider();
        let mut context = Context::new();
        assert!(update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"finish","status":"in_progress"}]}"#,
        ));
        assert!(reminder(&mut provider, &storage, &mut context).is_some());

        assert!(update(
            &provider,
            &storage,
            r#"{"todos":[{"content":"finish","status":"completed"}]}"#,
        ));
        assert!(reminder(&mut provider, &storage, &mut context).is_none());

        assert!(update(&provider, &storage, r#"{"todos":[]}"#));
        assert!(reminder(&mut provider, &storage, &mut context).is_none());
    }
}
