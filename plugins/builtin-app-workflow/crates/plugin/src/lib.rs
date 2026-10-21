//! The Workflows system app, and the `workflows` Lua package that lists the
//! loaded Workflows and what starts each.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};
use barracuda_app_manager_plugin::{AppManager, BuiltinApp, BuiltinBackend, BuiltinPackage};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
};
use barracuda_workflow_plugin::{WorkflowId, WorkflowService};
use core::{future::Future, pin::Pin};
use serde_json::{Value, json};

/// The app's ID.
pub const WORKFLOW: &str = "workflow";
/// Its package's name.
pub const WORKFLOWS: &str = "workflows";

/// Ships the Workflows app and answers its `workflows` package.
#[barracuda_plugin::macros::plugin]
pub struct WorkflowAppPlugin;

impl WorkflowAppPlugin {
    /// Creates the unregistered app.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for WorkflowAppPlugin {
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
        let backend = Workflows(context.require::<WorkflowService>("workflow")?);
        let registration = apps
            .add_builtin(BuiltinApp {
                id: WORKFLOW,
                filesystem: context.filesystem()?.clone(),
                opens: &[],
                package: Some(BuiltinPackage {
                    name: WORKFLOWS,
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

/// Answers the `workflows` package.
struct Workflows(Rc<WorkflowService>);

impl BuiltinBackend for Workflows {
    fn call<'a>(
        &'a self,
        call: &'a str,
        args: Vec<Option<String>>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            match call {
                "list" => Ok(Value::Array(
                    self.0
                        .definitions()
                        .iter()
                        .map(|definition| {
                            json!({
                                "id": definition.id().as_str(),
                                "event": definition.event().as_str(),
                                "trigger": trigger(definition.event().as_str()),
                                "enabled": self.0.is_enabled(definition.id()),
                            })
                        })
                        .collect(),
                )),
                "set_enabled" => {
                    let id = args
                        .first()
                        .cloned()
                        .flatten()
                        .and_then(|id| WorkflowId::try_from(id).ok())
                        .ok_or_else(|| String::from("workflows.set_enabled needs a Workflow ID"))?;
                    let enabled = args.get(1).cloned().flatten().as_deref() == Some("true");
                    self.0
                        .set_enabled(&id, enabled)
                        .await
                        .map_err(|error| format!("{error}"))?;
                    Ok(Value::Bool(enabled))
                }
                _ => Err(format!("workflows.{call} does not exist")),
            }
        })
    }
}

/// What starts a Workflow, from its Event rule.
fn trigger(rule: &str) -> String {
    match rule.split('.').next() {
        Some("time" | "scheduler") => String::from("Scheduled"),
        Some("wifi") => String::from("On network change"),
        Some("app") => String::from("App event"),
        Some("agent") => String::from("Agent event"),
        Some("imessage" | "gateway") => String::from("On message"),
        _ => format!("Event {rule}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_read_from_the_event_rule() {
        assert_eq!(trigger("scheduler.fired"), "Scheduled");
        assert_eq!(trigger("gateway.message.received"), "On message");
        assert_eq!(trigger("session.event"), "Event session.event");
    }
}
