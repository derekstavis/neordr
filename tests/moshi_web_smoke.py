#!/usr/bin/env python3
"""Run the stock Moshi web UI against isolated synthetic agent sessions."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def browser():
    if override := os.environ.get('CHROME_BIN'):
        return override
    for candidate in ('chromium', 'chromium-browser', 'google-chrome',
                      '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'):
        if found := shutil.which(candidate):
            return found
    raise SystemExit('Chromium or Chrome is required; set CHROME_BIN to its executable')


def main():
    node = shutil.which('node')
    if not node:
        raise SystemExit('Node 22+ is required')
    with tempfile.TemporaryDirectory(prefix='neordr-web-test-', dir='/tmp') as directory:
        ready = Path(directory) / 'ready.json'
        env = {**os.environ, 'NEORDR_BROWSER_READY_FILE': str(ready), 'CHROME_BIN': browser()}
        fixture = subprocess.Popen([sys.executable, '-u', str(ROOT / 'tests/moshi_smoke.py')], env=env)
        try:
            deadline = time.monotonic() + 180
            while not ready.exists() and time.monotonic() < deadline:
                if fixture.poll() is not None:
                    raise RuntimeError(f'Moshi fixture exited {fixture.returncode}')
                time.sleep(.1)
            if not ready.exists():
                raise TimeoutError('Moshi browser fixture did not become ready')
            subprocess.run([node, str(ROOT / 'tests/moshi_web_browser.mjs'), str(ready)],
                           env=env, check=True, timeout=150)
        finally:
            ready.unlink(missing_ok=True)
            try:
                fixture.wait(timeout=15)
            except subprocess.TimeoutExpired:
                fixture.terminate()
                try:
                    fixture.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait()
        if fixture.returncode != 0:
            raise RuntimeError(f'Moshi fixture exited {fixture.returncode} during cleanup')


if __name__ == '__main__':
    main()
