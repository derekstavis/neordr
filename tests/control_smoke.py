#!/usr/bin/env python3
"""JSON control frames, full editor input, resize and UI-only teardown; no Moshi required."""
import base64
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from moshi_smoke import BIN, eventually, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-control-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {k:v for k,v in os.environ.items() if k != 'NVIM' and not k.startswith(('HERDR_', 'NEORDR_'))}
        env.update(XDG_CONFIG_HOME=str(root/'config'), NVIM_LOG_FILE=str(root/'nvim.log'), TERM='xterm-256color')
        address, endpoint = root/'nvim.sock', root/'config/herdr/sessions/control/herdr.sock'
        processes=[]
        try:
            processes.append(subprocess.Popen(['nvim','--headless','-u','NONE','-i','NONE','--listen',str(address)],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
            eventually(address.exists)
            processes.append(subprocess.Popen([str(BIN),'--session','control','serve','--nvim',str(address)],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
            eventually(lambda: endpoint.exists() and rpc(endpoint,'ping'))
            ws=rpc(endpoint,'workspace.list')['workspaces'][0]['workspace_id']
            pane=rpc(endpoint,'tab.create',{'workspace_id':ws,'focus':True})['root_pane']['pane_id']
            job=lua(env,address,'return vim.fn.jobpid(vim.bo.channel)')
            def count():return lua(env,address,'return #vim.api.nvim_list_uis()')
            for case in ['release','eof','sigterm','malformed','blocked-output','stdout-close']:
                p=subprocess.Popen([str(BIN),'--session','control','terminal','session','control',pane,'--takeover','--cols','91','--rows','27'],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
                processes.append(p)
                frames=[]; errors=[]
                def drain():
                    try:
                        for line in p.stdout:
                            record=json.loads(line)
                            if record['type']=='terminal.frame':
                                assert record['encoding']=='ansi'
                                base64.b64decode(record['bytes'],validate=True)
                            frames.append(record)
                    except Exception as e:errors.append(e)
                reader=None
                if case not in ['blocked-output','stdout-close']:
                    reader=threading.Thread(target=drain,daemon=True);reader.start()
                eventually(lambda:count()=='1')
                if reader:eventually(lambda:any(f['type']=='terminal.frame' for f in frames))
                assert lua(env,address,'return vim.o.columns')=='91'
                def send(d):p.stdin.write(json.dumps(d).encode()+b'\n');p.stdin.flush()
                if case=='release':
                    send({'type':'terminal.resize','cols':103,'rows':31})
                    eventually(lambda:lua(env,address,'return vim.o.columns')=='103')
                    send({'type':'terminal.input','bytes':base64.b64encode(b'\x1c\x0e:enew\r:lua vim.api.nvim_buf_set_lines(0,0,-1,false,{"FULL_EDITOR"})\r').decode()})
                    eventually(lambda:lua(env,address,'return vim.api.nvim_get_current_line()')=='FULL_EDITOR')
                    lua(env,address,"vim.o.mouse='a'; vim.o.scrolloff=0; local ls={}; for i=1,200 do ls[i]='row '..i end; vim.api.nvim_buf_set_lines(0,0,-1,false,ls); vim.api.nvim_win_set_cursor(0,{1,0}); return true")
                    send({'type':'terminal.scroll','direction':'down','lines':2})
                    eventually(lambda:int(lua(env,address,"return vim.fn.line('w0')"))>1)
                    send({'type':'terminal.scroll','direction':'up','lines':1,'source':'page_key'})
                    send({'type':'terminal.release'})
                elif case=='eof':p.stdin.close()
                elif case=='stdout-close':p.stdout.close()
                elif case=='malformed':send({'type':'terminal.input','text':'x','bytes':'eA=='})
                elif case=='blocked-output':
                    # Generate continuous redraws while the controller's output is unread.
                    lua(env,address,"vim.g.control_test_timer=vim.fn.timer_start(1,function() vim.cmd('redraw!') end,{['repeat']=-1}); return true")
                    import time;time.sleep(.5)
                    p.terminate()
                else:p.terminate()
                status=p.wait(timeout=8)
                assert status==(1 if case=='malformed' else 0),(case,status,p.stderr.read())
                if reader:reader.join(timeout=2)
                assert not errors,errors
                eventually(lambda:count()=='0')
                if case=='blocked-output':lua(env,address,'vim.fn.timer_stop(vim.g.control_test_timer); return true')
                assert processes[0].poll() is None
                os.kill(int(job),0)
                print(f'control {case}: PASS',flush=True)
            # Reject cross-session targets without attaching or focusing anything.
            bad=subprocess.run([str(BIN),'--session','wrong','terminal','session','control',pane],env=env,capture_output=True)
            assert bad.returncode!=0 and b'another session' in bad.stderr
            assert count()=='0'
        finally:
            for p in reversed(processes):
                if p.poll() is None:
                    p.terminate()
                    try:p.wait(timeout=8)
                    except subprocess.TimeoutExpired:p.kill();p.wait()


if __name__=='__main__':main()
