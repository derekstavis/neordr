#!/usr/bin/env python3
"""Old terminal environments retain Chat View through typed live associations.
Uses disposable agents, transcripts, and an installed Moshi gateway; no model calls.
"""
import json
from pathlib import Path
import tempfile
from approval_smoke import Rig
from moshi_smoke import eventually, http, lua, transcript


def main():
    with tempfile.TemporaryDirectory(prefix='nlegacy-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        rig = Rig(root)
        rig.env['XDG_STATE_HOME'] = str(root / 'nvim-state')
        try:
            rig.setup()
            # Reproduce an editor/agent that predates HERDR_SESSION=default.
            lua(rig.env, rig.address, "local b=require('neordr.backend'); local old=b.prepare; "
                "b.prepare=function(...) local e=old(...); e.HERDR_SESSION='approval'; return e end; return true")
            for source in ['claude', 'codex']:
                rig.agent(source)
            expected = {s: a['session'] for s, a in rig.agents.items()}
            def conversations():
                status, tree = http(rig.port, '/v1/workspaces?mux=herdr:default')
                assert status == 200
                return {p.get('agent'): p.get('sessionId') for g in tree['groups']
                        for t in g['children'] for p in t.get('panes', []) if p.get('agent')}
            eventually(lambda: conversations() == expected)
            for event in ['PreToolUse', 'Stop', 'UserPromptSubmit']:
                for source in ['claude', 'codex']:
                    rig.show(source, 'READY', {'hook_event_name': event, 'tool_name': 'Read',
                             'tool_input': {'file_path': 'example.txt'}, 'prompt': 'Synthetic legacy session'})
                eventually(lambda: conversations() == expected)
            for source, agent in rig.agents.items():
                record = json.loads((root / 'state' / (source + '-sessions') / (agent['session'] + '.json')).read_text())
                assert record['herdrSession'] == 'approval'  # Never rewritten by Neordr.
                marker = 'LEGACY_TRANSCRIPT_' + source
                if source == 'claude':
                    row = {'type': 'user', 'sessionId': agent['session'], 'uuid': marker,
                           'message': {'role': 'user', 'content': marker}}
                else:
                    row = {'type': 'response_item', 'payload': {'type': 'message', 'role': 'user',
                           'content': [{'type': 'input_text', 'text': marker}]}}
                with agent['path'].open('a') as out:
                    out.write(json.dumps(row) + '\n')
                transcript(rig.port, source, agent['session'], marker, agent['path'])
                before = {s: rig.input(s) for s in rig.agents}
                status, _ = http(rig.port, '/v1/prompt?mux=herdr:default',
                                  {'source': source, 'sessionId': agent['session'], 'text': marker})
                assert status == 200
                eventually(lambda: marker.encode() in rig.input(source)[len(before[source]):])
                for other in rig.agents:
                    if other != source:
                        assert rig.input(other) == before[other]
            print('Legacy sessions: discovery, subsequent hooks, transcript backlog/append and isolated Chat input: PASS')
        finally:
            rig.close()


if __name__ == '__main__':
    main()
