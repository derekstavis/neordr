#!/usr/bin/env python3
"""Offline plugin-manager installation, startup diagnostics, and terminal lifecycle.

Uses only an isolated native package directory and a local Git fixture. It does
not install into the user's configuration or fetch a plugin from the network.
"""
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import threading

from moshi_smoke import BIN, ROOT, eventually, lua, rpc


def copy_plugin(destination):
    destination.mkdir(parents=True, exist_ok=True)
    for directory in ('lua', 'doc'):
        shutil.copytree(ROOT / directory, destination / directory)


def check_ping_failures(root, env, address):
    def probe(path):
        return json.loads(lua(env, address,
            "local result, err = require('neordr.diagnostics').ping(" +
            json.dumps(str(path)) + "); return vim.json.encode({ok=result ~= nil, error=err or false})"))

    missing = probe(root / 'absent.sock')
    assert not missing['ok'] and missing['error'], missing
    stop, accepted = threading.Event(), threading.Event()
    failures = []
    with socket.socket(socket.AF_UNIX) as listener:
        path = root / 'silent.sock'
        listener.bind(str(path))
        listener.listen(1)
        listener.settimeout(5)

        def silent_server():
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(5)
                    assert connection.recv(4096), 'Health sent no request'
                    accepted.set()
                    stop.wait(5)
            except Exception as error:
                failures.append(error)

        thread = threading.Thread(target=silent_server, daemon=True)
        thread.start()
        try:
            timed_out = probe(path)
            assert accepted.is_set() and not failures, failures
            assert not timed_out['ok'] and 'timed out' in timed_out['error'], timed_out
        finally:
            stop.set()
            thread.join(timeout=6)
        assert not thread.is_alive() and not failures, failures


def run_installation(root, method):
    env = {**os.environ, 'TERM': 'xterm-256color',
           'XDG_CONFIG_HOME': str(root / 'c'), 'XDG_DATA_HOME': str(root / 'd'),
           'XDG_STATE_HOME': str(root / 's'), 'XDG_CACHE_HOME': str(root / 'cache'),
           'XDG_RUNTIME_DIR': str(root / 'runtime'),
           'NVIM_LOG_FILE': str(root / 'nvim.log'),
           'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull}
    for key in list(env):
        if key == 'NVIM' or key == 'NVIM_LISTEN_ADDRESS' or key == 'NVIM_APPNAME' \
                or key.startswith(('HERDR_', 'NEORDR_')):
            env.pop(key, None)
    root.mkdir(parents=True, exist_ok=True)
    (root / 'runtime').mkdir(mode=0o700)
    if method == 'native':
        package = root / 'd/nvim/site/pack/test/start/neordr'
        copy_plugin(package)
        load = "vim.cmd('packadd neordr')"
    else:
        source = root / 'source'
        copy_plugin(source)
        def git(*args):
            subprocess.run(['git', '-c', 'user.name=Neordr tests', '-c',
                            'user.email=tests@example.invalid', *args], cwd=source,
                           env=env, check=True, capture_output=True, text=True)
        git('init', '--initial-branch=main')
        git('add', 'lua', 'doc')
        git('commit', '-m', 'Plugin installation fixture')
        git('tag', 'plugin-test')
        load = ("vim.pack.add({{src=" + json.dumps(source.as_uri()) +
                ", name='neordr', version='plugin-test'}}, {confirm=false})")

    missing = root / 'missing'
    nonexecutable = root / 'not-executable'
    nonexecutable.write_text('#!/bin/sh\nexit 0\n')
    nonexecutable.chmod(0o644)
    wrong = root / 'wrong-version'
    wrong.write_text('#!/bin/sh\nprintf "neordr 999.0.0\\n"\n')
    wrong.chmod(0o755)
    slow = root / 'slow-version'
    slow.write_text('#!/bin/sh\nexec sleep 10\n')
    slow.chmod(0o755)
    address = root / 'n.sock'
    report = root / 'ready.json'
    init = root / 'init.lua'
    session = 'plugin-' + method
    init.write_text(load + '\n' + r'''
local plugin = require('neordr')
local backend = require('neordr.backend')
local notices = {}
vim.g.neordr_plugin_test_notices = notices
vim.notify = function(message)
  notices[#notices + 1] = tostring(message)
  vim.g.neordr_plugin_test_notices = notices
end
local function snapshot()
  return {server=vim.v.servername, backend=backend.session or false,
    generation=vim.g.neordr_generation or false, buffers=vim.api.nvim_list_bufs(),
    autocmds=vim.api.nvim_get_autocmds({}), job=plugin.job or false,
    timer=plugin.router_timer and true or false, pending=plugin.bridge_starting or false,
    session=vim.env.HERDR_SESSION or false}
end
local baseline = snapshot()
for _, opts in ipairs({
  {bin=@BINARY@, focus_terminal=false},
  {bin=@BINARY@, session='default'},
  {bin=@BINARY@, session='../invalid'},
}) do
  local ok, err = plugin.setup(opts)
  assert(ok == false and type(err) == 'string' and #err > 0,
    'Invalid options must return false and a useful error')
  assert(vim.deep_equal(snapshot(), baseline), 'Invalid options mutated editor or bridge state')
end
for _, bin in ipairs(@FAILURES@) do
  local ok, err = plugin.setup({bin=bin, session=@SESSION@})
  assert(ok == false and type(err) == 'string' and #err > 0,
    'Invalid binary must return false and a useful error')
  assert(plugin.last_error == err, 'Startup failure must be available to diagnostics')
  assert(vim.deep_equal(snapshot(), baseline), 'Failed setup mutated editor or bridge state')
  assert(not vim.uv.fs_stat(vim.env.XDG_CONFIG_HOME .. '/herdr/herdr.sock'),
    'Failed setup started the router')
  assert(not vim.uv.fs_stat(vim.env.XDG_CONFIG_HOME .. '/herdr/sessions'),
    'Failed setup started a session bridge')
end
local reports, saved = {}, {}
for _, name in ipairs({'start', 'info', 'ok', 'warn', 'error'}) do
  saved[name] = vim.health[name]
  vim.health[name] = function(message)
    reports[#reports + 1] = {kind=name, message=tostring(message)}
  end
end
require('neordr.health').check()
for name, fn in pairs(saved) do vim.health[name] = fn end
local errors, unconfigured = false, false
for _, report in ipairs(reports) do
  errors = errors or report.kind == 'error'
  unconfigured = unconfigured or report.message:find('not configured', 1, true)
end
assert(errors and unconfigured, 'Health did not explain unsuccessful setup')
assert(vim.deep_equal(snapshot(), baseline), 'Health mutated editor or bridge state')
vim.g.plugin_init_continued = true
local ok, err = plugin.setup({bin=@BINARY@, session=@SESSION@})
assert(ok == true, err)
assert(plugin.last_error == nil, 'Successful retry did not clear startup failure')
local job = plugin.job
assert(job == nil and plugin.bridge_starting, 'Bridge must wait until init.lua finishes')
assert(plugin.setup({bin=@BINARY@, session=@SESSION@}) == true)
assert(plugin.job == nil and plugin.bridge_starting, 'Repeated setup lost pending bridge startup')
local original = vim.fn.getcwd()
vim.fn.mkdir(original .. '/changed-directory')
vim.cmd.cd(original .. '/changed-directory')
assert(plugin.setup({bin=@BINARY@}) == true, 'Setup after cd rejected the current session')
assert(plugin.session == @SESSION@ and plugin.job == job,
  'Setup after cd changed the session or replaced the running bridge')
vim.cmd.cd(original)
local configured = snapshot()
local switched, switch_error = plugin.setup({bin=@BINARY@, session='other-session'})
assert(switched == false and type(switch_error) == 'string', 'Changing a live session must be rejected')
assert(vim.deep_equal(snapshot(), configured), 'Rejected session change mutated the running bridge')
assert(plugin.session == @SESSION@ and plugin.bridge_starting and plugin.job == nil)
assert(plugin.setup({bin=@BINARY@}) == true, 'Setup could not recover from rejected session change')
-- Simulate another plugin's synchronous initialization after Neordr setup.
-- This exceeds the bridge's ordinary 500ms editor RPC deadline.
vim.uv.sleep(1000)
vim.fn.serverstart(@ADDRESS@)
vim.api.nvim_create_autocmd('VimEnter', {once=true, callback=function()
  vim.schedule(function()
    local started = plugin.job
    assert(type(started) == 'number' and started > 0 and not plugin.bridge_starting,
      'Bridge did not launch after VimEnter')
    vim.fn.writefile({vim.json.encode({job=started, notices=notices,
      session=plugin.session, version=require('neordr.version'),
      continued=vim.g.plugin_init_continued})}, @REPORT@)
  end)
end})
'''.replace('@FAILURES@', '{' + ','.join(json.dumps(str(path)) for path in (missing, nonexecutable, wrong, slow)) + '}')
       .replace('@SESSION@', json.dumps(session))
       .replace('@BINARY@', json.dumps(str(BIN)))
       .replace('@ADDRESS@', json.dumps(str(address)))
       .replace('@REPORT@', json.dumps(str(report))))
    editor = None
    router_pids = set()
    router = root / 'c/herdr/herdr.sock'
    endpoint = root / 'c/herdr/sessions' / session / 'herdr.sock'
    with open(root / 'startup.log', 'w') as log:
        try:
            editor = subprocess.Popen(['nvim', '--headless', '-u', str(init), '-i', 'NONE'],
                cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            eventually(report.exists, seconds=30)
            startup = json.loads(report.read_text())
            assert startup['continued'] and startup['session'] == session, startup
            package = (ROOT / 'Cargo.toml').read_text().split('[package]', 1)[1].split('\n[', 1)[0]
            rust_version = re.search(r'(?m)^version\s*=\s*"([^"]+)"', package).group(1)
            assert startup['version'] == rust_version, (startup['version'], rust_version)
            def ping_router():
                result = rpc(router, 'ping')
                assert result['neordr_router'], result
                router_pids.add(result['pid'])
                return True
            eventually(ping_router)
            eventually(lambda: rpc(endpoint, 'ping'))
            live = json.loads(lua(env, address,
                "local p=require('neordr'); local job=p.job; local session=p.session; "
                "local ok, err=p.setup({bin=" + json.dumps(str(BIN)) +
                ",session='other-session'}); assert(ok == false and err); "
                "assert(p.job == job and p.session == session and vim.fn.jobwait({job}, 0)[1] == -1); "
                "assert(p.setup({bin=" + json.dumps(str(BIN)) + "})); "
                "return vim.json.encode({job=p.job, session=p.session})"))
            assert live == {'job': startup['job'], 'session': session}, live
            check_ping_failures(root, env, address)

            captured = root / 'terminal-env.json'
            helper = root / 'terminal.py'
            helper.write_text('import json, os, time\nfrom pathlib import Path\n'
                f'Path({str(captured)!r}).write_text(json.dumps({{key: os.environ.get(key) '
                'for key in ("HERDR_ENV", "HERDR_SESSION", "HERDR_PANE_ID")}))\n'
                'time.sleep(120)\n')
            state = json.loads(lua(env, address,
                "vim.api.nvim_cmd({cmd='NeordrTerm', args={'python3'," +
                json.dumps(str(helper)) + "}}, {}); "
                "return vim.json.encode({kind=vim.bo.buftype, pane=vim.b.neordr_pane, "
                "channel=vim.bo.channel})"))
            eventually(captured.exists)
            terminal_env = json.loads(captured.read_text())
            assert state['kind'] == 'terminal' and state['channel'] > 0, state
            assert terminal_env['HERDR_SESSION'] == 'default', terminal_env
            assert terminal_env['HERDR_ENV'] == '1', terminal_env
            assert terminal_env['HERDR_PANE_ID'] == state['pane'], state
            panes = rpc(endpoint, 'pane.list')['panes']
            assert any(pane['pane_id'] == state['pane'] for pane in panes), panes

            # Exercise the real :checkhealth renderer and public help entry.
            health = lua(env, address, "vim.cmd('checkhealth neordr'); "
                "return table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\\n')")
            assert 'neordr' in health.lower() and 'OK' in health, health
            for label in ('Neovim 0.12+', 'Binary:', 'Session bridge', 'Router'):
                assert label in health, health
            assert 'ERROR' not in health, health

            # Diagnostics must report a missing router, never repair it as a
            # side effect. Stop this editor's recovery timer before the check.
            lua(env, address, "require('neordr').router_timer:stop(); return true")
            eventually(lambda: lua(env, address,
                "return require('neordr').router_starting == false") == 'true')
            old_router = rpc(router, 'ping')['pid']
            os.kill(old_router, signal.SIGTERM)

            def router_stopped():
                try:
                    rpc(router, 'ping')
                    return False
                except OSError:
                    return True

            eventually(router_stopped)
            unhealthy = lua(env, address, "vim.cmd('checkhealth neordr'); "
                "return table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\\n')")
            assert 'ERROR' in unhealthy and 'Router:' in unhealthy, unhealthy
            assert router_stopped(), 'Health restarted a stopped router'
            lua(env, address, "assert(require('neordr').setup({bin=" +
                json.dumps(str(BIN)) + "})); return true")
            eventually(ping_router)
            assert rpc(router, 'ping')['pid'] != old_router
            assert lua(env, address, "return require('neordr').job") == str(startup['job'])
            assert lua(env, address, "return require('neordr').router_timer:is_active()") == 'true'

            help_state = json.loads(lua(env, address,
                "local path=vim.api.nvim_get_runtime_file('doc/neordr.txt', false)[1]; "
                "assert(path, 'Plugin help is missing'); "
                "vim.cmd('helptags ' .. vim.fn.fnameescape(vim.fn.fnamemodify(path, ':h'))); "
                "vim.cmd('help neordr'); "
                "return vim.json.encode({kind=vim.bo.buftype, file=vim.api.nvim_buf_get_name(0)})"))
            assert help_state['kind'] == 'help', help_state
            assert Path(help_state['file']).name == 'neordr.txt', help_state
            lua(env, address, "vim.schedule(function() vim.cmd('qa!') end); return true")
            assert editor.wait(timeout=10) == 0
            eventually(lambda: not endpoint.exists())
        except Exception as error:
            diagnostics = {'editor_exit': editor.poll() if editor is not None else None}
            if report.exists():
                diagnostics['startup_report'] = json.loads(report.read_text())
            if address.exists():
                try:
                    diagnostics['plugin'] = json.loads(lua(env, address,
                        "local p=require('neordr'); return vim.json.encode({"
                        "last_error=p.last_error or false, job=p.job or false, "
                        "job_state=p.job and vim.fn.jobwait({p.job}, 0)[1] or false, "
                        "servername=vim.v.servername, session=p.session or false, "
                        "router_log=p.router_log or false, router_starting=p.router_starting, "
                        "notices=vim.g.neordr_plugin_test_notices or {}})"))
                except Exception as diagnostic_error:
                    diagnostics['snapshot_error'] = str(diagnostic_error)
            raise AssertionError(f'{method} plugin installation failed: {error}\n'
                                 + json.dumps(diagnostics, indent=2) + '\n'
                                 + (root / 'startup.log').read_text()) from error
        finally:
            if editor is not None and editor.poll() is None:
                editor.terminate()
                try:
                    editor.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    editor.kill()
                    editor.wait()
            # A startup failure may precede the normal readiness check.
            try:
                result = rpc(router, 'ping')
                if result.get('neordr_router'):
                    router_pids.add(result['pid'])
            except (OSError, ValueError):
                pass
            for pid in router_pids:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass


def main():
    for method in ('native', 'vim-pack'):
        with tempfile.TemporaryDirectory(prefix='np-', dir='/tmp') as directory:
            run_installation(Path(directory).resolve(), method)
    print('Native package and vim.pack installation, startup recovery, health, help, and terminal lifecycle: PASS')


if __name__ == '__main__':
    main()
