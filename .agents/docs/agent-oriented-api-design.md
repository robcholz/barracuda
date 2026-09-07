# Agent-Oriented API Design

This document defines conventions for model-facing ToolCalls. Internal APIs may
take any shape as long as the Agent-facing contract follows these rules.

## ToolCall nature

Every ToolCall has one fixed nature: **ShortRun** or **LongRun**. The nature is
part of the ToolCall definition. It does not change based on arguments, actual
elapsed time, or a choice made by the Agent.

Use this test:

> When the first ToolCall result is returned, has the requested work reached a
> terminal state?

- If yes, the ToolCall is ShortRun.
- If work continues after that result, the ToolCall is LongRun.

A persistent resource does not by itself make a ToolCall LongRun. For example,
`schedule_create` can be ShortRun when the schedule has been fully created
before the call returns.

## ShortRun ToolCalls

A ShortRun ToolCall is one atomic request/result exchange:

```text
call -> succeeded | failed | timed_out
```

The first result is always terminal. A ShortRun ToolCall:

- returns the requested result or a terminal error;
- does not return a handle for continuing work;
- has no `status`, `wait`, or `cancel` ToolCalls;
- is retried by making a new ToolCall.

Name a ShortRun ToolCall after its domain action, such as `file_read`,
`memory_recall`, `schedule_create`, or `code_validate`. Do not add lifecycle
verbs when no lifecycle exists.

## LongRun ToolCalls

A LongRun ToolCall starts work that continues after the first result:

```text
start -> accepted(handle) -> running -> terminal completion
```

The initial result is not completion. It confirms that the system has accepted
ownership of the work and returns a stable handle that later ToolCalls can
reference.

Every LongRun capability has the following Agent-facing actions.

### Start

Start the work and return its handle. Use the domain-appropriate verb, such as
`start`, `submit`, `open`, `spawn`, or `subscribe`.

Return `accepted` only after the system has taken ownership of the work. If the
work cannot be started, return a terminal failure instead of a handle.

### Status

Return the current state immediately without waiting. The result should include
the handle, current state, and a short progress summary when one is available.
If the work is terminal, status must expose its terminal state and make its
result recoverable.

`status` is an observation tool, not a polling recommendation. Normal
completion is delivered automatically.

### Wait

Block the current ToolCall until the work reaches a terminal state or the wait
deadline expires.

A wait deadline applies only to that wait call. Reaching it must not cancel or
change the underlying work. If the work is already terminal, `wait` returns its
stored result immediately.

The Agent should wait only when its next step requires the result and no useful
independent work can continue.

### Cancel

Request termination of unfinished work. If cancellation is not supported, the
ToolCall description must say so explicitly.

Cancellation is distinct from closing an input stream, stopping a continuous
service, or deleting stored state. Use the verb that matches the real domain
action.

### Automatic completion

Deliver terminal completion to the Agent automatically when it is not already
observed through `wait`. The Agent must not need to poll `status` to discover
completion.

If `wait` returns the terminal result, do not deliver the same completion to the
Agent a second time. Completion may be queued while the Agent is busy and
delivered when it can process a new turn.

## Optional LongRun actions

Add these only when the capability's nature requires them:

- `list`: recover handles when multiple runs may coexist;
- `send` or `append`: provide more input to interactive work;
- `close`: finish an input stream or session normally;
- `interrupt`: replace or redirect active work;
- `pause` and `resume`: control work that supports suspension;
- `result` or `read`: retrieve a large or retained terminal result;
- `delete`: remove a resource or retained state after execution.

Do not use `watch` for an immediate snapshot. `status` returns a snapshot;
`watch` is reserved for a ToolCall that actually waits for a state change or
streams changes.

## Examples

| ToolCall | Nature | Reason |
| --- | --- | --- |
| `http_request` | ShortRun | The response is terminal when returned. |
| `schedule_create` | ShortRun | The creation finishes even though the schedule persists. |
| `deployment_start` | LongRun | Deployment continues after acceptance. |
| `download_submit` | LongRun | The transfer continues under a returned handle. |
| `terminal_open` | LongRun | The session remains active and accepts later input. |
| `metrics_subscribe` | LongRun | The subscription continues producing observations. |

## Review checklist

For every new ToolCall:

1. Declare its nature as ShortRun or LongRun.
2. Make the first result match that nature: terminal result or accepted handle.
3. For LongRun, define `status`, `wait`, cancellation behavior, and automatic
   completion.
4. Add only the domain-specific lifecycle actions that are genuinely supported.
5. Describe completion behavior clearly so the Agent does not need to poll.
