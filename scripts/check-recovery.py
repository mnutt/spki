#!/usr/bin/env python3
"""Simulate a changed SDK fingerprint without replacing the VM or installed app."""
import json, pathlib, subprocess, sys, tempfile
grain = pathlib.Path(sys.argv[1])
root = grain/'sandbox'
active = root/'active.json'
if len(sys.argv) > 2 and sys.argv[2] == 'prepare':
    saved = json.loads(active.read_text())
    saved['revision'] = '0'*64
    active.write_text(json.dumps(saved))
    (root/'upgrade-test-original.json').write_bytes(active.read_bytes())
    sys.exit(0)
client = ['/opt/app/target/debug/grain-smoke', str(grain/'socket')]
def request(path, body=None, mode='api'):
    with tempfile.NamedTemporaryFile(mode='w') as file:
        if body is not None: json.dump(body,file); file.flush()
        result = subprocess.run(client+[path,file.name if body is not None else '-',mode],capture_output=True,text=True,timeout=30)
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()
info = json.loads(request('__spki/v1/status'))
assert info['recovery'] is True, info
assert 'needs redeployment' in request('', mode='web')
original = (root/'upgrade-test-original.json').read_bytes()
assert active.read_bytes() == original, 'recovery startup overwrote saved code'
source = json.loads(original)['source']
request('__spki/v1/activate', {'source':source, 'expectedRevision':info['revision']})
assert json.loads(request('__spki/v1/status'))['recovery'] is False
assert (root/'active.previous.json').read_bytes() == original
assert json.loads(request('',mode='web')) == {'count':1}
assert json.loads(request('version',mode='web')) == {'version':2}
print('PASS: SDK upgrade recovery, source backup, redeployment and preserved storage')
