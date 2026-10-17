Use `workflow_switch` to pause a Workflow without deleting it: with `enabled`
false it stays loaded and listed, but no Event starts it until it is switched
on again. Executions already running finish. Bundled Workflows, such as
`imessage-to-agent`, can be switched too. An unknown ID returns `not_found`.
