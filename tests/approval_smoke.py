#!/usr/bin/env python3
"""Real Moshi gateway/hooks + Neordr, with simulated agent screens and raw input.

No model calls, paired host, real chats, or user terminal jobs are involved.
This checks local answer delivery, not cloud push or native-client rendering.
"""
import json
from datetime import datetime, timezone
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import uuid

from moshi_smoke import BIN, HOOK, eventually, http, lua, rpc


FIXTURE = r'''
import json, os, select, subprocess, sys, tty
from pathlib import Path
root=Path(sys.argv[1]); source=Path(__file__).name
rollout=open(sys.argv[2])
tty.setraw(0)
capture=open(root/(source+'.input'),'ab',buffering=0)
last=None
while True:
    control=root/(source+'.control')
    if control.exists():
        data=json.loads(control.read_text())
        if data['id']!=last:
            last=data['id']
            os.write(1, ('\x1b[2J\x1b[H'+data['screen'].replace('\n','\r\n')).encode())
            event=data.get('event')
            if event:
                reply=subprocess.run([sys.argv[3],source+'-hook'],input=json.dumps(event),text=True,capture_output=True,timeout=10)
                (root/(source+'.hook')).write_text(reply.stdout+reply.stderr)
                if reply.returncode: raise RuntimeError('hook failed: '+str(reply.returncode))
            (root/(source+'.ack')).write_text(last)
    if select.select([0],[],[],.02)[0]:
        data=os.read(0,4096)
        if not data: break
        capture.write(data)
'''


class Rig:
    def __init__(self, root):
        self.root = root
        self.processes = []
        (root / 'bin').mkdir()
        (root / 'bin/herdr').symlink_to(BIN)
        self.env = {**os.environ, 'HOME': str(root), 'XDG_CONFIG_HOME': str(root / 'config'),
                    'MOSHI_STATE_DIR': str(root / 'state'), 'MOSHI_CONFIG_DIR': str(root / 'state'),
                    'MOSHI_SOCKET_PATH': str(root / 'moshi.sock'), 'MOSHI_API_BASE': 'http://127.0.0.1:9',
                    'CLAUDE_CONFIG_DIR': str(root / '.claude'), 'CODEX_HOME': str(root / 'codex'),
                    'MOSHI_HERDR_PATH': str(root / 'bin/herdr'), 'TMPDIR': str(root),
                    'NVIM_LOG_FILE': str(root / 'nvim.log'), 'NEORDR_TRACE': '1',
                    'PATH': str(root / 'bin') + os.pathsep + os.environ['PATH']}
        for key in ['TMUX', 'TMUX_PANE', 'ZELLIJ', 'HERDR_ENV', 'HERDR_SESSION', 'HERDR_PANE_ID',
                    'HERDR_CONFIG_PATH', 'HERDR_SOCKET_PATH', 'NVIM']:
            self.env.pop(key, None)
        self.address = root / 'nvim.sock'
        self.endpoint = root / 'config/herdr/sessions/approval/herdr.sock'
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            self.port = reserve.getsockname()[1]
        self.agents = {}

    def start(self, args, name):
        log = open(self.root / (name + '.log'), 'w')
        process = subprocess.Popen(args, env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        self.processes.append((process, log))

    def setup(self):
        for setting in ['always-on-discovery', 'usage-collection']:
            subprocess.run([HOOK, 'set', setting, 'off'], env=self.env, check=True, capture_output=True, timeout=10)
        self.start(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '--listen', str(self.address)], 'nvim')
        eventually(lambda: self.address.exists())
        self.start([str(BIN), '--session', 'approval', 'serve', '--nvim', str(self.address)], 'neordr')
        eventually(lambda: self.endpoint.exists() and rpc(self.endpoint, 'ping'))
        self.start([str(BIN), 'router'], 'router')
        eventually(lambda: (self.root / 'config/herdr/herdr.sock').exists())
        self.start([HOOK, 'serve', '--gateway-listen', f'127.0.0.1:{self.port}'], 'moshi')
        eventually(lambda: http(self.port, '/v1/version')[0] == 200)

    def agent(self, source):
        session = str(uuid.uuid4())
        if source == 'claude':
            path = self.root / '.claude/projects/neordr-test' / (session + '.jsonl')
            rows = [{'type': 'user', 'sessionId': session, 'uuid': str(uuid.uuid4()),
                     'timestamp': datetime.now(timezone.utc).isoformat(), 'message': {'role': 'user', 'content': 'Approval test'}}]
        else:
            now = datetime.now(timezone.utc)
            path = self.root / 'codex/sessions' / now.strftime('%Y/%m/%d') / f'rollout-{now:%Y-%m-%dT%H-%M-%S}-{session}.jsonl'
            rows = [{'type': 'session_meta', 'timestamp': datetime.now(timezone.utc).isoformat(),
                     'payload': {'id': session, 'cwd': str(self.root), 'source': 'cli', 'originator': 'codex_cli_rs'}}]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
        child = self.root / 'bin' / source
        child.write_text(FIXTURE)
        code = "vim.cmd.enew(); local b=vim.api.nvim_get_current_buf(); local e=require('neordr.backend').prepare(b); "
        argv = '{' + ','.join(json.dumps(arg) for arg in ['python3', str(child), str(self.root), str(path), HOOK]) + '}'
        code += 'vim.fn.jobstart(' + argv + ',{term=true,env=e}); return e.HERDR_PANE_ID'
        pane = lua(self.env, self.address, code)
        self.agents[source] = {'session': session, 'pane': pane, 'path': path}
        self.show(source, 'READY', {'hook_event_name': 'SessionStart'})
        # Moshi ignores ordinary tool hooks for three seconds after SessionStart.
        time.sleep(3.1)
        return self.agents[source]

    def show(self, source, screen, event=None):
        agent = self.agents[source]
        token = str(uuid.uuid4())
        if event:
            event = {**event, 'session_id': agent['session'], 'cwd': str(self.root), 'transcript_path': str(agent['path'])}
        temporary = self.root / 'control.tmp'
        temporary.write_text(json.dumps({'id': token, 'screen': screen, 'event': event}))
        temporary.replace(self.root / (source + '.control'))
        ack = self.root / (source + '.ack')
        eventually(lambda: ack.exists() and ack.read_text() == token)
        eventually(lambda: ''.join(screen.split()) == ''.join(rpc(self.endpoint, 'pane.read', {'pane_id': agent['pane']})['read']['text'].split()))

    def context(self, source):
        env = {**self.env, 'HERDR_ENV': '1', 'HERDR_SESSION': 'approval', 'HERDR_PANE_ID': self.agents[source]['pane']}
        return json.loads(subprocess.check_output([HOOK, 'context'], env=env, timeout=15))

    def close(self):
        for process, _ in reversed(self.processes):
            process.terminate()
        for process, log in reversed(self.processes):
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            log.close()

    def input(self, source):
        return (self.root / (source + '.input')).read_bytes()

    def answer(self, source, path, body, expected, status=200, named=False):
        before = {name: self.input(name) for name in self.agents}
        body = {'source': source, 'sessionId': self.agents[source]['session'], **body}
        scope = '?mux=herdr:approval' if named else '?mux=herdr'
        actual, response = http(self.port, path + scope, body)
        assert actual == status, (source, path, actual, response)
        if expected:
            eventually(lambda: self.input(source) == before[source] + expected)
        # Catch accidental extra keys as well as input delivered to another pane.
        time.sleep(.05)
        for name in before:
            assert self.input(name) == before[name] + (expected if name == source else b''), (name, self.input(name), before)
        return response

    def resize(self, source, columns):
        rpc(self.endpoint, 'pane.focus', {'pane_id': self.agents[source]['pane']})
        lua(self.env, self.address, f'vim.o.columns={columns}; vim.o.lines=35; return vim.o.columns')


def question_case(rig, source, columns):
    rig.show(source, 'Working', {'hook_event_name': 'UserPromptSubmit', 'prompt': f'Question test at {columns} columns'})
    call = f'question-{columns}'
    text = 'Which color should we use for the application navigation bar?'
    question = {'header': 'Color', 'question': text, 'multiSelect': False,
                'options': [{'label': 'Blue', 'description': 'Use blue'}, {'label': 'Green', 'description': 'Use green'}]}
    if source == 'codex': question['id'] = 'color'
    tool = {'tool_name': 'AskUserQuestion' if source == 'claude' else 'request_user_input',
            'tool_use_id': call, 'tool_input': {'questions': [question]}}
    screen = 'Question 1/1\nColor\n' + text + '\n1. Blue\n2. Green\nTab/arrow keys to navigate\nEnter to submit'
    rig.show(source, screen, {'hook_event_name': 'PreToolUse', **tool})
    if source == 'claude':
        rig.show(source, screen, {'hook_event_name': 'PermissionRequest', **tool})
        context = rig.context(source)['agent']
        assert 'prompt' in context, (context, (rig.root / 'state/claude-sessions' / (rig.agents[source]['session'] + '.json')).read_text())
        prompt = context['prompt']
        assert prompt['kind'] == 'question' and prompt['toolUseId'] == call, prompt
    body = {'toolUseId': call, 'questions': [{**question, 'options': ['Blue', 'Green']}],
            'answers': [{'questionId': question.get('id', ''), 'optionIndexes': [1]}]}
    if source == 'claude' and columns == 120 and os.environ.get('NEORDR_PROBE_STALE_HOOK') == '1':
        # Opt-in reproducer for the observed 0.3.26 behavior, not a safety guarantee.
        rig.show(source, 'Prompt dismissed')
        rig.answer(source, '/v1/questions/answer', body, b'2\r')
        print('MOSHI 0.3.26 LIMIT: stale Claude hook state permits input after the visible question disappears', flush=True)
    # Claude trusts matching pending hook state, even after the screen changes.
    # Exercise normal completion before asserting stale-answer rejection.
    rig.show(source, 'Prompt dismissed', {'hook_event_name': 'PostToolUse', **tool})
    rig.answer(source, '/v1/questions/answer', body, b'', status=409)
    rig.show(source, screen, {'hook_event_name': 'PreToolUse', **tool})
    rig.answer(source, '/v1/questions/answer', body, b'2\r' if source == 'claude' else b'2', named=columns == 120)
    print(f'{source} {columns} columns: question selection and completed-question rejection PASS', flush=True)


def plan_case(rig, source, columns):
    call = f'plan-{columns}'
    plan = 'Add a blue navigation bar and verify the selected color.'
    path = rig.root / 'plan.md'
    path.write_text(plan)
    tool = {'tool_name': 'ExitPlanMode', 'tool_use_id': call,
            'tool_input': {'plan': plan, 'planFilePath': str(path)}}
    options = ['Yes, implement this plan', 'No, keep planning']
    screen = 'Plan approval\n' + plan + '\n1. ' + options[0] + '\n2. ' + options[1] + '\nEnter to select'
    rig.show(source, screen, {'hook_event_name': 'PreToolUse', **tool})
    rig.show(source, screen, {'hook_event_name': 'PermissionRequest', **tool})
    agent = rig.context(source)['agent']
    assert agent['planMenu']['options'] == options, agent
    # Both columns exercise the actual recent_unwrapped capture used by Moshi.
    if columns == 120:
        assert agent['planMenu']['title'] == plan, agent
    elif agent['planMenu']['title'] != plan:
        print(f'{source}: known narrow-screen limit: inferred plan title is {agent["planMenu"]["title"]!r}', flush=True)
    body = {'toolUseId': call, 'prompt': {'title': 'Plan approval', 'plan': plan, 'options': options, 'optionIndex': 0}}
    rig.show(source, 'Plan dismissed', {'hook_event_name': 'PostToolUse', **tool})
    rig.answer(source, '/v1/plans/answer', body, b'', status=409)
    rig.show(source, screen, {'hook_event_name': 'PreToolUse', **tool})
    rig.answer(source, '/v1/plans/answer', body, b'1', named=columns == 120)
    print(f'{source} {columns} columns: plan selection and completed-plan rejection PASS', flush=True)


def permission_case(rig, source, columns):
    command = 'printf neordr-permission'
    screen = ('Do you want to proceed?\n' + command + '\n1. Yes\n2. No\nEsc to cancel'
              if source == 'claude' else
              'Would you like to run the following command?\n$ ' + command +
              '\n1. Yes, proceed (y)\n2. No, and tell Codex what to do differently (esc)')
    for decision in ['approve', 'deny']:
        action = f'permission-{columns}-{decision}'
        event = {'hook_event_name': 'PermissionRequest', 'tool_name': 'Bash' if source == 'claude' else 'exec_command',
                 'tool_use_id': action, 'action_id': action,
                 'tool_input': {'command': command, 'cmd': command, 'description': command}}
        rig.show(source, screen, event)
        body = {'actionId': action, 'decision': decision}
        rig.show(source, 'Permission dismissed')
        rig.answer(source, '/v1/approvals/answer', body, b'', status=409)
        # Re-register a fresh request after the failed screen verification.
        action += '-fresh'
        event.update(tool_use_id=action, action_id=action)
        body['actionId'] = action
        rig.show(source, screen, event)
        expected = (b'1' if source == 'claude' else b'y') if decision == 'approve' else (b'2' if source == 'claude' else b'\x1b')
        rig.answer(source, '/v1/approvals/answer', body, expected, named=columns == 120)
        rig.answer(source, '/v1/approvals/answer', body, b'', status=409)
    print(f'{source} {columns} columns: approve, deny, stale screen and duplicate-answer rejection PASS', flush=True)


def main():
    for columns in [int(n) for n in os.environ.get('NEORDR_TEST_COLUMNS', '120,32').split(',')]:
        # Fresh sessions isolate each width from Moshi's asynchronous permission watchers.
        with tempfile.TemporaryDirectory(prefix='neordr-approval-', dir='/tmp') as directory:
            rig = Rig(Path(directory).resolve())
            try:
                rig.setup()
                for source in ['claude', 'codex']:
                    rig.agent(source)
                for source in rig.agents:
                    rig.resize(source, columns)
                    question_case(rig, source, columns)
                    plan_case(rig, source, columns)
                    permission_case(rig, source, columns)
            except Exception:
                for path in rig.root.rglob('*.log'):
                    print(path.name, path.read_text()[-5000:])
                raise
            finally:
                rig.close()
    print('Moshi local approval/question/plan compatibility: PASS', flush=True)


if __name__ == '__main__':
    main()
