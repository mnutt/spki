#!/usr/bin/env python3
"""Production-mode frontend artifact in an ordinary supervised grain; no installation."""
import json, pathlib, subprocess, tempfile, time
root=pathlib.Path('/opt/app')
receipt=json.loads((root/'build/frontend-pack-test.json').read_text())
image=root/pathlib.Path(receipt['unpacked']).relative_to('/home/michael/p/personal/spki')
probe=root/'target/debug/grain-smoke'
with tempfile.TemporaryDirectory(prefix='spki-frontend-production-') as temp:
    temp=pathlib.Path(temp)
    supervisor=temp/'sandstorm-supervisor';supervisor.symlink_to('/opt/sandstorm/latest/sandstorm')
    process=None
    def start(new=False):
        global process
        log=(temp/'supervisor.log').open('w')
        process=subprocess.Popen(['sudo',str(supervisor),'--uid',str(__import__('os').getuid()),'--pkg',str(image),'--var',str(temp/'grain'),*(['--new'] if new else []),'frontendprobe','frontendprobe','/bin/isolate-preview','app','/bin/isolate-host','/app/worker.js'],stdout=log,stderr=subprocess.STDOUT)
        log.close()
        for _ in range(100):
            if (temp/'grain/socket').exists(): return
            if process.poll() is not None: raise RuntimeError((temp/'supervisor.log').read_text())
            time.sleep(.1)
        raise RuntimeError('grain startup timed out')
    def stop():
        if process and process.poll() is None:
            subprocess.run(['sudo','kill',str(process.pid)],check=True)
            process.wait(timeout=10)
        (temp/'grain/socket').unlink(missing_ok=True)
    def fetch(path,ok=True):
        result=subprocess.run([str(probe),str(temp/'grain/socket'),path],capture_output=True,text=True,timeout=30)
        assert (result.returncode==0)==ok,(result.stdout,result.stderr)
        return result.stdout if ok else result.stderr
    try:
        start(True)
        assert 'Typed browser RPC' in fetch('')
        assert 'connectBrowserNativeCapnp' in fetch('assets/main.js')
        assert 'rgb(20, 40, 60)' in fetch('assets/main.css')
        assert 'BrowserCounter' in fetch('__sandstorm/capnp/counter.capnp.js')
        assert 'BrowserIsolateBridge' in fetch('__sandstorm/native-capnp/client.js')
        for route in ['__spki/v1/logs','__spki/v1/activate','__spki_internal_close_session?id=test']:
            assert 'NotFound' in fetch(route,False)
        stop();start()
        assert 'Typed browser RPC' in fetch('')
        print('PASS: production frontend, browser SDK/schema assets, restart persistence, disabled Preview control routes')
    finally: stop()
