//! Model-callable mutation for the agent-owned execution checklist.

use alloc::string::ToString;
use alloc::vec::Vec;

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, ToolFuture, ToolHandler, ToolInvocation, ToolOutput, ToolSpec,
};
use serde::Deserialize;

use super::{store_todos, TodoItem, TodoList, TodoStatus};
use crate::engine::AgentStorage;

#[derive(Deserialize)]
pub(super) struct TodoUpdateArgs {
    todos: Vec<TodoItem>,
}

pub(super) struct TodoUpdateTool {
    pub(super) storage: AgentStorage,
}

impl ToolSpec for TodoUpdateTool {
    tool_metadata!("todo_update");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for TodoUpdateTool {
    type Args = TodoUpdateArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        alloc::boxed::Box::pin(async move {
            let todos = match normalize(args.todos) {
                Ok(todos) => todos,
                Err(message) => return Ok(output(message, false)),
            };
            match store_todos(&self.storage, &todos) {
                Ok(()) => Ok(output("Todo list updated.", true)),
                Err(error) => Ok(output(&error.to_string(), false)),
            }
        })
    }
}

fn normalize(mut items: Vec<TodoItem>) -> Result<TodoList, &'static str> {
    let in_progress = items
        .iter()
        .filter(|item| item.status == TodoStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err("At most one todo may be in_progress.");
    }
    for item in &mut items {
        let content = item.content.trim();
        if content.is_empty() {
            return Err("Todo content must not be blank.");
        }
        item.content = content.to_string();
    }
    Ok(TodoList { items })
}

fn output(content: &str, ok: bool) -> ToolOutput {
    ToolOutput {
        content: content.to_string(),
        ok,
    }
}
