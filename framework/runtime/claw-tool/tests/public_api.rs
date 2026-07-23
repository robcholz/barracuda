use core::future::Future;
use core::task::{Context, Poll};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::Waker;

use anyhow::{anyhow, Result};
use claw_persistence::{DurableState, DurableStateCodec};
use claw_tool::{
    RetryCount, SyncToolHandler, Tool, ToolError, ToolExecution, ToolGroup, ToolInvocation,
    ToolInvokeError, ToolOutput, ToolRegistry, ToolRegistryError, ToolRegistryState, ToolResult,
    ToolRunInvocation, ToolRunResult, ToolRunner, ToolSetHandle, ToolSpec,
};
use futures_lite::StreamExt as _;

#[test]
fn local_tool_runs_through_public_tool_surface() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set();
    tool_set.add_group(ToolGroup::new("local", true, [Tool::from_sync(EchoTool)]))?;

    let handle = tool_set.begin()?;
    assert_eq!(
        handle.schemas_json(),
        r#"[{"type":"function","function":{"name":"echo"}}]"#
    );
    assert_eq!(handle.tool_context(), "Echoes the normalized arguments.");

    let call = invocation("echo", r#" { "message": "hi" } "#)?;
    let outcome = execute_tool(&handle, &call)?;

    assert_eq!(
        outcome,
        ToolExecution {
            content: r#"{ "message": "hi" }"#.into(),
            ok: true,
        }
    );
    Ok(())
}

#[test]
fn local_group_id_cannot_equal_its_tool_name() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set();

    let error = match tool_set.add_group(ToolGroup::new("echo", true, [Tool::from_sync(EchoTool)]))
    {
        Ok(()) => return Err(anyhow!("group and tool names must be distinct")),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        claw_tool::ToolSetError::AmbiguousName(name) if name == "echo"
    ));
    Ok(())
}

#[test]
fn temporary_disable_blocks_runner_but_keeps_tool_context() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set();
    tool_set.add_group(ToolGroup::new("local", true, [Tool::from_sync(EchoTool)]))?;

    tool_set.temporarily_disable_tool("echo".into())?;

    {
        let handle = tool_set.begin()?;
        assert_eq!(
            handle.schemas_json(),
            r#"[{"type":"function","function":{"name":"echo"}}]"#
        );
        assert_eq!(
            handle.extra_tool_context(),
            "Tool `echo` is temporarily unavailable."
        );

        let call = invocation("echo", "{}")?;
        let outcome = execute_tool(&handle, &call)?;
        assert_eq!(
            outcome,
            ToolExecution {
                content: "tool invocation rejected: tool is temporarily unavailable: echo".into(),
                ok: false,
            }
        );
    }

    tool_set.clear_temporary_tools();

    let handle = tool_set.begin()?;
    assert_eq!(handle.extra_tool_context(), "no extra tool context");

    let call = invocation("echo", "{}")?;
    let outcome = execute_tool(&handle, &call)?;
    assert_eq!(
        outcome,
        ToolExecution {
            content: "{}".into(),
            ok: true,
        }
    );
    Ok(())
}

#[test]
fn registry_tools_appear_only_after_registry_is_started() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new("test", true, [Tool::from_sync(EchoTool)]))?;
    let mut tool_set = registry.tool_set();

    {
        let handle = tool_set.begin()?;
        assert_eq!(handle.schemas_json(), "no schemas");

        let call = invocation("echo", "{}")?;
        let outcome = execute_tool(&handle, &call)?;
        assert_eq!(
            outcome,
            ToolExecution {
                content: "tool not found: echo".into(),
                ok: false,
            }
        );
    }

    registry.start_all()?;

    let handle = tool_set.begin()?;
    assert_eq!(
        handle.schemas_json(),
        r#"[{"type":"function","function":{"name":"echo"}}]"#
    );

    let call = invocation("echo", "{}")?;
    let outcome = execute_tool(&handle, &call)?;
    assert_eq!(
        outcome,
        ToolExecution {
            content: "{}".into(),
            ok: true,
        }
    );
    Ok(())
}

#[test]
fn registry_rejects_duplicate_tools_across_groups() -> Result<()> {
    let registry = ToolRegistry::new();

    registry.register_group(ToolGroup::new("first", true, [Tool::from_sync(EchoTool)]))?;
    let err = match registry.register_group(ToolGroup::new(
        "second",
        true,
        [Tool::from_sync(EchoTool)],
    )) {
        Ok(()) => return Err(anyhow!("duplicate tool should fail")),
        Err(error) => error,
    };

    assert!(matches!(err, ToolRegistryError::AlreadyExists(name) if name == "echo"));
    Ok(())
}

#[test]
fn registry_rejects_a_group_id_that_is_also_a_tool_name() -> Result<()> {
    let registry = ToolRegistry::new();

    let error =
        match registry.register_group(ToolGroup::new("echo", true, [Tool::from_sync(EchoTool)])) {
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
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new("allowed", true, [Tool::from_sync(EchoTool)]))?;
    registry.register_group(ToolGroup::new(
        "blocked",
        true,
        [Tool::from_sync(OtherTool)],
    ))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set_with_blacklist(&["blocked"]);
    let handle = tool_set.begin()?;

    let allowed = execute_tool(&handle, &invocation("echo", "{}")?)?;
    assert_eq!(
        allowed,
        ToolExecution {
            content: "{}".into(),
            ok: true,
        }
    );

    let blocked = execute_tool(&handle, &invocation("other", "{}")?)?;
    assert_eq!(
        blocked,
        ToolExecution {
            content: "tool not found: other".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn tool_set_blacklist_matches_one_exact_tool_name() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new(
        "mixed",
        true,
        [Tool::from_sync(EchoTool), Tool::from_sync(OtherTool)],
    ))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set_with_blacklist(&["other"]);
    let handle = tool_set.begin()?;

    assert!(matches!(
        execute_tool(&handle, &invocation("echo", "{}")?)?,
        ToolExecution { ok: true, .. }
    ));
    assert_eq!(
        execute_tool(&handle, &invocation("other", "{}")?)?,
        ToolExecution {
            content: "tool not found: other".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn tool_set_blacklist_applies_to_groups_added_after_construction() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set_with_blacklist(&["plan"]);

    tool_set.add_group(ToolGroup::new("plan", true, [Tool::from_sync(EchoTool)]))?;

    let handle = tool_set.begin()?;
    assert_eq!(handle.schemas_json(), "no schemas");
    assert_eq!(
        execute_tool(&handle, &invocation("echo", "{}")?)?,
        ToolExecution {
            content: "tool not found: echo".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn blacklist_does_not_interpret_wildcards() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set_with_blacklist(&["plan_*"]);

    tool_set.add_group(ToolGroup::new("plan", true, [Tool::from_sync(EchoTool)]))?;

    let handle = tool_set.begin()?;
    assert!(matches!(
        execute_tool(&handle, &invocation("echo", "{}")?)?,
        ToolExecution { ok: true, .. }
    ));
    Ok(())
}

#[test]
fn blacklist_applies_to_registry_groups_registered_later() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set_with_blacklist(&["late"]);

    registry.register_group(ToolGroup::new("late", true, [Tool::from_sync(EchoTool)]))?;
    registry.start_all()?;

    let handle = tool_set.begin()?;
    assert_eq!(handle.schemas_json(), "no schemas");
    Ok(())
}

#[test]
fn tool_set_uses_registry_group_default_visibility() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new("hidden", false, [Tool::from_sync(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set();
    let handle = tool_set.begin()?;
    let outcome = execute_tool(&handle, &invocation("echo", "{}")?)?;
    assert_eq!(
        outcome,
        ToolExecution {
            content: "tool not found: echo".into(),
            ok: false,
        }
    );
    Ok(())
}

#[test]
fn hidden_group_is_searchable_then_loadable() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new(
        "visible",
        true,
        [Tool::from_sync(OtherTool)],
    ))?;
    registry.register_group(ToolGroup::new("hidden", false, [Tool::from_sync(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set();
    let discovery = tool_set.discovery();

    // The hidden tool is registered but not part of the default schema surface,
    // so it is not callable yet — only surfaced through the discovery catalog.
    {
        let handle = tool_set.begin()?;
        assert_eq!(
            handle.schemas_json(),
            r#"[{"type":"function","function":{"name":"other"}}]"#
        );
        let blocked = execute_tool(&handle, &invocation("echo", "{}")?)?;
        assert_eq!(
            blocked,
            ToolExecution {
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
    let outcome = execute_tool(&handle, &invocation("echo", "{}")?)?;
    assert_eq!(
        outcome,
        ToolExecution {
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
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new("hidden", false, [Tool::from_sync(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set_with_blacklist(&["hidden"]);
    let discovery = tool_set.discovery();
    let handle = tool_set.begin()?;

    assert_eq!(handle.schemas_json(), "no schemas");
    assert!(discovery.catalog().is_empty());
    assert!(!discovery.request_load("hidden"));
    Ok(())
}

#[test]
fn loaded_groups_reports_only_explicitly_loaded_hidden_groups() -> Result<()> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register_group(ToolGroup::new("hidden", false, [Tool::from_sync(EchoTool)]))?;
    registry.start_all()?;
    let mut tool_set = registry.tool_set();

    let _ = tool_set.begin()?;
    assert!(tool_set.loaded_groups().is_empty());
    assert!(tool_set.discovery().request_load("hidden"));
    assert!(tool_set.loaded_groups().is_empty());
    let _ = tool_set.begin()?;
    assert_eq!(tool_set.loaded_groups(), vec!["hidden"]);
    Ok(())
}

#[test]
fn durable_overrides_apply_to_a_rebuilt_registry() -> Result<()> {
    let state = DurableState::new(ToolRegistryState::default());
    let registry = ToolRegistry::from_state(state.clone());
    registry.register_group(ToolGroup::new("test", true, [Tool::from_sync(EchoTool)]))?;
    registry.disable("echo")?;

    let registry = Arc::new(ToolRegistry::from_state(state));
    registry.register_group(ToolGroup::new("test", true, [Tool::from_sync(EchoTool)]))?;
    registry.start_all()?;

    let mut tool_set = registry.tool_set();
    assert_eq!(tool_set.begin()?.schemas_json(), "no schemas");
    Ok(())
}

#[test]
fn registry_state_contains_only_explicit_overrides() -> Result<()> {
    let state = DurableState::new(ToolRegistryState::default());
    let registry = ToolRegistry::from_state(state.clone());
    registry.register_group(ToolGroup::new("test", true, [Tool::from_sync(EchoTool)]))?;

    registry.disable("echo")?;
    let encoded = state.get().encode_state()?.into_owned();
    let payload: serde_json::Value = serde_json::from_slice(&encoded.bytes)?;
    assert_eq!(payload, serde_json::json!({"overrides": {"echo": false}}));
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

#[test]
fn runner_retries_tool_according_to_retry_count() -> Result<()> {
    let attempts = Arc::new(AtomicU32::new(0));
    let outcome = run_retry_tool(RetryCount::extra(1), Arc::clone(&attempts))?;

    assert_eq!(
        outcome,
        ToolExecution {
            content: "ok".into(),
            ok: true,
        }
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn runner_does_not_retry_by_default() -> Result<()> {
    let attempts = Arc::new(AtomicU32::new(0));
    let outcome = run_retry_tool(RetryCount::none(), Arc::clone(&attempts))?;

    assert_eq!(
        outcome,
        ToolExecution {
            content: "tool invocation rejected: try again".into(),
            ok: false,
        }
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    Ok(())
}

struct EchoTool;

impl ToolSpec for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn schema(&self) -> &str {
        r#"{"type":"function","function":{"name":"echo"}}"#
    }

    fn usage(&self) -> Option<&str> {
        Some("Echoes the normalized arguments.")
    }
}

impl SyncToolHandler for EchoTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
        if call.name() != self.name() {
            return Err(ToolError::NotFound(call.name().to_owned()).into());
        }
        Ok(ToolOutput {
            output: call.arguments_json().to_owned(),
            ok: true,
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
}

impl SyncToolHandler for OtherTool {
    fn invoke(&self, call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
        if call.name() != self.name() {
            return Err(ToolError::NotFound(call.name().to_owned()).into());
        }
        Ok(ToolOutput {
            output: "other".into(),
            ok: true,
        })
    }
}

struct FailBeforeSuccess {
    attempts: Arc<AtomicU32>,
    retry_count: RetryCount,
}

impl ToolSpec for FailBeforeSuccess {
    fn name(&self) -> &str {
        "retry_demo"
    }

    fn schema(&self) -> &str {
        "{}"
    }

    fn retry_count(&self) -> RetryCount {
        self.retry_count
    }
}

impl SyncToolHandler for FailBeforeSuccess {
    fn invoke(&self, _call: &ToolInvocation<'_>) -> ToolResult<ToolOutput> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ToolInvokeError::new(ToolError::InvokeRejected(
                "try again".into(),
            )));
        }
        Ok(ToolOutput {
            output: "ok".into(),
            ok: true,
        })
    }
}

fn invocation(name: &'static str, arguments_json: &'static str) -> Result<ToolInvocation<'static>> {
    ToolInvocation::try_new(None, name, arguments_json).map_err(|error| anyhow!("{error:?}"))
}

fn execute_tool(handle: &ToolSetHandle<'_>, call: &ToolInvocation<'_>) -> Result<ToolExecution> {
    let call = ToolRunInvocation::try_new(call.id(), call.name(), call.arguments_json())
        .map_err(|error| anyhow!("{error:?}"))?;
    let (mut join, detached) = ToolRunner::new(handle).run(vec![call]);
    if detached.is_some() {
        return Err(anyhow!("test helper does not accept detached tools"));
    }
    poll_ready(async move {
        join.next()
            .await
            .map(ToolRunResult::into_parts)
            .map(|(_, execution)| execution)
            .ok_or_else(|| anyhow!("join stream ended without a result"))
    })?
}

fn run_retry_tool(retry_count: RetryCount, attempts: Arc<AtomicU32>) -> Result<ToolExecution> {
    let registry = Arc::new(ToolRegistry::new());
    let mut tool_set = registry.tool_set();
    tool_set.add_group(ToolGroup::new(
        "retry",
        true,
        [Tool::from_sync(FailBeforeSuccess {
            attempts,
            retry_count,
        })],
    ))?;
    let handle = tool_set.begin()?;
    let call = invocation("retry_demo", "{}")?;
    execute_tool(&handle, &call)
}

fn poll_ready<T>(future: impl Future<Output = T>) -> Result<T> {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => Ok(output),
        Poll::Pending => Err(anyhow!("future was pending")),
    }
}
