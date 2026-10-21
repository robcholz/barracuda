-- Wrap: shows what the test writes; it has no program of its own to speak of.
local app = require("app")
assert(app.view())
for event in app.events() do
  if event == "app:terminate" then
    break
  end
end
