#!/usr/bin/env python3
"""Staging regression with disposable binaries and dependencies; no host download."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

script = Path(__file__).with_name('stage-preview.py').resolve()
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    def write(name, data):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)
        return path
    host = write('build/host/bin/isolate-host', 'test host')
    write('target/debug/isolate-preview', 'test preview')
    write('runtime/isolate-host.json', json.dumps({'binarySha256': hashlib.sha256(host.read_bytes()).hexdigest()}))
    for name in ['build/host/licenses/third-party', 'build/host/LICENSE', 'build/host/NOTICE', 'build/host/manifest.json', 'preview/fixtures/sdk-counter.js', 'crates/isolate-preview/sdk/LICENSE', 'crates/isolate-preview/sdk/modules.json', 'crates/isolate-preview/sdk/entry.js', 'crates/isolate-preview/sdk/provenance.json', 'crates/isolate-preview/browser/client.js', 'crates/isolate-preview/browser/provenance.json', 'crates/isolate-protocol/licenses/LICENSE']:
        write(name, name)
    ldd = write('tools/ldd', '#!/bin/sh\nexit 0\n')
    ldd.chmod(0o755)
    env = dict(os.environ, PATH=str(ldd.parent) + os.pathsep + os.environ['PATH'])
    def stage(ok=True):
        result = subprocess.run([sys.executable, str(script)], cwd=root, env=env, capture_output=True, text=True)
        assert (result.returncode == 0) == ok, result.stderr
    stage()
    published = root / 'build/grain-root'
    previous = published.resolve()
    (published / 'bin/obsolete').write_text('must not ship')
    stage()
    assert published.resolve() != previous
    assert not (published / 'bin/obsolete').exists()
    receipt = json.loads((published / 'runtime.json').read_text())
    assert 'bin/obsolete' not in receipt['files']
    assert 'bin/isolate-host' in receipt['files']
    assert (previous / 'bin/obsolete').is_file(), 'existing readers lost their generation'
    previous = published.resolve()
    ldd.write_text('#!/bin/sh\nexit 1\n')
    stage(ok=False)
    assert published.resolve() == previous, 'failed staging changed the published runtime'
print('PASS: clean staging, explicit inventory, atomic publication and failure preservation')
