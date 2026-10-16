//! The Sessions system app, and the `sessions` Lua package that is its way to
//! the Agent's conversations.

#![no_std]

extern crate alloc;

mod words;

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};
use barracuda_agent_plugin::AgentRuntime;
use barracuda_app_manager_plugin::{
    AgentState, AgentStatus, AppManager, BuiltinApp, BuiltinBackend, BuiltinPackage,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_time_plugin::UtcClock;
use core::{future::Future, pin::Pin};
use serde_json::{Value, json};

/// The app's ID, and its package's name.
pub const SESSIONS: &str = "sessions";

/// Ships the Sessions app and answers its `sessions` package.
#[barracuda_plugin::macros::plugin]
pub struct SessionsAppPlugin;

impl SessionsAppPlugin {
    /// Creates the unregistered app.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for SessionsAppPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let apps = context.require::<AppManager>("app-manager")?;
        let backend = Sessions {
            agent: context.require::<AgentRuntime>("agent")?,
            status: context.require::<AgentStatus>("app-manager")?,
            clock: context.require::<UtcClock>("time")?,
        };
        let registration = apps
            .add_builtin(BuiltinApp {
                id: SESSIONS,
                filesystem: context.filesystem()?.clone(),
                opens: &[],
                package: Some(BuiltinPackage {
                    name: SESSIONS,
                    backend: Rc::new(backend),
                }),
            })
            .map_err(PluginError::registration)?;
        if let Some(registration) = registration {
            context.retain(registration);
        }
        Ok(())
    }
}

/// Answers the `sessions` package.
struct Sessions {
    agent: Rc<AgentRuntime>,
    status: Rc<AgentStatus>,
    clock: Rc<UtcClock>,
}

impl BuiltinBackend for Sessions {
    fn call<'a>(
        &'a self,
        call: &'a str,
        _args: Vec<Option<String>>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            match call {
                "list" => Ok(self.list().await),
                "agent" => Ok(json!({
                    "state": match self.status.state() {
                        AgentState::Standby => "standby",
                        AgentState::Listening => "listening",
                        AgentState::Thinking => "thinking",
                        AgentState::Answering => "answering",
                    },
                    "question": self.status.question(),
                    "answer": self.status.answer(),
                })),
                _ => Err(format!("sessions.{call} does not exist")),
            }
        })
    }
}

impl Sessions {
    /// The conversations, most recently used first.
    async fn list(&self) -> Value {
        let now = self.clock.now().ok().map(u64::from);
        let mut sessions = self.agent.describe_sessions().await;
        sessions.sort_by_key(|session| core::cmp::Reverse(session.updated_at));
        Value::Array(
            sessions
                .into_iter()
                .map(|session| {
                    json!({
                        "id": format!("{}", session.session),
                        "title": session.title.unwrap_or_else(|| String::from("新对话")),
                        "when": session
                            .updated_at
                            .map(|at| words::when(at, now))
                            .unwrap_or_default(),
                    })
                })
                .collect(),
        )
    }
}
