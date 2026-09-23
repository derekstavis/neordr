local M = {}

function M.check()
  local health, diagnostics = vim.health, require('neordr.diagnostics')
  health.start('Neordr')
  health.info('Lua version: ' .. require('neordr.version'))
  local supported, reason = diagnostics.requirements()
  if not supported then health.error(reason); return end
  health.ok('Neovim 0.12+ on ' .. vim.uv.os_uname().sysname)
  local plugin = require('neordr')
  local validated, problem = diagnostics.validate(plugin.options or plugin.requested_options or {})
  if validated then health.ok('Binary: ' .. validated.bin .. ' (' .. validated.version .. ')')
  else health.error(problem, 'See :help neordr-installation') end
  if plugin.last_error then health.error('Last setup/startup error: ' .. plugin.last_error) end
  if not plugin.session then
    health.warn('Neordr is not configured', "Call require('neordr').setup() during startup")
    return
  end
  health.info('Session: ' .. plugin.session .. '\nNeovim socket: ' .. vim.v.servername)
  if not plugin.job or vim.fn.jobwait({ plugin.job }, 0)[1] ~= -1 then
    health.error('Session bridge job is not running', 'Retry setup with your original options after correcting the error')
  end
  local root, root_error = diagnostics.config_root()
  if not root then health.error(root_error); return end
  local session, session_error = diagnostics.ping(root .. '/herdr/sessions/' .. plugin.session .. '/herdr.sock')
  if not session then health.error('Session bridge: ' .. session_error)
  elseif not session.neordr or session.neordr.pid ~= vim.fn.getpid()
      or session.neordr.generation ~= vim.g.neordr_generation then
    health.error('Session bridge belongs to another Neovim instance')
  elseif session.version ~= 'neordr/' .. require('neordr.version') then
    health.error('Session bridge version mismatch: ' .. tostring(session.version), 'Restart this editor after upgrading')
  else health.ok('Session bridge is responding for this editor') end
  local router, router_error = diagnostics.ping(root .. '/herdr/herdr.sock')
  if not router then health.error('Router: ' .. router_error)
  elseif router.neordr_router ~= true then health.error('Default socket belongs to another server')
  elseif router.version ~= 'neordr/' .. require('neordr.version') then
    health.warn('Router version differs: ' .. tostring(router.version), 'An older shared router may still be running')
  else health.ok('Router is responding') end
end

return M
