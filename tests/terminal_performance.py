#!/usr/bin/env python3
"""Opt-in terminal size benchmark on disposable editors and a private Moshi hook.

Measures input-to-ANSI arrival, not client rendering or remote network latency.
The reader acknowledges received WebSocket bytes immediately. No real editors,
agent conversations, settings, or UI connections are used.
"""
import argparse,base64,fcntl,json,os,pty,struct,subprocess,sys,tempfile,threading,time,statistics,tty,termios
from pathlib import Path
from urllib.parse import urlencode
sys.path.insert(0,str(Path(__file__).resolve().parent))
from approval_smoke import Rig
from moshi_smoke import BIN,eventually,lua,rpc
from moshi_performance import WebSocket
class Output:
 def __init__(self):self.chunks=[];self.cv=threading.Condition()
 def add(self,data,wire):
  with self.cv:self.chunks.append((time.perf_counter(),data,wire));self.cv.notify_all()
 def wait(self,offset,marker):
  with self.cv:
   end=time.monotonic()+3
   while True:
    chunks=self.chunks[offset:]
    if marker in b''.join(c[1] for c in chunks):return chunks[-1][0]
    left=end-time.monotonic()
    if left<=0:raise RuntimeError('marker timeout '+repr(b''.join(c[1] for c in chunks)[-300:]))
    self.cv.wait(left)
 def stats(self,start,end):
  chunks=[c for c in self.chunks if start<=c[0]<=end]
  return dict(chunks=len(chunks),bytes=sum(len(c[1]) for c in chunks),wire_bytes=sum(c[2] for c in chunks))
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--modes',nargs='+',choices=['remote-ui','control','gateway','gateway-whole'],default=['remote-ui','control','gateway','gateway-whole'])
    args=parser.parse_args()
    results=[]
    with tempfile.TemporaryDirectory(prefix='nsb-',dir='/tmp') as d:
     rig=Rig(Path(d).resolve());rig.env['XDG_STATE_HOME']=str(rig.root/'nvim-state')
     try:
      rig.setup();pane=rpc(rig.endpoint,'pane.list')['panes'][0]['pane_id']
      for mode in args.modes:
       for cols,rows in [(120,36),(240,72),(480,144)]:
        out=Output();proc=None;ws=None;master=None;thread=None
        try:
         if mode.startswith('gateway'):
          query=dict(mux='herdr',cols=cols,rows=rows)
          if mode=='gateway':query['pane']=pane
          else:query.update(muxSession='approval',hideSidebar='true')
          ws=WebSocket(rig.port,'/v1/pty?'+urlencode(query))
          send_lock=threading.Lock()
          def send_json(obj):
           data=json.dumps(obj).encode();mask=os.urandom(4);n=len(data);assert n<126
           with send_lock:ws.client.sendall(bytes([0x81,0x80|n])+mask+bytes(b^mask[i%4] for i,b in enumerate(data)))
          send_json(dict(type='ack',bytes=0));send_json(dict(type='resize',cols=cols,rows=rows))
          def read():
           total=0
           try:
            while True:
             op,data=ws.frame()
             if op==8:return
             out.add(data,len(data));total+=len(data);send_json(dict(type='ack',bytes=total))
           except (OSError,EOFError):pass
          def send(data):send_json(dict(type='input',data=data.decode()))
         elif mode=='control':
          proc=subprocess.Popen([str(BIN),'--session','approval','terminal','session','control',pane,'--cols',str(cols),'--rows',str(rows)],env=rig.env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
          def read():
           for line in proc.stdout:
            frame=json.loads(line)
            if frame['type']=='terminal.frame':out.add(base64.b64decode(frame['bytes']),len(line))
          def send(data):proc.stdin.write(json.dumps(dict(type='terminal.input',bytes=base64.b64encode(data).decode())).encode()+b'\n');proc.stdin.flush()
         else:
          master,slave=pty.openpty();tty.setraw(slave);fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',rows,cols,0,0))
          proc=subprocess.Popen(['nvim','--server',str(rig.address),'--remote-ui'],env=rig.env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);os.close(slave)
          def read():
           try:
            while True:
             data=os.read(master,65536)
             if not data:return
             out.add(data,len(data))
           except OSError:pass
          def send(data):os.write(master,data)
         thread=threading.Thread(target=read,daemon=True);thread.start()
         eventually(lambda:lua(rig.env,rig.address,'return vim.o.columns')==str(cols))
         setup='''vim.cmd('stopinsert'); vim.o.laststatus=0; vim.o.showtabline=0; vim.o.ruler=false; vim.o.showcmd=false; vim.o.number=false; vim.o.relativenumber=false; vim.o.wrap=false; vim.o.scrolloff=0; vim.o.guicursor='n:block-blinkon0'; local lines={}; for i=1,1200 do lines[i]=string.format('%04d ',i)..string.rep('AbCdEfGhIjKlMnOpQrStUvWxYz',40) end; vim.api.nvim_buf_set_lines(0,0,-1,false,lines); vim.api.nvim_win_set_cursor(0,{1,0}); vim.g.screenbench_flip=false; vim.keymap.set('n','a',function() vim.g.screenbench_flip=not vim.g.screenbench_flip; local text=vim.g.screenbench_flip and 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn' or 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN'; vim.api.nvim_buf_set_lines(0,0,1,false,{text}); vim.cmd('redraw') end); vim.cmd('redraw'); return true'''
         lua(rig.env,rig.address,setup);time.sleep(.3)
         latency=[];sizes=[]
         for i in range(20):
          offset=len(out.chunks);t=time.perf_counter();send(b'a');marker=(b'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn' if i%2==0 else b'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN');end=out.wait(offset,marker);latency.append((end-t)*1000);sizes.append(out.stats(t,end)['bytes']);time.sleep(.01)
         row=dict(mode=mode,cols=cols,rows=rows,typing_median_ms=statistics.median(latency),typing_p95_ms=sorted(latency)[18],typing_bytes_median=statistics.median(sizes),workloads={})
         for kind in ['scroll','full-change']:
          action="vim.cmd('normal! '..string.char(5))" if kind=='scroll' else "local lines={}; local pattern=count%2==0 and 'abcdefghijklmnopqrstuvwxyz' or 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'; for i=1,vim.o.lines-1 do lines[i]=string.rep(pattern,math.ceil(vim.o.columns/26)):sub(1,vim.o.columns-1) end; vim.api.nvim_buf_set_lines(0,0,vim.o.lines-1,false,lines); vim.cmd('redraw')"
          lua(rig.env,rig.address,"vim.api.nvim_win_set_cursor(0,{1,0}); vim.cmd('normal! zt'); vim.g.screenbench_done=false; return true");time.sleep(.15)
          start=time.perf_counter()
          lua(rig.env,rig.address,"local count=0; vim.g.screenbench_start=vim.uv.hrtime(); vim.fn.timer_start(20,function(t) "+action+"; count=count+1; if count==100 then vim.fn.timer_stop(t); vim.g.screenbench_elapsed=(vim.uv.hrtime()-vim.g.screenbench_start)/1e6; vim.g.screenbench_done=true end end,{['repeat']=-1}); return true")
          eventually(lambda:lua(rig.env,rig.address,'return vim.g.screenbench_done')=='true',seconds=15);time.sleep(.1);end=time.perf_counter()
          row['workloads'][kind]=dict(out.stats(start,end),producer_ms=float(lua(rig.env,rig.address,'return vim.g.screenbench_elapsed')))
         results.append(row);args.output.write_text(json.dumps(results,indent=2));print(json.dumps(row),flush=True)
        finally:
         if ws:ws.close()
         if proc:
          proc.terminate()
          try:proc.wait(timeout=5)
          except subprocess.TimeoutExpired:proc.kill();proc.wait()
         if master is not None:os.close(master)
         if thread:thread.join(timeout=2)
         eventually(lambda:lua(rig.env,rig.address,'return #vim.api.nvim_list_uis()')=='0')
     finally:rig.close()
    args.output.write_text(json.dumps(results,indent=2))


if __name__ == "__main__":
    main()
