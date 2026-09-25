local M = type(package.loaded['neordr']) == 'table' and package.loaded['neordr'] or {}
local backend = require('neordr.backend')
local diagnostics = require('neordr.diagnostics')

local function notify(message)
  vim.schedule(function()
    if not vim.g.neordr_exiting then
      vim.notify('Neordr: ' .. message .. '\nRun :checkhealth neordr for details.', vim.log.levels.ERROR)
    end
  end)
end

local function fail(message)
  message = tostring(message)
  if M.last_error ~= message then notify(message) end
  M.last_error = message
  return false, message
end

local function owned_job_running()
  return M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1
end

-- Router recovery can own the bridge process. A plugin job ID is therefore
-- not the source of truth for this editor's connection.
function M.bridge_status()
  local response, problem = diagnostics.bridge(M.session)
  if response and M.last_error and M.last_error:match('^Session bridge exited') then
    M.last_error = nil
  end
  return response, problem
end

local function running()
  return owned_job_running() or (M.session and M.bridge_status() ~= nil)
end

local function commands()
  vim.api.nvim_create_user_command('NeordrTerm', function(ev)
    if not running() and not M.bridge_starting then
      notify('Session bridge is not running. Call setup() after fixing the startup error'); return
    end
    vim.cmd('botright new')
    local env = M.prepare(vim.api.nvim_get_current_buf())
    local cmd = #ev.fargs > 0 and ev.fargs or { vim.o.shell }
    local ok, job = pcall(vim.fn.jobstart, cmd, { term = true, env = env })
    if not ok or job <= 0 then
      vim.cmd('close!')
      notify('Cannot start terminal command: ' .. (ok and tostring(job) or job))
      return
    end
    vim.cmd.startinsert()
  end, { nargs = '*', complete = 'shellcmd', force = true })
  vim.api.nvim_create_user_command('NeordrStatus', function()
    local lines = { 'Neordr Lua ' .. require('neordr.version'),
      'Session: ' .. (M.session or 'not configured'),
      'Binary: ' .. (M.bin or 'not configured'),
      'Bridge job: ' .. (running() and 'running' or M.bridge_starting and 'starting' or 'stopped'),
      'Neovim socket: ' .. (vim.v.servername ~= '' and vim.v.servername or 'not started') }
    if M.last_error then lines[#lines + 1] = 'Last error: ' .. M.last_error end
    lines[#lines + 1] = 'Run :checkhealth neordr to verify connections.'
    vim.notify(table.concat(lines, '\n'))
  end, { force = true })
end

local function start(opts, bin, session, generation)
  if vim.v.servername == '' then vim.fn.serverstart() end
  vim.g.neordr_generation = generation
  backend.configure(session)
  backend.focus_terminal = opts.focus_terminal
  backend.label_terminal = opts.label_terminal
  backend.create_terminal = opts.create_terminal
  backend.split_terminal = opts.split_terminal
  backend.close_terminal = opts.close_terminal
  backend.reorder_terminals = opts.reorder_terminals
  M.session, M.bin, M.options = session, bin, vim.tbl_extend('force', {}, opts, { bin = bin })
  local function ensure_router()
    if vim.g.neordr_exiting or M.router_starting then return end
    M.router_starting = true
    local errors = {}
    local spawned, job = pcall(vim.fn.jobstart, { bin, 'router', '--background' }, {
      on_stderr = function(_, lines)
        for _, line in ipairs(lines) do if line ~= '' then table.insert(errors, line) end end
      end,
      on_exit = function(_, code)
        M.router_starting = false
        if code ~= 0 and not vim.g.neordr_exiting then
          local detail = table.concat(errors, '\n')
          local message = 'Router exited with code ' .. code .. (detail ~= '' and ': ' .. detail or '')
          if M.router_log ~= message then
            vim.schedule(function() vim.notify('Neordr router: ' .. message, vim.log.levels.ERROR) end)
          end
          M.router_log = message
        elseif code == 0 then
          M.router_log = nil
          if not owned_job_running() and not M.bridge_starting then
            vim.schedule(function() if not vim.g.neordr_exiting then M.bridge_status() end end)
          end
        end
      end,
    })
    if not spawned or job <= 0 then
      M.router_starting = false
      local message = 'Cannot start router process: ' .. tostring(job)
      if M.router_log ~= message then notify(message) end
      M.router_log = message
    end
  end
  ensure_router()
  if M.router_timer then M.router_timer:stop(); M.router_timer:close() end
  M.router_timer = vim.uv.new_timer()
  M.router_timer:start(5000, 5000, vim.schedule_wrap(ensure_router))
  local group = vim.api.nvim_create_augroup('NeordrLifecycle', { clear = true })
  vim.api.nvim_create_autocmd('VimLeavePre', { group = group, callback = function()
    vim.g.neordr_exiting = true
    if M.router_timer then M.router_timer:stop(); M.router_timer:close(); M.router_timer = nil end
    if M.job then vim.fn.jobstop(M.job) end
  end })
  if M.bridge_starting or running() then return true end
  M.bridge_starting = true
  local function launch_bridge()
    if vim.g.neordr_exiting then M.bridge_starting = false; return end
    if running() then M.bridge_starting = false; return end
    local stderr = {}
    local spawned, job = pcall(vim.fn.jobstart, { bin, '--session', session, 'serve', '--nvim', vim.v.servername }, {
      detach = true,
      on_stderr = function(_, lines) for _, line in ipairs(lines) do if line ~= '' then table.insert(stderr, line) end end end,
      on_exit = function(id, code)
        if M.job == id then M.job = nil end
        if code ~= 0 and not vim.g.neordr_exiting then
          local detail = table.concat(stderr, '\n')
          vim.schedule(function()
            if not vim.g.neordr_exiting and not running() then
              fail('Session bridge exited with code ' .. code .. (detail ~= '' and ': ' .. detail or ''))
            end
          end)
        end
      end,
    })
    M.bridge_starting = false
    if not spawned or job <= 0 then fail('Cannot start session bridge: ' .. tostring(job)); return end
    M.job = job
  end
  -- The bridge's initial RPC has a bounded timeout. Starting it inside init.lua
  -- races the rest of the user's configuration, which can occupy Neovim longer
  -- than that timeout. Configure terminal identity now, connect after VimEnter.
  if vim.v.vim_did_enter == 0 then
    vim.api.nvim_create_autocmd('VimEnter', {
      group = vim.api.nvim_create_augroup('NeordrStartup', { clear = true }),
      once = true, callback = function() vim.schedule(launch_bridge) end,
    })
  else vim.schedule(launch_bridge) end
  return true
end

---Start this editor's integration. Expected startup failures never abort init.lua.
---@return boolean success
---@return string? error
function M.setup(opts)
  if opts == nil then opts = {} end
  M.requested_options = opts
  local supported, reason = diagnostics.requirements()
  if not supported then return fail(reason) end
  commands()
  local validated, problem = diagnostics.validate(opts)
  if not validated then return fail(problem) end
  local generation = vim.g.neordr_generation or (vim.fn.getpid() .. '-' .. string.format('%x', vim.uv.hrtime()))
  local project = vim.fn.fnamemodify(vim.fn.getcwd(), ':t'):gsub('[^%w_-]', '-'):sub(1,30)
  local managed = vim.env.NEORDR_WORKSPACE_ID
  local session = (managed and managed ~= '' and managed) or opts.session or M.session or ('nvim-' .. project .. '-' .. vim.fn.getpid() .. '-' .. generation:sub(-8))
  if backend.session and backend.session ~= session then
    return fail('This editor is already registered as ' .. backend.session .. '. Restart Neovim to change session')
  end
  if (running() or M.bridge_starting) and M.bin ~= validated.bin then
    return fail('Restart Neovim to change the binary of a running bridge')
  end
  local root, root_error = diagnostics.config_root()
  if not root then return fail(root_error) end
  local endpoint = root .. '/herdr/sessions/' .. session .. '/herdr-client.sock'
  local limit = vim.uv.os_uname().sysname == 'Darwin' and 103 or 107
  if #endpoint > limit then
    return fail('Session socket path exceeds ' .. limit .. ' bytes: ' .. endpoint .. '. Set a shorter session name or XDG_CONFIG_HOME')
  end
  local ok, result, message = pcall(start, opts, validated.bin, session, generation)
  if not ok then return fail(result) end
  if not result then return false, message end
  M.last_error = nil
  return true
end

function M.prepare(buf, env) return backend.prepare(buf, env) end
return M
