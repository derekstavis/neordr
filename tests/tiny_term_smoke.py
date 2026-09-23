#!/usr/bin/env python3
"""Validate the user's tiny-term lifecycle using a disposable config copy."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from moshi_smoke import eventually, lua, rpc


def main():
    config = Path(os.environ.get('NEORDR_NVIM_CONFIG', Path.home()/'.config/nvim'))
    binary = Path(os.environ.get('NEORDR_BIN', Path.home()/'.local/bin/neordr')).resolve()
    with tempfile.TemporaryDirectory(prefix='neordr-tiny-',dir='/tmp') as directory:
        root = Path(directory).resolve()
        shutil.copytree(config,root/'config/nvim',ignore=shutil.ignore_patterns('.git','.codex','.agents'))
        bridge_config = root/'config/nvim/lua/config/neordr.lua'
        bridge_config.write_text(bridge_config.read_text().replace('bin = bin,', 'bin = ' + json.dumps(str(binary)) + ',' +
            "\n  close_terminal = require('neordr.tiny_term').close,\n  reorder_terminals = require('neordr.tiny_term').reorder,"))
        env={**os.environ,'XDG_CONFIG_HOME':str(root/'config'),'NVIM_LOG_FILE':str(root/'nvim.log')}
        for key in ['NVIM','HERDR_SESSION','HERDR_ENV']: env.pop(key,None)
        log=open(root/'startup.log','w')
        router=subprocess.Popen([str(binary),'router'],env=env,stdout=log,stderr=log)
        address=root/'nvim.sock'
        editor=None
        try:
            eventually(lambda:(root/'config/herdr/herdr.sock').exists())
            editor=subprocess.Popen(['nvim','--headless','-i','NONE','--listen',str(address)],
                env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log)
            eventually(lambda:address.exists())
            session=eventually(lambda:lua(env,address,"return require('neordr').session"))
            endpoint=root/'config/herdr/sessions'/session/'herdr.sock'
            eventually(lambda:endpoint.exists() and rpc(endpoint,'ping'))
            terms=json.loads(lua(env,address,"local m=require('config.terminal'); local a=m.new('bottom',false); "
                "local b=m.new('bottom',false); return vim.json.encode({a={buf=a.buf,job=a.job_id,pane=vim.b[a.buf].neordr_pane},"
                "b={buf=b.buf,job=b.job_id,pane=vim.b[b.buf].neordr_pane}})"))
            first,second=terms['a'],terms['b']
            assert first['pane'] != second['pane']
            lua(env,address,"package.loaded['config.terminal']=nil; require('config.terminal'); return true")
            capture=root/'terminal-env.txt'
            command=f"printf '%s' \"$HERDR_PANE_ID\" > '{capture}'\r"
            lua(env,address,f"vim.api.nvim_chan_send({first['job']},{json.dumps(command)}); return true")
            eventually(lambda:capture.exists() and capture.read_text()==first['pane'])
            lua(env,address,f"require('config.terminal').focus_buffer({first['buf']}); require('config.terminal').toggle(); return true")
            assert lua(env,address,f"return vim.fn.bufwinid({first['buf']})") == '-1'
            assert lua(env,address,f"return vim.fn.bufwinid({second['buf']})") == '-1'
            assert len(rpc(endpoint,'pane.list')['panes']) == 2
            workspace = rpc(endpoint,'workspace.list')['workspaces'][0]['workspace_id']
            project = root/'project with spaces'
            project.mkdir()
            created = rpc(endpoint,'tab.create',{'workspace_id':workspace,'cwd':str(project),
                'label':'Remote tests','focus':True,'env':{'NEORDR_PROBE':'adapter-kept'}})
            pane = created['root_pane']['pane_id']
            remote = json.loads(lua(env,address,"local t; for _,v in ipairs(require('tiny-term').list()) do "
                f"if vim.b[v.buf].neordr_pane=={json.dumps(pane)} then t=v end end; "
                "assert(t); return vim.json.encode({buf=t.buf,job=t.job_id,label=require('config.terminal_tabs').label(t.buf)})"))
            assert remote['label'] == 'Remote tests', remote
            marker=root/'adapter-env.txt'
            command=f"printf '%s\\n' \"$PWD\" \"$NEORDR_PROBE\" \"$HERDR_PANE_ID\" > '{marker}'\r"
            lua(env,address,f"vim.api.nvim_chan_send({remote['job']},{json.dumps(command)}); return true")
            eventually(lambda:marker.exists() and pane in marker.read_text())
            assert marker.read_text() == f'{project}\nadapter-kept\n{pane}\n'
            rpc(endpoint,'tab.rename',{'tab_id':created['tab']['tab_id'],'label':'Renamed tests'})
            assert lua(env,address,f"return require('config.terminal_tabs').label({remote['buf']})") == 'Renamed tests'
            rpc(endpoint,'tab.move',{'tab_id':created['tab']['tab_id'],'insert_index':0})
            assert rpc(endpoint,'tab.list')['tabs'][0]['tab_id'] == created['tab']['tab_id']
            def panel_buffers():
                return json.loads(lua(env,address,"local w=require('tiny-term.window'); local t=require('tiny-term.terminal'); "
                    "local bs={}; for _,id in ipairs(w.get_split_terminals(vim.api.nvim_get_current_win())) do bs[#bs+1]=t.get(id).buf end; return vim.json.encode(bs)"))
            assert panel_buffers() == [remote['buf'],first['buf'],second['buf']]
            lua(env,address,"require('config.terminal').toggle(); return true")
            rpc(endpoint,'tab.move',{'tab_id':created['tab']['tab_id'],'insert_index':3})
            rpc(endpoint,'pane.focus',{'pane_id':pane})
            assert panel_buffers() == [first['buf'],second['buf'],remote['buf']]
            rpc(endpoint,'tab.close',{'tab_id':created['tab']['tab_id']})
            eventually(lambda:len(rpc(endpoint,'pane.list')['panes'])==2)
            rpc(endpoint,'pane.focus',{'pane_id':first['pane']})
            assert lua(env,address,'return vim.api.nvim_get_current_buf()') == str(first['buf'])
            assert len(rpc(endpoint,'pane.list')['panes']) == 2
            rpc(endpoint,'pane.close',{'pane_id':first['pane']})
            eventually(lambda:len(rpc(endpoint,'pane.list')['panes'])==1)
            assert rpc(endpoint,'pane.list')['panes'][0]['pane_id'] == second['pane']
            other = rpc(endpoint,'workspace.create',{'label':'Disposable workspace','focus':True})
            other_ws = other['workspace']['workspace_id']
            rpc(endpoint,'tab.create',{'workspace_id':other_ws,'label':'Second disposable terminal','focus':True})
            rpc(endpoint,'pane.focus',{'pane_id':second['pane']})
            survivor = lua(env,address,'return vim.api.nvim_get_current_buf()')
            rpc(endpoint,'workspace.close',{'workspace_id':other_ws})
            assert lua(env,address,'return vim.api.nvim_get_current_buf()') == survivor
            assert [p['pane_id'] for p in rpc(endpoint,'pane.list')['panes']] == [second['pane']]
            assert lua(env,address,"return #require('tiny-term').list()") == '1'
            lua(env,address,"vim.schedule(function() vim.cmd('qa!') end); return true")
            assert editor.wait(timeout=5)==0
            eventually(lambda:not endpoint.exists())
            print('Full config: creation, naming, visible/hidden reorder, pane/tab/workspace close, surviving jobs and focus: PASS')
        except Exception:
            log.flush()
            print((root/'startup.log').read_text()[-5000:])
            raise
        finally:
            for child in [editor,router]:
                if child is not None and child.poll() is None:
                    child.terminate()
                    child.wait(timeout=5)
            log.close()


if __name__=='__main__':main()
