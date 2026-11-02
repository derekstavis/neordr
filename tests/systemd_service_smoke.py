#!/usr/bin/env python3
"""Exercise a real systemd user service in a disposable, dedicated user account."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile

from moshi_smoke import BIN, eventually, rpc


def main():
    if os.environ.get('NEORDR_SYSTEMD_TEST') != '1':
        raise SystemExit('Use only with NEORDR_SYSTEMD_TEST=1 in a disposable user account')
    home = Path.home()
    config = home / '.config'
    assert Path(os.environ['XDG_CONFIG_HOME']) == config
    assert not (config / 'herdr').exists(), 'dedicated account must have no existing Herdr state'
    subprocess.run(['systemctl', '--user', 'show-environment'], check=True, capture_output=True)
    label = f'neordr-smoke-{os.getpid()}'
    env = {**os.environ, 'NEORDR_SERVICE_LABEL': label}
    for key in ('NVIM', 'HERDR_SESSION', 'HERDR_ENV', 'HERDR_PANE_ID'):
        env.pop(key, None)
    router = config / 'herdr/herdr.sock'
    unit = config / 'systemd/user' / f'{label}.service'
    adhoc = None
    workspace = None

    def neordr(*args):
        result = subprocess.run([str(BIN), *args], env=env, capture_output=True,
                                text=True, timeout=30)
        if result.returncode:
            raise AssertionError(f'neordr {args}: {result.stdout}\n{result.stderr}')
        return result.stdout

    def accepting():
        with socket.socket(socket.AF_UNIX) as client:
            try:
                client.connect(str(router))
                return True
            except OSError:
                return False

    def alive(pid):
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False

    try:
        assert 'not responding' in neordr('service', 'status')
        adhoc = subprocess.Popen([str(BIN), 'router'], env=env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 start_new_session=True)
        eventually(lambda: rpc(router, 'ping')['neordr_router'])
        assert rpc(router, 'ping')['pid'] == adhoc.pid
        assert 'keeps serving' in neordr('service', 'install')
        assert unit.exists() and rpc(router, 'ping')['pid'] == adhoc.pid
        assert 'started outside the service' in neordr('service', 'status')
        adhoc.terminate()
        adhoc.wait(timeout=10)
        eventually(lambda: not accepting())
        neordr('rpc', 'ping')
        eventually(lambda: rpc(router, 'ping')['neordr_router'])
        status = neordr('service', 'status')
        assert 'run by the service' in status and 'serves this config root: true' in status
        before = rpc(router, 'ping')['pid']
        neordr('service', 'install')
        assert rpc(router, 'ping')['pid'] == before
        with tempfile.TemporaryDirectory(prefix='neordr-systemd-workspace-', dir='/tmp') as cwd:
            created = json.loads(neordr('rpc', 'workspace.create', '--params',
                                        json.dumps({'cwd': cwd, 'focus': False})))
            assert created['root_pane']['cwd'] == cwd
            matches = subprocess.run(['pgrep', '-f', f'nvim --headless --listen /tmp/neordr-{before}-'],
                                     capture_output=True, text=True, check=True).stdout.split()
            assert len(matches) == 1, matches
            workspace = int(matches[0])
            neordr('service', 'restart')
            assert rpc(router, 'ping')['pid'] != before
            assert alive(workspace), 'restart killed the workspace'
            neordr('service', 'uninstall')
            assert not unit.exists()
            assert alive(workspace), 'uninstall killed the workspace'
    finally:
        subprocess.run([str(BIN), 'service', 'uninstall'], env=env, capture_output=True, timeout=30)
        if adhoc is not None and adhoc.poll() is None:
            adhoc.terminate()
            adhoc.wait(timeout=10)
        if workspace is not None and alive(workspace):
            os.kill(workspace, signal.SIGTERM)
    print('router service (systemd): install, takeover, restart, workspace survival, uninstall: PASS')


if __name__ == '__main__':
    main()
