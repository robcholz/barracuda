-- Camera: an example installable app that takes photos with the Board's
-- camera and keeps them in the app's storage. It asks for the `camera`
-- package.
local app = require("app")
local camera = require("camera")
local view = assert(app.view())

local CAPACITY = 256 * 1024
local shots = app.kv.get("shots") or 0
local device = camera.available() and camera.open() or nil

local function finder(text)
  assert(view:set("finder", text))
end

assert(view:set("busy", false))
finder(device and "Viewfinder" or "No camera")
for event in app.events() do
  if event == "shoot" and device then
    assert(view:set("busy", true))
    local photo, message = device:capture(CAPACITY)
    if photo then
      shots = shots + 1
      app.fs.mkdir("/photos")
      assert(app.fs.write(string.format("/photos/%04d.jpg", shots), photo))
      assert(app.kv.set("shots", shots))
      finder("Saved " .. shots .. (shots == 1 and " photo" or " photos"))
    else
      finder(message or "Couldn't take photo")
    end
    assert(view:set("busy", false))
  elseif event == "app:terminate" then
    break
  end
end
