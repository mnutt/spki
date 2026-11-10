#!/usr/bin/env python3
"""Offline project initialization/configuration regression checks."""
import os
import re
from pathlib import Path
import subprocess
import tempfile
root=Path(__file__).resolve().parent.parent
cli=root/'target/debug/spki'
with tempfile.TemporaryDirectory() as temp:
    base=Path(temp)
    env=dict(os.environ,XDG_CONFIG_HOME=str(base/'credentials'))
    def run(*args,cwd=base,ok=True):
        result=subprocess.run([str(cli),*args],cwd=cwd,env=env,capture_output=True,text=True,timeout=25)
        assert (result.returncode==0)==ok,(args,result.stdout,result.stderr)
        return result
    project=base/'example'
    run('--profile','example','init',str(project))
    config=project/'sandstorm-pkgdef.capnp'
    original=config.read_text()
    assert 'profile = "example"' in original
    assert not (base/'credentials').exists(), 'init must not create credentials'
    run('build',cwd=project/'src')
    output=project/'dist/worker.js'
    first=output.read_text()
    assert 'Hello from your isolate' in first
    assert str(project) not in first
    (project/'src/greeting.ts').write_text("export const greeting: string = 'Updated';")
    run('--project',str(project),'build')
    assert 'Updated' in output.read_text()
    # Init is transactional and never overwrites existing project files.
    run('init',str(project),ok=False)
    assert config.read_text()==original
    empty=base/'empty';empty.mkdir()
    inode=empty.stat().st_ino
    run('init',str(empty))
    assert empty.stat().st_ino == inode, 'init replaced an existing directory'
    # Keep the same shell cwd across init and auth, matching normal interactive use.
    inplace=base/'inplace';inplace.mkdir()
    result=subprocess.run(['bash','-c','"$1" init && "$1" auth','bash',str(cli)],cwd=inplace,env=env,input='',capture_output=True,text=True,timeout=25)
    assert result.returncode != 0 and 'no webkey supplied' in result.stderr, (result.stdout,result.stderr)
    # Enter submits the prompted input even while stdin remains open; EOF is unnecessary.
    process=subprocess.Popen([str(cli),'auth'],cwd=inplace,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        process.stdin.write('not-a-webkey\n');process.stdin.flush()
        process.wait(timeout=5)
        error=process.stderr.read()
        assert process.returncode != 0 and 'invalid webkey URL' in error and 'press Enter' in error, error
        assert 'not-a-webkey' not in error, 'prompt echoed the submitted key'
    finally:
        if process.poll() is None: process.kill();process.wait()
        process.stdin.close();process.stdout.close();process.stderr.close()
    run('build',cwd=inplace)
    for bad in [
        original.replace('formatVersion = 1','formatVersion = 99'),
        original.replace('2026-06-10','2000-01-01'),
        original.replace('release = "0.1.2"','release = "wrong"'),
        original.replace('name = "rolldown"','name = "other"'),
        original.replace('src/worker.ts','../worker.ts'),
        original.replace('profile = "example"','profile = "../bad"'),
        re.sub(r'project = "[^"]+"', 'project = "../bad"', original),
        original.replace('appVersion = 1','appVersion = 1, continueCommand = (argv = ["/bin/native"])'),
        original.replace('worker = (','unknownField = true, worker = ('),
    ]:
        config.write_text(bad)
        run('build',cwd=project,ok=False)
        assert 'Updated' in output.read_text(),'failed build overwrote last output'
    config.write_text(original)
    # Explicit entrypoints continue to work without project config.
    run('build','--entry',str(project/'src/worker.ts'),'--output',str(base/'explicit.js'))
    # The embedded compiler ignores obsolete external tool environment settings.
    env['SPKI_ESBUILD']='/does/not/exist'
    env['CAPNP']='/does/not/exist'
    env['PATH']='/no-host-tools'
    run('init', str(base/'no-host-tools'))
    run('build',cwd=project/'src')
    # CLI override wins without changing the configured profile.
    assert 'no readable credential' in run('auth','status',cwd=project,ok=False).stderr
    assert not (base/'credentials').exists()
print('PASS: init, subdirectory discovery, offline/repeated builds, profile/path resolution, pin validation, safe failures')
