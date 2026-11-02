#!/usr/bin/env python3
"""Real Moshi creation/launch requests; disposable editors and fake agents only."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
from moshi_smoke import BIN, HOOK, eventually, http, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-create-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        (root / 'bin').mkdir()
        (root / 'bin/herdr').symlink_to(BIN)
        env = {**os.environ, 'HOME': str(root), 'XDG_CONFIG_HOME': str(root / 'config'),
               'MOSHI_STATE_DIR': str(root / 'state'), 'MOSHI_CONFIG_DIR': str(root / 'state'),
               'MOSHI_SOCKET_PATH': str(root / 'moshi.sock'), 'MOSHI_API_BASE': 'http://127.0.0.1:9',
               'MOSHI_HERDR_PATH': str(root / 'bin/herdr'), 'NVIM_LOG_FILE': str(root / 'nvim.log'),
               'PATH': str(root / 'bin') + os.pathsep + os.environ['PATH']}
        for key in ['NVIM', 'HERDR_ENV', 'HERDR_SESSION', 'HERDR_PANE_ID', 'HERDR_SOCKET_PATH', 'TMUX', 'ZELLIJ']:
            env.pop(key, None)
        for source in ['claude', 'codex']:
            fake = root / 'bin' / source
            fake.write_text('#!/usr/bin/env python3\nimport os,json,tty\n'
                'tty.setraw(0)\n'
                f'with open({str(root / "launches.jsonl")!r}, "a") as f:\n'
                f' f.write(json.dumps(dict(agent={source!r},cwd=os.getcwd(),pane=os.environ.get("HERDR_PANE_ID"),session=os.environ.get("HERDR_SESSION")))+"\\n")\n'
                f'os.write(1,b"READY_{source}\\r\\n")\n'
                'while True:\n'
                ' data=os.read(0,4096)\n'
                ' if not data or b"\\x04" in data: break\n'
                ' os.write(1,b"RECEIVED "+data.hex().encode()+b"\\r\\n")\n')
            fake.chmod(0o755)
        children = []
        managed = []
        (root/'config/nvim').mkdir(parents=True)
        (root/'config/nvim/init.lua').write_text("vim.o.shell='/bin/sh'; vim.o.shadafile='NONE'\n")
        address = root / 'nvim.sock'
        endpoint = root / 'config/herdr/sessions/create/herdr.sock'
        router = root / 'config/herdr/herdr.sock'
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]

        def start(args, name):
            log = open(root / (name + '.log'), 'w')
            child = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            children.append((child, log))

        try:
            start(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '--listen', str(address)], 'nvim')
            eventually(address.exists)
            lua(env, address, 'vim.o.shell=' + json.dumps(os.environ.get('NEORDR_TEST_SHELL', '/bin/sh')) + '; return true')
            start([str(BIN), '--session', 'create', 'serve', '--nvim', str(address)], 'bridge')
            eventually(lambda: endpoint.exists() and rpc(endpoint, 'ping'))
            start([str(BIN), 'router'], 'router')
            eventually(lambda: router.exists() and rpc(router, 'ping'))
            for setting in ['always-on-discovery', 'usage-collection']:
                subprocess.run([HOOK, 'set', setting, 'off'], env=env, check=True, capture_output=True)
            start([HOOK, 'serve', '--gateway-listen', f'127.0.0.1:{port}'], 'moshi')
            eventually(lambda: http(port, '/v1/version')[0] == 200)
            for mux in ['herdr', 'herdr:default']:
                cwd = root / ('project ' + mux.replace(':', '-') + " ' quoted")
                cwd.mkdir()
                status, reply = http(port, '/v1/workspaces/create?mux=' + mux,
                                     {'cwd': str(cwd), 'label': 'Remote project'})
                assert status == 200, (status, reply)
                ws = next(w for w in rpc(router, 'workspace.list')['workspaces'] if w['label'] == 'Remote project')
                workspace = ws['workspace_id']
                key = workspace.split(':')[0]
                endpoint = root/'config/herdr/sessions'/key/'herdr.sock'
                managed.append(rpc(endpoint, 'ping')['neordr'])
                # Rename at all supported levels, including a user-visible name that differs from cwd.
                status, reply = http(port, '/v1/workspaces/rename?mux=' + mux,
                                     {'workspaceId': workspace, 'label': mux + ' project'})
                assert status == 200, (status, reply)
                for source in ['claude', 'codex']:
                    label = mux + ' ' + source
                    status, reply = http(port, '/v1/workspaces/create?mux=' + mux,
                        {'workspaceId': workspace, 'label': label, 'agent': source})
                    assert status == 200, (status, reply)
                    pane = next(p for p in rpc(endpoint, 'pane.list')['panes'] if p['label'] == label)
                    pid = pane['pane_id']
                    eventually(lambda: rpc(endpoint, 'pane.get', {'pane_id': pid})['pane'].get('agent') == source)
                    eventually(lambda: 'READY_' + source in rpc(endpoint, 'pane.read', {'pane_id': pid})['read']['text'])
                    launches = [json.loads(l) for l in (root / 'launches.jsonl').read_text().splitlines()]
                    # New agents use the global router identity, even in a named editor bridge.
                    assert any(l == {'agent': source, 'pane': pid, 'session': 'default', 'cwd': str(cwd)} for l in launches), launches
                    rpc(router, 'pane.send_text', {'pane_id': pid, 'text': 'agent-input'})
                    eventually(lambda: '6167656e742d696e707574' in rpc(endpoint, 'pane.read', {'pane_id': pid})['read']['text'])
                    # Return to the same shell. The launch exception must not allow stale chat input.
                    rpc(endpoint, 'pane.send_text', {'pane_id': pid, 'text': '\x04'})
                    eventually(lambda: 'agent' not in rpc(endpoint, 'pane.get', {'pane_id': pid})['pane'])
                    for method, params in [('pane.send_text', {'text': 'claude'}), ('pane.send_keys', {'keys': ['enter']})]:
                        try:
                            rpc(router, method, {'pane_id': pid, **params})
                        except AssertionError:
                            pass
                        else:
                            raise AssertionError('reused shell accepted stale input')
                # Also exercise Moshi's separate launch action on a newly created shell.
                status, reply = http(port, '/v1/workspaces/create?mux=' + mux,
                    {'workspaceId': workspace, 'label': mux + ' shell'})
                assert status == 200, (status, reply)
                pane = next(p for p in rpc(endpoint, 'pane.list')['panes'] if p['label'] == mux + ' shell')
                status, reply = http(port, '/v1/workspaces/launch?mux=' + mux,
                    {'paneId': pane['pane_id'], 'agent': 'codex'})
                assert status == 200, (status, reply)
                eventually(lambda: rpc(endpoint, 'pane.get', {'pane_id': pane['pane_id']})['pane'].get('agent') == 'codex')
                print(mux + ': workspace creation, naming, terminal creation, Claude/Codex launch, input, and stale-chat refusal: PASS')
                tabs = [t for t in rpc(endpoint, 'tab.list')['tabs'] if t['workspace_id'] == workspace]
                status, reply = http(port, '/v1/workspaces/move?mux=' + mux,
                    {'tabId': tabs[0]['tab_id'], 'index': len(tabs)})
                assert status == 200, (status, reply)
                moved = [t for t in rpc(endpoint, 'tab.list')['tabs'] if t['workspace_id'] == workspace]
                assert moved[-1]['tab_id'] == tabs[0]['tab_id']
                for field, target in [('paneId', pid), ('tabId', tabs[0]['tab_id'])]:
                    status, reply = http(port, '/v1/workspaces/close?mux=' + mux, {field: target})
                    assert status == 200, (status, reply)
                status, reply = http(port, '/v1/workspaces/move?mux=' + mux,
                    {'workspaceId': workspace, 'index': 0})
                assert status == 200, (status, reply)
                status, reply = http(port, '/v1/workspaces/close?mux=' + mux, {'workspaceId': workspace})
                assert status == 200, (status, reply)
                assert all(w['workspace_id'] != workspace for w in rpc(router, 'workspace.list')['workspaces'])
                print(mux + ': terminal/workspace reorder and pane/tab/workspace close: PASS')
        except Exception:
            for p in root.glob('*.log'):
                print(p.name, p.read_text()[-2000:])
            raise
        finally:
            for instance in managed:
                if Path(instance['socket']).exists():
                    try: os.kill(instance['pid'], 15)
                    except ProcessLookupError: pass
            for child, log in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()
                log.close()


if __name__ == '__main__':
    main()
