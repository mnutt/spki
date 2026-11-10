#!/usr/bin/env python3
"""Verify production HTTP streams under ordinary supervision; no upload/install."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

root = Path('/opt/app')
with tempfile.TemporaryDirectory(prefix='spki-streaming-') as directory:
    temp = Path(directory)
    package = temp / 'package'
    shutil.copytree(root / 'build/grain-root', package)
    shutil.copyfile(root / 'examples/streaming/dist/worker.js', package / 'app/worker.js')
    (package / 'app/config.json').write_text('{"title":"HTTP streaming"}')
    supervisor = temp / 'sandstorm-supervisor'
    supervisor.symlink_to('/opt/sandstorm/latest/sandstorm')
    grain = temp / 'grain'
    with (temp / 'supervisor.log').open('w+') as log:
        process = subprocess.Popen([
            'sudo', str(supervisor), '--uid', str(os.getuid()), '--pkg', str(package),
            '--var', str(grain), '--new', 'streamtest', 'streamtest',
            '/bin/isolate-preview', 'app', '/bin/isolate-host', '/app/worker.js',
        ], stdout=log, stderr=log)
        try:
            for _ in range(100):
                if (grain / 'socket').exists():
                    break
                assert process.poll() is None, (temp / 'supervisor.log').read_text()
                time.sleep(.1)
            probe = [str(root / 'target/debug/grain-smoke'), str(grain / 'socket')]
            for mode in ['web', 'api']:
                response = subprocess.run(probe + ['download?totalBytes=2097152&chunkBytes=65536&delayMs=0', '-', mode],
                    env=dict(os.environ, SPKI_SMOKE_STREAM='1'), capture_output=True, timeout=30)
                assert response.returncode == 0, response.stderr
                assert len(response.stdout) == 2097152, len(response.stdout)
            response = subprocess.run(probe + ['', '-', 'web'], env=dict(os.environ, SPKI_SMOKE_STREAM='1'),
                capture_output=True, timeout=30)
            assert response.returncode == 0, response.stderr
            assert b'Isolate Streaming' in response.stdout
            assert b'__spki/v1/reload.js' not in response.stdout
            print('PASS: production WebSession/ApiSession streaming, 2 MiB bodies, clean EOF, no Preview injection')
        except Exception:
            print((temp / 'supervisor.log').read_text())
            subprocess.run(['sudo', 'cat', str(grain / 'log')], check=False)
            raise
        finally:
            subprocess.run(['sudo', 'kill', str(process.pid)], check=False, capture_output=True)
            process.wait(timeout=15)
