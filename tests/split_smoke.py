#!/usr/bin/env python3
"""Moshi split actions preserve the full editor and independent terminal identities."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
from moshi_smoke import BIN, HOOK, eventually, http, lua, rpc


def main():
    full = os.environ.get('NEORDR_NVIM_CONFIG')
    overlay = os.environ.get('NEORDR_CONFIG_OVERLAY')
    with tempfile.TemporaryDirectory(prefix='neordr-split-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        config = root/'config'
        env = {**os.environ, 'XDG_CONFIG_HOME': str(config), 'NVIM_LOG_FILE': str(root/'nvim.log'),
               'MOSHI_STATE_DIR': str(root/'state'), 'MOSHI_CONFIG_DIR': str(root/'state'),
               'MOSHI_SOCKET_PATH': str(root/'moshi.sock'), 'MOSHI_API_BASE': 'http://127.0.0.1:9'}
        for k in ['NVIM','HERDR_ENV','HERDR_SESSION','HERDR_PANE_ID','HERDR_SOCKET_PATH','TMUX','ZELLIJ']:
            env.pop(k, None)
        (root/'bin').mkdir()
        (root/'bin/herdr').symlink_to(BIN)
        env['MOSHI_HERDR_PATH'] = str(root/'bin/herdr')
        env['PATH'] = str(root/'bin') + os.pathsep + os.environ['PATH']
        agent = root/'bin/codex'
        agent.write_text('#!/usr/bin/env python3\nimport os,json,tty\ntty.setraw(0)\n'
            f'with open({str(root/"launches")!r},"a") as f: f.write(os.environ["HERDR_PANE_ID"]+"\\n")\n'
            'while os.read(0,4096): pass\n')
        agent.chmod(0o755)
        if full:
            shutil.copytree(full, config/'nvim', ignore=shutil.ignore_patterns('.git','.codex','.agents'))
            if overlay:
                for name in ['terminal.lua','neordr.lua']:
                    shutil.copyfile(Path(overlay)/name, config/'nvim/lua/config'/name)
            p=config/'nvim/lua/config/neordr.lua'
            p.write_text(p.read_text().replace('bin = bin,','bin = '+json.dumps(str(BIN))+','))
        address = root/'nvim.sock'
        children=[]
        log=open(root/'log','w')
        def start(args):
            child=subprocess.Popen(args,env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log)
            children.append(child)
            return child
        try:
            start([str(BIN),'router'])
            router=config/'herdr/herdr.sock'
            eventually(lambda:router.exists() and rpc(router,'ping'))
            start(['nvim','--headless','-i','NONE','--listen',str(address)]+([] if full else ['-u','NONE']))
            eventually(address.exists)
            if full:
                session=eventually(lambda:lua(env,address,"return require('neordr').session"))
            else:
                session='split'
                start([str(BIN),'--session',session,'serve','--nvim',str(address)])
            endpoint=config/'herdr/sessions'/session/'herdr.sock'
            eventually(lambda:endpoint.exists() and rpc(endpoint,'ping'))
            def evaluate(code): return lua(env,address,code)
            def snapshot(): return json.loads(evaluate("return vim.json.encode(require('neordr.backend').snapshot())"))
            def details(pane): return next(p for p in snapshot()['panes'] if p['pane_id']==pane)
            def win(buf): return int(evaluate(f'return vim.fn.bufwinid({buf})'))
            def geometry(w):
                return json.loads(evaluate(f'local w={w}; return vim.json.encode({{pos=vim.api.nvim_win_get_position(w),width=vim.api.nvim_win_get_width(w),height=vim.api.nvim_win_get_height(w)}})'))
            # Enough space for nested splits; an unsaved editor must remain visible.
            editor=json.loads(evaluate("vim.o.columns=160; vim.o.lines=60; vim.api.nvim_buf_set_lines(0,0,-1,false,{'KEEP UNSAVED EDITOR'}); return vim.json.encode({win=vim.api.nvim_get_current_win(),buf=vim.api.nvim_get_current_buf()})"))
            ws=rpc(endpoint,'workspace.list')['workspaces'][0]['workspace_id']
            a=rpc(endpoint,'tab.create',{'workspace_id':ws,'focus':True,'label':'Anchor'})['root_pane']
            a_buf=details(a['pane_id'])['neordr']['buf']
            a_job=details(a['pane_id'])['neordr']['channel']
            a_win=win(a_buf)
            # Invalid targets/directions fail without allocating a terminal/window.
            before=snapshot()
            for params in [{'target_pane_id':session+':missing','direction':'right'},
                           {'target_pane_id':a['pane_id'],'direction':'left'},
                           {'target_pane_id':a['pane_id'],'direction':'right','ratio':2}]:
                try: rpc(router,'pane.split',params)
                except AssertionError: pass
                else: raise AssertionError('invalid split succeeded')
            assert [p['pane_id'] for p in snapshot()['panes']]==[p['pane_id'] for p in before['panes']]
            if full:
                before_wins=evaluate('return vim.json.encode(vim.api.nvim_list_wins())')
                evaluate("local b=require('neordr.backend'); b.test_split=b.split_terminal; b.split_terminal=function(opts) b.test_split(opts); error('injected split failure') end; return true")
                try: rpc(router,'pane.split',{'target_pane_id':a['pane_id'],'direction':'right','focus':True})
                except AssertionError: pass
                else: raise AssertionError('failing adapter succeeded')
                evaluate("local b=require('neordr.backend'); b.split_terminal=b.test_split; b.test_split=nil; return true")
                assert evaluate('return vim.json.encode(vim.api.nvim_list_wins())')==before_wins
                assert evaluate("return #require('tiny-term').list()")=='1'
                assert [p['pane_id'] for p in snapshot()['panes']]==[a['pane_id']]
            b=rpc(router,'pane.split',{'target_pane_id':a['pane_id'],'direction':'right','focus':True,'label':'Right','env':{'NEORDR_SPLIT_PROBE':'kept'}})['pane']
            b_buf=details(b['pane_id'])['neordr']['buf']
            b_win=win(b_buf)
            assert a['tab_id'] != b['tab_id'] and a['workspace_id']==b['workspace_id']
            assert win(a_buf)==a_win and b_win!=a_win
            assert geometry(b_win)['pos'][1] > geometry(a_win)['pos'][1]
            assert geometry(b_win)['pos'][0] == geometry(a_win)['pos'][0]
            # A nested split stays adjacent to the requested buffer, regardless of current focus.
            rpc(endpoint,'pane.focus',{'pane_id':a['pane_id']})
            previous=int(evaluate('return vim.api.nvim_get_current_win()'))
            c=rpc(router,'pane.split',{'target_pane_id':b['pane_id'],'direction':'down','focus':False,'label':'Below'})['pane']
            c_buf=details(c['pane_id'])['neordr']['buf']
            c_win=win(c_buf)
            assert int(evaluate('return vim.api.nvim_get_current_win()'))==previous
            assert geometry(c_win)['pos'][0]>geometry(b_win)['pos'][0]
            assert geometry(c_win)['pos'][1]==geometry(b_win)['pos'][1]
            assert evaluate(f'return vim.api.nvim_win_get_buf({editor["win"]})')==str(editor['buf'])
            assert evaluate(f'return vim.api.nvim_buf_get_lines({editor["buf"]},0,1,false)[1]')=='KEEP UNSAVED EDITOR'
            assert details(a['pane_id'])['neordr']['channel']==a_job
            marker=root/'env'
            text=f"printf '%s\\n' \"$PWD\" \"$NEORDR_SPLIT_PROBE\" \"$HERDR_PANE_ID\" > '{marker}'\r"
            rpc(endpoint,'pane.send_text',{'pane_id':b['pane_id'],'text':text})
            eventually(lambda:marker.exists() and b['pane_id'] in marker.read_text())
            assert marker.read_text().splitlines()==[b['cwd'],'kept',b['pane_id']]
            if full:
                rpc(endpoint,'pane.focus',{'pane_id':b['pane_id']})
                evaluate("require('config.terminal').toggle(); return true")
                assert win(b_buf)==-1 and win(a_buf)==a_win
                rpc(endpoint,'pane.focus',{'pane_id':b['pane_id']})
                assert win(b_buf)>0 and win(a_buf)==a_win
                assert win(c_buf)>0
                # Hide the original bottom panel, then split its hidden terminal.
                rpc(endpoint,'pane.focus',{'pane_id':a['pane_id']})
                evaluate("require('config.terminal').toggle(); return true")
                assert win(a_buf)==-1
                hidden=rpc(router,'pane.split',{'target_pane_id':a['pane_id'],'direction':'down','focus':True})['pane']
                assert win(a_buf)>0 and win(b_buf)>0
                rpc(endpoint,'pane.close',{'pane_id':hidden['pane_id']})
            with_moshi = os.environ.get('NEORDR_SKIP_MOSHI') != '1'
            if with_moshi:
                # Real Moshi HTTP request and response decoding, default and named sockets.
                for setting in ['always-on-discovery','usage-collection']:
                    subprocess.run([HOOK,'set',setting,'off'],env=env,check=True,capture_output=True)
                with socket.socket() as s: s.bind(('127.0.0.1',0)); port=s.getsockname()[1]
                start([HOOK,'serve','--gateway-listen',f'127.0.0.1:{port}'])
                eventually(lambda:http(port,'/v1/version')[0]==200)
                for mux in ['herdr','herdr:'+session]:
                    ids={p['pane_id'] for p in snapshot()['panes']}
                    status,reply=http(port,'/v1/workspaces/create?mux='+mux,{'paneId':a['pane_id'],'direction':'right'})
                    assert status==200,(status,reply)
                    created=[p for p in snapshot()['panes'] if p['pane_id'] not in ids]
                    assert len(created)==1
                    d=created[0]
                    # Moshi rejects combining agent launch with a pane split; use its
                    # separate launch action on the newly created terminal.
                    status,reply=http(port,'/v1/workspaces/launch?mux='+mux,{'paneId':d['pane_id'],'agent':'codex'})
                    assert status==200,(status,reply)
                    eventually(lambda:(root/'launches').exists() and d['pane_id'] in (root/'launches').read_text().splitlines())
                    status,reply=http(port,'/v1/workspaces/focus?mux='+mux,{'paneId':d['pane_id'],'workspaceId':d['workspace_id'],'tabId':d['tab_id']})
                    assert status==200,(status,reply)
                    assert int(evaluate('return vim.api.nvim_get_current_buf()'))==d['neordr']['buf']
                    status,reply=http(port,'/v1/workspaces/close?mux='+mux,{'paneId':d['pane_id']})
                    assert status==200,(status,reply)
            for pane in [c,b]: rpc(endpoint,'pane.close',{'pane_id':pane['pane_id']})
            assert [p['pane_id'] for p in snapshot()['panes']]==[a['pane_id']]
            assert details(a['pane_id'])['neordr']['channel']==a_job
            assert evaluate(f'return vim.api.nvim_buf_get_lines({editor["buf"]},0,1,false)[1]')=='KEEP UNSAVED EDITOR'
            if full: assert evaluate("return #require('tiny-term').list()")=='1'
            print(('Full config' if full else 'Minimal config')+': right/down/nested splits, focus, independent IDs, editor preservation, input, hide/restore, close' + (', and real Moshi actions' if with_moshi else ' (Moshi gateway check explicitly skipped)') + ': PASS')
        except Exception:
            log.flush(); print((root/'log').read_text()[-4000:]); raise
        finally:
            for child in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    try: child.wait(timeout=5)
                    except subprocess.TimeoutExpired: child.kill(); child.wait()
            log.close()

if __name__=='__main__': main()
