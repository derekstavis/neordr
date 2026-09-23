#!/usr/bin/env python3
"""Opt-in contract test against an installed moshi-hook, with synthetic chats.

No accounts, real conversations, or external API calls are needed. Every process,
socket, transcript and hook state belongs to a temporary test directory.
"""
import base64
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("NEORDR_BIN", ROOT / "target/debug/neordr")).resolve()
HOOK = os.environ.get("MOSHI_HOOK_BIN", shutil.which("moshi-hook") or "moshi-hook")


def eventually(fn, seconds=10):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            value = fn()
            if value:
                return value
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            last = error
        time.sleep(0.05)
    raise AssertionError(f"Timed out: {last}")


def lua(env, address, code):
    expression = "luaeval(" + json.dumps("(function() " + code + " end)()") + ")"
    result = subprocess.run(["nvim", "--server", str(address), "--remote-expr", expression],
                            env=env, capture_output=True, text=True, timeout=10, check=True)
    return result.stdout.strip()


def rpc(address, method, params=None):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(5)
        client.connect(str(address))
        client.sendall(json.dumps({"id": "smoke", "method": method, "params": params or {}}).encode() + b"\n")
        result = json.loads(client.makefile("rb").readline())
        assert "error" not in result, result
        return result["result"]


def http(port, path, body=None):
    request = urllib.request.Request(f"http://127.0.0.1:{port}{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def transcript(port, source, session, marker, path):
    with socket.create_connection(("127.0.0.1", port), timeout=10) as client:
        key = base64.b64encode(os.urandom(16)).decode()
        client.sendall((f"GET /v1/transcripts?source={source}&session={session} HTTP/1.1\r\n"
                       f"Host: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                       f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        reader = client.makefile("rb")
        status = reader.readline()
        headers = {}
        while True:
            line = reader.readline()
            if line == b"\r\n": break
            key, value = line.decode().split(':', 1)
            headers[key.lower()] = value.strip()
        assert b"101" in status, (status, reader.read(int(headers.get('content-length', '0'))))
        received = ""
        appended = False
        for _ in range(20):
            header = reader.read(2)
            assert len(header) == 2
            size = header[1] & 127
            if size == 126:
                size = struct.unpack("!H", reader.read(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", reader.read(8))[0]
            assert size < 4 * 1024 * 1024
            received += reader.read(size).decode(errors="replace")
            if marker + '_APPEND' in received:
                return
            if marker in received and not appended:
                row = json.loads(path.read_text().splitlines()[-1])
                if 'uuid' in row: row['uuid'] = str(uuid.uuid4())
                with path.open('a') as log:
                    log.write(json.dumps(row).replace(marker, marker + '_APPEND') + '\n')
                appended = True
        raise AssertionError(received)


def pty_attachment(port, env, address, root):
    """Exercise Moshi's real sidebar preflight and WebSocket PTY attachment."""
    for iteration, (hidden, named) in enumerate([('true', True), ('false', True), ('false', False)], 1):
        with socket.create_connection(('127.0.0.1', port), timeout=10) as client:
            key = base64.b64encode(os.urandom(16)).decode()
            session_query = '&muxSession=smoke' if named else ''
            client.sendall((f'GET /v1/pty?mux=herdr{session_query}&hideSidebar={hidden} HTTP/1.1\r\n'
                f'Host: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                f'Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
            reader = client.makefile('rb')
            status = reader.readline()
            while reader.readline() != b'\r\n': pass
            assert b'101' in status, status
            output = bytearray()
            def drain():
                try:
                    while True:
                        head = reader.read(2)
                        if len(head) != 2: return
                        size = head[1] & 127
                        if size == 126: size = struct.unpack('!H', reader.read(2))[0]
                        elif size == 127: size = struct.unpack('!Q', reader.read(8))[0]
                        assert size < 4 * 1024 * 1024
                        output.extend(reader.read(size))
                        if head[0] & 15 == 8: return
                except (OSError, ValueError): pass
            thread = threading.Thread(target=drain, daemon=True)
            thread.start()
            def send(value):
                data = json.dumps(value).encode()
                mask = os.urandom(4)
                header = bytes([0x81, 0x80 | len(data)]) if len(data) < 126 else bytes([0x81, 0xfe]) + struct.pack('!H', len(data))
                client.sendall(header + mask + bytes(c ^ mask[i % 4] for i, c in enumerate(data)))
            try:
                eventually(lambda: lua(env, address, 'return #vim.api.nvim_list_uis()') == '1')
                send({'type': 'resize', 'cols': 100, 'rows': 30})
                send({'type': 'input', 'data': f':lua vim.g.moshi_pty_probe = {iteration}\r'})
                eventually(lambda: lua(env, address, 'return vim.g.moshi_pty_probe or 0') == str(iteration))
                send({'type': 'input', 'data': ':detach\r'})
                eventually(lambda: lua(env, address, 'return #vim.api.nvim_list_uis()') == '0')
                config = (root / 'config/herdr/config.toml').read_text()
                assert f'sidebar_collapsed_mode = "{ "hidden" if hidden == "true" else "compact" }"' in config, config
            except Exception:
                print('PTY output tail:', repr(output[-1500:].decode(errors='replace')))
                raise
            finally:
                try: client.shutdown(socket.SHUT_RDWR)
                except OSError: pass
                thread.join(timeout=2)
                reader.close()
    print('Moshi sidebar preflight, WebSocket PTY attach, input, detach, and reattach: PASS')


def main():
    processes = []
    with tempfile.TemporaryDirectory(prefix="neordr-moshi-", dir="/tmp") as directory:
        root = Path(directory).resolve()
        (root / "bin").mkdir()
        (root / "bin/herdr").symlink_to(BIN)
        env = {**os.environ, "HOME": str(root), "XDG_CONFIG_HOME": str(root / "config"),
               "MOSHI_STATE_DIR": str(root / "state"), "MOSHI_SOCKET_PATH": str(root / "moshi.sock"),
               "MOSHI_CONFIG_DIR": str(root / "state"), "CLAUDE_CONFIG_DIR": str(root / '.claude'),
               "MOSHI_API_BASE": "http://127.0.0.1:9", "CODEX_HOME": str(root / "codex"),
               "MOSHI_HERDR_PATH": str(root / "bin/herdr"), "TMPDIR": str(root),
               "NVIM_LOG_FILE": str(root / "nvim.log"),
               "NEORDR_TRACE": "1",
               "PATH": str(root / "bin") + os.pathsep + os.environ["PATH"]}
        for key in ["TMUX", "TMUX_PANE", "ZELLIJ", "HERDR_ENV", "HERDR_SESSION", "HERDR_PANE_ID", "HERDR_CONFIG_PATH", "NVIM"]:
            env.pop(key, None)
        for setting in ['always-on-discovery', 'usage-collection']:
            subprocess.run([HOOK, 'set', setting, 'off'], env=env, check=True, capture_output=True, timeout=10)
        address = root / "nvim.sock"
        endpoint = root / "config/herdr/sessions/smoke/herdr.sock"
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]

        def start(args, name):
            log = open(root / (name + ".log"), "w")
            process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            processes.append((process, log))
            return process

        try:
            start(["nvim", "--headless", "-u", "NONE", "-i", "NONE", "--listen", str(address)], "nvim")
            eventually(lambda: address.exists())
            start([str(BIN), "--session", "smoke", "serve", "--nvim", str(address)], "neordr")
            eventually(lambda: endpoint.exists() and rpc(endpoint, "ping"))
            start([str(BIN), 'router'], 'router')
            eventually(lambda: (root / 'config/herdr/herdr.sock').exists())
            start([HOOK, "serve", "--gateway-listen", f"127.0.0.1:{port}"], "moshi")
            eventually(lambda: http(port, "/v1/version")[0] == 200)
            pty_attachment(port, env, address, root)
            sessions = {}
            for source in ["claude", "codex"]:
                session = str(uuid.uuid4())
                marker = f"NEORDR_{source.upper()}_TRANSCRIPT"
                if source == "claude":
                    path = root / '.claude/projects/neordr-test' / (session + '.jsonl')
                    rows = [{"type": "user", "sessionId": session, "uuid": str(uuid.uuid4()),
                             "timestamp": "2026-09-21T00:00:00Z", "message": {"role": "user", "content": marker}}]
                else:
                    path = root / "codex/sessions/2026/09/21" / f"rollout-2026-09-21T00-00-00-{session}.jsonl"
                    rows = [{"type": "session_meta", "timestamp": "2026-09-21T00:00:00Z",
                             "payload": {"id": session, "cwd": str(root), "source": "cli", "originator": "codex_cli_rs"}},
                            {"type": "response_item", "timestamp": "2026-09-21T00:00:01Z",
                             "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": marker}]}}]
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("".join(json.dumps(row) + "\n" for row in rows))
                event = {"hook_event_name": "SessionStart", "session_id": session,
                         "cwd": str(root), "transcript_path": str(path)}
                child = root / 'bin' / source
                child.write_text("import os,subprocess,json,tty\n"
                    + f"rollout=open({str(path)!r})\n"
                    + f"subprocess.run([{HOOK!r},{(source+'-hook')!r}],input={json.dumps(event)!r},text=True,timeout=10)\n"
                    + "tty.setraw(0)\nos.write(1,b'\\x1b[2J\\x1b[HREADY\\r\\n')\n"
                    + f"capture=open({str(root/(source+'.input'))!r},'ab',buffering=0)\n"
                    + "while True:\n data=os.read(0,4096)\n if not data: break\n capture.write(data)\n os.write(1,b'INPUT '+data.hex().encode()+b'\\r\\n')\n")
                code = "vim.cmd.enew(); local b=vim.api.nvim_get_current_buf(); " \
                       "local env=require('neordr.backend').prepare(b); " \
                       "vim.fn.jobstart({'python3'," + json.dumps(str(child)) + "},{term=true,env=env}); " \
                       "return env.HERDR_PANE_ID"
                pane = lua(env, address, code)
                eventually(lambda: "READY" in rpc(endpoint, "pane.read", {"pane_id": pane, "source": "visible"})["read"]["text"])
                sessions[source] = (session, pane, marker, path)
            agents = rpc(endpoint, "agent.list")["agents"]
            assert sorted(a["agent"] for a in agents) == ["claude", "codex"], agents
            for source, (session, pane, marker, path) in sessions.items():
                rpc(endpoint, 'agent.focus', {'target': pane})
                context_env = {**env, "HERDR_ENV": "1", "HERDR_SESSION": "smoke", "HERDR_PANE_ID": pane}
                context = json.loads(subprocess.check_output([HOOK, "context"], env=context_env, timeout=15))
                print(source, "context:", json.dumps({"kind":context.get('kind'),"agent":context.get('agent',{}).get('name'),"session":context.get('agent',{}).get('session')}))
                assert context["kind"] == "herdr", context
                assert context['agent']['session'] == session, context
                transcript(port, source, session, marker, path)
                print(source, "transcript: PASS")
                prompt = 'neordr prompt\nsecond line'
                status, response = http(port, "/v1/prompt?mux=herdr:smoke", {"source": source, "sessionId": session, "text": prompt})
                assert status == 200, (source, status, response)
                eventually(lambda: (root / (source + ".input")).read_bytes().endswith(b"\r"))
                received = (root / (source + ".input")).read_bytes()
                assert received == b'\x1b[200~' + prompt.encode() + b'\x1b[201~\r', received
                print(source, "prompt: PASS")
            status, tree = http(port, "/v1/workspaces?mux=herdr:smoke")
            assert status == 200 and tree.get("groups"), (status, tree)
            for group in tree['groups']:
                for child in group['children']:
                    from urllib.parse import urlencode
                    query = urlencode({'mux':'herdr:smoke','groupId':group['id'],'childId':child['id']})
                    status, panes = http(port, '/v1/workspaces/panes?' + query)
                    assert status == 200 and panes.get('panes'), (status, panes)
                    pane = panes['panes'][0]
                    print('workspace pane:', json.dumps(pane))
                    status, focused = http(port, '/v1/workspaces/focus?mux=herdr:smoke',
                        {'workspaceId':group['id'],'tabId':child['id'],'paneId':pane['id']})
                    assert status == 200, (status, focused)
            print("workspace tree and focus: PASS")
            print("Moshi binary smoke test: PASS")
            # Optional manual browser verification against these disposable agents.
            # No real chats, credentials, or model calls enter this web frontend.
            if ready_file := os.environ.get('NEORDR_BROWSER_READY_FILE'):
                with socket.socket() as reserve:
                    reserve.bind(('127.0.0.1', 0))
                    web_port = reserve.getsockname()[1]
                start([HOOK, '--no-open', '--listen', f'127.0.0.1:{web_port}'], 'web')
                from browser_proxy import start_proxy
                proxy = start_proxy(web_port, port)
                ready = Path(ready_file)
                ready.write_text(json.dumps({'url': f'http://127.0.0.1:{proxy.server_address[1]}', 'root': str(root)}))
                deadline = time.monotonic() + 300
                while ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.2)
                proxy.shutdown()
                proxy.server_close()
        except Exception:
            for path in root.glob("*.log"):
                print(path.name, path.read_text()[-5000:])
            for path in (root / "state").rglob("*.json"):
                print(path.relative_to(root), path.read_text()[:2000])
            raise
        finally:
            for process, _ in reversed(processes):
                process.terminate()
            for process, log in reversed(processes):
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                log.close()


if __name__ == "__main__":
    main()
