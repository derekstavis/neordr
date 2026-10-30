#!/usr/bin/env python3
"""Bidirectional PTY backpressure, disconnects, and UI-only ownership cleanup."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from moshi_smoke import BIN, eventually, lua, rpc
from attach_prompt_smoke import Client


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try: process.wait(timeout=3)
        except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=3)


def outer(command, env, drain=True):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 51, 0, 0))
    process = subprocess.Popen(command, env=env, stdin=slave, stdout=slave,
                               stderr=slave, start_new_session=True)
    os.close(slave)
    reader = None
    if drain:
        def read():
            try:
                while os.read(master, 65536): pass
            except OSError: pass
        reader = threading.Thread(target=read, daemon=True); reader.start()
    return process, master, reader


def main():
    with tempfile.TemporaryDirectory(prefix='nal-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        env = {k:v for k,v in os.environ.items() if k != 'NVIM' and not k.startswith(('HERDR_', 'NEORDR_'))}
        env.update(XDG_CONFIG_HOME=str(root/'c'), NVIM_LOG_FILE=str(root/'nvim.log'), TERM='xterm-256color')
        address, endpoint = root/'nvim.sock', root/'c/herdr/sessions/ui/herdr.sock'
        server = bridge = wide = None
        try:
            server = subprocess.Popen(['nvim','--headless','-u','NONE','-i','NONE','--listen',str(address)],
                env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            eventually(address.exists)
            bridge = subprocess.Popen([str(BIN),'--session','ui','serve','--nvim',str(address)],
                env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            eventually(lambda: rpc(endpoint,'ping'))
            # Deterministic transport peer: fill UI output before reading input.
            # The former blocking forwarder deadlocks when both PTYs fill.
            fake = root/'bin'; fake.mkdir()
            payload = b'lossless-pty-data-' * 32768
            peer = fake/'nvim'
            peer.write_text(f'#!{sys.executable}\nimport os,tty,hashlib,time\nfrom pathlib import Path\n'
                'tty.setraw(0)\n'
                f'Path({str(root/"peer.pid")!r}).write_text(str(os.getpid()))\n'
                f'while not Path({str(root/"go")!r}).exists(): time.sleep(.01)\n'
                "data=b'OUTPUT-'*150000\nwhile data:\n n=os.write(1,data); data=data[n:]\n"
                f'wanted={len(payload)}\nreceived=b""\n'
                'while len(received)<wanted: received+=os.read(0,min(4096,wanted-len(received)))\n'
                f'Path({str(root/"received")!r}).write_text(hashlib.sha256(received).hexdigest())\n'
                'time.sleep(120)\n')
            peer.chmod(0o755)
            fake_env = {**env,'PATH':str(fake)+os.pathsep+env['PATH']}
            for case in ['lossless', 'outer-write-before-read', 'blocked-output-sigterm']:
                for path in [root/'peer.pid',root/'received',root/'go']:path.unlink(missing_ok=True)
                attached, master, reader = outer([str(BIN),'--session','ui'], fake_env, drain=case=='lossless')
                writer = None
                try:
                    eventually((root/'peer.pid').exists)
                    if case in ['lossless', 'outer-write-before-read']:
                        output=[]
                        def send():
                            try:
                                remaining=payload
                                while remaining:
                                    n=os.write(master,remaining);remaining=remaining[n:]
                                # Like Mosh's synchronous PTY write: it cannot
                                # read terminal output until this write finishes.
                                if case == 'outer-write-before-read':
                                    remaining=7*150000
                                    while remaining:
                                        data=os.read(master,min(65536,remaining))
                                        if not data:break
                                        output.append(data);remaining-=len(data)
                            except OSError: pass
                        writer=threading.Thread(target=send,daemon=True);writer.start()
                        time.sleep(.2) # Fill UI input before allowing its output burst.
                        (root/'go').touch()
                        eventually((root/'received').exists,seconds=10)
                        assert (root/'received').read_text()==hashlib.sha256(payload).hexdigest()
                        writer.join(timeout=2);assert not writer.is_alive()
                        if case == 'outer-write-before-read':
                            assert b''.join(output)==b'OUTPUT-'*150000
                    else:
                        (root/'go').touch()
                        time.sleep(.3) # Let the unattended outer PTY fill.
                    attached.terminate()
                    assert attached.wait(timeout=4)==0, case
                    print(case+': PASS',flush=True)
                finally:
                    # The old implementation cannot service SIGTERM in a blocked write.
                    if (root/'peer.pid').exists():
                        try: os.kill(int((root/'peer.pid').read_text()),signal.SIGKILL)
                        except ProcessLookupError: pass
                    stop(attached);os.close(master)
                    if reader:reader.join(timeout=1)
                    if writer:writer.join(timeout=1)
            if '--pump-only' in sys.argv:return
            # A real editor and terminal survive every UI teardown. A wide UI
            # makes failure to release the narrow client's size observable.
            job=int(lua(env,address,"vim.fn.jobstart({'/bin/sh','-c','sleep 120'},{term=true}); "
                "return vim.fn.jobpid(vim.bo.channel)"))
            wide=Client(['nvim','--server',str(address),'--remote-ui'],env)
            eventually(lambda:lua(env,address,'return vim.o.columns')=='100')
            for case in ['sighup','sigterm','outer-hangup','owner-death']:
                marker=root/'attachment.pid';marker.unlink(missing_ok=True)
                launcher=root/'launcher.py'
                launcher.write_text('import subprocess,time\nfrom pathlib import Path\n'
                    f'p=subprocess.Popen({[str(BIN),"--session","ui"]!r},start_new_session=True)\n'
                    f'Path({str(marker)!r}).write_text(str(p.pid))\n'
                    'p.wait()\n')
                # A blocked read on the same master can make close hang on macOS.
                # This case intentionally closes the PTY, so no reader is needed.
                owner,master,reader=outer([sys.executable,str(launcher)],env,drain=case!='outer-hangup')
                attach_pid=None
                try:
                    eventually(marker.exists);attach_pid=int(marker.read_text())
                    eventually(lambda:lua(env,address,'return #vim.api.nvim_list_uis()')=='2')
                    eventually(lambda:lua(env,address,'return vim.o.columns')=='51')
                    if case=='owner-death':owner.kill();owner.wait(timeout=3) # Keep PTY open: no HUP shortcut.
                    elif case=='outer-hangup':os.close(master);master=None
                    else:os.kill(attach_pid,signal.SIGHUP if case=='sighup' else signal.SIGTERM)
                    eventually(lambda:lua(env,address,'return #vim.api.nvim_list_uis()')=='1',seconds=6)
                    eventually(lambda:lua(env,address,'return vim.o.columns')=='100')
                    assert server.poll() is None
                    os.kill(job,0)
                    print(case+': UI detached, size restored, terminal job preserved',flush=True)
                finally:
                    if attach_pid:
                        try:os.kill(attach_pid,signal.SIGKILL)
                        except ProcessLookupError:pass
                    stop(owner)
                    if master is not None:os.close(master)
                    if reader:reader.join(timeout=1)
        finally:
            if wide:wide.close()
            stop(bridge);stop(server)


if __name__=='__main__':main()
