use super::set::ToolSetHandle;
use super::tool::ToolInvocation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolExecution {
    pub content: String,
    pub ok: bool,
}

/// Executes already-authorized tool calls. Permission evaluation is a separate
/// caller-owned phase and never occurs here.
pub struct ToolExecutor<'a> {
    tools: &'a ToolSetHandle<'a>,
}

impl<'a> ToolExecutor<'a> {
    pub fn new(tools: &'a ToolSetHandle<'a>) -> Self {
        Self { tools }
    }

    pub async fn execute<'call>(&self, call: &'call ToolInvocation<'call>) -> ToolExecution {
        match self.tools.invoke(call).await {
            Ok(output) => ToolExecution {
                content: output.output,
                ok: output.ok,
            },
            Err(error) => ToolExecution {
                content: error.to_string(),
                ok: false,
            },
        }
    }
}
