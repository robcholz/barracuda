A background tool call returns `[background:accepted]` with an `id` and keeps
running. Its progress and final result arrive automatically as
`[background:progress]`, `[background:completed]`, or `[background:failed]`
updates naming that `id`. Use `background_list` only when you need to recover
an `id` or check a status before deciding what to do; finished calls leave the
list once their result is delivered.
