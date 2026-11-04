#!/usr/bin/env python3
"""Restart router and multiple bridges without losing editors, buffers or jobs."""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading

from moshi_smoke import BIN, ROOT, eventually, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='nr-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        replacement = root / 'new-neordr'
        shutil.copy2(BIN, replacement)
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root / 'c'),
               'XDG_STATE_HOME': str(root / 's'), 'NVIM_LOG_FILE': str(root / 'nvim.log')}
        for key in ('NVIM', 'HERDR_SESSION', 'HERDR_ENV', 'HERDR_PANE_ID', 'NEORDR_SERVICE_LABEL'):
            env.pop(key, None)
        router = root / 'c/herdr/herdr.sock'
        editors, processes = [], []
        control = None
        log = open(root / 'log', 'w')

        def attach_control(session_name, pane):
            process = subprocess.Popen([str(BIN), '--session', session_name, 'terminal', 'session', 'control',
                                        pane, '--takeover', '--cols', '80', '--rows', '24'],
                                       env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log)
            threading.Thread(target=lambda: process.stdout.read(), daemon=True).start()
            return process

        try:
            for index in range(2):
                address = root / f'nvim{index}.sock'
                process = subprocess.Popen(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE',
                                            '--listen', str(address)], cwd=root, env=env,
                                           stdin=subprocess.DEVNULL, stdout=log, stderr=log)
                processes.append(process)
                eventually(address.exists)
                session = lua(env, address, f"vim.opt.runtimepath:prepend({json.dumps(str(ROOT))}); "
                              f"assert(require('neordr').setup({{bin={json.dumps(str(BIN))}}})); "
                              "return require('neordr').session")
                endpoint = root / 'c/herdr/sessions' / session / 'herdr.sock'
                eventually(lambda: rpc(endpoint, 'ping'))
                identity = rpc(endpoint, 'ping')
                capture = root / f'job{index}.input'
                worker = root / f'job{index}.py'
                worker.write_text('import os,tty\nfrom pathlib import Path\ntty.setraw(0)\n'
                                  f'with open({str(capture)!r},"ab",buffering=0) as out:\n'
                                  ' while True:\n  out.write(os.read(0,4096))\n')
                data = json.loads(lua(env, address,
                    "vim.api.nvim_buf_set_lines(0,0,-1,false,{'unsaved restart test'}); "
                    "local b=vim.api.nvim_get_current_buf(); vim.cmd('new'); "
                    "local e=require('neordr').prepare(vim.api.nvim_get_current_buf()); "
                    f"local j=vim.fn.jobstart({{'python3',{json.dumps(str(worker))}}},{{term=true,env=e}}); "
                    "return vim.json.encode({buffer=b,job=vim.fn.jobpid(j),pane=e.HERDR_PANE_ID})"))
                eventually(capture.exists)
                editors.append((process, address, endpoint, identity, data, capture))
            eventually(lambda: rpc(router, 'ping')['neordr_router'])
            old_router = rpc(router, 'ping')['pid']
            first = editors[0]
            session_name = first[4]['pane'].split(':', 1)[0]
            control = attach_control(session_name, first[4]['pane'])
            try:
                eventually(lambda: lua(env, first[1], 'return #vim.api.nvim_list_uis()') == '1')
            except AssertionError as error:
                raise AssertionError((str(error), control.poll(), (root / 'log').read_text())) from error
            result = subprocess.run([str(replacement), 'restart'], env=env, capture_output=True,
                                    text=True, timeout=60)
            assert result.returncode == 0, (result.stdout, result.stderr, (root / 'log').read_text())
            new_router = rpc(router, 'ping')['pid']
            assert new_router != old_router
            for process, address, endpoint, identity, data, capture in editors:
                new = rpc(endpoint, 'ping')
                assert new['pid'] != identity['pid']
                assert new['neordr']['pid'] == identity['neordr']['pid']
                assert new['neordr']['generation'] == identity['neordr']['generation']
                assert process.poll() is None
                os.kill(data['job'], 0)
                assert lua(env, address, f"return vim.api.nvim_buf_get_lines({data['buffer']},0,1,false)[1]") == 'unsaved restart test'
                assert lua(env, address, f"return vim.bo[{data['buffer']}].modified") == 'true'
                assert rpc(router, 'pane.get', {'pane_id': data['pane']})['pane']['pane_id'] == data['pane']
                rpc(endpoint, 'pane.send_text', {'pane_id': data['pane'], 'text': 'after-restart'})
                eventually(lambda: capture.read_bytes() == b'after-restart')
                eventually(lambda: lua(env, address, "local p=require('neordr'); p.bridge_status(); return p.last_error == nil") == 'true')
            if control.poll() is None:
                control.stdin.write(b'{"type":"terminal.release"}\n')
                control.stdin.flush()
                assert control.wait(timeout=10) == 0
            eventually(lambda: lua(env, first[1], 'return #vim.api.nvim_list_uis()') == '0')
            control = attach_control(session_name, first[4]['pane'])
            eventually(lambda: lua(env, first[1], 'return #vim.api.nvim_list_uis()') == '1')
            control.stdin.write(b'{"type":"terminal.release"}\n')
            control.stdin.flush()
            assert control.wait(timeout=10) == 0
            eventually(lambda: lua(env, first[1], 'return #vim.api.nvim_list_uis()') == '0')
            print('Restart: router and two bridges replaced; editors, modified buffers, terminal jobs and input survive: PASS')
            print('Restart: existing control detaches cleanly and a new control connection attaches: PASS')
        finally:
            if control is not None and control.poll() is None:
                control.terminate()
                control.wait(timeout=10)
            for process in processes:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=10)
            try:
                current = rpc(router, 'ping')
                if current.get('neordr_router'):
                    os.kill(current['pid'], signal.SIGTERM)
            except (OSError, AssertionError):
                pass
            log.close()

    # A foreign endpoint cannot cause the restart command to signal an
    # unrelated process by returning a fabricated PID in its ping response.
    with tempfile.TemporaryDirectory(prefix='nf-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root)}
        path = root / 'herdr/herdr.sock'
        path.parent.mkdir()
        victim = subprocess.Popen(['/bin/sleep', '60'])
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(path))
        listener.listen()
        listener.settimeout(10)

        def serve():
            try:
                with listener.accept()[0] as conn:
                    request = json.loads(conn.makefile('rb').readline())
                    response = {'id': request['id'], 'result': {'type': 'pong', 'version': 'neordr/0.2.0',
                                'protocol': 22, 'neordr_router': True, 'pid': victim.pid}}
                    conn.sendall((json.dumps(response) + '\n').encode())
                with listener.accept()[0]:
                    pass
            except OSError:
                pass

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        try:
            result = subprocess.run([str(BIN), 'restart'], env=env, capture_output=True, text=True, timeout=15)
            assert result.returncode != 0 and 'PID does not match socket peer' in result.stderr, result.stderr
            assert victim.poll() is None, 'restart signalled an unrelated process'
            print('Restart: refuses a forged router PID and leaves the unrelated process alive: PASS')
        finally:
            listener.close()
            thread.join(timeout=2)
            victim.terminate()
            victim.wait(timeout=5)


if __name__ == '__main__':
    main()
