#!/usr/bin/env python3
"""Verify automatic router startup, editor restart, and crash recovery."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
from moshi_smoke import BIN, ROOT, eventually, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-lifecycle-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root / 'config'),
               'NVIM_LOG_FILE': str(root / 'nvim.log')}
        for key in ['NVIM', 'HERDR_SESSION', 'HERDR_ENV', 'HERDR_PANE_ID']:
            env.pop(key, None)
        address = root / 'nvim.sock'
        router = root / 'config/herdr/herdr.sock'
        editor = None
        router_pids = set()
        log = open(root / 'startup.log', 'w')

        def ping():
            result = rpc(router, 'ping')
            assert result['neordr_router']
            router_pids.add(result['pid'])
            return result['pid']

        def start():
            nonlocal editor
            process = subprocess.Popen(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE',
                '--listen', str(address)], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            editor = process
            eventually(lambda: address.exists())
            lua(env, address, f"vim.opt.runtimepath:prepend({json.dumps(str(ROOT))}); "
                f"require('neordr').setup({{bin={json.dumps(str(BIN))}}}); return true")
            eventually(ping)
            return process

        try:
            editor = start()
            initial_pid = ping()
            assert lua(env, address, "local old=require('neordr'); local job=old.job; "
                f"local updated=assert(loadfile({json.dumps(str(ROOT / 'lua/neordr/init.lua'))}))(); "
                f"updated.setup({{bin={json.dumps(str(BIN))}}}); "
                "return updated==old and updated.job==job and updated.router_timer:is_active()") == 'true'
            lua(env, address, "vim.schedule(function() vim.cmd('qa!') end); return true")
            assert editor.wait(timeout=5) == 0
            eventually(lambda: not address.exists())
            assert ping() == initial_pid, 'router died with its launching editor'
            editor = start()
            assert ping() == initial_pid, 'restart should reuse the existing router'
            # A crash leaves a socket and ownership marker, just like the reported failure.
            os.kill(initial_pid, signal.SIGKILL)
            eventually(lambda: ping() != initial_pid, seconds=12)
            agent = root / 'claude'
            received = root / 'received'
            ready = root / 'ready'
            agent.write_text('import os,tty\nfrom pathlib import Path\ntty.setraw(0)\n'
                f'Path({str(ready)!r}).touch()\n'
                f'with open({str(received)!r},"ab",buffering=0) as f:\n'
                ' while True:\n  f.write(os.read(0,4096))\n')
            pane = lua(env, address, "local b=vim.api.nvim_get_current_buf(); "
                "local e=require('neordr').prepare(b); "
                f"vim.fn.jobstart({{'python3',{json.dumps(str(agent))}}},{{term=true,env=e}}); "
                "return e.HERDR_PANE_ID")
            eventually(ready.exists)
            rpc(router, 'pane.send_text', {'pane_id': pane, 'text': 'recovered-input'})
            eventually(lambda: received.exists() and received.read_bytes() == b'recovered-input')
            print('Automatic router startup, editor restart, crash recovery, and routed input: PASS')
        finally:
            if editor is not None and editor.poll() is None:
                editor.terminate()
                editor.wait(timeout=5)
            for pid in router_pids:
                try: os.kill(pid, signal.SIGTERM)
                except ProcessLookupError: pass
            log.close()


if __name__ == '__main__':
    main()
