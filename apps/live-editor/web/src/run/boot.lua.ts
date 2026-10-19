/**
 * The Lua half of the preview's app runtime, run before the app's entry program. It builds the
 * `app` and `notify` packages as the device binds them (docs/plugins/app-manager/app-format.mdx),
 * over one host function, and runs the program in a coroutine the host resumes: `app.events()`
 * yields until the host has an event, and `notify.ask` yields until the person answers.
 *
 * Values cross to the host as JSON text: `encode` writes a table that is an array or has string
 * keys, integers as integers and floats with a point, as the device keeps them apart.
 */
export const BOOT = String.raw`
local host, source, entry, limit = __host, __source, __entry, __limit
__host, __source, __entry, __limit = nil, nil, nil, nil
local yield, sethook, traceback = coroutine.yield, debug.sethook, debug.traceback

local function quote(text)
  return '"' .. text:gsub('[%c"\\]', function(c)
    local named = { ['"'] = '\\"', ['\\'] = '\\\\', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }
    return named[c] or string.format('\\u%04x', c:byte())
  end) .. '"'
end

-- strict: the rules of app.kv.set; otherwise as view:set reads a table, arrays by #t.
local function encode(value, strict, depth, out)
  local kind = type(value)
  if kind == 'nil' then out[#out + 1] = 'null'
  elseif kind == 'boolean' then out[#out + 1] = tostring(value)
  elseif kind == 'number' then
    if math.type(value) == 'integer' then out[#out + 1] = string.format('%d', value)
    elseif value ~= value or value == math.huge or value == -math.huge then
      if strict then return 'a finite number' end
      out[#out + 1] = 'null'
    else
      local text = string.format('%.17g', value)
      if not text:find('[%.eEn]') then text = text .. '.0' end
      out[#out + 1] = text
    end
  elseif kind == 'string' then
    if not utf8.len(value) then
      if strict then return 'UTF-8 text' end
      out[#out + 1] = 'null'
    else out[#out + 1] = quote(value) end
  elseif kind == 'table' then
    if depth > 16 then return 'a shallower table' end
    local count, length = 0, #value
    for _ in pairs(value) do count = count + 1 end
    if length > 0 and (not strict or length == count) then
      out[#out + 1] = '['
      for i = 1, length do
        if i > 1 then out[#out + 1] = ',' end
        local problem = encode(value[i], strict, depth + 1, out)
        if problem then return problem end
      end
      out[#out + 1] = ']'
    else
      local keys = {}
      for key in pairs(value) do
        if type(key) == 'string' then keys[#keys + 1] = key
        elseif strict then return 'tables that are arrays or have string keys' end
      end
      table.sort(keys)
      out[#out + 1] = '{'
      for i, key in ipairs(keys) do
        if i > 1 then out[#out + 1] = ',' end
        out[#out + 1] = quote(key) .. ':'
        local problem = encode(value[key], strict, depth + 1, out)
        if problem then return problem end
      end
      out[#out + 1] = '}'
    end
  elseif strict then return 'booleans, numbers, strings and tables'
  else out[#out + 1] = 'null' end
end

local function to_json(value, strict)
  local out = {}
  local problem = encode(value, strict, 0, out)
  if problem then return nil, problem end
  return table.concat(out)
end

local function decode(text)
  local at = 1
  local value
  local function space() at = text:find('[^ \t\r\n]', at) or #text + 1 end
  local function string_at()
    local parts = {}
    at = at + 1
    while true do
      local stop = text:find('["\\]', at)
      parts[#parts + 1] = text:sub(at, stop - 1)
      if text:sub(stop, stop) == '"' then at = stop + 1 return table.concat(parts) end
      local escape = text:sub(stop + 1, stop + 1)
      if escape == 'u' then
        local code = tonumber(text:sub(stop + 2, stop + 5), 16)
        if code >= 0xD800 and code < 0xDC00 then
          local low = tonumber(text:sub(stop + 8, stop + 11), 16)
          code = 0x10000 + (code - 0xD800) * 0x400 + (low - 0xDC00)
          at = stop + 12
        else at = stop + 6 end
        parts[#parts + 1] = utf8.char(code)
      else
        local named = { b = '\b', f = '\f', n = '\n', r = '\r', t = '\t' }
        parts[#parts + 1] = named[escape] or escape
        at = stop + 2
      end
    end
  end
  function value()
    space()
    local c = text:sub(at, at)
    if c == '{' then
      local object = {}
      at = at + 1
      space()
      if text:sub(at, at) == '}' then at = at + 1 return object end
      while true do
        space()
        local key = string_at()
        space()
        at = at + 1
        object[key] = value()
        space()
        local sep = text:sub(at, at)
        at = at + 1
        if sep == '}' then return object end
      end
    elseif c == '[' then
      local array = {}
      at = at + 1
      space()
      if text:sub(at, at) == ']' then at = at + 1 return array end
      local n = 0
      while true do
        n = n + 1
        array[n] = value()
        space()
        local sep = text:sub(at, at)
        at = at + 1
        if sep == ']' then return array end
      end
    elseif c == '"' then return string_at()
    elseif text:sub(at, at + 3) == 'true' then at = at + 4 return true
    elseif text:sub(at, at + 4) == 'false' then at = at + 5 return false
    elseif text:sub(at, at + 3) == 'null' then at = at + 4 return nil
    else
      local number = text:match('^-?%d+%.?%d*[eE]?[-+]?%d*', at)
      at = at + #number
      if number:find('[%.eE]') then return tonumber(number) + 0.0 end
      return math.tointeger(tonumber(number)) or tonumber(number)
    end
  end
  return value()
end

-- One call to the host: it answers {"v": value} or {"e": message}.
local function call(op, ...)
  local reply = decode(host(op, ...))
  if reply.e then return nil, reply.e end
  return reply.v
end
local function ok(...)
  local result, problem = call(...)
  if problem then return nil, problem end
  return true
end

local manifest = decode(host('manifest'))
local app = { id = manifest.id, name = manifest.name, version = manifest.version }
local exited, ended = false, false

local view
if manifest.view then
  local open = true
  local methods = {}
  function methods:set(name, value)
    if not open then return nil, 'the view is closed' end
    return ok('view.set', name, (to_json(value, false)))
  end
  function methods:is_open() return open end
  function methods:close() open = false host('view.close') end
  view = setmetatable({}, { __index = methods, __close = function() methods.close() end, __name = 'view' })
end

function app.device() return call('device') end
function app.view() return view end
function app.exit() exited = true end
function app.navigation(depth) return ok('navigation', math.tointeger(depth) or 0) end
function app.events(options)
  if type(options) == 'table' and options.every ~= nil then
    local fine, problem = ok('every', options.every)
    if not fine then error(problem, 2) end
  end
  return function()
    if exited or ended then return nil end
    -- An event is [count, name, args...]: the count keeps a nil argument from ending the list.
    local event = decode(yield('wait'))
    if event == nil then ended = true return nil end
    return table.unpack(event, 2, event[1] + 1)
  end
end

app.kv = {}
function app.kv.get(key) return call('kv.get', key) end
function app.kv.set(key, value)
  local text, problem = to_json(value, true)
  if problem then return nil, 'the store keeps ' .. problem end
  return ok('kv.set', key, text)
end
function app.kv.delete(key) return ok('kv.delete', key) end
function app.kv.keys() return call('kv.keys') end

app.fs = {}
function app.fs.read(path) return call('fs.read', path) end
function app.fs.write(path, data) return ok('fs.write', path, tostring(data)) end
function app.fs.append(path, data) return ok('fs.append', path, tostring(data)) end
function app.fs.list(dir) return call('fs.list', dir or '') end
function app.fs.exists(path) return call('fs.exists', path) end
function app.fs.remove(path) return ok('fs.remove', path) end
function app.fs.mkdir(path) return ok('fs.mkdir', path) end

local notify = {}
function notify.post(title, body) return call('notify.post', tostring(title), body and tostring(body) or nil) end
function notify.ask(title, body, allow, deny)
  for _, label in ipairs({ allow or '', deny or '' }) do
    if utf8.len(label) and utf8.len(label) > 6 then return nil, 'button labels have at most 6 characters' end
  end
  return decode(yield('ask', (to_json({ title = tostring(title), body = body, allow = allow, deny = deny }, false))))
end

local packages = { app = app }
for _, name in ipairs(manifest.packages or {}) do
  if name == 'notify' then packages.notify = notify end
end
local named = {}
for _, name in ipairs(manifest.named or {}) do named[name] = true end
function require(name)
  if packages[name] then return packages[name] end
  if named[name] then error("the preview has no '" .. name .. "' package: try it on the device", 2) end
  error("module '" .. tostring(name) .. "' not found: the app's manifest does not name it", 2)
end

function print(...)
  local parts = table.pack(...)
  for i = 1, parts.n do parts[i] = tostring(parts[i]) end
  host('print', table.concat(parts, '\t', 1, parts.n))
end

-- What a run has besides the packages: no files to load, no process to leave.
io, dofile, loadfile, debug = nil, nil, nil, nil
for _, name in ipairs({ 'exit', 'execute', 'getenv', 'remove', 'rename', 'tmpname', 'setlocale' }) do os[name] = nil end

local main, problem = load(source, '@' .. entry, 't')
local co = main and coroutine.create(main)
local deadline = 0
if co then
  sethook(co, function()
    if host('now') > deadline then error(entry .. ' ran for ' .. (limit // 1000) .. ' s without returning to app.events()', 0) end
  end, '', 100000)
end

-- The host steps the program: it answers {"wait"}, {"ask", question}, {"done"} or {"error", text}.
return function(payload)
  if not co then return '["error",' .. quote(problem) .. ']' end
  deadline = host('now') + limit
  local fine, what, detail = coroutine.resume(co, payload)
  if not fine then return '["error",' .. quote(traceback(co, tostring(what))) .. ']' end
  if coroutine.status(co) == 'dead' then return '["done"]' end
  if what == 'ask' then return '["ask",' .. detail .. ']' end
  return '["wait"]'
end
`;
