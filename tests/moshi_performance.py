#!/usr/bin/env python3
"""Opt-in Moshi gateway timings on disposable editors and synthetic transcripts.

Measures protocol readiness, not browser paint, SSH or Mosh. NEORDR_BIN selects
the build; MOSHI_HOOK_BIN selects the installed hook. No real conversations are
read or changed. RPC tracing records methods and durations, never message text.
"""
import argparse
import base64
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import socket
import socketserver
import statistics
import struct
import tempfile
import threading
import time
from urllib.parse import urlencode

from approval_smoke import Rig
from moshi_smoke import BIN, HOOK, eventually, http, lua, rpc


class RpcTrace:
    def __init__(self, path):
        self.records = []
        upstream = path.with_suffix('.upstream')
        path.rename(upstream)
        records = self.records

        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                try:
                    self.request.settimeout(10)
                    line = self.rfile.readline(4 * 1024 * 1024)
                    if not line:
                        return
                    request = json.loads(line)
                    start = time.perf_counter()
                    with socket.socket(socket.AF_UNIX) as peer:
                        peer.settimeout(10)
                        peer.connect(str(upstream))
                        peer.sendall(line)
                        with peer.makefile('rb') as reader:
                            reply = reader.readline(4 * 1024 * 1024)
                    records.append({'endpoint': path.parent.name, 'method': request['method'],
                                    'ms': (time.perf_counter() - start) * 1000})
                    self.wfile.write(reply)
                except (OSError, ValueError):
                    pass  # Clients can close a readiness probe without a frame.

        self.server = socketserver.ThreadingUnixStreamServer(str(path), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class WebSocket:
    def __init__(self, port, path):
        self.client = socket.create_connection(('127.0.0.1', port), timeout=15)
        self.reader = self.client.makefile('rb')
        key = base64.b64encode(os.urandom(16)).decode()
        self.client.sendall((f'GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n'
            f'Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n'
            'Sec-WebSocket-Version: 13\r\n\r\n').encode())
        status = self.reader.readline()
        if b'101' not in status:
            self.close()
            raise AssertionError(status)
        while True:
            line = self.reader.readline()
            if line == b'\r\n':
                break
            if not line:
                self.close()
                raise EOFError('WebSocket headers truncated')

    def frame(self):
        header = self.reader.read(2)
        if len(header) != 2:
            raise EOFError('WebSocket closed')
        length = header[1] & 127
        if length == 126:
            length = struct.unpack('!H', self.reader.read(2))[0]
        elif length == 127:
            length = struct.unpack('!Q', self.reader.read(8))[0]
        assert not header[1] & 128 and length <= 32 * 1024 * 1024
        data = self.reader.read(length)
        assert len(data) == length
        return header[0] & 15, data

    def close(self):
        try:
            self.client.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.reader.close()
        self.client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--samples', type=int, default=10)
    parser.add_argument('--editors', type=int, default=6)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.editors < 1:
        parser.error('samples and editors must be positive')
    results = {}
    with tempfile.TemporaryDirectory(prefix='nmp-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        rig = Rig(root)
        rig.env['XDG_STATE_HOME'] = str(root / 'nvim-state')
        traces = []
        try:
            rig.setup()
            for i in range(1, args.editors):
                address = root / f'n{i}.sock'
                rig.start(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '--listen', str(address)], f'n{i}')
                eventually(address.exists)
                rig.start([str(BIN), '--session', f'e{i}', 'serve', '--nvim', str(address)], f'b{i}')
                endpoint = root / f'config/herdr/sessions/e{i}/herdr.sock'
                eventually(lambda: endpoint.exists() and rpc(endpoint, 'ping'))
            for source in ['claude', 'codex']:
                rig.agent(source)
            router = root / 'config/herdr/herdr.sock'
            for path in [router, rig.endpoint]:
                traces.append(RpcTrace(path))

            def measure(name, fn, cleanup=lambda: None):
                values, observations, calls = [], [], []
                for i in range(args.samples + 1):
                    offsets = [len(t.records) for t in traces]
                    start = time.perf_counter()
                    observation = fn()
                    elapsed = (time.perf_counter() - start) * 1000
                    batch = [r for t, offset in zip(traces, offsets) for r in t.records[offset:]]
                    cleanup()  # Exclude UI detachment from readiness time.
                    if i == 0:
                        first = elapsed
                    else:
                        values.append(elapsed)
                        observations.append(observation)
                        calls.append(batch)
                result = {'first_ms': first, 'median_ms': statistics.median(values),
                          'p95_ms': sorted(values)[math.ceil(.95 * len(values)) - 1],
                          'values_ms': values, 'observations': observations, 'rpc': calls}
                results[name] = result
                print(name, json.dumps({k: result[k] for k in ['first_ms', 'median_ms', 'p95_ms']}),
                      'RPC', dict(Counter(r['endpoint'] + ':' + r['method'] for batch in calls for r in batch)), flush=True)

            def get(path):
                status, body = http(rig.port, path)
                assert status == 200, (status, body)
                return len(json.dumps(body))

            measure('router.snapshot', lambda: len(rpc(router, 'session.snapshot')['snapshot']['workspaces']))
            measure('gateway.workspaces', lambda: get('/v1/workspaces?mux=herdr:default'))
            _, tree = http(rig.port, '/v1/workspaces?mux=herdr:default')
            pane = rig.agents['codex']['pane']
            group, child = next((g, c) for g in tree['groups'] for c in g['children']
                                if any(p['id'] == pane for p in c.get('panes', [])))
            measure('gateway.panes', lambda: get('/v1/workspaces/panes?' + urlencode(
                {'mux': 'herdr:default', 'groupId': group['id'], 'childId': child['id']})))

            def focus():
                status, body = http(rig.port, '/v1/workspaces/focus?mux=herdr:default',
                    {'workspaceId': group['id'], 'tabId': child['id'], 'paneId': pane})
                assert status == 200, (status, body)
                return status
            measure('gateway.focus', focus)
            for source in rig.agents:
                agent = rig.agents[source]
                for count in [100, 10000]:
                    rows = []
                    if source == 'codex':
                        rows.append(json.loads(agent['path'].read_text().splitlines()[0]))
                    for i in range(count):
                        message = f'SYNTHETIC_PERFORMANCE_{i} ' + ('x' * 200)
                        if source == 'claude':
                            row = {'type': 'user', 'sessionId': agent['session'], 'uuid': str(i),
                                   'timestamp': '2026-10-01T00:00:00Z',
                                   'message': {'role': 'user', 'content': message}}
                        else:
                            row = {'type': 'response_item', 'timestamp': '2026-10-01T00:00:00Z',
                                   'payload': {'type': 'message', 'role': 'user',
                                               'content': [{'type': 'input_text', 'text': message}]}}
                        rows.append(row)
                    agent['path'].write_text(''.join(json.dumps(r) + '\n' for r in rows))

                    def backlog():
                        ws = WebSocket(rig.port, '/v1/transcripts?' + urlencode(
                            {'source': source, 'session': agent['session']}))
                        try:
                            opcode, data = ws.frame()
                            assert opcode == 1
                            message = json.loads(data)
                            assert message['type'] == 'backlog', message.get('type')
                            assert message.get('entries'), 'missing synthetic transcript'
                            return {'bytes': len(data), 'entries': len(message['entries']),
                                    'hasMore': message.get('hasMore')}
                        finally:
                            ws.close()
                    measure(f'gateway.transcript.{source}.{count}', backlog)

            agent = rig.agents['codex']
            rig.show('codex', 'SYNTHETIC_TERMINAL_READY')

            def terminal():
                ws = WebSocket(rig.port, '/v1/pty?' + urlencode({'mux': 'herdr',
                    'pane': agent['pane'], 'cols': 100, 'rows': 30}))
                try:
                    data = b''
                    for _ in range(100):
                        opcode, chunk = ws.frame()
                        assert opcode != 8, chunk
                        data += chunk
                        if b'SYNTHETIC_TERMINAL_READY' in data:
                            return len(data)
                    raise AssertionError('terminal marker missing')
                finally:
                    ws.close()
            measure('gateway.terminal.new_connection', terminal,
                    lambda: eventually(lambda: lua(rig.env, rig.address, 'return #vim.api.nvim_list_uis()') == '0'))
            version = http(rig.port, '/v1/version')[1]
        finally:
            for trace in traces:
                trace.close()
            rig.close()
    args.output.write_text(json.dumps({'platform': platform.platform(), 'binary': str(BIN),
        'sha256': hashlib.sha256(BIN.read_bytes()).hexdigest(), 'hook': HOOK, 'version': version,
        'editors': args.editors, 'results': results}, indent=2) + '\n')


if __name__ == '__main__':
    main()
