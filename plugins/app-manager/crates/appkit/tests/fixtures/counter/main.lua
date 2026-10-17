-- Counter: the view (app.tsx) shows `count` and `history` and calls
-- `increment(by)` or `reset()`. This program owns the state and keeps it in
-- the app's key-value store, so it survives being terminated.
local app = require("app")
local view = assert(app.view())

local saved = app.kv.get("state") or {}
local count, history, next_id = saved.count or 0, saved.history or {}, saved.next_id or 1

local function publish()
  assert(view:set("count", count))
  assert(view:set("history", history))
end

local function save()
  assert(app.kv.set("state", { count = count, history = history, next_id = next_id }))
end

publish()
for event, by in app.events() do
  if event == "increment" then
    count = count + by
    table.insert(history, 1, { id = next_id, label = "+" .. by .. " = " .. count })
    next_id = next_id + 1
    if #history > 3 then
      table.remove(history)
    end
    publish()
  elseif event == "reset" then
    count, history = 0, {}
    publish()
  elseif event == "app:background" then
    save() -- a suspended app may be terminated without another event
  elseif event == "app:terminate" then
    save()
    break
  end
end
