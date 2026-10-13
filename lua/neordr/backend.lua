local M = type(package.loaded['neordr.backend']) == 'table' and package.loaded['neordr.backend']
  or { session = nil, next_pane = 0 }
M.activity_version = 1
M.agent_activity = M.agent_activity or {}
M.activity_clock = M.activity_clock or 0
M.launches = M.launches or {}
M.gesture_markers = M.gesture_markers or {}
M.gesture_results = M.gesture_results or {}
M.next_marker = M.next_marker or 0
local api = vim.api
local function array(t) return setmetatable(t or {}, vim._empty_array_mt) end
local function fail(code, message) error({ code = code, message = message }, 0) end
local function workspace() return assert(M.session) .. ':g' .. assert(vim.g.neordr_generation) .. ':w1' end
local function tab_of(buf)
  local tab = vim.b[buf].neordr_tabpage
  if tab and api.nvim_tabpage_is_valid(tab) then return tab end
  for _, win in ipairs(vim.fn.win_findbuf(buf)) do return api.nvim_win_get_tabpage(win) end
  return api.nvim_get_current_tabpage()
end

function M.register(buf)
  if not vim.b[buf].neordr_pane then
    M.next_pane = M.next_pane + 1
    vim.b[buf].neordr_pane = workspace(tab_of(buf)) .. ':p' .. M.next_pane
    vim.b[buf].neordr_tabpage = tab_of(buf)
  end
  return vim.b[buf].neordr_pane
end

function M.configure(session)
  assert(not M.session or M.session == session, 'Neovim is already registered under another Neordr session')
  M.session = session
  M.cwd = M.cwd or vim.fn.getcwd()
  local group = api.nvim_create_augroup('NeordrBackend', { clear = true })
  api.nvim_create_autocmd('TermOpen', { group = group, callback = function(ev) M.register(ev.buf) end })
  api.nvim_create_autocmd('TermRequest', { group = group, callback = function(ev)
    local seq = ev.data and ev.data.sequence or ''
    local dir = seq:match('\27%]7;file://[^/]*(/.*)')
    if dir then
      dir = dir:gsub('[\7\27\\]+$', ''):gsub('%%(%x%x)', function(h) return string.char(tonumber(h, 16)) end)
      vim.b[ev.buf].osc7_dir = dir
    end
  end })
  -- A launch allowance belongs only to an untouched, newly created shell.
  vim.on_key(function(_, typed)
    if typed and typed ~= '' then M.launches[api.nvim_get_current_buf()] = nil end
  end, api.nvim_create_namespace('NeordrLaunch'))
  vim.on_key(function(key, typed)
    return M.gesture_key(key, typed)
  end, api.nvim_create_namespace('NeordrRemoteInput'))
  api.nvim_create_autocmd({ 'TermClose', 'BufWipeout' }, { group = group,
    callback = function(ev) M.launches[ev.buf] = nil end })
  M.snapshot()
  return true
end

-- Call after creating the buffer, before jobstart(..., {term=true}).
function M.prepare(buf, env)
  assert(M.session, 'Neordr is not configured')
  -- Panes report the default session: Moshi only matches hook records whose
  -- session equals the one in its request (plain `mux=herdr` means default).
  -- Pane IDs already identify the owning editor.
  local result = vim.tbl_extend('force', {}, env or {}, {
    HERDR_ENV = '1', HERDR_SESSION = 'default', HERDR_PANE_ID = M.register(buf),
    HERDR_SOCKET_PATH = '', NEORDR_WORKSPACE_ID = '',
    TMUX = '', TMUX_PANE = '', ZELLIJ = '', ZELLIJ_SESSION_NAME = '', ZELLIJ_PANE_ID = '',
  })
  return result
end

local function pane_layout(pane)
    local win = pane.neordr.buf and vim.fn.bufwinid(pane.neordr.buf) or -1
    if M.zoom and api.nvim_win_is_valid(M.zoom.win) and api.nvim_win_get_buf(M.zoom.win) == pane.neordr.buf then
      win = M.zoom.win
    end
    local rect = { x = 0, y = 0, width = win > 0 and api.nvim_win_get_width(win) or vim.o.columns,
      height = win > 0 and api.nvim_win_get_height(win) or vim.o.lines - 2 }
    return { workspace_id = pane.workspace_id, tab_id = pane.tab_id,
      zoomed = M.zoom ~= nil and api.nvim_win_is_valid(M.zoom.win), area = rect, focused_pane_id = pane.pane_id,
      panes = array({ { pane_id = pane.pane_id, focused = true, rect = rect } }), splits = array() }
end

local function activity_screen(buf)
  local text = table.concat(api.nvim_buf_get_lines(buf, math.max(0, api.nvim_buf_line_count(buf) - 80), -1, false), '\n')
  if #text <= 65536 then return text end
  local start = #text - 65536 + 1
  -- The JSON boundary requires valid UTF-8 even when the byte cap cuts a glyph.
  while text:byte(start) and text:byte(start) >= 128 and text:byte(start) < 192 do start = start + 1 end
  return text:sub(start)
end

-- Metadata only: safe in nvim__exec_lua_fast, including modal prompts.
-- Reject observations from an older snapshot or an already superseded reader.
function M.update_activity(votes, generation)
  if generation ~= vim.g.neordr_generation then fail('stale_instance', 'Editor generation changed') end
  local result = {}
  local panes = {}
  for _, pane in ipairs(M.last_snapshot and M.last_snapshot.panes or {}) do panes[pane.pane_id] = pane end
  local function tick()
    local seconds, micros = vim.uv.gettimeofday()
    M.activity_clock = math.max(M.activity_clock + 1, seconds * 1000000 + micros)
    return M.activity_clock
  end
  for _, vote in ipairs(votes) do
    local pane = panes[vote.pane_id]
    local previous = M.agent_activity[vote.pane_id] or { status = 'unknown', seq = 0, observation = 0 }
    if pane and pane.revision == vote.revision and pane.terminal_title == vote.title
        and previous.observation == vote.observation then
      local state = vim.deepcopy(previous)
      local reset = state.identity ~= vote.identity
      if reset then state = { status = 'unknown', seq = 0, identity = vote.identity } end
      local detected = vote.status ~= '' and vote.status or state.status
      -- A completion is only a witnessed working/blocked -> idle transition,
      -- never initial discovery of an already idle process or opening history.
      if detected == 'idle' and (state.status == 'working' or state.status == 'blocked') then
        detected = pane.focused and 'idle' or 'done'
        state.completion = tick()
      elseif detected == 'idle' and state.status == 'done' and not pane.focused then
        detected = 'done'
      elseif state.status == 'done' and pane.focused and vote.status == '' then
        detected = 'idle'
      end
      if detected ~= state.status or reset then state.seq = tick() end
      state.status, state.observation = detected, tick()
      M.agent_activity[vote.pane_id] = state
      previous = state
    end
    result[vote.pane_id] = previous
  end
  return result
end

function M.snapshot()
  -- Do not call jobwait or rebuild plugin state reentrantly from a modal input
  -- loop. Keep the last normal snapshot visible until the user exits the prompt.
  if api.nvim_get_mode().blocking then
    if M.last_snapshot then return vim.deepcopy(M.last_snapshot) end
    fail('editor_busy', 'No workspace snapshot is available yet')
  end
  local panes, tabs, workspaces = array(), array(), array()
  local current_tab, current_buf = api.nvim_get_current_tabpage(), api.nvim_get_current_buf()
  local by_tab = {}
  for _, tab in ipairs(api.nvim_list_tabpages()) do by_tab[tab] = {} end
  for _, buf in ipairs(api.nvim_list_bufs()) do
    if api.nvim_buf_is_loaded(buf) and vim.bo[buf].buftype == 'terminal' then
      local channel = vim.bo[buf].channel
      if channel > 0 and vim.fn.jobwait({ channel }, 0)[1] == -1 then
        local pane = M.register(buf)
        local tab = tab_of(buf)
        vim.b[buf].neordr_tabpage = tab
        local ws = workspace(tab)
        local title = vim.b[buf].term_title or api.nvim_buf_get_name(buf)
        local row = {
          pane_id = pane, terminal_id = pane, workspace_id = ws, tab_id = pane .. ':tab',
          focused = tab == current_tab and buf == current_buf,
          cwd = vim.b[buf].osc7_dir or vim.fn.getcwd(-1, api.nvim_tabpage_get_number(tab)),
          title = title, terminal_title = title,
          label = vim.b[buf].neordr_label or (M.label_terminal and M.label_terminal(buf) or title),
          agent_status = (M.agent_activity[pane] or {}).status or 'unknown', revision = api.nvim_buf_get_changedtick(buf),
          neordr = { buf = buf, channel = channel, shell_pid = vim.fn.jobpid(channel),
            tty = api.nvim_get_chan_info(channel).pty, tabpage = tab,
            activity = M.agent_activity[pane],
            screen = activity_screen(buf) },
        }
        table.insert(by_tab[tab], row)
      end
    end
  end
  local ws, rows = workspace(), {}
  for _, tab in ipairs(api.nvim_list_tabpages()) do
    for _, row in ipairs(by_tab[tab]) do rows[#rows + 1] = row end
  end
  -- Native tabpages stay editor layout, not separate Herdr workspaces.
  if #rows == 0 then
    local pane = ws .. ':editor'
    rows[1] = { pane_id = pane, terminal_id = pane, workspace_id = ws, tab_id = pane .. ':tab',
      focused = true, cwd = M.cwd, label = 'Editor', agent_status = 'unknown', revision = 0,
      neordr = { tabpage = current_tab } }
  end
  local rank = {}
  for i, buf in ipairs(M.terminal_order or {}) do rank[buf] = i end
  table.sort(rows, function(a, b)
    local ab, bb = a.neordr.buf or 0, b.neordr.buf or 0
    local ar, br = rank[ab] or math.huge, rank[bb] or math.huge
    return ar == br and ab < bb or ar < br
  end)
  local active = rows[1]
  for _, row in ipairs(rows) do if row.focused then active = row end end
  active.focused = true
  for i, row in ipairs(rows) do
    panes[#panes + 1] = row
    tabs[#tabs + 1] = { tab_id = row.tab_id, workspace_id = ws, number = i,
      label = row.label, focused = row.focused, pane_count = 1, agent_status = row.agent_status }
  end
  workspaces[1] = { workspace_id = ws, number = 1,
    label = M.label or vim.fn.fnamemodify(M.cwd, ':t'), focused = true,
    pane_count = #rows, tab_count = #rows, active_tab_id = active.tab_id, agent_status = 'unknown' }
  local present = {}
  for _, pane in ipairs(panes) do present[pane.pane_id] = true end
  for id in pairs(M.agent_activity) do if not present[id] then M.agent_activity[id] = nil end end
  local layouts = array()
  for _, pane in ipairs(panes) do layouts[#layouts + 1] = pane_layout(pane) end
  M.last_snapshot = { panes = panes, tabs = tabs, workspaces = workspaces, layouts = layouts }
  return M.last_snapshot
end

local function find_pane(params)
  local target = params.pane_id or params.target
  for _, pane in ipairs(M.snapshot().panes) do
    if pane.pane_id == target or pane.terminal_id == target or pane.tab_id == params.tab_id then return pane end
    if not target and not params.tab_id and pane.focused then return pane end
  end
  fail('not_found', 'No live pane matches this target')
end

local function focus(pane)
  if M.unzoom then M.unzoom() end
  api.nvim_set_current_tabpage(pane.neordr.tabpage)
  local buf = pane.neordr.buf
  if not buf then return end
  if M.focus_terminal and M.focus_terminal(buf) then return end
  for _, win in ipairs(vim.fn.win_findbuf(buf)) do
    if api.nvim_win_get_tabpage(win) == pane.neordr.tabpage then
      api.nvim_set_current_win(win)
      return
    end
  end
  vim.cmd('botright split')
  api.nvim_win_set_buf(0, buf)
  api.nvim_win_set_height(0, math.min(14, math.max(3, math.floor(vim.o.lines / 3))))
end

local function find_workspace(id)
  if id == nil then return api.nvim_get_current_tabpage() end
  if workspace() == id then return api.nvim_get_current_tabpage() end
  fail('not_found', 'Unknown workspace')
end

local function label_param(params)
  if params.label == nil then return nil end
  if type(params.label) ~= 'string' or params.label:find('%c') then
    fail('invalid_params', 'label must be text without control characters')
  end
  return params.label ~= '' and params.label or nil
end

local function create(method, params)
  local label = label_param(params)
  if params.focus ~= nil and type(params.focus) ~= 'boolean' then fail('invalid_params', 'focus must be boolean') end
  local target
  if method == 'pane.split' then
    if type(params.target_pane_id) ~= 'string' then fail('invalid_params', 'target_pane_id is required') end
    if params.direction ~= 'right' and params.direction ~= 'down' then fail('invalid_params', 'direction must be right or down') end
    if params.ratio ~= nil and (type(params.ratio) ~= 'number' or params.ratio ~= params.ratio
      or params.ratio <= 0 or params.ratio >= 1) then fail('invalid_params', 'ratio must be between 0 and 1') end
    target = find_pane({ pane_id = params.target_pane_id })
    if params.workspace_id and params.workspace_id ~= target.workspace_id then fail('invalid_params', 'Target is in another workspace') end
    if M.create_terminal and not M.split_terminal then fail('invalid_request', 'Terminal plugin requires a split_terminal callback') end
  end
  local parent = target and target.neordr.tabpage or find_workspace(params.workspace_id)
  local cwd = params.cwd or (target and target.cwd) or vim.fn.getcwd(-1, api.nvim_tabpage_get_number(parent))
  if type(cwd) ~= 'string' or cwd:find('%z') or cwd == '' then fail('invalid_params', 'cwd must be a directory') end
  if cwd == '~' or cwd:sub(1, 2) == '~/' then cwd = vim.fn.expand('~') .. cwd:sub(2) end
  if cwd:sub(1, 1) ~= '/' then fail('invalid_params', 'cwd must be absolute') end
  cwd = vim.fs.normalize(cwd)
  if vim.fn.isdirectory(cwd) ~= 1 then fail('invalid_params', 'cwd is not a directory: ' .. cwd) end
  local env = params.env or {}
  if type(env) ~= 'table' then fail('invalid_params', 'env must be an object') end
  for k, v in pairs(env) do
    if type(k) ~= 'string' or not k:match('^[%a_][%w_]*$') or type(v) ~= 'string' or v:find('%z') then
      fail('invalid_params', 'env must contain environment variable names and string values')
    end
  end
  local previous = api.nvim_get_current_win()
  local previous_buf = api.nvim_get_current_buf()
  local before = {}
  for _, b in ipairs(api.nvim_list_bufs()) do before[b] = true end
  local before_wins = {}
  for _, w in ipairs(api.nvim_list_wins()) do before_wins[w] = true end
  local buf
  local ok, result = pcall(function()
    api.nvim_set_current_tabpage(parent)
    if target then
      focus(target)
      local anchor = api.nvim_get_current_win()
      if M.split_terminal then
        buf = M.split_terminal({ cwd = cwd, env = env, label = label, target_buf = target.neordr.buf,
          target_win = anchor, direction = params.direction, ratio = params.ratio or 0.5 })
      else
        local size = params.direction == 'right' and api.nvim_win_get_width(anchor) or api.nvim_win_get_height(anchor)
        vim.cmd(params.direction == 'right' and 'rightbelow vnew' or 'rightbelow new')
        local new_size = math.max(1, math.floor(size * (1 - (params.ratio or 0.5))))
        if params.direction == 'right' then api.nvim_win_set_width(0, new_size) else api.nvim_win_set_height(0, new_size) end
        buf = api.nvim_get_current_buf()
        local job = vim.fn.jobstart({ vim.o.shell }, { term = true, cwd = cwd, env = M.prepare(buf, env) })
        if job <= 0 then fail('create_failed', 'Cannot start terminal shell') end
      end
    elseif M.create_terminal then
      buf = M.create_terminal({ cwd = cwd, env = env, label = label })
    else
      vim.cmd('botright new')
      buf = api.nvim_get_current_buf()
      local job = vim.fn.jobstart({ vim.o.shell }, { term = true, cwd = cwd, env = M.prepare(buf, env) })
      if job <= 0 then fail('create_failed', 'Cannot start terminal shell') end
    end
    if type(buf) ~= 'number' or before[buf] or not api.nvim_buf_is_valid(buf)
      or vim.bo[buf].buftype ~= 'terminal' or vim.bo[buf].channel <= 0 then
      fail('create_failed', 'create_terminal must return a new running terminal buffer')
    end
    local job = vim.bo[buf].channel
    if vim.fn.jobwait({ job }, 0)[1] ~= -1 then fail('create_failed', 'Terminal shell exited') end
    vim.b[buf].neordr_label = label
    vim.b[buf].osc7_dir = cwd
    M.register(buf)
    M.launches[buf] = { job = job, expires = vim.uv.hrtime() + 30e9 }
    local snap = M.snapshot()
    local pane
    for _, p in ipairs(snap.panes) do if p.neordr.buf == buf then pane = p; break end end
    if not pane then fail('create_failed', 'Created terminal is not discoverable') end
    local tab
    for _, t in ipairs(snap.tabs) do if t.tab_id == pane.tab_id then tab = t end end
    if params.focus then focus(pane) end
    pane = vim.deepcopy(pane)
    pane.neordr = nil
    return { type = method == 'pane.split' and 'pane_info' or 'tab_created',
      pane = method == 'pane.split' and pane or nil,
      tab = tab, root_pane = pane }
  end)
  if not ok then
    -- Roll back only buffers and the tab allocated by this request.
    for _, b in ipairs(api.nvim_list_bufs()) do
      if not before[b] then
        M.launches[b] = nil
        local handled = false
        if M.close_terminal then local done, value = pcall(M.close_terminal, b); handled = done and value end
        if not handled and api.nvim_buf_is_valid(b) then pcall(api.nvim_buf_delete, b, { force = true }) end
      end
    end
    if target then
      for _, win in ipairs(api.nvim_list_wins()) do
        if not before_wins[win] then pcall(api.nvim_win_close, win, true) end
      end
    end

  end
  if (not ok or not params.focus) and api.nvim_win_is_valid(previous) then
    api.nvim_set_current_win(previous)
    if target and api.nvim_buf_is_valid(previous_buf) and api.nvim_win_get_buf(previous) ~= previous_buf then
      if not M.focus_terminal or not M.focus_terminal(previous_buf) then api.nvim_win_set_buf(previous, previous_buf) end
      if api.nvim_win_is_valid(previous) then api.nvim_set_current_win(previous) end
    end
  end
  if not ok then error(result, 0) end
  if not params.focus then
    local snap = M.snapshot()
    for _, pane in ipairs(snap.panes) do
      if pane.pane_id == result.root_pane.pane_id then
        result.root_pane.focused = pane.focused
        if result.pane then result.pane.focused = pane.focused end
        result.tab.focused = pane.focused
      end
    end

  end
  if target then return { type = 'pane_info', pane = result.root_pane } end
  return result
end

-- Herdr indexes the insertion boundary before removing the source item.
local function move_item(items, source, index)
  if type(index) ~= 'number' or index % 1 ~= 0 or index < 0 or index > #items then
    fail('invalid_params', 'insert_index must be an integer between 0 and the item count')
  end
  local destination = index + 1
  if source < destination then destination = destination - 1 end
  table.insert(items, destination, table.remove(items, source))
  return destination
end

local function move_terminal(params)
  if type(params.tab_id) ~= 'string' then fail('invalid_params', 'tab_id is required') end
  local pane = find_pane(params)
  local buffers, source = {}, nil
  for _, row in ipairs(M.snapshot().panes) do
    if row.workspace_id == pane.workspace_id and row.neordr.buf then
      buffers[#buffers + 1] = row.neordr.buf
      if row.pane_id == pane.pane_id then source = #buffers end
    end
  end
  if not source then fail('not_a_terminal', 'Editor panes cannot be reordered') end
  move_item(buffers, source, params.insert_index)
  -- Integration callbacks order terminals in the current native tab only.
  if M.reorder_terminals then
    local local_buffers = {}
    for _, buf in ipairs(buffers) do
      if tab_of(buf) == api.nvim_get_current_tabpage() then local_buffers[#local_buffers + 1] = buf end
    end
    M.reorder_terminals(local_buffers)
  end
  M.terminal_order = buffers
  vim.cmd.redrawstatus()
  return { type = 'ok' }
end

local function close_terminal(pane)
  local buf = pane.neordr.buf
  if not buf then fail('not_a_terminal', 'Editor panes cannot be closed as terminals') end
  M.launches[buf] = nil
  -- Keep an editor window alive when the terminal owns the last normal window.
  local wins = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(pane.neordr.tabpage)) do
    if api.nvim_win_get_config(win).relative == '' then wins[#wins + 1] = win end
  end
  if #wins == 1 and api.nvim_win_get_buf(wins[1]) == buf then
    api.nvim_set_current_win(wins[1])
    vim.cmd('aboveleft new')
  end
  if not M.close_terminal or not M.close_terminal(buf) then
    api.nvim_buf_delete(buf, { force = true })
  end
end

local function close(method, params)
  local field = method == 'tab.close' and 'tab_id' or 'pane_id'
  if type(params[field]) ~= 'string' then fail('invalid_params', field .. ' is required') end
  local previous = api.nvim_get_current_win()
  local pane = find_pane(params)
  if not pane.neordr.buf then fail('not_a_terminal', 'Editor panes cannot be closed as terminals') end
  M.unzoom()
  close_terminal(pane)
  if api.nvim_win_is_valid(previous) then api.nvim_set_current_win(previous) end
  M.snapshot()
  return { type = 'ok' }
end

-- Resume input after explicit remote focus, including plugin-managed terminals.
local function resume_terminal()
  if vim.bo.buftype ~= 'terminal' then return end
  -- A focus callback can call :startinsert while the previous terminal loop
  -- is still unwinding. Queue an unmapped mode transition before later input
  -- so its first character cannot become a Normal-mode command.
  local keys = api.nvim_replace_termcodes('<C-\\><C-N>i', true, false, true)
  api.nvim_feedkeys(keys, 'n', false)
end

-- Remote-only gesture actions. A floating view provides reversible zoom without
-- closing/recreating any split, disturbing plugin window IDs, or stopping jobs.
local function zoom_config()
  return { relative = 'editor', row = 0, col = 0, width = vim.o.columns,
    height = math.max(1, vim.o.lines - vim.o.cmdheight), style = 'minimal', zindex = 60 }
end

function M.unzoom(owner)
  local zoom = M.zoom
  if not zoom or (owner and zoom.owner ~= owner) then return end
  local was_current = api.nvim_get_current_win() == zoom.win
  M.zoom = nil
  if api.nvim_win_is_valid(zoom.win) then api.nvim_win_close(zoom.win, true) end
  if was_current and api.nvim_win_is_valid(zoom.source) then api.nvim_set_current_win(zoom.source) end
  resume_terminal()
end

local function close_navigator(owner)
  local nav = M.navigator
  if not nav or (owner and nav.owner ~= owner) then return end
  M.navigator = nil
  if api.nvim_win_is_valid(nav.win) then api.nvim_win_close(nav.win, true) end
  if api.nvim_buf_is_valid(nav.buf) then api.nvim_buf_delete(nav.buf, { force = true }) end
end

local function gesture(params)
  local action, owner = params.action, params.owner
  if action == 'barrier' then return { type = 'ok' } end
  if action == 'cleanup' then
    for _, pending in pairs(M.gesture_markers) do
      if pending.owner == owner then pending.cancelled = true end
    end
    M.gesture_results[owner] = nil
    close_navigator(owner)
    M.unzoom(owner)
    return { type = 'ok' }
  end
  if action == 'workspace_open' then
    close_navigator()
    local rows, index = params.workspaces or M.snapshot().workspaces, 1
    local lines = { ' Workspaces - arrows, Enter, Esc' }
    for i, ws in ipairs(rows) do
      lines[#lines + 1] = (' %d  %s'):format(i, ws.label)
      if ws.focused then index = i end
    end
    local buf = api.nvim_create_buf(false, true)
    api.nvim_buf_set_lines(buf, 0, -1, false, lines)
    local width, height = math.min(60, vim.o.columns), math.min(#lines, math.max(1, vim.o.lines - 2))
    local win = api.nvim_open_win(buf, false, { relative = 'editor', row = 0,
      col = math.max(0, math.floor((vim.o.columns - width) / 2)), width = width, height = height,
      style = 'minimal', zindex = 70, focusable = false })
    vim.wo[win].cursorline = true
    M.navigator = { owner = owner, rows = rows, index = index, buf = buf, win = win }
    api.nvim_win_set_cursor(win, { index + 1, 0 })
    return { type = 'ok' }
  end
  if action:match('^workspace_') then
    local nav = M.navigator
    if not nav or nav.owner ~= owner then return { type = 'ok' } end
    if action == 'workspace_cancel' then close_navigator(owner)
    elseif action == 'workspace_accept' then
      local ws = nav.rows[nav.index].workspace_id
      close_navigator(owner)
      M.unzoom()
      -- The host switches remote UIs across editors after this input barrier.
      return { type = 'ok', workspace_id = ws }
    else
      local delta = action == 'workspace_up' and -1 or 1
      nav.index = (nav.index - 1 + delta) % #nav.rows + 1
      if api.nvim_win_is_valid(nav.win) then api.nvim_win_set_cursor(nav.win, { nav.index + 1, 0 }) end
    end
    return { type = 'ok' }
  end
  if action == 'zoom' then
    if M.zoom and api.nvim_win_is_valid(M.zoom.win) then M.unzoom(); return { type = 'ok' } end
    local source, buf = api.nvim_get_current_win(), api.nvim_get_current_buf()
    local win = api.nvim_open_win(buf, true, zoom_config())
    M.zoom = { owner = owner, win = win, source = source }
    vim.wo[win].winhighlight = 'NormalFloat:Normal'
    resume_terminal()
    local group = api.nvim_create_augroup('NeordrZoom', { clear = true })
    api.nvim_create_autocmd('VimResized', { group = group, callback = function()
      if M.zoom and api.nvim_win_is_valid(M.zoom.win) then api.nvim_win_set_config(M.zoom.win, zoom_config()) end
    end })
    return { type = 'ok' }
  end
  if action == 'new_tab' then
    M.unzoom()
    local result = create('tab.create', { focus = true })
    resume_terminal()
    return result
  end
  if action == 'next_tab' or action == 'previous_tab' or action == 'tab' then
    local snap, panes, index = M.snapshot(), {}, 1
    local ws = workspace(api.nvim_get_current_tabpage())
    for _, pane in ipairs(snap.panes) do
      if pane.workspace_id == ws then
        panes[#panes + 1] = pane
        if pane.focused then index = #panes end
      end
    end
    local target = action == 'tab' and params.index
      or ((index - 1 + (action == 'next_tab' and 1 or -1)) % #panes + 1)
    if panes[target] then focus(panes[target]) end
  elseif action == 'next_pane' or action == 'previous_pane' then
    M.unzoom()
    local wins, current, index = {}, api.nvim_get_current_win(), 1
    for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
      if api.nvim_win_get_config(win).relative == '' then
        wins[#wins + 1] = win
        if win == current then index = #wins end
      end
    end
    local delta = action == 'next_pane' and 1 or -1
    if #wins > 0 then api.nvim_set_current_win(wins[(index - 1 + delta) % #wins + 1]) end
  elseif action == 'left' or action == 'right' or action == 'up' or action == 'down' then
    M.unzoom()
    vim.cmd('wincmd ' .. ({ left = 'h', right = 'l', up = 'k', down = 'j' })[action])
  else fail('unsupported_gesture', 'Unknown gesture: ' .. tostring(action)) end
  resume_terminal()
  return { type = 'ok' }
end

-- Put the action behind a one-use input marker in the TUI's own byte stream.
-- This is an ordering barrier: all preceding typing is consumed before focus
-- changes, and the transport waits for completion before forwarding more text.
-- Plane-15 private-use characters are not local mappings and are swallowed by
-- on_key before they can reach a buffer or terminal job.
function M.gesture_key(key, typed)
  local pending = M.gesture_markers[typed] or M.gesture_markers[key]
  if not pending then return end
  M.gesture_markers[typed], M.gesture_markers[key] = nil, nil
  if not pending.cancelled then
    vim.schedule(function()
      if pending.cancelled then return end
      local ok, result = pcall(gesture, pending)
      M.gesture_results[pending.owner] = { done = true, ok = ok,
        workspace_id = ok and result.workspace_id or nil,
        message = not ok and (type(result) == 'table' and result.message or tostring(result)) or nil }
    end)
  end
  return ''
end

local function prepare_gesture(params)
  if type(params.owner) ~= 'string' or type(params.action) ~= 'string' then
    fail('invalid_params', 'gesture requires owner and action')
  end
  for _ = 1, 65534 do
    M.next_marker = M.next_marker % 65534 + 1
    local marker = vim.fn.nr2char(0xeffff + M.next_marker)
    if not M.gesture_markers[marker] then
      M.gesture_markers[marker] = params
      M.gesture_results[params.owner] = { done = false }
      return { marker = marker }
    end
  end
  fail('busy', 'Too many outstanding remote input markers')
end

local function dispatch(method, params)
  if method == 'neordr.snapshot' then return M.snapshot() end
  if method == 'neordr.gesture' then return gesture(params) end
  if method == 'neordr.gesture_prepare' then return prepare_gesture(params) end
  if method == 'neordr.gesture_result' then return M.gesture_results[params.owner] or { done = false } end
  if method == 'tab.create' or method == 'pane.split' then return create(method, params) end
  if method == 'workspace.move' then
    find_workspace(params.workspace_id)
    if params.insert_index ~= 0 and params.insert_index ~= 1 then fail('invalid_params', 'One workspace per editor') end
    return { type = 'ok' }
  end
  if method == 'tab.move' then return move_terminal(params) end
  if method == 'tab.close' or method == 'pane.close' then return close(method, params) end
  if method == 'workspace.rename' then
    find_workspace(params.workspace_id)
    M.label = label_param(params)
    return { type = 'ok' }
  end
  if method == 'workspace.focus' then
    M.unzoom()
    find_workspace(params.workspace_id)
    resume_terminal()
    return { type = 'ok' }
  end
  local pane = find_pane(params)
  if method == 'pane.rename' or method == 'tab.rename' then
    if not pane.neordr.buf then fail('not_a_terminal', 'Editor panes cannot be renamed') end
    vim.b[pane.neordr.buf].neordr_label = label_param(params)
    vim.cmd.redrawstatus()
    return { type = 'ok' }
  end
  if method == 'neordr.launch' then
    local buf = pane.neordr.buf
    local launch = buf and M.launches[buf]
    if not launch or launch.job ~= pane.neordr.channel or vim.uv.hrtime() > launch.expires then
      fail('launch_refused', 'Only an untouched terminal created in the last 30 seconds can launch an agent')
    end
    if params.command == 'claude' or params.command == 'codex' then
      if launch.command then fail('launch_refused', 'An agent launch is already pending') end
      launch.command = params.command
    elseif params.enter == true and launch.command then
      M.launches[buf] = nil
      api.nvim_chan_send(launch.job, launch.command .. '\r')
    else
      M.launches[buf] = nil
      fail('launch_refused', 'Expected a Claude/Codex command followed by Enter')
    end
    return { type = 'ok' }
  end
  if method == 'pane.layout' then
    return { type = 'pane_layout', layout = pane_layout(pane) }
  end
  if method == 'pane.focus' or method == 'tab.focus' or method == 'agent.focus' then
    focus(pane)
    resume_terminal()
    return { type = 'ok' }
  end
  if method == 'neordr.send' then
    if not pane.neordr.channel then fail('not_a_terminal', 'Editor panes do not accept terminal input') end
    if type(params.text) ~= 'string' then fail('invalid_params', 'text must be a string') end
    M.launches[pane.neordr.buf] = nil
    api.nvim_chan_send(pane.neordr.channel, params.text)
    return { type = 'ok' }
  end
  if method == 'pane.read' then
    if params.format and params.format ~= 'text' then fail('unsupported_format', 'Only text capture is supported') end
    local source = params.source or 'visible'
    local recent = source == 'recent' or source == 'recent_unwrapped'
    if source ~= 'visible' and source ~= 'detection' and not recent then
      fail('unsupported_source', 'Use visible, detection, recent or recent_unwrapped')
    end
    local lines, truncated = {}, false
    if pane.neordr.buf then
      local count = api.nvim_buf_line_count(pane.neordr.buf)
      local rows = params.neordr_rows
      if (source == 'visible' or source == 'detection') and not rows then
        fail('screen_unavailable', 'Cannot determine the terminal screen dimensions')
      end
      -- Neovim does not expose terminal soft-wrap boundaries. Preserve rows,
      -- including hard newlines, instead of guessing from their display width.
      local limit = recent and (params.lines or 1000) or rows
      if params.lines then limit = math.min(limit, params.lines) end
      if type(limit) ~= 'number' or limit < 1 or limit % 1 ~= 0 then fail('invalid_params', 'lines must be a positive integer') end
      limit = math.min(limit, 10000)
      local start = math.max(0, count - limit)
      lines = api.nvim_buf_get_lines(pane.neordr.buf, start, -1, false)
      truncated = recent and start > 0 or (not recent and params.lines ~= nil and rows ~= nil and params.lines < rows)
    end
    return { type = 'pane_read', read = { pane_id = pane.pane_id, workspace_id = pane.workspace_id,
      tab_id = pane.tab_id, source = source, format = 'text', text = table.concat(lines, '\n'),
      revision = pane.revision, truncated = truncated } }
  end
  fail('invalid_request', 'Unsupported backend method: ' .. method)
end

function M.dispatch(method, params, generation)
  if generation ~= vim.g.neordr_generation then
    return { error = { code = 'stale_instance', message = 'Neovim instance generation changed' } }
  end
  local ok, result = pcall(dispatch, method, params)
  if ok then return { result = result } end
  if type(result) == 'table' and result.code then return { error = result } end
  return { error = { code = 'backend_error', message = tostring(result) } }
end

return M
