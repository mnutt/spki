#!/usr/bin/env python3
"""Run the native SPK's unpacked production image under ordinary Sandstorm supervision."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
root=Path('/opt/app')
receipt=json.loads((root/'build/native-pack-test.json').read_text())
def image(key):
    return root/'build'/Path(receipt[key]).relative_to(Path(receipt['directory']).parent)
probe=root/'target/debug/grain-smoke'
with tempfile.TemporaryDirectory(prefix='spki-packed-') as temp:
    temp=Path(temp);supervisor=temp/'sandstorm-supervisor';supervisor.symlink_to('/opt/sandstorm/latest/sandstorm')
    grain=temp/'grain';log=(temp/'supervisor.log').open('w+')
    process=None
    def stop():
        global process
        if process is not None:
            subprocess.run(['sudo','kill',str(process.pid)],check=False,capture_output=True)
            process.wait(timeout=15);process=None
    def start(package,new=False):
        global process
        socket=grain/'socket'
        if socket.exists(): subprocess.run(['sudo','rm','-f',str(socket)],check=True)
        command=['sudo',str(supervisor),'--uid',str(os.getuid()),'--pkg',str(package),'--var',str(grain)]
        if new: command+=['--new']
        command+=['packtest','packtest','/bin/isolate-preview','app','/bin/isolate-host','/app/worker.js']
        process=subprocess.Popen(command,stdout=log,stderr=log)
        for _ in range(150):
            if socket.exists(): return
            if process.poll() is not None: raise RuntimeError((temp/'supervisor.log').read_text())
            time.sleep(.1)
        raise RuntimeError('grain startup timed out')
    def fetch(path,mode='web',ok=True,body=None):
        payload=temp/'payload.json'
        if body is not None: payload.write_text(json.dumps(body))
        result=subprocess.run([str(probe),str(grain/'socket'),path,str(payload) if body is not None else '-',mode],capture_output=True,text=True,timeout=30)
        assert (result.returncode==0)==ok,(result.stdout,result.stderr,(temp/'supervisor.log').read_text(),subprocess.run(['sudo','cat',str(grain/'log')],capture_output=True,text=True).stdout)
        return result.stdout.strip() if ok else result.stderr
    try:
        start(image('unpacked'),True)
        assert json.loads(fetch(''))=={'count':0}
        assert json.loads(fetch('increment'))=={'count':1}
        assert json.loads(fetch('',mode='api'))=={'count':1}
        for route in ['__spki/v1/logs','__spki/v1/info','__spki/v1/status','__spki/v1/revision','__spki/v1/reload.js','__spki_internal_ready']:
            for mode in ['web','api']:
                assert 'NotFound' in fetch(route,mode=mode,ok=False)
        assert 'NotFound' in fetch('__spki/v1/activate',mode='api',ok=False,body={'source':'export default {};','expectedRevision':'ignored'})
        stop()
        # Production must never load Preview's mutable-source state, even if restored into /var.
        subprocess.run(['sudo','sh','-c','printf broken > "$1/active.json"','sh',str(grain/'sandbox')],check=True)
        start(image('unpacked'))
        assert json.loads(fetch(''))=={'count':1}
        if 'upgraded' in receipt:
            stop();start(image('upgraded'))
            assert json.loads(fetch('version'))=={'version':2}
            assert json.loads(fetch(''))=={'count':1}
        print('PASS: native production package, ordinary API, no deployment/reload endpoints, restart/upgrade data persistence, packaged source authority')
    finally: stop()
