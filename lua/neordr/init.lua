local M = type(package.loaded['neordr']) == 'table' and package.loaded['neordr'] or {}
local backend = require('neordr.backend')

function M.setup(opts)
  opts = opts or {}
  local bin = opts.bin or vim.fn.exepath('neordr')
  assert(bin ~= '', 'neordr executable is not on PATH')
  vim.g.neordr_generation = vim.g.neordr_generation or (vim.fn.getpid() .. '-' .. string.format('%x', vim.uv.hrtime()))
  local project = vim.fn.fnamemodify(vim.fn.getcwd(), ':t'):gsub('[^%w_-]', '-'):sub(1,30)
  local session = opts.session or ('nvim-' .. project .. '-' .. vim.fn.getpid() .. '-' .. vim.g.neordr_generation:sub(-8))
  if vim.v.servername == '' then vim.fn.serverstart() end
  backend.configure(session)
  backend.focus_terminal = opts.focus_terminal
  backend.label_terminal = opts.label_terminal
  backend.create_terminal = opts.create_terminal
  backend.split_terminal = opts.split_terminal
  backend.close_terminal = opts.close_terminal
  backend.reorder_terminals = opts.reorder_terminals
  M.session, M.bin = session, bin
  vim.api.nvim_create_user_command('NeordrTerm', function(ev)
    vim.cmd('botright new')
    local env = M.prepare(vim.api.nvim_get_current_buf())
    local cmd = #ev.fargs > 0 and ev.fargs or { vim.o.shell }
    vim.fn.jobstart(cmd, { term = true, env = env })
    vim.cmd.startinsert()
  end, { nargs = '*', complete = 'shellcmd', force = true })
  vim.api.nvim_create_user_command('NeordrStatus', function()
    vim.notify('Neordr session ' .. session .. '\n' .. vim.v.servername)
  end, { force = true })
  local function ensure_router()
    if vim.g.neordr_exiting or M.router_starting then return end
    M.router_starting = true
    local errors = {}
    local job = vim.fn.jobstart({ bin, 'router', '--background' }, {
      on_stderr = function(_, lines)
        for _, line in ipairs(lines) do if line ~= '' then table.insert(errors, line) end end
      end,
      on_exit = function(_, code)
        M.router_starting = false
        if code ~= 0 and not vim.g.neordr_exiting then
          local message = table.concat(errors, '\n')
          if M.router_log ~= message then
            vim.schedule(function() vim.notify('Neordr router: ' .. message, vim.log.levels.ERROR) end)
          end
          M.router_log = message
        elseif code == 0 then M.router_log = nil end
      end,
    })
    if job <= 0 then M.router_starting = false end
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
  if M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1 then return end
  local stderr = {}
  M.job = vim.fn.jobstart({ bin, '--session', session, 'serve', '--nvim', vim.v.servername }, {
    detach = true,
    on_stderr = function(_, lines) for _, line in ipairs(lines) do if line ~= '' then table.insert(stderr, line) end end end,
    on_exit = function(_, code)
      M.job = nil
      if code ~= 0 and not vim.g.neordr_exiting then
        vim.schedule(function() vim.notify('Neordr: ' .. table.concat(stderr, '\n'), vim.log.levels.ERROR) end)
      end
    end,
  })
end

function M.prepare(buf, env) return backend.prepare(buf, env) end
return M
