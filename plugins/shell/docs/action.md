# Shell Workflow Actions

The `shell` Plugin registers `shell.notify`, `shell.confirm`, and
`shell.agent`. Their request and response contracts live under
`schemas/action/`.

`shell.notify` takes `{"title":"门铃","body":"有人在门口","icon":"alert"}`,
with `body` and `icon` optional, posts the notice to the notification center,
and returns `{"id":3}`. `icon` names a system icon; without one the notice
shows the title's first character. Titles and bodies longer than 64
characters are cut, and beyond eight notices the oldest is dropped.

`shell.confirm` takes `{"title":"打开客厅灯带？","body":"林夏 · Telegram"}`,
with `body` optional, asks the person on a card listed before every notice
and on the home screen, and returns once they answer:
`{"allowed":true,"answered":true}`. A card dismissed or dropped unanswered
returns `{"allowed":false,"answered":false}`.

`shell.agent` takes `{"state":"answering","question":"明天几点开会？","answer":"9:30 产品评审"}`,
with `question` and `answer` optional, and shows the Agent's state on the
home screen: `standby`, `listening`, `thinking`, or `answering`. It returns
`{}`. An answer returns to standby after nine seconds or when tapped.
