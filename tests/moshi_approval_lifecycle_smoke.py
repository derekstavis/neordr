#!/usr/bin/env python3
"""Approval delivery AND Chat's agent-status subscription, using disposable agents.

No native/web UI, model calls, real approvals, or cloud pairing are involved.
Run with NEORDR_BIN=target/release/neordr; MOSHI_HOOK_BIN selects the hook.
NEORDR_APPROVAL_CONCURRENCY=3 also checks simultaneous duplicate submission;
this deliberately fails against moshi-hook 0.4.30's observed duplicate race.
"""
import base64
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import socket
import struct
import tempfile
import threading
import time

from approval_smoke import Rig
from moshi_smoke import eventually, http, rpc


class JSONStream:
    """Read bounded JSON websocket messages, including ping and fragmentation."""
    def __init__(self, port, path):
        self.frames = []
        self.error = None
        self.stopping = False
        self.client = socket.create_connection(('127.0.0.1', port), timeout=10)
        self.reader = self.client.makefile('rb')
        key = base64.b64encode(os.urandom(16)).decode()
        self.client.sendall((f'GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n'
                            'Upgrade: websocket\r\nConnection: Upgrade\r\n'
                            f'Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
        try:
            assert b'101' in self.reader.readline()
            while True:
                line = self.reader.readline()
                assert line, 'EOF during websocket handshake'
                if line == b'\r\n':
                    break
            self.client.settimeout(None)
        except BaseException:
            self.reader.close()
            self.client.close()
            raise
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()

    def send(self, data, opcode=1):
        mask = os.urandom(4)
        header = (bytes([0x80 | opcode, 0x80 | len(data)]) if len(data) < 126 else
                  bytes([0x80 | opcode, 0xfe]) + struct.pack('!H', len(data)))
        self.client.sendall(header + mask + bytes(v ^ mask[i % 4] for i, v in enumerate(data)))

    def drain(self):
        try:
            message = bytearray()
            while True:
                head = self.reader.read(2)
                assert len(head) == 2, 'event stream closed'
                size, opcode = head[1] & 127, head[0] & 15
                assert not head[1] & 128, 'server frames must not be masked'
                if size == 126:
                    size = struct.unpack('!H', self.reader.read(2))[0]
                elif size == 127:
                    size = struct.unpack('!Q', self.reader.read(8))[0]
                assert size <= 8 * 1024 * 1024, size
                data = self.reader.read(size)
                assert len(data) == size, 'truncated event frame'
                if opcode == 9:
                    self.send(data, 10)
                    continue
                if opcode == 10:
                    continue
                assert opcode in (0, 1), f'unexpected websocket opcode {opcode}'
                message.extend(data)
                if not head[0] & 128:
                    continue
                value = json.loads(message)
                message.clear()
                self.record(value)
        except Exception as error:
            if not self.stopping:
                self.error = error

    def record(self, value):
        self.frames.append((time.monotonic(), value))

    def wait(self, predicate, after=0):
        def matching():
            if self.error:
                raise AssertionError(f'event reader failed: {self.error}')
            return next((frame for frame in reversed(self.frames[after:]) if predicate(frame[1])), None)
        return eventually(matching, seconds=8)

    def close(self):
        self.stopping = True
        try:
            self.client.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.thread.join(timeout=2)
        self.reader.close()
        self.client.close()
        assert not self.thread.is_alive(), 'event reader did not stop'


class AgentWatch(JSONStream):
    """Use the same /events watch request as Moshi's bundled Chat client."""
    def __init__(self, port, session, mux='herdr:approval'):
        super().__init__(port, '/events')
        self.send(json.dumps({'watch': {'agent': {'source': 'codex', 'session': session},
                                        'mux': mux, 'workspaces': False,
                                        'context': False, 'usage': False}}).encode())

    def record(self, value):
        if 'agentStatus' in value:
            super().record(value['agentStatus'])


COMMAND = 'printf neordr-permission'
SCREEN = ('Would you like to run the following command?\n$ ' + COMMAND +
          '\n1. Yes, proceed (y)\n2. No, and tell Codex what to do differently (esc)')
WORKING = '• Working (3s • esc to interrupt)\n› '


def permission(rig, name, action):
    event = {'hook_event_name': 'PermissionRequest', 'tool_name': 'exec_command',
             'tool_use_id': action, 'action_id': action,
             'tool_input': {'command': COMMAND, 'cmd': COMMAND, 'description': COMMAND}}
    rig.show(name, SCREEN, event)
    return event


def prompt_lifecycle(rig):
    """Plan delivery and the current Codex async-question transcript sequence."""
    # Do not mix fake plan hooks with question-only transcript history: Moshi's
    # background transcript watcher can legitimately supersede those hook states.
    plan_name, question_name = 'plan-codex', 'question-codex'
    rig.agent('codex', name=plan_name)
    agent = rig.agent('codex', name=question_name)
    stream = JSONStream(rig.port, f'/v1/transcripts?source=codex&session={agent["session"]}&limit=100')
    replies = []

    def append(row):
        with agent['path'].open('a') as out:
            out.write(json.dumps({'type': 'response_item', 'payload': row}) + '\n')

    def contains(packet, row):
        return any(entry.get('raw', {}).get('payload') == row for entry in packet.get('entries', []))

    try:
        stream.wait(lambda packet: packet.get('type') == 'backlog')
        for columns in [120, 32]:
            for option in [0, 1]:
                rig.resize(plan_name, columns)
                rig.show(plan_name, WORKING, {'hook_event_name': 'UserPromptSubmit', 'prompt': 'Synthetic turn'},
                         title='⠋ Working')
                call = f'plan-{columns}-{option}'
                plan = 'Add a blue navigation bar.'
                options = ['Yes, implement this plan', 'No, keep planning']
                path = rig.root / 'plan.md'
                path.write_text(plan)
                event = {'tool_name': 'ExitPlanMode', 'tool_use_id': call,
                         'tool_input': {'plan': plan, 'planFilePath': str(path)}}
                screen = 'Plan approval\n' + plan + '\n1. ' + options[0] + '\n2. ' + options[1] + '\nEnter to select'
                # Codex emits this OSC title while blocked. Without real blocker
                # chrome this synthetic menu races hook state against idle detection.
                rig.show(plan_name, screen, {**event, 'hook_event_name': 'PreToolUse'}, title='Action Required')
                rig.show(plan_name, screen, {**event, 'hook_event_name': 'PermissionRequest'}, title='Action Required')
                try:
                    eventually(lambda: 'planMenu' in rig.context(plan_name).get('agent', {}))
                except AssertionError:
                    context = rig.context(plan_name).get('agent', {})
                    print('missing plan menu', call, {k: v for k, v in context.items() if k != 'commands'})
                    print('plan screen', rpc(rig.endpoint, 'pane.read',
                          {'pane_id': rig.agents[plan_name]['pane'], 'source': 'recent_unwrapped'}))
                    record = rig.root / 'state/codex-sessions' / (rig.agents[plan_name]['session'] + '.json')
                    print('plan hook state', record.read_text())
                    raise
                body = {'toolUseId': call, 'prompt': {'title': 'Plan approval', 'plan': plan,
                                                     'options': options, 'optionIndex': option}}
                rig.answer(plan_name, '/v1/plans/answer', body, str(option + 1).encode())
                rig.show(plan_name, WORKING, {**event, 'hook_event_name': 'PostToolUse'}, title='⠋ Working')
                eventually(lambda: 'planMenu' not in rig.context(plan_name).get('agent', {}))
                rig.answer(plan_name, '/v1/plans/answer', body, b'', status=409)

                rig.resize(question_name, columns)
                call = f'question-{columns}-{option}'
                question = 'Which synthetic color?'
                arguments = {'questions': [{'title': question, 'options': ['Blue', 'Green']}]}
                invocation = {'type': 'function_call', 'name': 'request_user_input_async',
                              'call_id': call, 'arguments': json.dumps(arguments)}
                accepted = {'type': 'function_call_output', 'call_id': call, 'output': '{"accepted":true}'}
                append(invocation)
                append(accepted)
                stream.wait(lambda packet: contains(packet, invocation))
                stream.wait(lambda packet: contains(packet, accepted))
                tool = {'tool_name': 'request_user_input_async', 'tool_use_id': call, 'tool_input': arguments}
                rig.show(question_name, 'Question 1/1\n' + question + '\n1. Blue\n2. Green\nEnter to submit',
                         {**tool, 'hook_event_name': 'PreToolUse'})
                body = {'toolUseId': call, 'async': True,
                        'questions': [{'id': 'color', 'header': 'Color', 'question': question,
                                       'multiSelect': False, 'options': ['Blue', 'Green']}],
                        'answers': [{'questionId': 'color', 'optionIndexes': [option]}]}
                rig.answer(question_name, '/v1/questions/answer', body, str(option + 1).encode())
                # This is a later user-message record, NOT a second function_call_output.
                # Its shape matches the live October 16 Codex answers; content is fabricated.
                item_id = json.dumps(['request_user_input_async', call, 0], separators=(',', ':')).replace('"', '\\"')
                text = '<send_user_message_question_reply>\n' + json.dumps([
                    {'answer': ['Blue', 'Green'][option], 'question': question, 'questionItemId': item_id}
                ]) + '\n</send_user_message_question_reply>'
                reply = {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}
                time.sleep(.25)
                append(reply)
                replies.append(reply)
                stream.wait(lambda packet: contains(packet, reply))
                rig.show(question_name, WORKING, {**tool, 'hook_event_name': 'PostToolUse'})
                rig.answer(question_name, '/v1/questions/answer', body, b'', status=409)
            print(f'Codex {columns} columns: plan choices/dismissal, async-question delivery and '
                  'accepted-then-user-reply transcript streaming PASS', flush=True)
        reconnect = JSONStream(rig.port, f'/v1/transcripts?source=codex&session={agent["session"]}&limit=100')
        try:
            _, backlog = reconnect.wait(lambda packet: packet.get('type') == 'backlog')
            assert all(contains(backlog, reply) for reply in replies), 'answers lost after transcript reconnect'
        finally:
            reconnect.close()
    finally:
        stream.close()


def main():
    latencies = []
    concurrency = int(os.environ.get('NEORDR_APPROVAL_CONCURRENCY', '1'))
    assert 1 <= concurrency <= 8
    with tempfile.TemporaryDirectory(prefix='napproval-', dir='/tmp') as directory:
        rig = Rig(Path(directory).resolve())
        watches = []
        try:
            rig.setup()
            a = rig.agent('codex')
            b = rig.agent('codex', name='other-codex')
            for session in [a['session'], a['session'], b['session']]:
                watch = AgentWatch(rig.port, session)
                watches.append(watch)
                watch.wait(lambda state: 'status' in state)
            left, mirror, other = watches
            permission(rig, 'other-codex', 'other-pending')
            other.wait(lambda s: s.get('pendingApproval', {}).get('actionId') == 'other-pending')
            untouched = rig.input('other-codex')
            previous = None
            for columns in [120, 32]:
                rig.resize('codex', columns)
                for iteration in range(6):
                    action = f'approval-{columns}-{iteration}'
                    decision = 'approve' if iteration % 2 == 0 else 'deny'
                    expected = b'y' if decision == 'approve' else b'\x1b'
                    event = permission(rig, 'codex', action)
                    for watch in [left, mirror]:
                        watch.wait(lambda s: s.get('pendingApproval', {}).get('actionId') == action)
                    body = {'source': 'codex', 'sessionId': a['session'],
                            'actionId': action, 'decision': decision}
                    if previous:
                        status, _ = http(rig.port, '/v1/approvals/answer', {**body, 'actionId': previous})
                        assert status == 409, ('stale answer', status)
                    before = rig.input('codex')
                    offsets = [len(w.frames) for w in [left, mirror]]
                    start = time.monotonic()
                    # Opt-in strict race check: only one concurrent answer may deliver keys.
                    with ThreadPoolExecutor(max_workers=concurrency) as pool:
                        responses = list(pool.map(lambda _: http(rig.port, '/v1/approvals/answer', body), range(concurrency)))
                    eventually(lambda: len(rig.input('codex')) > len(before))
                    assert sorted(status for status, _ in responses) == [200] + [409] * (concurrency - 1), (
                        responses, 'delivered bytes', rig.input('codex')[len(before):])
                    eventually(lambda: rig.input('codex') == before + expected)
                    assert http(rig.port, '/v1/approvals/answer', body)[0] == 409, 'sequential duplicate accepted'
                    # Simulate a slow terminal repaint independently of hook completion.
                    delay = [0, .25, 1][iteration % 3]
                    time.sleep(delay)
                    rig.show('codex', WORKING)
                    for watch, offset in zip([left, mirror], offsets):
                        completed = watch.wait(lambda s: not s.get('pendingApproval'), after=offset)
                        latencies.append(completed[0] - start)
                    rig.show('codex', WORKING, {**event, 'hook_event_name': 'PostToolUse'})
                    assert rig.input('codex') == before + expected, 'duplicate input'
                    assert rig.input('other-codex') == untouched, 'cross-pane input'
                    assert other.frames[-1][1].get('pendingApproval', {}).get('actionId') == 'other-pending'
                    # Reconnect while no approval is pending: initial snapshot must be current.
                    if iteration == 5:
                        reconnect = AgentWatch(rig.port, a['session'], mux='herdr:default')
                        try:
                            reconnect.wait(lambda s: not s.get('pendingApproval'))
                        finally:
                            reconnect.close()
                    previous = action
                print(f'Codex {columns} columns: 6 repeated approve/deny cycles, duplicate checks, '
                      'delayed paint, stale IDs, two subscribers, reconnect and pane isolation PASS', flush=True)
            # Local dismissal must also reach an already subscribed Chat client.
            event = permission(rig, 'codex', 'local-dismissal')
            left.wait(lambda s: s.get('pendingApproval', {}).get('actionId') == 'local-dismissal')
            offset = len(left.frames)
            rig.show('codex', WORKING, {**event, 'hook_event_name': 'PostToolUse'})
            left.wait(lambda s: not s.get('pendingApproval'), after=offset)
            prompt_lifecycle(rig)
            print(f'Approval lifecycle: 12 decisions / {12 * concurrency} submissions + sequential duplicates; '
                  f'completion frames observed within {max(latencies):.3f}s of submission; '
                  'local dismissal PASS', flush=True)
        except Exception:
            for path in rig.root.glob('*.log'):
                print(path.name, path.read_text()[-3000:])
            for watch in watches:
                print('agent-status tail', [{k: v for k, v in s.items() if k != 'commands'}
                                            for _, s in watch.frames[-5:]])
            raise
        finally:
            try:
                for watch in watches:
                    watch.close()
            finally:
                rig.close()


if __name__ == '__main__':
    main()
