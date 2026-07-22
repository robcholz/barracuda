# Objects (Injections)

## BaseAgent

`BaseAgent<H, T>` owns the assembled single-Agent runtime only:

- `Box<dyn Transcript>`
- `ToolSet`
- `Arc<dyn PermissionPolicy>`
- assembled `Context`
- `SharedApiManager` and this Agent's `ApiUsage`
- `Vec<Box<dyn ContextAdapter>>`
- `AgentEffectInbox`

For each tool round, BaseAgent statically injects its permission implementation
into the iteration loop. The implementation alone owns approval emission and
waiting; `ToolExecutor` receives only authorized calls.

`BaseAgent::submit(&mut self, Message)` returns the sole
`AgentStreamHandle<'_>`. Message queues, session state, multiagent graph state,
filesystem persistence, and scheduling ownership remain outside BaseAgent.
