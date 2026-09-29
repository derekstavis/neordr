#!/usr/bin/env python3
"""Doctor distinguishes unavailable, missing and mismatched association without mutations."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from moshi_smoke import BIN, eventually, lua, rpc


def main():
    with tempfile.TemporaryDirectory(prefix='neordr-doctor-',dir='/tmp') as d:
        root=Path(d).resolve();root.joinpath('bin').mkdir()
        env={k:v for k,v in os.environ.items() if k!='NVIM' and not k.startswith(('HERDR_','NEORDR_'))}
        env.update(HOME=str(root),XDG_CONFIG_HOME=str(root/'config'),MOSHI_STATE_DIR=str(root/'state'),MOSHI_CONFIG_DIR=str(root/'state'),NVIM_LOG_FILE=str(root/'nvim.log'))
        pane=None; advertised='missing'; body=[]
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_GET(self):
                result={'version':'fixture'} if self.path=='/v1/version' else {'groups':[{'children':[{'panes':body}]}]}
                # Exercise the same chunked encoding used for larger Go responses.
                data=json.dumps(result).encode();self.send_response(200);self.send_header('Transfer-Encoding','chunked');self.end_headers()
                self.wfile.write(f'{len(data):x}\r\n'.encode()+data+b'\r\n0\r\n\r\n')
        http=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread=threading.Thread(target=http.serve_forever,daemon=True);thread.start()
        address=root/'nvim.sock';endpoint=root/'config/herdr/sessions/doctor/herdr.sock'
        children=[]
        def doctor(port=None):
            p=subprocess.run([str(BIN),'doctor','--json','--gateway-port',str(port or http.server_port)],env=env,capture_output=True,text=True,timeout=20)
            assert p.returncode==0,p.stderr
            return json.loads(p.stdout)
        try:
            absent=doctor()
            assert any(c['subject']=='router' and c['status']=='warn' for c in absent['checks'])
            assert not (root/'config/herdr/herdr.sock').exists(), 'Doctor must not start services'
            children.append(subprocess.Popen(['nvim','--headless','-u','NONE','-i','NONE','--listen',str(address)],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
            eventually(address.exists)
            children.append(subprocess.Popen([str(BIN),'--session','doctor','serve','--nvim',str(address)],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
            eventually(lambda: endpoint.exists() and rpc(endpoint,'ping'))
            fake=root/'bin/codex';fake.write_text('import time\ntime.sleep(120)\n')
            pane=lua(env,address,'vim.cmd.enew(); local b=vim.api.nvim_get_current_buf(); local e=require("neordr.backend").prepare(b); vim.fn.jobstart({"python3",'+json.dumps(str(fake))+'},{term=true,env=e}); return e.HERDR_PANE_ID')
            eventually(lambda: rpc(endpoint,'agent.list')['agents'])
            body.append({'id':pane})
            path=root/'state/codex-sessions';path.mkdir(parents=True)
            (path/'saved.json').write_text(json.dumps({'herdrPane':pane}))
            r=doctor();assert any('no conversation ID' in c['detail'] for c in r['checks']),r
            assert r['editors'][0]['uis']==[]
            assert r['editors'][0]['panes'][0]['hook_conversations']==['saved']
            body[0]['sessionId']='different'
            assert any('differs from saved' in c['detail'] for c in doctor()['checks'])
            body[0]['sessionId']='saved'
            assert any(c['subject']==pane and c['status']=='ok' for c in doctor()['checks'])
            assert any(c['subject']=='Moshi gateway' and c['status']=='warn' for c in doctor(1)['checks'])
            assert not (root/'config/herdr/herdr.sock').exists()
            assert children[0].poll() is None
            print('doctor: missing/mismatched/matching conversation, unavailable gateway, no service startup: PASS')
        finally:
            http.shutdown();http.server_close();thread.join(timeout=2)
            for p in reversed(children):
                p.terminate()
                try:p.wait(timeout=5)
                except subprocess.TimeoutExpired:p.kill();p.wait()


if __name__=='__main__':main()
