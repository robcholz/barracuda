//! The `background` group: model-facing control over the Agent's
//! [`BackgroundToolPool`].

use alloc::{boxed::Box, string::String, string::ToString};

use barracuda_agent_permission::{Action, RiskClass};
use barracuda_agent_tool::{
    tool_metadata, BackgroundToolPool, EmptyArgs, Tool, ToolFuture, ToolGroup, ToolHandler,
    ToolInvocation, ToolOutput, ToolSpec,
};
use embassy_time::Timer;
use futures_lite::future;
use serde::Deserialize;

/// Build the always-visible group that lists, waits for, feeds, and cancels
/// the calls in `pool`.
pub(crate) fn background_tools(pool: &BackgroundToolPool) -> ToolGroup {
    ToolGroup::new(
        "background",
        true,
        [
            Tool::new(BackgroundListTool { pool: pool.clone() }),
            Tool::new(BackgroundWaitTool { pool: pool.clone() }),
            Tool::new(BackgroundInputTool { pool: pool.clone() }),
            Tool::new(BackgroundCancelTool { pool: pool.clone() }),
        ],
    )
}

#[derive(Deserialize)]
struct CallArgs {
    id: u32,
}

#[derive(Deserialize)]
struct WaitArgs {
    id: u32,
    timeout_ms: u32,
}

#[derive(Deserialize)]
struct InputArgs {
    id: u32,
    input: Option<String>,
}

struct BackgroundListTool {
    pool: BackgroundToolPool,
}

impl ToolSpec for BackgroundListTool {
    tool_metadata!("background_list");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }

    fn concurrent(&self) -> bool {
        true
    }
}

impl ToolHandler for BackgroundListTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            Ok(ToolOutput {
                content: serde_json::json!({ "calls": self.pool.list() }).to_string(),
                ok: true,
            })
        })
    }
}

struct BackgroundWaitTool {
    pool: BackgroundToolPool,
}

impl ToolSpec for BackgroundWaitTool {
    tool_metadata!("background_wait");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Safe)
    }
}

impl ToolHandler for BackgroundWaitTool {
    type Args = WaitArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let wait = self.pool.wait(args.id)?;
            future::or(wait, async {
                Timer::after_millis(u64::from(args.timeout_ms)).await;
                Ok(ToolOutput {
                    content: format!(
                        "Background call {} is still running after {} ms; its result will be delivered automatically.",
                        args.id, args.timeout_ms
                    ),
                    ok: true,
                })
            })
            .await
        })
    }
}

struct BackgroundInputTool {
    pool: BackgroundToolPool,
}

impl ToolSpec for BackgroundInputTool {
    tool_metadata!("background_input");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Moderate)
    }
}

impl ToolHandler for BackgroundInputTool {
    type Args = InputArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        // The schema admits exactly one of `input` or `eof: true`.
        Box::pin(async move { self.pool.input(args.id, args.input).await })
    }
}

struct BackgroundCancelTool {
    pool: BackgroundToolPool,
}

impl ToolSpec for BackgroundCancelTool {
    tool_metadata!("background_cancel");

    fn classify(&self, _call: &ToolInvocation) -> Action {
        Action::new(self.name(), RiskClass::Moderate)
    }
}

impl ToolHandler for BackgroundCancelTool {
    type Args = CallArgs;

    fn invoke<'a>(&'a self, args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            self.pool.cancel(args.id)?;
            Ok(ToolOutput {
                content: format!("Background call {} cancelled.", args.id),
                ok: true,
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use alloc::vec;

    use barracuda_agent_tool::{
        BackgroundTool, BackgroundToolFuture, BackgroundToolHandler, ToolInvocation, ToolRunner,
        ToolSet,
    };
    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;

    struct PendingTool;

    impl ToolSpec for PendingTool {
        fn name(&self) -> &str {
            "pending"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"pending","parameters":{"type":"object"}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator =
                json_validator::validator!("resources/tools/background_list/schema.json");
            &VALIDATOR
        }
    }

    impl BackgroundToolHandler for PendingTool {
        type Args = EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> BackgroundToolFuture<'a> {
            Box::pin(async {
                Ok(BackgroundTool::new(
                    ToolOutput {
                        content: "accepted".to_string(),
                        ok: true,
                    },
                    Box::pin(core::future::pending()),
                ))
            })
        }
    }

    fn run(
        tools: &mut ToolSet,
        pool: &BackgroundToolPool,
        name: &str,
        arguments: &str,
    ) -> ToolOutput {
        let tools = tools.begin().expect("tool set begins");
        let call = ToolInvocation::try_new(Some("call"), name, arguments).expect("valid call");
        block_on(
            ToolRunner::new(&tools)
                .with_background(pool)
                .run(vec![call])
                .collect::<alloc::vec::Vec<_>>(),
        )
        .pop()
        .expect("tool result")
        .1
    }

    #[test]
    fn background_tools_list_reject_input_and_cancel_by_id() {
        let pool = BackgroundToolPool::new();
        let mut tools = ToolSet::empty();
        tools
            .add_group(background_tools(&pool))
            .expect("background tools register");
        tools
            .add_group(ToolGroup::new(
                "test",
                true,
                [Tool::background(PendingTool)],
            ))
            .expect("test tool registers");

        let accepted = run(&mut tools, &pool, "pending", "{}");
        assert!(accepted
            .content
            .starts_with("[background:accepted]\nid: 1\n"));

        let listed = run(&mut tools, &pool, "background_list", "{}");
        assert_eq!(
            listed.content,
            r#"{"calls":[{"id":1,"status":"running","tool":"pending"}]}"#
        );

        let input = run(
            &mut tools,
            &pool,
            "background_input",
            r#"{"id":1,"eof":true}"#,
        );
        assert!(!input.ok);

        let cancelled = run(&mut tools, &pool, "background_cancel", r#"{"id":1}"#);
        assert!(cancelled.ok);
        assert!(pool.is_empty());

        let missing = run(&mut tools, &pool, "background_cancel", r#"{"id":1}"#);
        assert!(!missing.ok);
    }
}
