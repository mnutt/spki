#!/usr/bin/env python3
"""Verify a bindings SPK under ordinary supervision; no upload or installation."""
import json, os, subprocess, tempfile, time
from pathlib import Path
root=Path('/opt/app')
receipt=json.loads((root/'build/bindings-pack-test.json').read_text())
package=root/'build'/Path(receipt['unpacked']).relative_to(Path(receipt['directory']).parent)
with tempfile.TemporaryDirectory(prefix='spki-bindings-') as directory:
    directory=Path(directory)
    supervisor=directory/'sandstorm-supervisor'
    supervisor.symlink_to('/opt/sandstorm/latest/sandstorm')
    grain=directory/'grain'
    log=(directory/'supervisor.log').open('w+')
    process=None
    def stop():
        global process
        if process is not None:
            subprocess.run(['sudo','kill',str(process.pid)],capture_output=True)
            process.wait(timeout=15);process=None
    def start(new=False):
        socket=grain/'socket'
        if socket.exists(): subprocess.run(['sudo','rm','-f',str(socket)],check=True)
        command=['sudo',str(supervisor),'--uid',str(os.getuid()),'--pkg',str(package),'--var',str(grain)]
        if new: command+=['--new']
        command+=['bindingstest','bindingstest','/bin/isolate-preview','app','/bin/isolate-host','/app/worker.js']
        global process
        process=subprocess.Popen(command,stdout=log,stderr=log)
        for _ in range(150):
            if socket.exists(): return
            if process.poll() is not None: raise RuntimeError((directory/'supervisor.log').read_text())
            time.sleep(.1)
        raise RuntimeError('grain startup timed out')
    def check():
        r=subprocess.run([str(root/'target/debug/grain-smoke'),str(grain/'socket'),'json','-','web'],capture_output=True,text=True,timeout=30)
        assert r.returncode==0,(r.stdout,r.stderr)
        data=json.loads(r.stdout)
        assert data['textBinding']=='Hello from a binding',data
        assert data['jsonBinding']=={'enabled':True,'count':3},data
        assert data['dataBinding']=={'bytes':3,'checksum':256},data
        assert data['body']['route']=='/target' and data['body']['body']=='hello from the caller route',data
    try:
        start(True);check();stop();start();check()
    finally:
        stop()
print('PASS: production SPK text/JSON/data/loopback bindings and restart persistence')
