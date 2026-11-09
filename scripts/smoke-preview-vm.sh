#!/usr/bin/env bash
# Invoke inside the development VM after cargo build --locked on the host.
set -euo pipefail
cd /opt/app
python3 - <<'PY'
import json
from pathlib import Path
import subprocess
import tempfile

command = ['./target/debug/isolate-preview', 'probe', './build/host/bin/isolate-host']
# Grain startup must reject an absent inherited socket before Tokio can reuse fd 3.
missing_supervisor = subprocess.run(
    [command[0], 'grain', command[2], 'preview/fixtures/hello.js'],
    close_fds=True, capture_output=True, text=True, timeout=10,
)
assert missing_supervisor.returncode != 0, 'missing supervisor socket unexpectedly accepted'
assert 'missing supervisor fd 3' in missing_supervisor.stderr, missing_supervisor.stderr
def run(worker):
    return subprocess.run(command + [str(worker)], capture_output=True, text=True, timeout=30)

hello = run('preview/fixtures/hello.js')
assert hello.returncode == 0, hello.stderr
body = json.loads(hello.stdout)
assert body['ok'] and body['pathname'] == '/probe' and body['method'] == 'GET', body
with tempfile.TemporaryDirectory() as directory:
    worker = Path(directory) / 'worker.js'
    worker.write_text('export default { fetch( BROKEN')
    result = run(worker)
    assert result.returncode != 0, 'invalid JavaScript unexpectedly loaded'
    assert "syntax error" in result.stderr, result.stderr
    worker.write_text('export default { fetch() { return new Response("x".repeat(1048577)); } }')
    result = run(worker)
    assert result.returncode != 0, 'oversized response unexpectedly accepted'
    assert 'deadline has elapsed' not in result.stderr, result.stderr
    worker.write_text('export default { fetch() { return new Response(null, {status: 204}); } }')
    result = run(worker)
    assert result.returncode != 0 and 'unexpected HTTP status 204' in result.stderr, result.stderr
print('PASS: missing supervisor rejection, worker fetch, invalid JavaScript, response bound, empty response completion')
PY
