# Shell Workflow Actions

The `shell` Plugin registers `shell.notify`. Its request and response
contracts live at `schemas/action/shell.notify/`.

`shell.notify` takes `{"title":"门铃","body":"有人在门口"}`, with `body`
optional, posts the notice to the notification center, and returns
`{"id":3}`. Titles and bodies longer than 64 characters are cut, and beyond
eight notices the oldest is dropped.
