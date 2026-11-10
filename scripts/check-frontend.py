#!/usr/bin/env python3
"""Frontend artifact reproducibility, revision changes, admission and deleted assets."""
import os, pathlib, shutil, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory() as temp:
    project = pathlib.Path(temp)/'project'
    env = dict(os.environ)
    def run(*args, ok=True):
        result = subprocess.run([str(root/'target/debug/spki'), *args], env=env, capture_output=True, text=True)
        assert (result.returncode == 0) == ok, result.stderr
        return result
    run('init',str(project))
    shutil.copytree(root/'preview/fixtures/frontend',project,dirs_exist_ok=True)
    config=project/'sandstorm-pkgdef.capnp'
    config.write_text(config.read_text().replace('src/worker.ts','worker.ts').replace('  compatibilityDate','  ui = (entrypoints = ["ui/main.ts"]),\n  assets = ["public"],\n  schemas = ["counter.capnp"],\n  compatibilityDate'))
    def build(ok=True):
        run('--project',str(project),'build',ok=ok)
        return (project/'dist/worker.js').read_bytes()
    # Imported schemas must be generated and resolved in both environments.
    (project/'common.capnp').write_text('@0xbaac70a32fc785de; struct Sample { text @0 :Text; }')
    schema=project/'counter.capnp'
    schema.write_text(schema.read_text().replace('interface BrowserCounter', 'using Common = import "common.capnp";\ninterface BrowserCounter').replace('  read @0', '  sample @6 () -> (value :Common.Sample);\n  read @0'))
    worker=project/'worker.ts'
    worker.write_text("import {Message} from '@mnutt/capnp-es';\nexport const message = new Message();\n"+worker.read_text())
    ui=project/'ui/main.ts'
    ui.write_text("import {Message} from '@mnutt/capnp-es';\nwindow.message = new Message();\n"+ui.read_text())
    first=build()
    assert first == build(), 'unchanged builds differ'
    assert str(project).encode() not in first and b'/tmp/' not in first, 'absolute build paths leaked'
    extra=project/'public/extra.txt';extra.write_text('EXTRA-ASSET')
    second=build();assert second != first
    extra.unlink();assert build() == first, 'deleted asset remains in artifact'
    style=project/'ui/style.css';style.write_text('body {color: red}')
    assert build() != first, 'CSS changes do not affect revision'
    (project/'public/.env').write_text('secret')
    build(ok=False)
    (project/'public/.env').unlink()
    (project/'public/link').symlink_to(project/'worker.ts');build(ok=False)
    (project/'public/link').unlink()
    (project/'public/__spki').write_text('reserved');build(ok=False)
print('PASS: deterministic frontend artifact, CSS/public edits, deleted files, private/symlink/reserved-path rejection')
