-- Counter: the view (app.tsx) shows `count` and `history` and calls
-- `increment(by)` or `reset()`; this file owns the state.
local ui = require("ui")
local view = assert(ui.attach(APP.ui))

local count, history, next_id = 0, {}, 1

local function publish()
  assert(view:set("count", count))
  assert(view:set("history", history))
end

publish()
while true do
  local event, by = view:next()
  if event == nil then
    break -- the view was closed
  elseif event == "increment" then
    count = count + by
    table.insert(history, 1, { id = next_id, label = "+" .. by .. " = " .. count })
    next_id = next_id + 1
    if #history > 3 then
      table.remove(history)
    end
  elseif event == "reset" then
    count, history = 0, {}
  end
  publish()
end
