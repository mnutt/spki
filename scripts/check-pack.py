#!/usr/bin/env python3
"""Native writer/SDK interoperability. Does not upload or install packages."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import sys
root=Path(__file__).resolve().parent.parent
resume=len(sys.argv)>1
work=Path(sys.argv[1]).resolve() if resume else Path(tempfile.mkdtemp(prefix='native-pack-',dir=root/'build'))
cli=root/'target/debug/spki'
env=dict(os.environ,SPKI_ESBUILD=str(root/'build/tools/esbuild'),XDG_CONFIG_HOME=str(work/'credentials'))
def run(*args,ok=True):
    result=subprocess.run([str(a) for a in args],env=env,capture_output=True,text=True,timeout=240)
    assert (result.returncode==0)==ok,(args,result.stdout,result.stderr)
    return result.stdout.strip()
project=work/'project'
keyring=work/'keys'
config=project/'sandstorm-pkgdef.capnp'
if not resume:
    run(cli,'init',project)
    # Test credentials and unrelated files must not enter the package.
    (project/'secret.key').write_text('PRIVATE-FILE-DO-NOT-PACK')
    (project/'.env').write_text('SECRET=PRIVATE-FILE-DO-NOT-PACK')
    (project/'src/worker.ts').write_text((root/'preview/fixtures/sdk-counter.js').read_text())
    app_id=run(cli,'keygen','--keyring',keyring)
    assert len(app_id)==52
    sdk_id=run('spk','keygen','--keyring='+str(keyring)).splitlines()[-1]
    assert app_id!=sdk_id and len(sdk_id)==52
    assert app_id in run('spk','listkeys','--keyring='+str(keyring))
    config.write_text(config.read_text().replace('const pkgdef :Spk.PackageDefinition = (','const pkgdef :Spk.PackageDefinition = (\n  id = "'+app_id+'",'))
else:
    import re
    app_id=re.search(r'id = "([^"]+)"',config.read_text()).group(1)
    sdk_id=[line for line in run('spk','listkeys','--keyring='+str(keyring)).splitlines() if len(line)==52 and line!=app_id][0]
original=config.read_bytes();keys=keyring.read_bytes()
output=work/'app.spk'
print('Packing native production app...',flush=True)
if not resume: run(cli,'--project',project,'pack','--output',output,'--runtime-root',root/'build/grain-root','--keyring',keyring)
native=json.loads(run(cli,'verify',output));sdk=json.loads(run('spk','verify',output))
assert native['appId']==sdk['appId']==app_id
assert native['version']==sdk['version']==1
assert config.read_bytes()==original and keyring.read_bytes()==keys
assert not (work/'credentials').exists()
unpacked=work/'unpacked'
if not unpacked.exists(): run('spk','unpack',output,unpacked)
for excluded in ['secret.key','.env','src','sandstorm-pkgdef.capnp','keys']:
    assert not (unpacked/excluded).exists(),excluded
assert not (unpacked/'app/active.json').exists()
assert json.loads((unpacked/'app/config.json').read_text())['title']=='My isolate'
manifest=subprocess.run(['capnp','decode','-I'+str(root/'crates/spki/schema'),str(root/'crates/spki/schema/sandstorm/package.capnp'),'Manifest'],input=(unpacked/'sandstorm-manifest').read_bytes(),capture_output=True,timeout=15)
assert manifest.returncode==0,manifest.stderr
text=manifest.stdout.decode()
assert '"app"' in text and 'isolate =' not in text and '"grain"' not in text
# The old SDK writer provides an independent verification vector.
fixture=work/'sdk-root';fixture.mkdir(exist_ok=True)
(fixture/'sandstorm-manifest').write_bytes((unpacked/'sandstorm-manifest').read_bytes())
pkgdef=work/'sdk.capnp'
pkgdef.write_text('@0xbdc044ad8f18f734;\nusing Spk = import "/sandstorm/package.capnp";\nconst pkgdef :Spk.PackageDefinition = (id = "'+sdk_id+'", manifest = (appTitle = (defaultText = "SDK vector"), appVersion = 1, appMarketingVersion = (defaultText = "1.0")), sourceMap = (searchPath = [(sourcePath = "sdk-root")]), alwaysInclude = ["."]);\n')
sdk_output=work/'sdk.spk'
run('spk','pack','--keyring='+str(keyring),'--pkg-def='+str(pkgdef)+':pkgdef',sdk_output)
assert json.loads(run(cli,'verify',sdk_output))['appId']==sdk_id
corrupted=work/'corrupt.spk';data=bytearray(output.read_bytes());data[len(data)//2]^=1;corrupted.write_bytes(data)
run(cli,'verify',corrupted,ok=False)
run(cli,'--project',project,'pack','--output',output,'--runtime-root',root/'build/grain-root','--keyring',keyring,ok=False)
run(cli,'--project',project,'pack','--output',work/'wrong.spk','--target','linux-arm64','--runtime-root',root/'build/grain-root','--keyring',keyring,ok=False)
assert not (work/'wrong.spk').exists()
# An independently signed second version must preserve app identity and use new packaged source.
config.write_text(config.read_text().replace('appVersion = 1','appVersion = 2'))
worker=project/'src/worker.ts'
worker.write_text(worker.read_text().replace('    const api =', "    if (new URL(request.url).pathname === '/version') return Response.json({version:2});\n    const api ="))
upgrade=work/'app-v2.spk'
print('Packing upgrade...',flush=True)
run(cli,'--project',project,'pack','--output',upgrade,'--runtime-root',root/'build/grain-root','--keyring',keyring)
verified=json.loads(run('spk','verify',upgrade))
assert verified['appId']==app_id and verified['version']==2
upgraded=work/'upgraded'
run('spk','unpack',upgrade,upgraded)
(root/'build/native-pack-test.json').write_text(json.dumps({'directory':str(work),'package':str(output),'unpacked':str(unpacked),'upgraded':str(upgraded),'appId':app_id},indent=2)+'\n')
print('PASS: native/SDK package and keyring interoperability, manifest, private-file exclusion, no auth dependency, corruption and target rejection')
print('Package:',output)
print('Unpacked:',unpacked)
