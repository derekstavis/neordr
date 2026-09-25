#!/usr/bin/env python3
"""Attach to real modal Neovim editors without answering decision prompts."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import threading
from moshi_smoke import BIN, eventually, lua, rpc


class Client:
    def __init__(self, command, env):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
        self.output = bytearray()
        self.stop = threading.Event()
        self.process = subprocess.Popen(command, env=env, stdin=slave, stdout=slave,
                                        stderr=slave, start_new_session=True)
        os.close(slave)
        def drain():
            try:
                while not self.stop.is_set():
                    if select.select([self.master], [], [], .1)[0]:
                        data = os.read(self.master, 65536)
                        if not data: break
                        self.output.extend(data)
            except OSError:
                pass
        self.reader = threading.Thread(target=drain, daemon=True)
        self.reader.start()

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)
        self.stop.set()
        self.reader.join(timeout=2)
        os.close(self.master)


def fast(env, address, script, code):
    result = subprocess.run(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '-l',
                             str(script), str(address), code], env=env,
                            capture_output=True, text=True, timeout=5, check=True)
    return json.loads(result.stdout)


def fast_script(root):
    script = root/'fast.lua'
    script.write_text("local c=vim.fn.sockconnect('pipe',arg[1],{rpc=true}); assert(c>0)\n"
                      "local r=vim.rpcrequest(c,'nvim__exec_lua_fast',arg[2],{})\n"
                      "io.write(vim.json.encode(r)); vim.fn.chanclose(c)\n")
    return script


def main():
    with tempfile.TemporaryDirectory(prefix='na-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {k: v for k, v in os.environ.items() if not k.startswith('HERDR_') and k != 'NVIM'}
        env.update(XDG_CONFIG_HOME=str(root/'c'), TERM='xterm-256color', NVIM_LOG_FILE=str(root/'nvim.log'))
        address, endpoint = root/'nvim.sock', root/'c/herdr/sessions/ui/herdr.sock'
        script = fast_script(root)
        children, clients = [], []
        with open(root/'log', 'w') as log:
            try:
                children.append(subprocess.Popen(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE',
                    '--listen', str(address)], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log))
                eventually(address.exists)
                children.append(subprocess.Popen([str(BIN), '--session', 'ui', 'serve', '--nvim', str(address)],
                                                  env=env, stdout=log, stderr=log))
                eventually(lambda: endpoint.exists() and rpc(endpoint, 'ping'))
                lua(env, address, "vim.api.nvim_buf_set_lines(0,0,-1,false,{'PROMPT-EDITOR','EXTRA-ENTER-TRAP'}); return true")
                primary = Client(['nvim', '--server', str(address), '--remote-ui'], env)
                clients.append(primary)
                eventually(lambda: b'PROMPT-EDITOR' in primary.output)
                read = lambda code: fast(env, address, script, code)
                mode = lambda: read('return vim.api.nvim_get_mode().mode')
                attach = lambda: Client([str(BIN), '--session', 'ui'], env)
                for command in [":lua error('ATTACH-ERROR')\r",
                                ":set nomore | echohl WarningMsg | for i in range(40) | echomsg 'ATTACH-WARNING' | endfor | echohl None\r"]:
                    os.write(primary.master, command.encode())
                    eventually(lambda: mode() == 'r')
                    # Discovery itself must not consume the acknowledgement.
                    rpc(endpoint, 'ping')
                    assert mode() == 'r'
                    # A bridge restart must reuse the configured backend while
                    # blocked, without requiring deferred Lua or dismissing it.
                    children[-1].terminate()
                    children[-1].wait(timeout=5)
                    children.append(subprocess.Popen(
                        [str(BIN), '--session', 'ui', 'serve', '--nvim', str(address)],
                        env=env, stdout=log, stderr=log))
                    eventually(lambda: endpoint.exists() and rpc(endpoint, 'ping'))
                    assert mode() == 'r', 'bridge recovery consumed the acknowledgement'
                    pair = [attach(), attach()]
                    clients.extend(pair)
                    for client in pair:
                        eventually(lambda: b'PROMPT-EDITOR' in client.output)
                        assert client.process.poll() is None
                    eventually(lambda: mode() == 'n')
                    assert read('return vim.api.nvim_get_current_line()') == 'PROMPT-EDITOR'
                    assert read('return vim.api.nvim_win_get_cursor(0)') == [1, 0]
                    for client in pair:
                        client.close(); clients.remove(client)
                    eventually(lambda: read('return #vim.api.nvim_list_uis()') == 1)
                # Enter would choose Yes: an attach must leave this unanswered.
                os.write(primary.master, b":lua vim.g.answer=vim.fn.confirm('KEEP THIS?', '&Yes\\n&No', 1)\r")
                eventually(lambda: mode() == 'r?')
                client = attach(); clients.append(client)
                eventually(lambda: b'KEEP THIS?' in client.output)
                assert client.process.poll() is None
                assert mode() == 'r?'
                assert read('return vim.g.answer') is None
                os.write(client.master, b'n')
                eventually(lambda: mode() == 'n')
                assert read('return vim.g.answer') == 2
                client.close(); clients.remove(client)
                # Input dialogs likewise must not receive any automated keys.
                os.write(primary.master, b":lua vim.g.answer=vim.fn.input('ANSWER: ')\r")
                eventually(lambda: mode() == 'c')
                client = attach(); clients.append(client)
                eventually(lambda: b'ANSWER:' in client.output)
                assert client.process.poll() is None
                assert read('return vim.g.answer') == 2
                os.write(client.master, b'kept\r')
                eventually(lambda: mode() == 'n')
                assert read('return vim.g.answer') == 'kept'
                print('Attach recovery: error/warning acknowledgements, concurrent clients, confirmation/input preservation: PASS')
            except Exception:
                log.flush()
                print((root/'log').read_text()[-3000:])
                for client in clients: print(bytes(client.output[-1000:]))
                raise
            finally:
                for client in reversed(clients): client.close()
                for child in reversed(children):
                    if child.poll() is None: child.terminate(); child.wait(timeout=5)


if __name__ == '__main__':
    main()
