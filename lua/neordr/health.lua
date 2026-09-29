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
  if not plugin.session then
    health.warn('Neordr is not configured', "Call require('neordr').setup() during startup")
    return
  end
  health.info('Session: ' .. plugin.session .. '\nNeovim socket: ' .. vim.v.servername)
  if plugin.bridge_starting then
    health.warn('Session bridge is starting; check again after editor startup')
    return
  end
  local root, root_error = diagnostics.config_root()
  if not root then health.error(root_error); return end
  local session, session_error = plugin.bridge_status()
  if not session then health.error('Session bridge: ' .. session_error)
  else health.ok('Session bridge is responding for this editor') end
  if plugin.last_error then health.error('Last setup/startup error: ' .. plugin.last_error) end
  local router, router_error = diagnostics.ping(root .. '/herdr/herdr.sock')
  if not router then health.error('Router: ' .. router_error)
  elseif router.neordr_router ~= true then health.error('Default socket belongs to another server')
  elseif router.version ~= 'neordr/' .. require('neordr.version') then
    health.warn('Router version differs: ' .. tostring(router.version), 'An older shared router may still be running')
  else health.ok('Router is responding') end
  if validated then
    health.start('Moshi and attached UIs')
    local result
    local process = vim.system({ validated.bin, '--session', plugin.session, 'doctor', '--json' },
      { text = true }, function(value) result = value end)
    -- Service editor RPC while the read-only CLI inspects the current instance.
    if not vim.wait(15000, function() return result ~= nil end, 20) then
      process:kill(15)
      health.warn('Neordr doctor timed out; run neordr doctor --json in a terminal')
      return
    end
    local ok, report = pcall(vim.json.decode, result.stdout or '')
    if result.code ~= 0 or not ok or type(report) ~= 'table' or type(report.checks) ~= 'table' then
      health.warn('Cannot read doctor report: ' .. vim.trim(result.stderr or ''))
      return
    end
    for _, check in ipairs(report.checks) do
      local log = check.status == 'ok' and health.ok or check.status == 'warn' and health.warn or health.info
      log(check.subject .. ': ' .. check.detail)
    end
    for _, editor in ipairs(report.editors or {}) do
      for _, ui in ipairs(editor.uis or {}) do
        health.info(('UI %s: %sx%s (%s)'):format(ui.channel, ui.width, ui.height, ui.client))
      end
    end
  end
end

return M
