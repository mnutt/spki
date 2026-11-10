#!/usr/bin/env python3
"""Build and verify a production SPK with no runtime directory or helper executables."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
cli = Path(os.environ.get('SPKI_TEST_BINARY', str(root / 'target/debug/spki'))).resolve()
(root / 'build').mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='embedded-runtime-test-', dir=root / 'build') as temporary:
    base = Path(temporary)
    env = dict(os.environ, PATH='/no-host-tools', TMPDIR=str(base), XDG_CONFIG_HOME=str(base / 'credentials'))
    def run(*args):
        result = subprocess.run([str(cli), *map(str, args)], env=env, cwd=base, capture_output=True, text=True, timeout=240)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout.strip()
    project = base / 'project'
    run('init', project)
    keyring = base / 'keys'
    app_id = run('keygen', '--keyring', keyring)
    package = base / 'app.spk'
    run('--project', project, 'pack', '--keyring', keyring, '--app-id', app_id, '--output', package)
    info = json.loads(run('verify', package))
    assert info['appId'] == app_id and info['version'] == 1, info
    assert not (base / 'credentials').exists()
    assert sorted(path.name for path in base.iterdir()) == ['app.spk', 'keys', 'project'], 'temporary packing files leaked'
print('PASS: embedded Linux runtime packs and verifies offline without a runtime directory or external tools')
