#!/usr/bin/env python3
"""Run against the ordinary supervisor socket created by smoke-grain-vm.sh."""
import json
import pathlib
import subprocess
import sys
import tempfile

socket = sys.argv[1]
client = ['/opt/app/target/debug/grain-smoke', socket]
def request(path, payload=None, mode='web', ok=True):
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json') as file:
        if payload is not None:
            json.dump(payload, file); file.flush()
        result = subprocess.run(client + [path, file.name if payload is not None else '-', mode], capture_output=True, text=True, timeout=20)
    assert (result.returncode == 0) == ok, (path, result.stdout, result.stderr)
    return json.loads(result.stdout) if ok else result.stderr

def status():
    return request('__spki/v1/status', mode='api')['revision']

session = request('session')
assert session['user']['displayName'] == 'Probe', session
assert session['sessionType'] == 'web', session
assert request('storage-check') == {'ok':True}
original = status()
source = pathlib.Path('/opt/app/preview/fixtures/sdk-counter.js').read_text()
v2 = source.replace('    const api =', "    if (new URL(request.url).pathname === '/version') return Response.json({version:2});\n    const api =")
payload = {'source': v2, 'expectedRevision': original}
assert 'NotFound' in request('__spki/v1/activate', payload, ok=False)
assert 'NotFound' in request('%5f%5fspki/v1/status', ok=False)
assert 'Forbidden' in request('__spki/v1/activate', payload, mode='api-denied', ok=False)
assert status() == original
assert 'syntax error' in request('__spki/v1/activate', {'source':'export default { fetch( BROKEN', 'expectedRevision':original}, mode='api', ok=False)
assert status() == original and request('')['count'] == 1
assert 'revision conflict' in request('__spki/v1/activate', {'source':v2, 'expectedRevision':'stale'}, mode='api', ok=False)
new = request('__spki/v1/activate', payload, mode='api')['revision']
assert new != original and status() == new
assert request('version') == {'version':2}
assert request('')['count'] == 1
assert request('__spki/v1/activate', payload, mode='api')['revision'] == new
# A valid module with the wrong export shape must also leave the live worker intact.
request('__spki/v1/activate', {'source':'export default {};', 'expectedRevision':new}, mode='api', ok=False)
assert status() == new and request('version') == {'version':2}
print('PASS: SDK session, permission boundaries, invalid candidate, conflicts, activation, idempotency, storage preservation')

# Two independent API sessions cannot both commit against the same old revision.
from concurrent.futures import ThreadPoolExecutor
v3 = v2.replace('version:2', 'version:3')
v4 = v2.replace('version:2', 'version:4')
def compete(code):
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json') as file:
        json.dump({'source':code,'expectedRevision':new},file);file.flush()
        return subprocess.run(client + ['__spki/v1/activate',file.name,'api'],capture_output=True,text=True,timeout=20)
with ThreadPoolExecutor(max_workers=2) as pool:
    results = list(pool.map(compete,[v3,v4]))
assert sorted(r.returncode == 0 for r in results) == [False,True], [(r.stdout,r.stderr) for r in results]
assert any('revision conflict' in r.stderr for r in results), [(r.stdout,r.stderr) for r in results]
request('__spki/v1/activate', {'source':v2,'expectedRevision':status()}, mode='api')
assert request('')['count'] == 1
print('PASS: concurrent activations commit exactly one revision')
