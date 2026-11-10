#!/usr/bin/env python3
"""VM integration: HTTP test facade -> actual ApiSession -> live ordinary grain.
This deliberately does not claim to test the Sandstorm shell's webkey flow.
"""
import http.server
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import time

socket_path = sys.argv[1]
probe = ['/opt/app/target/debug/grain-smoke', socket_path]
cli = os.environ.get('SPKI_TEST_BINARY', '/opt/app/target/debug/spki')
token = 'test-secret-never-print-9c8b'
state = {'redirect': False, 'drop': False, 'mismatch': False, 'posts': 0}

def grain(path, payload=None):
    with tempfile.NamedTemporaryFile(mode='w') as file:
        if payload is not None:
            json.dump(payload, file)
            file.flush()
        result = subprocess.run(probe + [path, file.name if payload is not None else '-', 'api'], capture_output=True, text=True, timeout=30)
    if result.returncode:
        return (409 if 'revision conflict' in result.stderr else 400), {'error': result.stderr}
    return 200, json.loads(result.stdout)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self): self.handle_request(False)
    def do_POST(self): self.handle_request(True)
    def handle_request(self, post):
        if state['redirect']:
            self.send_response(302)
            self.send_header('Location', '/redirect-target')
            self.end_headers()
            return
        if not post and self.path.startswith('/__spki/v1/logs') and self.headers.get('Authorization') == 'Bearer ' + token:
            process = subprocess.Popen(probe+[self.path.lstrip('/'),'-','api'],env=dict(os.environ,SPKI_SMOKE_STREAM='1'),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            try:
                self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
                while True:
                    data = process.stdout.read1(16384)
                    if not data: break
                    self.wfile.write(data); self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError): pass
            finally:
                process.terminate(); process.wait(timeout=5)
                process.stdout.close(); process.stderr.close()
            return
        if self.headers.get('Authorization') != 'Bearer ' + token:
            status, body = 403, {'error': 'denied ' + self.headers.get('Authorization', '').removeprefix('Bearer ')}
        else:
            assert '#' not in self.path and token not in self.path
            data = json.loads(self.rfile.read(int(self.headers['Content-Length']))) if post else None
            if post: state['posts'] += 1
            status, body = grain(self.path.lstrip('/'), data)
            if state['mismatch']: body['sdkRevision'] = 'wrong'
        if post and state['drop']:
            state['drop'] = False
            self.connection.shutdown(socket.SHUT_RDWR)
            self.connection.close()
            return
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
webkey = f'http://127.0.0.1:{server.server_port}/#{token}'
initial = pathlib.Path('/opt/app/preview/fixtures/sdk-counter.js').read_text()
def current(): return grain('__spki/v1/status')[1]['revision']
def wait_for(predicate):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(.1)
    raise AssertionError('condition timed out')

with tempfile.TemporaryDirectory() as temp:
    root = pathlib.Path(temp)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / 'config'))
    worker = root / 'worker.js'
    worker.write_text(initial)
    def run(*args, input=None, ok=True):
        result = subprocess.run([cli, *args], input=input, env=env, capture_output=True, text=True, timeout=40)
        assert token not in result.stdout + result.stderr, 'credential leaked'
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        return result
    auth = ('auth', '--webkey-stdin', '--allow-local-http')
    state['redirect'] = True
    run(*auth, input=webkey, ok=False)
    assert not (root/'config/spki/default.json').exists()
    state['redirect'] = False
    state['mismatch'] = True
    run(*auth, input=webkey, ok=False)
    state['mismatch'] = False
    run(*auth, input=webkey.replace(token, 'wrong'), ok=False)
    run(*auth, input=webkey)
    credential = root/'config/spki/default.json'
    assert credential.stat().st_mode & 0o777 == 0o600
    assert credential.parent.stat().st_mode & 0o777 == 0o700
    assert state['posts'] == 0, 'auth uploaded source'
    run('auth', 'status')
    run('dev', '--entry', str(worker), '--once')
    before = current()
    worker.write_text('export default { broken(')
    run('dev', '--entry', str(worker), '--once', ok=False)
    assert current() == before
    worker.write_text(initial + "\nexport const buildMarker = 'lost acknowledgement';\n")
    state['drop'] = True
    run('dev', '--entry', str(worker), '--once')
    assert current() != before
    before = current()
    # A watcher recovers from source errors but exits when another writer wins.
    with (root/'watch.log').open('w+') as log:
        watcher = subprocess.Popen([cli, 'dev', '--entry', str(worker)], env=env, stdout=log, stderr=log, text=True)
        try:
            wait_for(lambda: 'Live revision' in (root/'watch.log').read_text())
            worker.write_text('export default { broken(')
            wait_for(lambda: 'Deployment failed' in (root/'watch.log').read_text())
            assert current() == before
            worker.write_text(initial + "\nexport const buildMarker = 'watcher edit';\n")
            wait_for(lambda: current() != before)
            competing = root/'competing.js'
            competing.write_text(initial + "\nexport const buildMarker = 'competing writer';\n")
            run('dev', '--entry', str(competing), '--once')
            external = current()
            worker.write_text(initial + "\nexport const buildMarker = 'stale watcher';\n")
            watcher.wait(timeout=15)
            assert watcher.returncode != 0
            assert 'revision conflict' in (root/'watch.log').read_text()
            assert current() == external
        finally:
            if watcher.poll() is None: watcher.terminate(); watcher.wait(timeout=5)
        assert token not in (root/'watch.log').read_text()
    # TypeScript graph: edit an imported module, break it, then introduce a missing dependency.
    typed = root/'typed.ts'
    helper = root/'helper.ts'
    typed.write_text("import { message } from './helper';\nimport { sandstorm } from 'sandstorm:api';\nexport default {fetch(request: Request, env: unknown) { sandstorm(request, env); return new Response(message); }};\n")
    helper.write_text("export const message: string = 'first';")
    with (root/'typed.log').open('w+') as log:
        watcher = subprocess.Popen([cli, 'dev', '--entry', str(typed)], env=env, stdout=log, stderr=log, text=True)
        try:
            wait_for(lambda: 'Live revision' in (root/'typed.log').read_text())
            first_revision = current()
            helper.write_text("export const message: string = 'second';")
            wait_for(lambda: current() != first_revision)
            good = current()
            helper.write_text("export const message: string = ;")
            wait_for(lambda: 'bundle failed' in (root/'typed.log').read_text())
            assert current() == good
            helper.write_text("export {message} from './missing';")
            wait_for(lambda: 'missing' in (root/'typed.log').read_text())
            (root/'missing.ts').write_text("export const message: string = 'third';")
            wait_for(lambda: current() != good)
        finally:
            watcher.terminate(); watcher.wait(timeout=5)
    # Project watching works with no external compiler and invalidates configuration.
    import shlex, shutil
    project = root/'watch-project'
    run('init',str(project))
    # This legacy transport facade exercises the unscoped deployment API.
    import re
    config = project/'sandstorm-pkgdef.capnp'
    config.write_text(re.sub(r', project = "[^"]+"', '', config.read_text()))
    original_env = dict(env)
    count = root/'capnp.calls'
    wrapper = root/'capnp-wrapper'
    wrapper.write_text("#!/bin/sh\nprintf 'call\\n' >> " + shlex.quote(str(count)) + "\nexit 1\n")
    wrapper.chmod(0o755)
    env['CAPNP'] = str(wrapper)
    (project/'src/other.ts').write_text((project/'src/worker.ts').read_text() + "\nconsole.log('changed configuration');\n")
    with (root/'idle.log').open('w+') as log:
        watcher = subprocess.Popen([cli,'dev'],cwd=project,env=env,stdout=log,stderr=log,text=True)
        try:
            wait_for(lambda: 'Live revision' in (root/'idle.log').read_text())
            time.sleep(1.2)
            assert not count.exists(), 'watcher spawned external compiler'
            before_revision = current()
            (project/'src/greeting.ts').write_text("export const greeting = 'changed in cached project';")
            wait_for(lambda: current() != before_revision)
            assert not count.exists(), 'source edit spawned external compiler'
            before_revision = current()
            config = project/'sandstorm-pkgdef.capnp'
            config.write_text(config.read_text().replace('src/worker.ts','src/other.ts'))
            wait_for(lambda: current() != before_revision)
            time.sleep(0.5)
            good_config = config.read_text()
            good_revision = current()
            config.write_text(good_config.replace('formatVersion = 1','formatVersion = 99'))
            wait_for(lambda: 'unsupported spki formatVersion' in (root/'idle.log').read_text())
            (project/'src/greeting.ts').write_text("export const greeting = 'must wait for valid configuration';")
            time.sleep(1.2)
            assert current() == good_revision, 'invalid configuration used stale cached settings'
            config.write_text(good_config)
            wait_for(lambda: current() != good_revision)
        finally:
            watcher.terminate(); watcher.wait(timeout=5)
    env.clear(); env.update(original_env)
    print('PASS: watcher needs no external compiler, configuration invalidation and recovery')
    worker.write_text(initial)
    run('dev', '--entry', str(worker), '--once')
    run('logout')
    assert not credential.exists()
    run('auth', 'status', ok=False)
server.shutdown()
print('PASS: CLI auth, private credentials, redirects, compatibility, watch recovery, conflicts, lost acknowledgement, logout')
