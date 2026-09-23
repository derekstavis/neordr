#!/usr/bin/env python3
"""Attach an actual Neovim remote UI through the herdr alias, detach, reattach."""
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
    with tempfile.TemporaryDirectory(prefix='neordr-ui-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        alias = root / 'herdr'
        alias.symlink_to(BIN)
        env = {**os.environ, 'XDG_CONFIG_HOME':str(root/'config'), 'TERM':'xterm-256color',
               'NVIM_LOG_FILE':str(root/'nvim.log')}
        for key in ['NVIM','HERDR_SESSION','HERDR_ENV']: env.pop(key, None)
        address = root/'nvim.sock'
        endpoint = root/'config/herdr/sessions/ui/herdr.sock'
        server = subprocess.Popen(['nvim','--headless','-u','NONE','-i','NONE','--listen',str(address)],
                                  env=env,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        bridge = None
        try:
            eventually(lambda: address.exists())
            bridge = subprocess.Popen([str(BIN),'--session','ui','serve','--nvim',str(address)],
                                      env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            eventually(lambda: endpoint.exists() and rpc(endpoint,'ping'))
            for iteration in [1,2]:
                master, slave = pty.openpty()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH',30,100,0,0))
                attached = subprocess.Popen([str(alias),'--session','ui'],env=env,
                    stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
                os.close(slave)
                def drain(fd=master):
                    try:
                        while os.read(fd, 65536): pass
                    except OSError:
                        pass
                reader = threading.Thread(target=drain, daemon=True)
                reader.start()
                try:
                    eventually(lambda: lua(env,address,'return #vim.api.nvim_list_uis()') == '1')
                    os.write(master, f':lua vim.g.neordr_ui_probe = {iteration}\r'.encode())
                    eventually(lambda: lua(env,address,'return vim.g.neordr_ui_probe or 0') == str(iteration))
                    os.write(master,b':detach\r')
                    assert attached.wait(timeout=5) == 0
                    assert server.poll() is None
                    eventually(lambda: lua(env,address,'return #vim.api.nvim_list_uis()') == '0')
                finally:
                    if attached.poll() is None: attached.terminate(); attached.wait(timeout=5)
                    os.close(master)
                    reader.join(timeout=1)
            print('Actual remote UI attach, input, detach, and reattach: PASS')
        finally:
            for process in [bridge,server]:
                if process is not None:
                    process.terminate()
                    process.wait(timeout=5)


if __name__ == '__main__': main()
