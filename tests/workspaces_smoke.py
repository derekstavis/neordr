#!/usr/bin/env python3
"""Process-per-workspace lifecycle and cross-editor native Herdr attachment."""
import json
import fcntl
import pty
import struct
import termios
import os
from pathlib import Path
import queue
import signal
import subprocess
import tempfile
import threading
import time
from moshi_smoke import BIN, ROOT, eventually, lua, rpc


class Client:
    def __init__(self, socket, env, log):
        fixture = BIN.parent / 'examples/herdr_contract_fixture'
        self.process = subprocess.Popen([str(fixture), str(socket)], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True)
        self.messages = queue.Queue()
        self.counter = 0
        def drain():
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        threading.Thread(target=drain, daemon=True).start()
        self.send({'EndpointControl': {'kind': 'endpoint.hello.v1', 'data': json.dumps(dict(
            generation=1, cell_width_px=0, cell_height_px=0, surface_size=dict(cols=95, rows=30),
            pixel_mouse=False, direct_graphics=False, endpoint_keybindings=False, mouse_capture=False,
            snapshot_codecs=['shell.snapshot.v1'], surface_codecs=['shell.surface.v1'],
            input_codecs=['shell.input.semantic.v1'], blob_codecs=['shell.blob.v1']))}})
        self.boot = self.wait(lambda m: json.loads(m['EndpointControl']['data'])
            if m.get('EndpointControl', {}).get('kind') == 'shell.snapshot.v1' else None)['boot_id']

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + '\n')
        self.process.stdin.flush()

    def wait(self, predicate):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            message = self.messages.get(timeout=max(.01, deadline-time.monotonic()))
            result = predicate(message)
            if result: return result
        raise AssertionError('Native endpoint did not respond')

    def request(self, method, params=None, error=False):
        self.counter += 1
        request_id = str(self.counter)
        self.send({'ClientShellEndpointRequest': {'boot_id': self.boot, 'request': json.dumps(
            dict(id=request_id, method=method, params=params or {}))}})
        chunk = self.wait(lambda m: m.get('ClientShellEndpointResponseChunk')
            if m.get('ClientShellEndpointResponseChunk', {}).get('request_id') == request_id else None)
        reply = json.loads(bytes(chunk['data']))
        if error: return reply['error']
        assert 'error' not in reply, reply
        return reply['result']

    def close(self):
        if self.process.poll() is None:
            self.send('Detach')
            try: self.process.wait(timeout=5)
            except subprocess.TimeoutExpired: self.process.terminate(); self.process.wait(timeout=5)


def main():
    with tempfile.TemporaryDirectory(prefix='nw-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        config = root/'c'
        (config/'nvim').mkdir(parents=True)
        (config/'nvim/init.lua').write_text("vim.g.workspace_fixture=true; vim.o.shell='/bin/sh'; vim.o.shadafile='NONE'\n"
            + f"vim.opt.runtimepath:prepend({json.dumps(str(ROOT))}); require('neordr').setup({{bin={json.dumps(str(BIN))}}})\n")
        env = {k:v for k,v in os.environ.items() if not k.startswith('HERDR_') and k not in ['NVIM','NEORDR_WORKSPACE_ID']}
        env.update(XDG_CONFIG_HOME=str(config), NVIM_LOG_FILE=str(root/'nvim.log'), TERM='xterm-256color')
        router = config/'herdr/herdr.sock'
        children, editors, client = [], [], None
        log = open(root/'log', 'w')
        def start():
            child = subprocess.Popen([str(BIN), 'router'], env=env, stdout=log, stderr=log)
            children.append(child)
            eventually(lambda: rpc(router, 'ping'))
            return child
        def editor(workspace):
            key = workspace['workspace']['workspace_id'].split(':')[0]
            endpoint = config/'herdr/sessions'/key/'herdr.sock'
            info = rpc(endpoint, 'ping')
            editors.append(info['neordr'])
            return info['neordr'], endpoint, info['pid']
        def evaluate(info, code): return lua(env, info['socket'], code)
        def session_list():
            return json.loads(subprocess.check_output([str(BIN), 'session', 'list', '--json'], env=env))['sessions']
        try:
            server = start()
            assert [s['name'] for s in session_list()] == ['default']
            client = Client(router.with_name('herdr-client.sock'), env, log)
            assert not client.request('session.snapshot')['snapshot']['workspaces']
            # Creation through the interactive endpoint also works in an empty session.
            a = client.request('workspace.create', {'cwd': str(root), 'label': 'First', 'focus': True,
                'env': {'NEORDR_PROBE': 'literal value'}})
            ia, ea, bridge_a = editor(a)
            assert evaluate(ia, 'return vim.g.workspace_fixture') == 'true'
            assert evaluate(ia, 'return vim.env.NEORDR_PROBE') == 'literal value'
            eventually(lambda: evaluate(ia, 'return #vim.api.nvim_list_uis()') == '1')
            b = client.request('workspace.create', {'cwd': str(root), 'label': 'Second', 'focus': False})
            ib, eb, _ = editor(b)
            assert ia['pid'] != ib['pid'] and ia['cwd'] == ib['cwd'] == str(root)
            assert evaluate(ia, 'return #vim.api.nvim_list_uis()') == '1'
            assert evaluate(ib, 'return #vim.api.nvim_list_uis()') == '0'
            client.request('workspace.focus', {'workspace_id': b['workspace']['workspace_id']})
            eventually(lambda: evaluate(ib, 'return #vim.api.nvim_list_uis()') == '1')
            eventually(lambda: evaluate(ia, 'return #vim.api.nvim_list_uis()') == '0')
            # A stale pane ID must not be delivered after changing editors.
            marker = root/'input'
            client.send({'ClientShellPaneInput': {'pane_id': a['root_pane']['pane_id'],
                'events': [{'TextCommit': 'printf wrong > '+str(marker)+'\r'}]}})
            client.send({'ClientShellPaneInput': {'pane_id': b['root_pane']['pane_id'],
                'events': [{'TextCommit': 'printf right > '+str(marker)+'\r'}]}})
            try:
                eventually(lambda: marker.exists() and marker.read_text() == 'right')
            except AssertionError:
                print('Input failure:', evaluate(ib, "return vim.json.encode({mode=vim.api.nvim_get_mode(),buf=vim.api.nvim_get_current_buf(),snapshot=require('neordr.backend').snapshot()})")); print('file:',marker.read_text() if marker.exists() else 'missing'); raise
            # Native tabpages keep their directories and are not Herdr workspaces.
            evaluate(ib, "vim.cmd('stopinsert'); vim.cmd('tabnew'); return true")
            eventually(lambda: evaluate(ib, 'return vim.api.nvim_get_mode().mode') == 'n')
            client.send({'ClientShellPaneInput': {'pane_id': b['root_pane']['pane_id'],
                'events': [{'TextCommit': ":lua vim.g.native_tab_input='ok'\r"}]}})
            eventually(lambda: evaluate(ib, 'return vim.g.native_tab_input') == 'ok')
            terminal = client.request('tab.create', {'workspace_id': b['workspace']['workspace_id'], 'focus': True})
            assert len(rpc(eb, 'workspace.list')['workspaces']) == 1
            assert len(rpc(eb, 'pane.list')['panes']) == 2
            client.request('pane.focus', {'pane_id': b['root_pane']['pane_id']})
            assert evaluate(ib, 'return vim.api.nvim_tabpage_get_number(0)') == '1'
            client.request('pane.focus', {'pane_id': terminal['root_pane']['pane_id']})
            assert evaluate(ib, 'return vim.api.nvim_tabpage_get_number(0)') == '2'
            hidden = evaluate(ib, "local b=vim.api.nvim_create_buf(true,false); vim.api.nvim_buf_set_lines(b,0,-1,false,{'unsaved'}); return b")
            error = client.request('workspace.close', {'workspace_id': b['workspace']['workspace_id']}, error=True)
            assert error['code'] == 'modified_buffer', error
            assert evaluate(ib, 'return vim.fn.jobwait({vim.bo.channel},0)[1]') == '-1'
            evaluate(ib, f'vim.bo[{hidden}].modified=false; return true')
            client.close(); client = None
            eventually(lambda: evaluate(ib, 'return #vim.api.nvim_list_uis()') == '0')
            # Both the creating session server and a bridge may crash independently.
            for info in [ia, ib]:
                evaluate(info, "require('neordr').router_timer:stop(); return true")
            server.kill(); server.wait(timeout=5)
            os.kill(bridge_a, signal.SIGKILL)
            assert evaluate(ia, 'return vim.fn.getpid()') == str(ia['pid'])
            assert evaluate(ib, 'return vim.fn.getpid()') == str(ib['pid'])
            server = start()
            eventually(lambda: len(rpc(router, 'workspace.list')['workspaces']) == 2, seconds=15)
            assert rpc(ea, 'ping')['pid'] != bridge_a
            client = Client(router.with_name('herdr-client.sock'), env, log)
            client.request('workspace.close', {'workspace_id': b['workspace']['workspace_id']})
            eventually(lambda: not Path(ib['socket']).exists())
            client.request('workspace.close', {'workspace_id': a['workspace']['workspace_id']})
            eventually(lambda: not Path(ia['socket']).exists())
            assert not client.request('session.snapshot')['snapshot']['workspaces']
            assert [s['name'] for s in session_list()] == ['default']
            c = client.request('workspace.create', {'cwd': str(root), 'focus': True})
            ic, _, _ = editor(c)
            eventually(lambda: evaluate(ic, 'return #vim.api.nvim_list_uis()') == '1')
            client.request('workspace.close', {'workspace_id': c['workspace']['workspace_id']})
            assert not client.request('session.snapshot')['snapshot']['workspaces']
            # Discover a normal local TUI with no plugin and no explicit bridge.
            # An empty inherited managed key must not suppress discovery.
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
            address_file = root/'local-address'
            local = subprocess.Popen(['nvim', '-u', 'NONE', '-i', 'NONE', '-c',
                f'lua vim.fn.writefile({{vim.v.servername}}, {json.dumps(str(address_file))})'],
                cwd=root, env={**env, 'NEORDR_WORKSPACE_ID': ''}, stdin=slave, stdout=slave,
                stderr=slave, start_new_session=True)
            children.append(local)
            def drain_local():
                try:
                    while os.read(master, 65536): pass
                except OSError: pass
            threading.Thread(target=drain_local, daemon=True).start()
            try:
                eventually(address_file.exists)
                local_socket = address_file.read_text().strip()
                ws = eventually(lambda: rpc(router, 'workspace.list')['workspaces'], seconds=15)[0]
                info = rpc(config/'herdr/sessions'/ws['workspace_id'].split(':')[0]/'herdr.sock', 'ping')['neordr']
                editors.append(info)
                # Neovim's normal TUI can supervise a separate --embed editor.
                assert Path(info['socket']).resolve() == Path(local_socket).resolve()
                assert str(info['pid']) == lua(env, local_socket, 'return vim.fn.getpid()')
                eventually(lambda: lua(env, local_socket, 'return #vim.api.nvim_list_uis()') == '2')
                client.request('workspace.close', {'workspace_id': ws['workspace_id']})
                assert local.wait(timeout=5) == 0
            finally:
                os.close(slave); os.close(master)
            print('Workspace processes, config/env, native tabs, input isolation, dirty-buffer refusal, crash recovery, close/recreate, local TUI discovery/close: PASS')
        except Exception:
            log.flush(); print((root/'log').read_text()[-7000:]); raise
        finally:
            if client: client.close()
            for info in editors:
                if Path(info['socket']).exists():
                    try: os.kill(info['pid'], signal.SIGTERM)
                    except ProcessLookupError: pass
            for child in reversed(children):
                if child.poll() is None: child.terminate(); child.wait(timeout=5)
            log.close()

if __name__ == '__main__': main()
