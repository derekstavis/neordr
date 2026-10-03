#!/usr/bin/env python3
"""Opt-in integration with the actual Herdr TUI and disposable Neovim/PTYs."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import struct
import subprocess
import termios
import tempfile
import threading
import time
from moshi_smoke import BIN, eventually, lua, rpc
from herdr_ordering_smoke import check_tui_ordering, check_native_ordering, check_hit_enter
from attach_prompt_smoke import fast, fast_script

HERDR = Path(os.environ.get('HERDR_BIN', Path.home() / 'Downloads/herdr-macos-aarch64'))

def main():
    with tempfile.TemporaryDirectory(prefix='neordr-herdr-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {k:v for k,v in os.environ.items() if not k.startswith('HERDR_') and k not in ['NVIM','TMUX','TMUX_PANE']}
        env.update(XDG_STATE_HOME=str(root/'state'), XDG_CONFIG_HOME=str(root/'config'), TERM='xterm-256color', NVIM_LOG_FILE=str(root/'nvim.log'))
        (root/'config/herdr').mkdir(parents=True)
        (root/'config/herdr/config.toml').write_text('onboarding = false\n')
        full_config = os.environ.get('NEORDR_NVIM_CONFIG')
        if full_config:
            shutil.copytree(full_config, root/'config/nvim', ignore=shutil.ignore_patterns('.git','.codex','.agents'))
            config = root/'config/nvim/lua/config/neordr.lua'
            config.write_text(config.read_text().replace('bin = bin,', 'session = "test", bin = ' + json.dumps(str(BIN)) + ','))
        children, output, reference_output = [], bytearray(), bytearray()
        stop = threading.Event()
        log = open(root/'log','w')
        master, slave = pty.openpty()
        reference_master, reference_slave = pty.openpty()
        fcntl.ioctl(reference_slave,termios.TIOCSWINSZ,struct.pack('HHHH',45,160,0,0))
        def resize(rows,cols): fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',rows,cols,0,0))
        resize(36,120)
        def drain():
            try:
                while not stop.is_set():
                    for fd in select.select([master,reference_master],[],[],.1)[0]:
                        data = os.read(fd,65536)
                        (output if fd == master else reference_output).extend(data)
            except OSError: pass
        reader = threading.Thread(target=drain,daemon=True); reader.start()
        address = root/'nvim.sock'
        endpoint = root/'config/herdr/sessions/test/herdr.sock'
        try:
            children.append(subprocess.Popen([str(BIN),'router'],env=env,stdout=log,stderr=log))
            eventually(lambda: (root/'config/herdr/herdr.sock').exists())
            editor = subprocess.Popen(['nvim','--headless','-i','NONE','--listen',str(address)] + ([] if full_config else ['-u','NONE']),env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log)
            children.append(editor); eventually(address.exists)
            if not full_config:
                children.append(subprocess.Popen([str(BIN),'--session','test','serve','--nvim',str(address)],env=env,stdout=log,stderr=log))
            eventually(lambda: endpoint.exists() and rpc(endpoint,'ping'))
            # The real CLI checks ping.protocol before JSON API requests, even
            # though the interactive shell negotiates endpoint generation 1.
            for target, socket in [(['--session', 'test'], endpoint),
                                   ([], root/'config/herdr/herdr.sock')]:
                result = subprocess.run([str(HERDR), *target, 'pane', 'list'],
                    env=env, capture_output=True, text=True, timeout=10)
                assert result.returncode == 0, (result.stdout, result.stderr)
                assert json.loads(result.stdout)['result']['panes'] == rpc(socket, 'pane.list')['panes']
            def evaluate(code): return lua(env,address,code)
            editor_buf = evaluate("vim.api.nvim_buf_set_lines(0,0,-1,false,{'NEORDR-EDITOR-MARKER'}); return vim.api.nvim_get_current_buf()")
            reference = subprocess.Popen(['nvim','--server',str(address),'--remote-ui'],env=env,
                stdin=reference_slave,stdout=reference_slave,stderr=reference_slave,start_new_session=True)
            children.append(reference)
            eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '1')
            # Initial endpoint attach must recover the same modal error that
            # prevents nvim --remote-ui from drawing its first frame.
            script = fast_script(root)
            os.write(reference_master, b":lua error('HERDR-ATTACH-ERROR')\r")
            eventually(lambda: fast(env, address, script, 'return vim.api.nvim_get_mode().mode') == 'r')
            client = subprocess.Popen([str(HERDR),'--session','default'],env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
            children.append(client)
            eventually(lambda: b'NEORDR-EDITOR-MARKER' in output, seconds=15)
            assert client.poll() is None
            os.write(master,b'iEDITED-\x1b')
            eventually(lambda: evaluate('return vim.api.nvim_get_current_line()') == 'EDITED-NEORDR-EDITOR-MARKER')
            eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode') == 'n')
            check_hit_enter(master, evaluate, output)
            # Herdr's own new-tab and switching shortcuts invoke endpoint requests.
            os.write(master,b'\x02cFirst\r')
            eventually(lambda: any('channel' in p.get('neordr',{}) for p in json.loads(evaluate("return vim.json.encode(require('neordr.backend').snapshot().panes)"))))
            first = rpc(endpoint,'pane.list')['panes'][0]
            os.write(master,b'\x02cSecond\r')
            eventually(lambda: len(rpc(endpoint,'pane.list')['panes']) == 2)
            panes = rpc(endpoint,'pane.list')['panes']
            os.write(master,b'\x02p')
            eventually(lambda: next(p['pane_id'] for p in rpc(endpoint,'pane.list')['panes'] if p['focused']) == first['pane_id'])
            eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode') == 't')
            # The client must consume the new projection before targeting fresh input.
            time.sleep(.6)
            os.write(master,b"printf '\\nHERDR-INPUT-OK\\n'\r")
            eventually(lambda: 'HERDR-INPUT-OK' in rpc(endpoint,'pane.read',{'pane_id':first['pane_id'],'source':'recent'})['read']['text'].splitlines())
            captures = check_tui_ordering(root, endpoint, master, evaluate, panes)
            before = evaluate("local r={}; for _,p in ipairs(require('neordr.backend').snapshot().panes) do r[p.pane_id]=p.neordr.shell_pid end; return vim.json.encode(r)")
            resize(28,90)
            eventually(lambda: int(evaluate('return vim.o.columns')) < 100)
            os.write(master,b'\x02q')
            assert client.wait(timeout=10) == 0
            eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '1')
            eventually(lambda: evaluate('return vim.o.columns') == '160')
            assert editor.poll() is None
            assert evaluate("local r={}; for _,p in ipairs(require('neordr.backend').snapshot().panes) do r[p.pane_id]=p.neordr.shell_pid end; return vim.json.encode(r)") == before
            check_native_ordering(root, root/'config/herdr/herdr.sock', evaluate, panes, captures, reference_master, reference_output, env, log, children)
            client = subprocess.Popen([str(HERDR),'--session','default'],env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
            children.append(client)
            eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '2')
            client.kill(); client.wait(timeout=5)
            eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '1')
            eventually(lambda: evaluate('return vim.o.columns') == '160')
            assert reference.poll() is None
            assert editor.poll() is None
            assert evaluate("local r={}; for _,p in ipairs(require('neordr.backend').snapshot().panes) do r[p.pane_id]=p.neordr.shell_pid end; return vim.json.encode(r)") == before
            assert evaluate(f"return vim.api.nvim_buf_get_lines({editor_buf},0,1,false)[1]") == 'EDITED-NEORDR-EDITOR-MARKER'
            print(('Full config' if full_config else 'Minimal config') + ': Herdr TUI editor rendering/editing, native new-tab/switching, terminal input, resize, detach and killed-client cleanup: PASS')
        except Exception:
            log.flush()
            print((root/'log').read_text()[-4000:])
            for file in (root/'config').rglob('*.log'): print(file.name, file.read_text(errors='replace')[-5000:])
            print(bytes(output[-7000:]))
            raise
        finally:
            for child in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    try: child.wait(timeout=4)
                    except subprocess.TimeoutExpired: child.kill(); child.wait()
            stop.set(); reader.join(timeout=2)
            os.close(master); os.close(slave)
            os.close(reference_master); os.close(reference_slave); log.close()

if __name__ == '__main__': main()
