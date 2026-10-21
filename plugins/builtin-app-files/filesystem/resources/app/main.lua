-- Files, a builtin app: how many files each shared Workspace area holds, and
-- the files in one.
local app = require("app")
local files = require("files")
local view = assert(app.view())

local AREAS, AREA = 0, 1
local GLYPHS = {
  ["Media"] = "\u{E03C}",
  ["Resources"] = "\u{E060}",
  ["SD card"] = "\u{E034}",
  ["Cache"] = "\u{E050}",
}
local page = AREAS
local list = {}
local current = nil

local function clock()
  local ok, now = pcall(os.date, "%H:%M")
  return ok and now or ""
end

local function set(name, value)
  assert(view:set(name, value))
end

local function publish()
  set("clock", clock())
  set("page", page)
  if page == AREAS then
    list = files.areas() or {}
    local rows = {}
    for index, item in ipairs(list) do
      rows[index] = {
        id = index,
        name = item.name,
        count = item.present and tostring(item.count) or "-",
        glyph = GLYPHS[item.name] or "",
      }
    end
    set("areas", rows)
  elseif current then
    local shown = files.list(current.name) or { files = {} }
    local rows = {}
    for index, file in ipairs(shown.files) do
      if index > 5 then break end
      rows[index] = { id = index, path = file.path, size = file.label }
    end
    set("area", current.name)
    local noun = shown.count == 1 and " file · " or " files · "
    set("summary", shown.present and (shown.count .. noun .. shown.label) or "Not inserted")
    set("files", rows)
    set("empty", #rows == 0)
  end
end

set("areas", {})
set("files", {})
publish()
for event, value in app.events({ every = 15000 }) do
  if event == "open" then
    current = list[value]
    if current then
      page = AREA
      app.navigation(1)
      publish()
    end
  elseif event == "app:back" then
    page = AREAS
    app.navigation(0)
    publish()
  elseif event == "app:tick" or event == "app:foreground" then
    publish()
  elseif event == "app:terminate" then
    break
  end
end
