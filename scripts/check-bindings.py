#!/usr/bin/env python3
"""Offline binding validation, revision inputs, and production artifact identity."""
import base64, json, os, pathlib, shutil, subprocess, tempfile
root=pathlib.Path(__file__).resolve().parent.parent
work=pathlib.Path(tempfile.mkdtemp(prefix='bindings-pack-',dir=root/'build'))
project=work/'project'
shutil.copytree(root/'examples/service-binding', project, ignore=shutil.ignore_patterns('dist','.spki'))
cli=root/'target/debug/spki'
env=dict(os.environ,SPKI_ESBUILD=str(root/'build/tools/esbuild'))
def run(*args,ok=True):
    r=subprocess.run([str(a) for a in args],env=env,capture_output=True,text=True,timeout=300)
    assert (r.returncode==0)==ok,(args,r.stdout,r.stderr)
    return r.stdout.strip()
def build(ok=True): return run(cli,'--project',project,'build',ok=ok)
config=project/'sandstorm-pkgdef.capnp'
original=config.read_text()
build()
output=project/'dist/worker.js'
first=output.read_bytes()
metadata=json.loads(first.split(b'\n',1)[0].removeprefix(b'// spki-bindings-v1:'))
assert base64.b64decode(metadata[2]['value']['data'])==(project/'payload.bin').read_bytes()
(project/'payload.bin').write_bytes(b'\x00\xff\x01')
build();assert output.read_bytes()!=first
last=output.read_bytes()
for old,new in [
    ('MESSAGE','STORAGE'),('MESSAGE','__SANDSTORM_NATIVE_CAPNP'),('MESSAGE','PAYLOAD'),
    ('service = "main"','service = "other"'),('dataFile = "payload.bin"','dataFile = "../payload.bin"'),
    ('json = "{\\"enabled\\":true,\\"count\\":3}"','json = "{"'),
]:
    config.write_text(original.replace(old,new));build(ok=False)
    assert output.read_bytes()==last,'invalid configuration overwrote previous artifact'
config.write_text(original)
(project/'payload.bin').unlink();(project/'payload.bin').symlink_to(root/'examples/service-binding/payload.bin')
build(ok=False);assert output.read_bytes()==last
(project/'payload.bin').unlink();(project/'payload.bin').write_bytes(b'x'*(1024*1024+1))
build(ok=False);assert output.read_bytes()==last
(project/'payload.bin').write_bytes(b'\x00\xff\x01')
key=work/'keys';app_id=run(cli,'keygen','--keyring',key)
config.write_text(original.replace('const pkgdef :Spk.PackageDefinition = (','const pkgdef :Spk.PackageDefinition = (\n  id = "'+app_id+'",'))
build()
package=work/'app.spk'
run(cli,'--project',project,'pack','--output',package,'--runtime-root',root/'build/grain-root','--keyring',key)
assert json.loads(run('spk','verify',package))['appId']==app_id
unpacked=work/'unpacked';run('spk','unpack',package,unpacked)
assert (unpacked/'app/worker.js').read_bytes()==output.read_bytes()
(root/'build/bindings-pack-test.json').write_text(json.dumps({'directory':str(work),'unpacked':str(unpacked),'package':str(package)}))
print('PASS: binding inputs, reserved names, invalid JSON/service, unsafe/oversized files, failed-build preservation, production artifact identity')
