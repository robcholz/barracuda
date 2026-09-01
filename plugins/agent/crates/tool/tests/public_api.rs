#![allow(clippy::arc_with_non_send_sync)]

use core::future::Future;
use core::task::{Context, Poll};
use std::sync::Arc;
use std::task::Waker;

use anyhow::{anyhow, Result};
use barracuda_agent_persistence::{Persistence, SharedPersistence};
use barracuda_agent_tool::{
    EmptyArgs, Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvocation, ToolOutput,
    ToolRegistry, ToolRegistryError, ToolRunner, ToolSetHandle, ToolSpec,
};
use barracuda_platform_test::memory_vfs;
use futures_lite::{future::block_on, StreamExt as _};

#[test]
fn local_tool_runs_through_public_tool_surface() -> Result<()> {
    let registry = registry()?;
    let mut tool_set = registry.tool_set();
    tool_set.add_group(ToolGroup::new("local", true, [Tool::new(EchoTool)]))?;

    let handle = tool_set.begin()?;
    assert_eq!(
        handle.static_schemas(),
        r#"[{"type":"function","function":{"name":"echo"}}]"#
    );
    assert_eq!(handle.static_context(), "Echoes the normalized arguments.");
    assert_eq!(handle.deferred_context(), "");

    let call = invocation("echo", r#" { "message": "hi" } "#)?;
    let outcome = execute_tool(&handle, &call)?;

    assert!(outcome.ok);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&outcome.content)?,
        serde_json::json!({"message": "hi"})
    );
    Ok(())
}

#[test]
fn local_group_id_cannot_equal_its_tool_name() -> Result<()> {
    let registry = registry()?;
    let mut tool_set = registry.tool_set();

    let error = match tool_set.add_group(ToolGroup::new("echo", true, [Tool::new(EchoTool)])) {
        Ok(()) => return Err(anyhow!("group and tool names must be distinct")),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        barracuda_agent_tool::ToolSetError::AmbiguousName(name) if name == "echo"
    ));
    Ok(())
}

#[test]
fn temporary_disable_blocks_runner_but_keeps_tool_context() -> Result<()> {
    let registry = registry()?;
    let mut tool_set = registry.tool_set();
    tool_set.add_group(ToolGroup::new("local", true, [Tool::new(EchoTool)]))?;

    tool_set.temporarily_disable_tool("echo".into())?;

    {
        let handle = tool_set.begin()?;
        assert_eq!(
            handle.static_schemas(),
            r#"[{"type":"function","function":{"name":"echo"}}]"#
        );
        assert_eq!(
            handle.reminders(),
            "Tool `echo` is temporarily unavailable."
        );

        let call = invocation("echo", "{}")?;
        let outcome = execute_tool(&handle, &call)?;
        assert_eq!(
            outcome,
            ToolOutput {
                content: "tool invocation rejected: tool is temporarily unavailable: echo".into(),
                ok: false,
            }
        );
    }

    tool_set.clear_temporary_tools();

    let handle = tool_set.begin()?;
    assert_eq!(handle.reminders(), "no extra tool context");

    let call = invocation("echo", "{}")?;
    let outcome = execute_tool(&handle, &call)?;
    assert_eq!(
        outcome,
        ToolOutput {
            content: "{}".into(),
            ok: true,
        }
    );
    Ok(())
}

#[test]
fn registry_tools_appear_only_after_registry_is_started() -> Result<()> {
    let registry = registry()?;
    registry.register_group(ToolGroup::new("test", true, [Tool::new(EchoTool)]))?;
    let mut tool_set = registry.tool_set();

    {
        let handle = tool_set.begin()?;
        assert_eq!(handle.static_schemas(), "no schemas");

        let call = invocation("echo", "{}")?;
        let outcome = execute_tool(&handle, &call)?;
        assert_eq!(
            outcome,
            ToolOutput {
                content: "tool not found: echo".into(),
                ok: false,
            }
        );
    }

    registry.start_all()?;

    let handle = tool_set.begin()?;
    assert_eq!(
        handle.static_schemas(),
        r#"[{"type":"function","function":{"name":"echo"}}]"#
    );

    let call = invocation("echo", "{}")?;
    let outcome = execute_tool(&handle, &call)?;
    assert_eq!(
        outcome,
        ToolOutput {
            content: "{}".into(),
            ok: true,
        }
    );
    Ok(())
}

#[test]
fn registry_rejects_duplicate_tools_across_groups() -> Result<()> {
    let registry = registry()?;

    registry.register_group(ToolGroup::new("first", true, [Tool::new(EchoTool)]))?;
    let err = match registry.register_group(ToolGroup::new("second", true, [Tool::new(EchoTool)])) {
        Ok(()) => return Err(anyhow!("duplicate tool should fail")),
        Err(error) => error,
    };

    assert!(matches!(err, ToolRegistryError::AlreadyExists(name) if name == "echo"));
    Ok(())
}

#[test]
fn registry_rejects_a_group_id_that_is_also_a_tool_name() -> Result<()> {
    let registry = registry()?;

    let error = match registry.register_group(ToolGroup::new("echo", true, [Tool::new(EchoTool)])) {
        Ok(()) => return Err(anyhow!("group and tool names must be distinct")),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ToolRegistryError::AmbiguousName(name) if name == "echo"
    ));
    Ok(())
}

#[test]
fn tool_set_blacklist_matches_an_exact_registry_group() -> Result<()> {
    let registry = registry()?;
    registry.register_group(ToolGroup::new("allowed", true, [Tool::new(EchoTool)]))?;
    registry.register_group(ToolGroup::new("blocked", true, [Tool::new(OtherTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set_with_blacklist(&["blocked"]);
    let handle = tool_set.begin()?;

    let allowed = execute_tool(&handle, &invocation("echo", "{}")?)?;
    assert_eq!(
        allowed,
        ToolOutput {
            content: "{}".into(),
            ok: true,
        }
    );

    let blocked = execute_tool(&handle, &invocation("other", "{}")?)?;
    assert_eq!(
        blocked,
        ToolOutput {
            content: "tool not found: other".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn tool_set_blacklist_matches_one_exact_tool_name() -> Result<()> {
    let registry = registry()?;
    registry.register_group(ToolGroup::new(
        "mixed",
        true,
        [Tool::new(EchoTool), Tool::new(OtherTool)],
    ))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set_with_blacklist(&["other"]);
    let handle = tool_set.begin()?;

    assert!(matches!(
        execute_tool(&handle, &invocation("echo", "{}")?)?,
        ToolOutput { ok: true, .. }
    ));
    assert_eq!(
        execute_tool(&handle, &invocation("other", "{}")?)?,
        ToolOutput {
            content: "tool not found: other".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn tool_set_blacklist_applies_to_groups_added_after_construction() -> Result<()> {
    let registry = registry()?;
    let mut tool_set = registry.tool_set_with_blacklist(&["plan"]);

    tool_set.add_group(ToolGroup::new("plan", true, [Tool::new(EchoTool)]))?;

    let handle = tool_set.begin()?;
    assert_eq!(handle.static_schemas(), "no schemas");
    assert_eq!(
        execute_tool(&handle, &invocation("echo", "{}")?)?,
        ToolOutput {
            content: "tool not found: echo".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn blacklist_does_not_interpret_wildcards() -> Result<()> {
    let registry = registry()?;
    let mut tool_set = registry.tool_set_with_blacklist(&["plan_*"]);

    tool_set.add_group(ToolGroup::new("plan", true, [Tool::new(EchoTool)]))?;

    let handle = tool_set.begin()?;
    assert!(matches!(
        execute_tool(&handle, &invocation("echo", "{}")?)?,
        ToolOutput { ok: true, .. }
    ));
    Ok(())
}

#[test]
fn blacklist_applies_to_registry_groups_registered_later() -> Result<()> {
    let registry = registry()?;
    let mut tool_set = registry.tool_set_with_blacklist(&["late"]);

    registry.register_group(ToolGroup::new("late", true, [Tool::new(EchoTool)]))?;
    registry.start_all()?;

    let handle = tool_set.begin()?;
    assert_eq!(handle.static_schemas(), "no schemas");
    Ok(())
}

#[test]
fn tool_set_uses_registry_group_default_visibility() -> Result<()> {
    let registry = registry()?;
    registry.register_group(ToolGroup::new("hidden", false, [Tool::new(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set();
    let handle = tool_set.begin()?;
    let outcome = execute_tool(&handle, &invocation("echo", "{}")?)?;
    assert_eq!(
        outcome,
        ToolOutput {
            content: "tool not found: echo".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn hidden_group_is_searchable_then_loadable() -> Result<()> {
    let registry = registry()?;
    registry.register_group(ToolGroup::new("visible", true, [Tool::new(OtherTool)]))?;
    registry.register_group(ToolGroup::new("hidden", false, [Tool::new(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set();
    let discovery = tool_set.discovery();

    // The hidden tool is registered but not part of the default schema surface,
    // so it is not callable yet — only surfaced through the discovery catalog.
    {
        let handle = tool_set.begin()?;
        assert_eq!(
            handle.static_schemas(),
            r#"[{"type":"function","function":{"name":"other"}}]"#
        );
        assert_eq!(handle.deferred_context(), "");
        let blocked = execute_tool(&handle, &invocation("echo", "{}")?)?;
        assert_eq!(
            blocked,
            ToolOutput {
                content: "tool not found: echo".into(),
                ok: false,
            }
        );
    }

    let catalog = discovery.catalog();
    assert_eq!(catalog.len(), 1);
    let hidden = catalog
        .first()
        .ok_or_else(|| anyhow!("hidden group missing from discovery catalog"))?;
    assert_eq!(hidden.id, "hidden");
    assert_eq!(hidden.tools.len(), 1);
    let echo = hidden
        .tools
        .first()
        .ok_or_else(|| anyhow!("echo missing from hidden group"))?;
    assert_eq!(echo.name, "echo");
    assert_eq!(echo.description, "Echoes the normalized arguments.");

    // Loading the group queues it; the next projection applies the request and
    // makes the tool callable.
    assert!(discovery.request_load("hidden"));
    assert!(!discovery.request_load("nope"));

    let handle = tool_set.begin()?;
    assert_eq!(
        handle.static_schemas(),
        r#"[{"type":"function","function":{"name":"other"}}]"#
    );
    assert_eq!(
        handle.deferred_context(),
        concat!(
            "Echoes the normalized arguments.\n\n",
            r#"[{"type":"function","function":{"name":"echo"}}]"#
        )
    );
    let outcome = execute_tool(&handle, &invocation("echo", "{}")?)?;
    assert_eq!(
        outcome,
        ToolOutput {
            content: "{}".into(),
            ok: true,
        }
    );
    // Once loaded, the group drops out of the catalog.
    assert!(discovery.catalog().is_empty());
    Ok(())
}

#[test]
fn blacklisted_hidden_group_is_not_searchable_or_loadable() -> Result<()> {
    let registry = registry()?;
    registry.register_group(ToolGroup::new("hidden", false, [Tool::new(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set_with_blacklist(&["hidden"]);
    let discovery = tool_set.discovery();
    let handle = tool_set.begin()?;

    assert_eq!(handle.static_schemas(), "no schemas");
    assert!(discovery.catalog().is_empty());
    assert!(!discovery.request_load("hidden"));
    Ok(())
}

#[test]
fn durable_overrides_apply_to_a_rebuilt_registry() -> Result<()> {
    let persistence = persistence()?;
    let registry = block_on(ToolRegistry::new(Arc::clone(&persistence)))?;
    registry.register_group(ToolGroup::new("test", true, [Tool::new(EchoTool)]))?;
    registry.disable("echo")?;
    block_on(persistence.maybe_persist())?;
    drop(registry);

    let registry = Arc::new(block_on(ToolRegistry::new(persistence))?);
    registry.register_group(ToolGroup::new("test", true, [Tool::new(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set();
    assert_eq!(tool_set.begin()?.static_schemas(), "no schemas");
    Ok(())
}

#[test]
fn invocation_normalizes_empty_arguments() {
    let call = ToolInvocation::try_new(None, "demo", "  ");

    assert!(matches!(call, Ok(call) if call.arguments_json() == "{}"));
}

#[test]
fn invocation_rejects_non_object_arguments() {
    let call = ToolInvocation::try_new(None, "demo", "[]");

    assert!(matches!(
        call,
        Err(error) if matches!(error.error, ToolError::InvalidArgumentsJson(_))
    ));
}

struct EchoTool;

impl ToolSpec for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"echo"}}"#
    }

    fn arguments_validator(&self) -> &dyn barracuda_agent_tool::ToolArgumentsValidator {
        const VALIDATOR: json_validator::Validator =
            json_validator::validator!("tests/fixtures/object.json");
        &VALIDATOR
    }

    fn usage(&self) -> Option<&str> {
        Some("Echoes the normalized arguments.")
    }
}

impl ToolHandler for EchoTool {
    type Args = serde_json::Value;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            Ok(ToolOutput {
                content: serde_json::to_string(&args)
                    .map_err(|_| ToolError::InvokeRejected("failed to serialize args".into()))?,
                ok: true,
            })
        })
    }
}

struct OtherTool;

impl ToolSpec for OtherTool {
    fn name(&self) -> &str {
        "other"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"other"}}"#
    }

    fn arguments_validator(&self) -> &dyn barracuda_agent_tool::ToolArgumentsValidator {
        const VALIDATOR: json_validator::Validator =
            json_validator::validator!("tests/fixtures/object.json");
        &VALIDATOR
    }
}

impl ToolHandler for OtherTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            Ok(ToolOutput {
                content: "other".into(),
                ok: true,
            })
        })
    }
}

fn invocation(name: &'static str, arguments_json: &'static str) -> Result<ToolInvocation> {
    ToolInvocation::try_new(None, name, arguments_json).map_err(|error| anyhow!("{error:?}"))
}

fn execute_tool(handle: &ToolSetHandle<'_>, call: &ToolInvocation) -> Result<ToolOutput> {
    let call = ToolInvocation::try_new(call.id(), call.name(), call.arguments_json())
        .map_err(|error| anyhow!("{error:?}"))?;
    let (mut join, detached) = ToolRunner::new(handle).run(vec![call]);
    if detached.is_some() {
        return Err(anyhow!("test helper does not accept detached tools"));
    }
    poll_ready(async move {
        join.next()
            .await
            .map(|(_, output)| output)
            .ok_or_else(|| anyhow!("join stream ended without a result"))
    })?
}

fn persistence() -> Result<SharedPersistence> {
    block_on(async {
        Ok(Arc::new(
            Persistence::new(memory_vfs().await?, "/barracuda-agent-tool-tests").await?,
        ))
    })
}

fn registry() -> Result<Arc<ToolRegistry>> {
    Ok(Arc::new(block_on(ToolRegistry::new(persistence()?))?))
}

fn poll_ready<T>(future: impl Future<Output = T>) -> Result<T> {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => Ok(output),
        Poll::Pending => Err(anyhow!("future was pending")),
    }
}
