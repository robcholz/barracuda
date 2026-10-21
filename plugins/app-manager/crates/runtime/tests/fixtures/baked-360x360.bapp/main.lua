-- Baked: the view calls `confirm()`; the program prints it.
local app = require("app")
assert(app.view())
for event in app.events() do
  if event == "confirm" then
    print("confirmed")
  elseif event == "app:terminate" then
    break
  end
end
