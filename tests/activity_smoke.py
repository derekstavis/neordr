#!/usr/bin/env python3
"""Agent detection, persistent transition ordering, and optional real Moshi projection.
Uses disposable editors and fake processes; no model calls or user settings.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
from approval_smoke import Rig, FIXTURE
from moshi_smoke import BIN, HOOK, eventually, http, lua, rpc


class ActivityRig(Rig):
    def setup(self, moshi):
        self.start(['nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '--listen', str(self.address)], 'nvim')
        eventually(lambda: self.address.exists())
        self.start([str(BIN), '--session', 'approval', 'serve', '--nvim', str(self.address)], 'neordr')
        eventually(lambda: self.endpoint.exists() and rpc(self.endpoint, 'ping'))
        self.start([str(BIN), 'router'], 'router')
        eventually(lambda: (self.root / 'config/herdr/herdr.sock').exists())
        if moshi:
            for setting in ['always-on-discovery', 'usage-collection']:
                subprocess.run([HOOK, 'set', setting, 'off'], env=self.env, check=True, capture_output=True)
            self.start([HOOK, 'serve', '--gateway-listen', f'127.0.0.1:{self.port}'], 'moshi')
            eventually(lambda: http(self.port, '/v1/version')[0] == 200)

    def agent(self, source):
        child = self.root / 'bin' / source
        child.write_text(FIXTURE)
        path = self.root / (source + '.jsonl')
        path.touch()
        argv = ['python3', str(child), str(self.root), str(path), HOOK]
        code = 'vim.cmd.enew(); local b=vim.api.nvim_get_current_buf(); local e=require("neordr.backend").prepare(b); '
        code += 'vim.fn.jobstart(vim.json.decode(' + json.dumps(json.dumps(argv)) + '),{term=true,env=e}); return e.HERDR_PANE_ID'
        self.agents[source] = {'pane': lua(self.env, self.address, code)}
        self.show(source, 'READY')

    def title(self, source, title):
        lua(self.env, self.address, 'for _,p in ipairs(require("neordr.backend").snapshot().panes) do '
            'if p.pane_id == ' + json.dumps(self.agents[source]['pane']) + ' then vim.b[p.neordr.buf].term_title = '
            + json.dumps(title, ensure_ascii=False) + ' end end; return true')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--moshi', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='na-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        rig = ActivityRig(root)
        try:
            rig.setup(args.moshi)
            rig.agent('codex')
            rig.agent('claude')  # Codex is now hidden, so completion is unseen.
            router = root / 'config/herdr/herdr.sock'
            def agents(endpoint=router):
                return {a['agent']: a for a in rpc(endpoint, 'agent.list')['agents']}
            def expect(source, status):
                a = eventually(lambda: (a := agents().get(source)) and a['agent_status'] == status and a)
                snapshot = rpc(rig.endpoint, 'session.snapshot')['snapshot']
                row = next(p for p in snapshot['panes'] if p['pane_id'] == a['pane_id'])
                tab = next(t for t in snapshot['tabs'] if t['tab_id'] == a['tab_id'])
                assert row['agent_status'] == tab['agent_status'] == status
                assert agents(rig.endpoint)[source]['state_change_seq'] == a['state_change_seq']
                return a
            rig.title('codex', 'Codex')
            first = expect('codex', 'idle')
            assert first.get('completion_seq') is None
            rig.title('codex', '⠋ Building')
            working = expect('codex', 'working')
            assert working['state_change_seq'] > first['state_change_seq']
            assert expect('codex', 'working')['state_change_seq'] == working['state_change_seq']
            rig.show('codex', 'More output')
            assert expect('codex', 'working')['state_change_seq'] == working['state_change_seq']
            # A stale reader must not overwrite a newer observation.
            stale = lua(rig.env, rig.address, 'local b=require("neordr.backend"); local p=b.snapshot().panes[1]; '
                'local a=b.agent_activity[p.pane_id]; local before=vim.deepcopy(a); '
                'b.update_activity({{pane_id=p.pane_id,revision=p.revision,title=p.terminal_title,observation=0,identity="stale",status="idle"}},vim.g.neordr_generation); '
                'return vim.deep_equal(before,b.agent_activity[p.pane_id])')
            assert stale == 'true'
            rig.title('codex', 'Codex')
            rig.show('codex', '↑/↓ to scroll  pgup/pgdn to  home/end to jump  q to quit  esc to edit prev')
            assert expect('codex', 'working')['state_change_seq'] == working['state_change_seq']
            rig.show('codex', 'More output')
            rig.title('codex', '⠋ Building')
            rig.title('claude', '◐ Thinking')
            later = expect('claude', 'working')
            assert later['state_change_seq'] > working['state_change_seq']
            rig.title('codex', 'Action Required')
            expect('codex', 'blocked')
            assert rpc(router, 'workspace.list')['workspaces'][0]['agent_status'] == 'blocked'
            rig.title('codex', 'Codex')
            done = expect('codex', 'done')
            assert done['completion_seq'] > working['state_change_seq']
            assert expect('codex', 'done')['state_change_seq'] == done['state_change_seq']
            if args.moshi:
                def tree_ok():
                    _, tree = http(rig.port, '/v1/workspaces?mux=herdr:approval')
                    nodes = {n.get('agent'): n for g in tree['groups'] for n in g['children']}
                    return nodes.get('codex', {}).get('agentStatus') == 'done' and nodes.get('claude', {}).get('agentStatus') == 'working' and nodes
                nodes = eventually(tree_ok, seconds=15)
                assert nodes['codex']['stateChangeOrder'] > nodes['claude']['stateChangeOrder'], nodes
            # Restart just the disposable bridge; editor-owned state survives.
            bridge = rig.processes[1][0]
            bridge.terminate(); bridge.wait(timeout=5)
            eventually(lambda: not rig.endpoint.exists())
            rig.start([str(BIN), '--session', 'approval', 'serve', '--nvim', str(rig.address)], 'bridge-restarted')
            eventually(lambda: rpc(rig.endpoint, 'ping'))
            assert expect('codex', 'done')['state_change_seq'] == done['state_change_seq']
            rpc(router, 'pane.focus', {'pane_id': rig.agents['codex']['pane']})
            idle = expect('codex', 'idle')
            assert idle['completion_seq'] == done['completion_seq']
            # Exiting the process clears the agent and its previous status.
            lua(rig.env, rig.address, 'for _,p in ipairs(require("neordr.backend").snapshot().panes) do '
                'if p.pane_id == ' + json.dumps(rig.agents['codex']['pane']) + ' then vim.fn.jobstop(p.neordr.channel) end end; return true')
            eventually(lambda: 'codex' not in agents())
            print('Agent activity: state transitions, ordering, aggregate status, stale rejection, bridge restart, focus and exit: PASS')
        finally:
            rig.close()

if __name__ == '__main__':
    main()
