#!/usr/bin/env python3
"""Session-less terminal attachment follows focus across separate Neovim servers."""
import fcntl
import os
from pathlib import Path
import pty
import struct
import subprocess
import tempfile
import termios
import threading
from moshi_smoke import BIN, eventually, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-default-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        alias = root / 'herdr'
        alias.symlink_to(BIN)
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root / 'config'), 'TERM': 'xterm-256color',
               'NVIM_LOG_FILE': str(root / 'nvim.log')}
        for key in ['NVIM', 'HERDR_SESSION', 'HERDR_ENV']: env.pop(key, None)
        children, sockets, endpoints = [], {}, {}
        master = None
        try:
            for name in ['first', 'second']:
                address = root / (name + '.sock')
                sockets[name] = address
                children.append(subprocess.Popen(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE',
                    '--listen', str(address)], env=env, stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                eventually(address.exists)
                children.append(subprocess.Popen([str(BIN), '--session', name, 'serve', '--nvim', str(address)],
                    env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                endpoints[name] = root / 'config/herdr/sessions' / name / 'herdr.sock'
                eventually(lambda: endpoints[name].exists() and rpc(endpoints[name], 'ping'))
            children.append(subprocess.Popen([str(BIN), 'router'], env=env,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            router = root / 'config/herdr/herdr.sock'
            eventually(lambda: router.exists() and rpc(router, 'ping'))
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
            attached = subprocess.Popen([str(alias)], env=env, stdin=slave, stdout=slave, stderr=slave)
            children.append(attached)
            os.close(slave)
            output = bytearray()
            def drain():
                try:
                    while True:
                        data = os.read(master, 65536)
                        if not data: return
                        output.extend(data)
                except OSError: pass
            reader = threading.Thread(target=drain, daemon=True)
            reader.start()
            eventually(lambda: b'Session number:' in output)
            os.write(master, b'2\n')
            eventually(lambda: lua(env, sockets['second'], 'return #vim.api.nvim_list_uis()') == '1')
            # The default client must follow successful focus changes, not pick
            # the first session or keep forwarding input to the previous one.
            for iteration, name in enumerate(['first', 'second'], 1):
                workspace = rpc(endpoints[name], 'workspace.list')['workspaces'][0]['workspace_id']
                rpc(router, 'workspace.focus', {'workspace_id': workspace})
                eventually(lambda: lua(env, sockets[name], 'return #vim.api.nvim_list_uis()') == '1')
                other = 'second' if name == 'first' else 'first'
                eventually(lambda: lua(env, sockets[other], 'return #vim.api.nvim_list_uis()') == '0')
                os.write(master, f':lua vim.g.default_ui_marker = {iteration}\r'.encode())
                eventually(lambda: lua(env, sockets[name], 'return vim.g.default_ui_marker or 0') == str(iteration))
            os.write(master, b':detach\r')
            assert attached.wait(timeout=5) == 0
            assert all(child.poll() is None for child in children[:-1]), 'attachment killed an editor'
            print('Default picker, cross-editor focus switching, input isolation, and detach: PASS')
        finally:
            for child in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    try: child.wait(timeout=5)
                    except subprocess.TimeoutExpired: child.kill(); child.wait()
            if master is not None: os.close(master)


if __name__ == '__main__': main()
