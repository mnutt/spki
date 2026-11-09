#!/usr/bin/env python3
"""Verify frontend production packaging uses exactly the locally built artifact."""
import json, os, pathlib, shutil, subprocess, tempfile
root=pathlib.Path(__file__).resolve().parent.parent
work=pathlib.Path(tempfile.mkdtemp(prefix='frontend-pack-',dir=root/'build'))
project=work/'project'
cli=root/'target/debug/spki'
env=dict(os.environ,SPKI_ESBUILD=str(root/'build/tools/esbuild'))
def run(*args):
    result=subprocess.run([str(a) for a in args],env=env,capture_output=True,text=True,timeout=300)
    assert result.returncode==0,result.stderr
    return result.stdout.strip()
run(cli,'init',project)
shutil.copytree(root/'preview/fixtures/frontend',project,dirs_exist_ok=True)
key=work/'keys'; app_id=run(cli,'keygen','--keyring',key)
config=project/'sandstorm-pkgdef.capnp'
text=config.read_text().replace('src/worker.ts','worker.ts').replace('  compatibilityDate','  ui = (entrypoints = ["ui/main.ts"]),\n  assets = ["public"],\n  schemas = ["counter.capnp"],\n  compatibilityDate').replace('const pkgdef :Spk.PackageDefinition = (','const pkgdef :Spk.PackageDefinition = (\n  id = "'+app_id+'",')
config.write_text(text)
(project/'.env').write_text('PRIVATE-FILE-DO-NOT-PACK')
run(cli,'--project',project,'build')
print('Packing frontend production fixture...',flush=True)
package=work/'app.spk'
run(cli,'--project',project,'pack','--output',package,'--runtime-root',root/'build/grain-root','--keyring',key)
assert json.loads(run('spk','verify',package))['appId']==app_id
unpacked=work/'unpacked';run('spk','unpack',package,unpacked)
assert (unpacked/'app/worker.js').read_bytes()==(project/'dist/worker.js').read_bytes()
assert config.read_text()==text
assert not (unpacked/'.env').exists()
(root/'build/frontend-pack-test.json').write_text(json.dumps({'directory':str(work),'unpacked':str(unpacked),'package':str(package)}))
print('PASS: frontend dev/build/pack artifact identity, SDK verification, private-file exclusion, unchanged versions')
