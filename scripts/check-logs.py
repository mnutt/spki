#!/usr/bin/env python3
"""Actual worker console output -> Rust capture -> Cap'n Proto streaming response."""
import json, os, pathlib, re, select, subprocess, sys, tempfile, time
client = ['/opt/app/target/debug/grain-smoke',sys.argv[1]]
def request(path, body=None, mode='api', ok=True, stream=False):
    with tempfile.NamedTemporaryFile(mode='w') as file:
        if body is not None: json.dump(body,file); file.flush()
        result = subprocess.run(client+[path,file.name if body is not None else '-',mode],env=dict(os.environ,**({'SPKI_SMOKE_STREAM':'1'} if stream else {})),capture_output=True,text=True,timeout=30)
    assert (result.returncode == 0) == ok, (result.stdout,result.stderr)
    return result.stdout if ok else result.stderr

def revision(): return json.loads(request('__spki/v1/status'))['revision']
def deploy(source): request('__spki/v1/activate',{'source':source,'expectedRevision':revision()})
def open_stream(after=None):
    return subprocess.Popen(client+['__spki/v1/logs'+('' if after is None else '?after='+after),'-','api'],env=dict(os.environ,SPKI_SMOKE_STREAM='1'),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
def read_until(process, marker):
    data=b''; deadline=time.monotonic()+15
    while marker not in data:
        assert process.poll() is None, process.stderr.read().decode()
        assert time.monotonic()<deadline, data.decode(errors='replace')
        if select.select([process.stdout],[],[],.2)[0]:
            data += os.read(process.stdout.fileno(),65536)
    return data.decode()
def close(process):
    process.terminate(); process.wait(timeout=5); process.stdout.close(); process.stderr.close()
assert 'Forbidden' in request('__spki/v1/logs',mode='api-denied',ok=False)
assert 'NotFound' in request('__spki/v1/logs',mode='web',ok=False)
assert 'invalid log cursor' in request('__spki/v1/logs?after=bad',ok=False,stream=True)
source='''export default { fetch(request) {
 const path = new URL(request.url).pathname;
 if (path === '/throw') throw new Error('spki-exception-marker');
 console.log('spki-log-' + path.slice(1));
 return new Response('ok');
}};'''
deploy(source)
stream=open_stream()
try:
    read_until(stream,b': connected')
    request('first-marker',mode='web')
    data=read_until(stream,b'spki-log-first-marker')
    cursor=re.findall(r'id: ([a-f0-9]+:\d+)',data)[-1]
    request('throw',mode='web',ok=False)
    read_until(stream,b'spki-exception-marker')
finally: close(stream)
request('disconnected-marker',mode='web')
stream=open_stream(cursor)
try:
    data=read_until(stream,b'spki-log-disconnected-marker')
    assert 'spki-log-first-marker' not in data, data
    assert 'spki-exception-marker' in data, data
finally: close(stream)
deploy(pathlib.Path('/opt/app/preview/fixtures/sdk-counter.js').read_text())
print('PASS: live console and exception streaming, permissions, reconnect replay without duplicates')
