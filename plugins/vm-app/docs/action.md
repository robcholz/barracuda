# App Workflow Actions

The `app` Plugin registers `app.list`, `app.open`, `app.home`,
`app.interactive`, `app.terminate`, `app.status`, and `app.tap`. They are the
same operations the system UI performs; see [the lifecycle](plugin.md#lifecycle).
Their request and response contracts live at `schemas/action/<address>/`.
Rejections return `{"error":"<code>"}` so a Workflow can branch on them.

A running app is reported as:

```json
{"app":{"id":"counter","name":"计数器","version":"0.1.0","location":"bundled","has_view":true},
 "run_id":1,"state":"active"}
```

`state` is `active`, `inactive`, `background`, `suspended`, `running` (a
headless app), or `terminating`.

`app.list` takes `{}` and returns every launchable app, and every container
that breaks the [app format](app-format.md) with the reason, so its author can
fix it. `location` is `installed`, `bundled`, or `workspace`:

```json
{"apps":[{"id":"counter","name":"计数器","version":"0.1.0","location":"bundled","has_view":true}],
 "invalid":[{"location":"workspace","dir":"hello","problem":"main.lua is missing"}]}
```

`app.open` takes `{"id":"counter"}`. It resumes a running app or launches it,
returns the running app once its program has started, and fails with
`not_found`, `invalid_bundle`, `storage`, `view_rejected`,
`runtime_unavailable`, or `busy`.

`app.home` takes `{}` and returns `{"app": <running app or null>}`: the app
that moved to the background.

`app.interactive` takes `{"interactive":false}` while an overlay covers the
foreground app and `{"interactive":true}` when it closes. It returns the
foreground app or fails with `no_foreground_app`.

`app.terminate` takes `{"id":"counter"}` and returns `{"app": <running app or
null>}`, now `terminating`.

`app.status` takes `{}` and returns the foreground app, every running app most
recently used first, the text the foreground view shows, and how the last app
run ended:

```json
{"foreground":null,"running":[],
 "last_exit":{"id":"counter","run_id":1,"outcome":"success","output":[]}}
```

`app.tap` taps the active foreground view at view coordinates and returns the
handler it called, or `null` when nothing pressable is there:

```json
{"x":120,"y":200}
```

```json
{"handler":"increment"}
```

It fails with `no_foreground_app` or `not_interactive`.
