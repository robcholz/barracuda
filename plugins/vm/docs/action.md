# VM Workflow Actions

The VM Plugin registers `vm.run`, `vm.input`, and `vm.cancel` with the Workflow
Action registry. Their static request and response contracts live at
`schemas/action/<address>/`.

`vm.run` accepts a complete Lua source string and asynchronously waits until the
execution finishes. It has no Event Router frame limit:

```json
{"source":"io.print('hello')"}
```

A successful execution returns all complete `io.print(...)` messages in order:

```json
{"run_id":1,"outcome":"success","output":["hello"]}
```

Cancellation returns `outcome: "cancelled"`. Execution failures return
`outcome: "error"` with a stable `error` code and diagnostic. Rejection before
an execution slot is accepted returns `{"error":"<code>"}`.

If Lua waits in `io.input()`, the Workflow Action continues awaiting the same
execution; it does not return an intermediate response or emit a Workflow
Event. A concurrent workflow branch may supply input using `vm.input`.

`vm.input` supplies either one complete input string or EOF:

```json
{"run_id":1,"input":"answer"}
```

```json
{"run_id":1,"eof":true}
```

`vm.cancel` accepts `{"run_id":1}`. Control-operation rejections are returned
as `{"error":"<code>"}` so a Workflow can branch on them.
