-- Read-only, bounded checks shared by setup and :checkhealth.
local M = {}
local callbacks = { 'focus_terminal', 'label_terminal', 'create_terminal',
  'split_terminal', 'close_terminal', 'reorder_terminals' }

function M.requirements()
  if vim.fn.has('nvim-0.12') ~= 1 then return nil, 'Neovim 0.12 or newer is required' end
  local system = vim.uv.os_uname().sysname
  if system ~= 'Darwin' and system ~= 'Linux' then return nil, 'Neordr supports macOS and Linux' end
  return true
end

function M.validate(opts)
  local supported, reason = M.requirements()
  if not supported then return nil, reason end
  if type(opts) ~= 'table' then return nil, 'setup options must be a table' end
  for _, key in ipairs(callbacks) do
    if opts[key] ~= nil and type(opts[key]) ~= 'function' then
      return nil, key .. ' must be a function'
    end
  end
  if opts.session ~= nil and (type(opts.session) ~= 'string' or #opts.session == 0
      or #opts.session > 64 or opts.session == 'default' or opts.session:find('[^a-zA-Z0-9_-]')) then
    return nil, 'session must be 1-64 ASCII letters, digits, hyphens or underscores; default is reserved'
  end
  if opts.bin ~= nil and (type(opts.bin) ~= 'string' or opts.bin == '') then
    return nil, 'bin must be a nonempty executable path or name'
  end
  local requested = opts.bin or 'neordr'
  if requested:sub(1, 2) == '~/' then requested = vim.fn.expand('~') .. requested:sub(2) end
  local bin = vim.fn.exepath(requested)
  if bin == '' then
    return nil, 'Cannot execute ' .. requested .. '. Install the Neordr binary and add it to PATH, or set setup({ bin = "/path/to/neordr" })'
  end
  local ok, result = pcall(function()
    return vim.system({ bin, '--version' }, { text = true }):wait(1500)
  end)
  if not ok then return nil, 'Cannot run Neordr version check: ' .. tostring(result) end
  if result.code == 124 then return nil, 'Neordr version check timed out after 1.5 seconds' end
  if result.code ~= 0 then
    return nil, 'Neordr version check failed: ' .. vim.trim(result.stderr or '') .. ' (exit ' .. result.code .. ')'
  end
  local version = (result.stdout or ''):match('^neordr ([^%s]+)%s*$')
  local expected = require('neordr.version')
  if version ~= expected then
    return nil, 'Neordr version mismatch: Lua expects ' .. expected .. ', binary reports '
      .. (version or vim.trim(result.stdout or 'unknown')) .. '. Install the binary and plugin from the same release'
  end
  return { bin = bin, version = version }
end

function M.config_root()
  local config = vim.env.XDG_CONFIG_HOME
  if config and config ~= '' then
    if config:sub(1, 1) ~= '/' then return nil, 'XDG_CONFIG_HOME must be absolute' end
    return config
  end
  local home = vim.env.HOME
  if not home or home == '' then return nil, 'HOME is unset' end
  return home .. '/.config'
end

-- Health checks use ping only. In particular, they never call router --background.
-- vim.wait services Neovim RPC while the bridge validates this editor's identity.
function M.ping(path)
  local pipe = vim.uv.new_pipe(false)
  local done, problem, bytes = false, nil, ''
  local request = vim.json.encode({ id = 'neordr:health', method = 'ping', params = vim.empty_dict() }) .. '\n'
  local connected, connect_error = pcall(pipe.connect, pipe, path, function(err)
    if pipe:is_closing() then return end
    if err then problem = tostring(err); done = true; return end
    pipe:read_start(function(read_err, data)
      if read_err then problem = tostring(read_err); done = true
      elseif not data then problem = 'Connection closed before a response'; done = true
      else
        bytes = bytes .. data
        if #bytes > 32768 then problem = 'Oversized health response'; done = true
        elseif bytes:find('\n', 1, true) then done = true end
      end
      if done then pipe:read_stop() end
    end)
    pipe:write(request, function(write_err)
      if write_err then problem = tostring(write_err); done = true end
    end)
  end)
  if not connected then pipe:close(); return nil, tostring(connect_error) end
  local completed = vim.wait(1000, function() return done end, 10)
  if not pipe:is_closing() then pipe:read_stop(); pipe:close() end
  if not completed then return nil, 'Health check timed out after 1 second' end
  if problem then return nil, problem end
  local ok, reply = pcall(vim.json.decode, bytes:match('^[^\n]*'))
  if not ok or type(reply) ~= 'table' or reply.id ~= 'neordr:health' then
    return nil, 'Invalid health response'
  end
  if reply.error then return nil, vim.inspect(reply.error) end
  if type(reply.result) ~= 'table' or reply.result.type ~= 'pong' then return nil, 'Expected a pong response' end
  return reply.result
end

function M.bridge(session)
  if not session then return nil, 'Neordr is not configured' end
  local root, problem = M.config_root()
  if not root then return nil, problem end
  local path = root .. '/herdr/sessions/' .. session .. '/herdr.sock'
  if not vim.uv.fs_stat(path) then return nil, 'Session bridge socket is missing' end
  local response, err = M.ping(path)
  if not response then return nil, err end
  if not response.neordr or response.neordr.pid ~= vim.fn.getpid()
      or response.neordr.generation ~= vim.g.neordr_generation then
    return nil, 'Session bridge belongs to another Neovim instance'
  end
  if response.version ~= 'neordr/' .. require('neordr.version') then
    return nil, 'Session bridge version mismatch: ' .. tostring(response.version)
  end
  return response
end

return M
