# Agent Scheduler Plugin

`agent-scheduler` depends on `agent` and `scheduler`. During registration it adds the awaited `scheduler_schedule` and `scheduler_cancel` Tools to `AgentToolRegistry`. It owns no schedules, storage, clock, or background task.
