-- Optional adapter: retain tiny-term's job ownership, panel registry and callbacks.
local M = {}

function M.close(buf)
  for _, term in ipairs(require('tiny-term').list()) do
    if term.buf == buf then
      local state = term.config_panel
      if state and state.ids then
        for i = #state.ids, 1, -1 do
          if state.ids[i] == term.id then table.remove(state.ids, i) end
        end
        if state.active == term.id then state.active = state.ids[1] end
      end
      term:close()
      return true
    end
  end
  return false
end

function M.reorder(buffers)
  local tiny, windows = require('tiny-term'), require('tiny-term.window')
  local rank, terms = {}, tiny.list()
  for i, buf in ipairs(buffers) do rank[buf] = i end
  local ids = {}
  for _, term in ipairs(terms) do ids[term.id] = rank[term.buf] end
  -- Reorder each panel independently; moving a tab never migrates its process.
  local function reorder(items)
    local selected, positions = {}, {}
    for i, id in ipairs(items) do
      if ids[id] then selected[#selected + 1] = id; positions[#positions + 1] = i end
    end
    table.sort(selected, function(a, b) return ids[a] < ids[b] end)
    for i, position in ipairs(positions) do items[position] = selected[i] end
  end
  for _, win in ipairs(vim.api.nvim_list_wins()) do reorder(windows.get_split_terminals(win)) end
  local seen = {}
  for _, term in ipairs(terms) do
    local state = term.config_panel
    if state and state.ids and not seen[state] then reorder(state.ids); seen[state] = true end
  end
end

return M
