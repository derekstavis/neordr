#!/usr/bin/env python3
"""Verify the router service on macOS: install beside a running router, restart
that keeps workspaces, and uninstall. Uses a unique label and config root, so
it never touches an installed service or a real router."""
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
from moshi_smoke import BIN, eventually, rpc


def main():
    if platform.system() != 'Darwin':
        print('router service (launchd): SKIP (macOS only)')
        return
    uid = os.getuid()
    if subprocess.run(['launchctl', 'print', f'gui/{uid}'], capture_output=True).returncode:
        print('router service (launchd): SKIP (no GUI session)')
        return
    with tempfile.TemporaryDirectory(prefix='ns-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        label = f'dev.neordr.smoke{os.getpid()}'
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root / 'c'), 'NEORDR_SERVICE_LABEL': label}
        for key in ['NVIM', 'HERDR_SESSION', 'HERDR_ENV', 'HERDR_PANE_ID']:
            env.pop(key, None)
        plist = Path.home() / 'Library/LaunchAgents' / f'{label}.plist'
        router = root / 'c/herdr/herdr.sock'

        def neordr(*args):
            return subprocess.run([str(BIN), *args], env=env, capture_output=True, text=True, check=True).stdout

        def accepting(path):
            with socket.socket(socket.AF_UNIX) as client:
                try:
                    client.connect(str(path))
                    return True
                except OSError:
                    return False

        def pid_alive(pid):
            try:
                os.kill(pid, 0)
                return True
            except ProcessLookupError:
                return False

        workspace = None
        try:
            assert 'not responding' in neordr('service', 'status')
            # A router started outside the service must survive an install.
            adhoc = subprocess.Popen([str(BIN), 'router'], env=env, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
            eventually(lambda: rpc(router, 'ping')['neordr_router'])
            assert rpc(router, 'ping')['pid'] == adhoc.pid
            installed = neordr('service', 'install')
            assert plist.exists()
            assert 'keeps serving' in installed, installed
            assert adhoc.poll() is None and rpc(router, 'ping')['pid'] == adhoc.pid, 'install disturbed the running router'
            assert 'started outside the service' in neordr('service', 'status')
            # Once it exits, the next router request starts the service's router.
            adhoc.terminate()
            adhoc.wait(timeout=10)
            eventually(lambda: not accepting(router))
            neordr('rpc', 'ping')
            eventually(lambda: rpc(router, 'ping')['neordr_router'])
            status = neordr('service', 'status')
            assert 'run by the service' in status, status
            assert 'serves this config root: true' in status, status
            # A second install is idempotent and leaves the router alone.
            before = rpc(router, 'ping')['pid']
            neordr('service', 'install')
            assert rpc(router, 'ping')['pid'] == before

            created = json.loads(neordr('rpc', 'workspace.create', '--params',
                                        json.dumps({'cwd': '/tmp', 'focus': False})))
            assert created['root_pane']['cwd'] == '/tmp', created
            nvims = subprocess.run(['pgrep', '-f', f'nvim --headless --listen /tmp/neordr-{before}-'],
                                   capture_output=True, text=True).stdout.split()
            assert nvims, 'workspace editor not found'
            workspace = int(nvims[0])
            session = subprocess.run(['ps', '-E', '-ww', '-p', str(workspace)],
                                     capture_output=True, text=True).stdout
            assert f'XPC_SERVICE_NAME={label}' in session, 'workspace did not inherit the service job'

            neordr('service', 'restart')
            after = rpc(router, 'ping')['pid']
            assert after != before, 'restart did not replace the router'
            assert pid_alive(workspace), 'restart killed a workspace'
        finally:
            subprocess.run([str(BIN), 'service', 'uninstall'], env=env, capture_output=True)
            if workspace and pid_alive(workspace):
                os.kill(workspace, 15)
            shutil.rmtree(root / 'c', ignore_errors=True)
        assert not plist.exists()
    print('router service (launchd): install beside a running router, takeover, workspace environment, restart, uninstall: PASS')


if __name__ == '__main__':
    main()
