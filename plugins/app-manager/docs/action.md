# App Manager Workflow Actions

The App Manager Plugin registers `app.list`, `app.launch`, `app.stop`,
`app.status`, and `app.tap`. Their request and response contracts live at
`schemas/action/<address>/`. Rejections return `{"error":"<code>"}` so a
Workflow can branch on them.

`app.list` takes `{}` and returns every valid app:

```json
{"apps":[{"id":"counter","name":"计数器","version":"0.1.0","location":"bundled","has_view":true}]}
```

`app.launch` replaces the foreground app and returns once the program has
started, not when it exits:

```json
{"id":"counter"}
```

```json
{"app":{"id":"counter","name":"计数器","version":"0.1.0","location":"bundled","has_view":true},"run_id":1}
```

It fails with `not_found`, `invalid_bundle`, `storage`, `view_rejected`,
`runtime_unavailable`, or `busy`.

`app.stop` takes `{}` and returns the stopped app, or `null`:
`{"stopped":null}`.

`app.status` takes `{}` and returns the foreground app, the text its view
shows when it has one, and how the last app run ended:

```json
{"foreground":null,"last_exit":{"id":"counter","run_id":1,"outcome":"error","error":"lua_runtime","diagnostic":"…","output":[]}}
```

`app.tap` taps the foreground view at view coordinates and returns the
handler it called, or `null` when nothing pressable is there:

```json
{"x":120,"y":200}
```

```json
{"handler":"increment"}
```

It fails with `no_foreground_app` or `no_view`.
