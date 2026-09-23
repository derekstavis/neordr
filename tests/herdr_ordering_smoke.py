#!/usr/bin/env python3
"""Exact input checks shared by the real Herdr TUI and native endpoint tests.

All receivers run inside disposable Neovim terminal jobs. Capturing raw input
avoids mistaking echoed commands or redraw output for correctly delivered bytes.
"""
import json
import os
from pathlib import Path
import shlex
import sys
import time
from moshi_smoke import eventually, rpc


def receiver(root, endpoint, pane, name):
    capture, ready = root / (name + '.input'), root / (name + '.ready')
    program = root / (name + '.py')
    program.write_text(
        'import os, tty\nfrom pathlib import Path\ntty.setraw(0)\n'
        "os.write(1,b'\\x1b[?2004h')\n"
        f'Path({str(ready)!r}).touch()\n'
        f'with open({str(capture)!r}, "ab", buffering=0) as output:\n'
        ' while True:\n'
        '  data = os.read(0,65536)\n'
        '  if not data: break\n'
        '  output.write(data)\n')
    command = 'exec ' + shlex.quote(sys.executable) + ' ' + shlex.quote(str(program)) + '\r'
    rpc(endpoint, 'pane.send_text', {'pane_id': pane['pane_id'], 'text': command})
    eventually(ready.exists)
    eventually(capture.exists)
    return capture


def exact(capture, expected, seconds=15):
    try:
        eventually(lambda: capture.read_bytes() == expected, seconds=seconds)
    except AssertionError:
        actual = capture.read_bytes()
        mismatch = next((i for i, pair in enumerate(zip(actual, expected)) if pair[0] != pair[1]),
                        min(len(actual), len(expected)))
        raise AssertionError(f'{capture.name}: received {len(actual)} of {len(expected)} bytes; '
                             f'first difference at {mismatch}; '
                             f'actual {actual[max(0,mismatch-40):mismatch+80]!r}; '
                             f'expected {expected[max(0,mismatch-40):mismatch+80]!r}') from None


def write_all(fd, data):
    while data:
        data = data[os.write(fd, data):]



def check_hit_enter(master, evaluate, output):
    start = len(output)
    write_all(master, b":set nomore | for i in range(100) | echo 'HERDR-PROMPT' | endfor\r")
    eventually(lambda: b'Press ENTER' in output[start:], seconds=15)
    write_all(master, b'\r')
    eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode').startswith('n'))
    print('Actual Herdr TUI: hit-enter prompt dismisses through semantic input: PASS')

def check_tui_ordering(root, endpoint, master, evaluate, panes):
    first, second = panes
    capture_a = receiver(root, endpoint, first, 'herdr-a')
    capture_b = receiver(root, endpoint, second, 'herdr-b')
    rpc(endpoint, 'pane.focus', {'pane_id': first['pane_id']})
    eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode') == 't')
    # Allow Herdr to consume the initial projection. There is deliberately no
    # focus poll or sleep between the text and native switching below.
    time.sleep(.6)
    write_all(master, b'before-switch\x02nafter-switch')
    exact(capture_a, b'before-switch')
    eventually(lambda: next(p['pane_id'] for p in rpc(endpoint, 'pane.list')['panes'] if p['focused']) == second['pane_id'])
    time.sleep(.6)
    # Herdr 0.9.1 classifies the remaining batch against its old snapshot.
    # Neordr must reject that stale target; routing it to B would be unsafe.
    exact(capture_b, b'')
    write_all(master, b'fresh-after-switch')
    exact(capture_b, b'fresh-after-switch')
    write_all(master, b'\x02p')
    eventually(lambda: next(p['pane_id'] for p in rpc(endpoint, 'pane.list')['panes'] if p['focused']) == first['pane_id'])
    time.sleep(.4)
    # This uses the real client's Paste event and nvim_paste. It is separate
    # from the native TextCommit case, which exercises nvim_input backpressure.
    payload = ('large-paste-<literal>-é-' * 4096).encode()
    write_all(master, b'\x1b[200~' + payload + b'\x1b[201~')
    exact(capture_a, b'before-switch\x1b[200~' + payload + b'\x1b[201~')
    exact(capture_b, b'fresh-after-switch')
    print('Actual Herdr TUI: stale post-switch batch rejected, fresh input and large bracketed paste: PASS')
    return capture_a, capture_b


def check_native_ordering(root, endpoint, evaluate, panes, captures, reference_master, reference_output, env, log, children):
    """Deterministic semantic messages encoded with Herdr's upstream Rust types.

    Build the driver with `cargo build --example herdr_contract_fixture`.
    The real TUI test above covers UI integration; this driver can explicitly
    select each event's target independently from client snapshot latency.
    """
    import queue
    import subprocess
    import threading
    from moshi_smoke import BIN
    fixture = Path(os.environ.get('HERDR_FIXTURE_BIN', BIN.parent / 'examples/herdr_contract_fixture'))
    assert fixture.exists(), f'Build {fixture.name} with cargo build --example herdr_contract_fixture'
    native = endpoint.with_name('herdr-client.sock')
    driver = subprocess.Popen([str(fixture), str(native)], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=log, text=True, env=env)
    children.append(driver)
    messages = queue.Queue()

    def drain():
        for line in driver.stdout:
            messages.put(json.loads(line))
    threading.Thread(target=drain, daemon=True).start()

    def send(*messages):
        driver.stdin.write(''.join(json.dumps(message) + '\n' for message in messages))
        driver.stdin.flush()

    def snapshot(predicate=lambda value: True):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            message = messages.get(timeout=max(.01, deadline-time.monotonic()))
            control = message.get('EndpointControl')
            if control and control['kind'] == 'shell.snapshot.v1':
                value = json.loads(control['data'])
                if predicate(value): return value
        raise AssertionError('Missing native snapshot')

    hello = dict(generation=1, cell_width_px=0, cell_height_px=0,
                 surface_size=dict(cols=95, rows=30), pixel_mouse=False,
                 direct_graphics=False, endpoint_keybindings=False, mouse_capture=False,
                 snapshot_codecs=['shell.snapshot.v1'], surface_codecs=['shell.surface.v1'],
                 input_codecs=['shell.input.semantic.v1'], blob_codecs=['shell.blob.v1'])
    send({'EndpointControl': {'kind': 'endpoint.hello.v1', 'data': json.dumps(hello)}})
    initial = snapshot()
    first, second = panes
    capture_a, capture_b = captures
    expected_a, expected_b = capture_a.read_bytes(), capture_b.read_bytes()
    counter = 0

    def operation(method, params):
        nonlocal counter
        counter += 1
        request_id = f'ordering-{counter}'
        request = dict(id=request_id, method=method, params=params)
        return {'ClientShellEndpointRequest': {'boot_id': initial['boot_id'], 'request': json.dumps(request)}}, request_id

    def focus(pane):
        return operation('pane.focus', {'pane_id': pane['pane_id']})[0]

    def response(request_id):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            message = messages.get(timeout=max(.01, deadline-time.monotonic()))
            chunk = message.get('ClientShellEndpointResponseChunk')
            if chunk and chunk['request_id'] == request_id:
                assert chunk['final_chunk'], chunk
                return json.loads(bytes(chunk['data']))
        raise AssertionError('Missing endpoint response ' + request_id)

    def text(pane, text):
        return {'ClientShellPaneInput': {'pane_id': pane['pane_id'], 'events': [{'TextCommit': text}]}}

    send(focus(first))
    if initial['focused_pane_id'] != first['pane_id']:
        snapshot(lambda s: s['focused_pane_id'] == first['pane_id'])
    eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode') == 't')
    # One semantic TextCommit exceeds Neovim's input buffer; it must not be
    # confused with a complete bracketed paste or many small keyboard events.
    payload = 'input-<literal>-é-界-' * 8192
    send(text(first, payload))
    expected_a += payload.encode()
    exact(capture_a, expected_a, seconds=30)
    exact(capture_b, expected_b)
    operations = []
    for index in range(20):
        a, b = f'A{index:02d}-', f'B{index:02d}-'
        operations.extend([text(first, a), focus(second), text(second, b), focus(first)])
        expected_a += a.encode()
        expected_b += b.encode()
    # No focus polls or sleeps between any of these native messages.
    send(*operations)
    exact(capture_a, expected_a, seconds=30)
    exact(capture_b, expected_b, seconds=30)
    # Read-only operations must work while Neovim waits at a blocking prompt;
    # a focus operation must reject without consuming the prompt or closing UI.
    start = len(reference_output)
    send(text(first, "\x1c\x0e:set nomore | for i in range(100) | echo 'NATIVE-PROMPT' | endfor\r"))
    eventually(lambda: b'Press ENTER' in reference_output[start:])
    for method in ['session.snapshot', 'pane.list']:
        message, request_id = operation(method, {})
        send(message)
        result = response(request_id)
        assert 'error' not in result, result
    message, request_id = operation('pane.focus', {'pane_id': second['pane_id']})
    send(message)
    assert response(request_id)['error']['code'] == 'busy'
    send(text(first, '\r'))
    eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode').startswith('n'))
    send(text(first, 'i'))
    eventually(lambda: evaluate('return vim.api.nvim_get_mode().mode') == 't')
    exact(capture_a, expected_a)
    exact(capture_b, expected_b)
    # The independently attached Neovim remote UI changes the active terminal.
    # Input still carrying the driver's previous target must fail closed.
    command = ("<C-\\><C-N>:lua require('neordr.backend').dispatch('pane.focus', {pane_id=" +
               json.dumps(second['pane_id']) + "}, vim.g.neordr_generation)<CR>")
    # Send literal terminal escape/command bytes through that UI, not an RPC
    # mutation on the endpoint connection under test.
    command = command.replace('<C-\\><C-N>', '\x1c\x0e').replace('<CR>', '\r')
    write_all(reference_master, command.encode())
    eventually(lambda: next(p['pane_id'] for p in rpc(endpoint, 'pane.list')['panes'] if p['focused']) == second['pane_id'])
    send(text(first, 'MUST-NOT-REACH-EITHER-JOB'))
    time.sleep(.6)
    exact(capture_a, expected_a)
    exact(capture_b, expected_b)
    send(focus(first), text(first, 'fresh-target'))
    expected_a += b'fresh-target'
    exact(capture_a, expected_a)
    exact(capture_b, expected_b)
    send('Detach')
    driver.stdin.close()
    assert driver.wait(timeout=5) == 0
    eventually(lambda: evaluate('return #vim.api.nvim_list_uis()') == '1')
    print('Native typed Herdr: large TextCommit, 20 adjacent focus/input pairs, modal read/busy recovery, second-UI stale rejection: PASS')
