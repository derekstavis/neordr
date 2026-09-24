#!/usr/bin/env python3
"""Real remote PTY gestures, input isolation, reversible zoom, resize and cleanup.

Set NEORDR_NVIM_CONFIG to exercise a disposable copy of a full Neovim config.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from moshi_smoke import BIN, eventually, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-gestures-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root/'config'), 'TERM': 'xterm-256color',
               'NVIM_LOG_FILE': str(root/'nvim.log')}
        for key in ['NVIM', 'HERDR_SESSION', 'HERDR_ENV']: env.pop(key, None)
        full_config = os.environ.get('NEORDR_NVIM_CONFIG')
        if full_config:
            shutil.copytree(full_config, root/'config/nvim', ignore=shutil.ignore_patterns('.git', '.codex', '.agents'))
            config = root/'config/nvim/lua/config/neordr.lua'
            config.write_text(config.read_text().replace('bin = bin,', 'bin = ' + json.dumps(str(BIN)) + ','))
        address = root/'nvim.sock'
        children, master, slave, reader = [], None, None, None
        second_ws = None
        router = root/'config/herdr/herdr.sock'
        stop_reader = threading.Event()
        log = open(root/'log', 'w')
        try:
            children.append(subprocess.Popen([str(BIN), 'router'], env=env, stdout=log, stderr=log))
            eventually(lambda: (root/'config/herdr/herdr.sock').exists())
            editor = subprocess.Popen(['nvim', '--headless', '-i', 'NONE', '--listen', str(address)] +
                ([] if full_config else ['-u', 'NONE']), env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            children.append(editor)
            eventually(address.exists)
            if full_config:
                name = eventually(lambda: lua(env, address, "return require('neordr').session"))
            else:
                name = 'gestures'
                children.append(subprocess.Popen([str(BIN), '--session', name, 'serve', '--nvim', str(address)],
                                                  env=env, stdout=log, stderr=log))
            endpoint = root/'config/herdr/sessions'/name/'herdr.sock'
            eventually(lambda: endpoint.exists() and rpc(endpoint, 'ping'))
            def evaluate(code): return lua(env, address, code)
            original = evaluate("vim.api.nvim_buf_set_lines(0,0,-1,false,{'unsaved editor text'}); return vim.api.nvim_get_current_buf()")
            ws = rpc(endpoint, 'workspace.list')['workspaces'][0]['workspace_id']
            a = rpc(endpoint, 'tab.create', {'workspace_id': ws, 'label': 'First', 'focus': True})['root_pane']
            b = rpc(endpoint, 'tab.create', {'workspace_id': ws, 'label': 'Second', 'focus': True})['root_pane']
            second_ws = rpc(router, 'workspace.create', {'label': 'Other workspace', 'focus': True})['workspace']['workspace_id']
            rpc(router, 'workspace.move', {'workspace_id': ws, 'insert_index': 0})
            rpc(router, 'pane.focus', {'pane_id': a['pane_id']})
            identities = json.loads(evaluate("local r={}; for _,p in ipairs(require('neordr.backend').snapshot().panes) do "
                "if p.neordr.channel then r[p.pane_id]=vim.fn.jobpid(p.neordr.channel) end end; return vim.json.encode(r)"))
            mapping = evaluate("return vim.fn.sha256(vim.inspect({vim.api.nvim_get_keymap('n'),vim.api.nvim_get_keymap('t')}))")
            master, slave = pty.openpty()
            size = lambda rows, cols: fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
            size(32, 100)
            old_modes = termios.tcgetattr(slave)
            def restored_modes():
                actual = termios.tcgetattr(slave)
                # macOS marks pending canonical input when leaving raw mode.
                actual[3] &= ~getattr(termios, 'PENDIN', 0)
                expected = old_modes.copy()
                expected[3] &= ~getattr(termios, 'PENDIN', 0)
                return actual == expected
            def attach():
                child = subprocess.Popen([str(BIN), '--session', 'default'], env=env, stdin=slave,
                    stdout=slave, stderr=slave, start_new_session=True)
                children.append(child)
                eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '1')
                return child
            output = bytearray()
            def drain():
                try:
                    while not stop_reader.is_set():
                        if not select.select([master], [], [], .1)[0]: continue
                        data = os.read(master, 65536)
                        if not data: return
                        output.extend(data)
                except OSError: pass
            reader = threading.Thread(target=drain, daemon=True)
            reader.start()
            child = attach()
            def send(data): os.write(master, data)
            def focused(pane):
                return next(p['pane_id'] for p in rpc(endpoint, 'pane.list')['panes'] if p['focused']) == pane['pane_id']
            send(b'\x02n')
            eventually(lambda: focused(b))
            send(b'\x02p')
            eventually(lambda: focused(a))
            # A chord split over transport reads must still be recognized.
            send(b'\x02'); time.sleep(.06); send(b'2')
            eventually(lambda: focused(b))
            send(b'\x021')
            eventually(lambda: focused(a))
            source = evaluate('return vim.api.nvim_get_current_win()')
            layout = evaluate('return vim.json.encode(vim.fn.winlayout())')
            send(b'\x02z')
            eventually(lambda: evaluate("return require('neordr.backend').zoom ~= nil") == 'true')
            assert evaluate('return vim.api.nvim_win_get_width(0)') == '100'
            assert rpc(endpoint, 'pane.layout', {'pane_id': a['pane_id']})['layout']['zoomed']
            size(25, 75)
            eventually(lambda: evaluate('return vim.o.columns') == '75')
            eventually(lambda: evaluate('return vim.api.nvim_win_get_width(0)') == '75')
            send(b'\x02z')
            eventually(lambda: evaluate("return require('neordr.backend').zoom == nil") == 'true')
            assert evaluate('return vim.api.nvim_get_current_win()') == source
            assert evaluate('return vim.json.encode(vim.fn.winlayout())') == layout
            # Two-finger swipes cycle visible Neovim splits, including the editor.
            send(b'\x02\t')
            eventually(lambda: evaluate('return vim.api.nvim_get_current_win()') != source)
            send(b'\x02\x1b[Z')
            eventually(lambda: evaluate('return vim.api.nvim_get_current_win()') == source)
            # Wait for the navigator, then send the complete arrow sequence.
            # Decoder unit tests cover every read boundary deterministically;
            # a scheduled sleep can exceed the lone-Escape timeout on CI.
            send(b'\x02w')
            eventually(lambda: evaluate("return require('neordr.backend').navigator ~= nil") == 'true')
            send(b'\x1b[B\r')
            eventually(lambda: rpc(router, 'session.snapshot')['snapshot']['focused_workspace_id'] == second_ws)
            send(b'\x02w\x1b[A\r')
            eventually(lambda: rpc(router, 'session.snapshot')['snapshot']['focused_workspace_id'] == ws)
            send(b'\x02w\x1b[B\x1b')
            eventually(lambda: evaluate("return require('neordr.backend').navigator == nil") == 'true')
            assert rpc(router, 'session.snapshot')['snapshot']['focused_workspace_id'] == ws
            # No control prefix may reach the terminal job; bracketed paste is literal.
            capture, ready = root/'input', root/'ready'
            receiver = root/'receiver.py'
            receiver.write_text("import os,tty\nfrom pathlib import Path\ntty.setraw(0)\n"
                "os.write(1,b'\\x1b[?2004h')\n" + f"Path({str(ready)!r}).touch()\n" +
                f"with open({str(capture)!r},'ab',buffering=0) as f:\n while True: f.write(os.read(0,65536))\n")
            rpc(endpoint, 'pane.focus', {'pane_id': a['pane_id']})
            command = 'exec ' + shlex.quote(sys.executable) + ' ' + shlex.quote(str(receiver)) + '\r'
            rpc(endpoint, 'pane.send_text', {'pane_id': a['pane_id'], 'text': command})
            eventually(ready.exists)
            capture_b, ready_b = root/'input-b', root/'ready-b'
            receiver_b = root/'receiver-b.py'
            receiver_b.write_text(receiver.read_text().replace(str(ready), str(ready_b)).replace(str(capture), str(capture_b)))
            rpc(endpoint, 'pane.send_text', {'pane_id': b['pane_id'], 'text':
                'exec ' + shlex.quote(sys.executable) + ' ' + shlex.quote(str(receiver_b)) + '\r'})
            eventually(ready_b.exists)
            # Sidebar RPC focus must resume terminal input, including when the
            # previous terminal loop is unwinding or the user was in Normal mode.
            sidebar_a, sidebar_b = b'', b''
            for index, method in enumerate(['pane.focus', 'tab.focus', 'agent.focus']):
                send(b'\x1c\x0e')
                eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode').startswith('n'))
                for pane, capture_file in [(b, capture_b), (a, capture)]:
                    rpc(endpoint, method, {'pane_id': pane['pane_id']})
                    eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode') == 't')
                    payload = f'sidebar-{index}-'.encode()
                    send(payload)
                    if pane == a:
                        sidebar_a += payload
                        expected = sidebar_a
                    else:
                        sidebar_b += payload
                        expected = sidebar_b
                    eventually(lambda: capture_file.read_bytes() == expected)
            # Typing immediately adjacent to a swipe must keep its original target.
            send(b'before-swipe\x02nafter-swipe')
            eventually(lambda: capture.exists() and b'before-swipe' in capture.read_bytes())
            eventually(lambda: capture_b.exists() and b'after-swipe' in capture_b.read_bytes())
            assert b'after-swipe' not in capture.read_bytes()
            assert b'before-swipe' not in capture_b.read_bytes()
            send(b'\x02p')
            eventually(lambda: focused(a))
            burst = b''.join(f'A{i:02d}\x02nB{i:02d}\x02p'.encode() for i in range(20))
            send(burst)
            expected_a = sidebar_a + b'before-swipe' + b''.join(f'A{i:02d}'.encode() for i in range(20))
            expected_b = sidebar_b + b'after-swipe' + b''.join(f'B{i:02d}'.encode() for i in range(20))
            eventually(lambda: capture.read_bytes() == expected_a and capture_b.read_bytes() == expected_b)
            send(b'ordinary-input')
            eventually(lambda: capture.exists() and b'ordinary-input' in capture.read_bytes())
            send(b'\x02z\x02z')
            paste = b'PASTE-\x02q-\x02z-\x02w-END'
            send(b'\x1b[200~' + paste + b'\x1b[201~')
            eventually(lambda: paste in capture.read_bytes())
            assert evaluate("return require('neordr.backend').zoom == nil") == 'true'
            assert child.poll() is None
            send(b'\x02\x02')
            eventually(lambda: capture.read_bytes().endswith(b'\x02'))
            assert capture.read_bytes().startswith(expected_a + b'ordinary-input'), capture.read_bytes()
            # The shortcut panel's new-tab action uses the same terminal adapter.
            before_create = len(rpc(endpoint, 'pane.list')['panes'])
            send(b'\x02c')
            eventually(lambda: len(rpc(endpoint, 'pane.list')['panes']) == before_create + 1)
            rpc(endpoint, 'pane.focus', {'pane_id': a['pane_id']})
            layout = evaluate('return vim.json.encode(vim.fn.winlayout())')
            # Detach while zoomed restores original splits and only closes this UI.
            send(b'\x02z')
            eventually(lambda: evaluate("return require('neordr.backend').zoom ~= nil") == 'true')
            send(b'\x02q')
            assert child.wait(timeout=5) == 0
            eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '0')
            assert evaluate("return require('neordr.backend').zoom == nil") == 'true'
            assert restored_modes(), (old_modes, termios.tcgetattr(slave))
            assert evaluate('return vim.json.encode(vim.fn.winlayout())') == layout
            assert evaluate(f"return vim.api.nvim_buf_get_lines({original},0,1,false)[1]") == 'unsaved editor text'
            assert evaluate("return vim.fn.sha256(vim.inspect({vim.api.nvim_get_keymap('n'),vim.api.nvim_get_keymap('t')}))") == mapping
            survivors = json.loads(evaluate("local r={}; for _,p in ipairs(require('neordr.backend').snapshot().panes) do "
                "if p.neordr.channel then r[p.pane_id]=vim.fn.jobpid(p.neordr.channel) end end; return vim.json.encode(r)"))
            assert all(survivors.get(pane) == pid for pane, pid in identities.items())
            child = attach()
            send(b'\x02z')
            eventually(lambda: evaluate("return require('neordr.backend').zoom ~= nil") == 'true')
            child.terminate()
            assert child.wait(timeout=5) == 0
            eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '0')
            assert evaluate("return require('neordr.backend').zoom == nil") == 'true'
            assert restored_modes(), (old_modes, termios.tcgetattr(slave))
            os.close(slave)
            slave = None
            print(('Full config' if full_config else 'Minimal config') +
                ': sidebar input mode, tab/pane/workspace gestures, zoom/resize/restore, literal paste, jobs, detach and SIGTERM cleanup: PASS')
        except Exception:
            log.flush()
            print((root/'log').read_text()[-5000:])
            for file in root.glob('input*'): print(file.name, file.read_bytes())
            print(bytes(output[-3000:]) if 'output' in locals() else '')
            raise
        finally:
            if second_ws:
                try: rpc(router, 'workspace.close', {'workspace_id': second_ws})
                except Exception: pass
            for child in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    try: child.wait(timeout=5)
                    except subprocess.TimeoutExpired: child.kill(); child.wait()
            stop_reader.set()
            if reader: reader.join(timeout=2)
            if slave is not None: os.close(slave)
            if master is not None: os.close(master)
            log.close()


if __name__ == '__main__': main()
