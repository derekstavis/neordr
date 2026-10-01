#!/usr/bin/env python3
"""Opt-in timings on disposable editors. No speed assertions or real sessions."""
import argparse
import concurrent.futures
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import pty
import select
import statistics
import struct
import subprocess
import tempfile
import termios
import time
from moshi_smoke import eventually, lua, rpc


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=20)
    parser.add_argument('--editors', type=int, default=6)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.editors < 1:
        parser.error('--samples and --editors must be positive')
    binary = str(args.bin.resolve())
    results = {}

    def measure(name, fn, count=None):
        fn()  # warmup
        values = []
        for _ in range(count or args.samples):
            start = time.perf_counter()
            fn()
            values.append((time.perf_counter() - start) * 1000)
            time.sleep(.007)
        results[name] = {'samples': len(values), 'median_ms': statistics.median(values),
                         'p95_ms': sorted(values)[math.ceil(.95 * len(values)) - 1],
                         'max_ms': max(values), 'values_ms': values}
        print(name, json.dumps({k: v for k, v in results[name].items() if k != 'values_ms'}), flush=True)

    with tempfile.TemporaryDirectory(prefix='np-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {k: v for k, v in os.environ.items() if k != 'NVIM' and not k.startswith(('HERDR_', 'NEORDR_'))}
        env.update(XDG_CONFIG_HOME=str(root/'c'), XDG_STATE_HOME=str(root/'state'),
                   TERM='xterm-256color', NVIM_LOG_FILE=str(root/'nvim.log'))
        children, addresses, endpoints = [], [], []
        try:
            for i in range(args.editors):
                name = f'bench{i}'
                address = root/f'{name}.sock'
                addresses.append(address)
                children.append(subprocess.Popen(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '--listen', str(address)],
                                env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                eventually(address.exists)
                children.append(subprocess.Popen([binary, '--session', name, 'serve', '--nvim', str(address)],
                                env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                endpoint = root/'c/herdr/sessions'/name/'herdr.sock'
                endpoints.append(endpoint)
                eventually(lambda: endpoint.exists() and rpc(endpoint, 'ping'))
                lua(env, address, "vim.fn.jobstart({'/bin/sh','-c','sleep 600'},{term=true}); return true")
            children.append(subprocess.Popen([binary, 'router'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            router = root/'c/herdr/herdr.sock'
            eventually(lambda: router.exists() and rpc(router, 'ping'))
            snapshot = rpc(endpoints[0], 'session.snapshot')['snapshot']
            assert len(rpc(router, 'session.snapshot')['snapshot']['workspaces']) == args.editors
            workspace = snapshot['workspaces'][0]['workspace_id']
            pane = snapshot['panes'][0]['pane_id']
            rpc(router, 'workspace.focus', {'workspace_id': workspace})
            measure('router.ping', lambda: rpc(router, 'ping'))
            measure('bridge.ping', lambda: rpc(endpoints[0], 'ping'))
            measure('bridge.snapshot', lambda: rpc(endpoints[0], 'session.snapshot'))
            for method in ['session.snapshot', 'workspace.list', 'pane.list']:
                measure('router.'+method, lambda m=method: rpc(router, m))
            measure('router.pane.layout', lambda: rpc(router, 'pane.layout', {'pane_id': pane}))
            measure('router.pane.focus', lambda: rpc(router, 'pane.focus', {'pane_id': pane}))
            measure('cli.session.list', lambda: subprocess.run([binary, 'session', 'list', '--json'], env=env,
                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=True, timeout=10))
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                measure('four_snapshots_batch', lambda: list(pool.map(lambda _: rpc(router, 'session.snapshot'), range(4))), count=6)
            lua(env, addresses[0], "vim.cmd('enew'); vim.api.nvim_buf_set_lines(0,0,-1,false,{'NEORDR_BENCH_SCREEN'}); return true")

            def attach(named):
                master, slave = pty.openpty()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
                p = subprocess.Popen([binary, '--session', 'bench0' if named else 'default'], env=env,
                                     stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
                os.close(slave)
                start = time.perf_counter()
                data = b''
                try:
                    while b'NEORDR_BENCH_SCREEN' not in data:
                        assert time.perf_counter() - start < 8, 'attach frame timeout'
                        if select.select([master], [], [], .1)[0]:
                            data += os.read(master, 65536)
                finally:
                    # Exclude cleanup time from time-to-render measurement.
                    elapsed = time.perf_counter() - start
                    p.terminate(); p.wait(timeout=5); os.close(master)
                return elapsed * 1000

            for named in [True, False]:
                attach(named)
                values = [attach(named) for _ in range(args.samples)]
                name = 'attach.named.render' if named else 'attach.default.render'
                results[name] = {'samples': len(values), 'median_ms': statistics.median(values),
                                 'p95_ms': sorted(values)[math.ceil(.95 * len(values)) - 1], 'values_ms': values}
                print(name, results[name]['median_ms'], flush=True)
        finally:
            for p in reversed(children):
                if p.poll() is None:
                    p.terminate()
                    try: p.wait(timeout=5)
                    except subprocess.TimeoutExpired: p.kill(); p.wait()
    args.output.write_text(json.dumps({'platform': platform.platform(), 'binary': binary,
                                     'sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                                     'editors': args.editors, 'results': results}, indent=2)+'\n')


if __name__ == '__main__':
    main()
