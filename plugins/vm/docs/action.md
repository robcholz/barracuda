# VM Workflow Actions

The VM Plugin registers `vm.run`, `vm.input`, and `vm.cancel` with the
Workflow Action registry. Their static request and response contracts live at
`schemas/action/<address>/`.

`vm.run` accepts a complete Lua source string and immediately returns a
`run_id`. It has no Event Router frame limit:

```json
{"source":"io.print('hello')"}
```

`vm.input` supplies either one complete input string or EOF:

```json
{"run_id":1,"input":"answer"}
```

```json
{"run_id":1,"eof":true}
```

`vm.cancel` accepts `{"run_id":1}`. Business rejections are returned as
`{"error":"<code>"}` so a Workflow can branch on them.
