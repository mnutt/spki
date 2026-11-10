#!/usr/bin/env python3
"""Stage the Rust grain probe and pinned host; no Sandstorm checkout needed."""
from pathlib import Path
import hashlib, json, shutil, subprocess
root = Path('build/grain-root')
pin = json.loads(Path('runtime/isolate-host.json').read_text())
host = Path('build/host/bin/isolate-host')
assert hashlib.sha256(host.read_bytes()).hexdigest() == pin['binarySha256']
for directory in ['dev', 'tmp', 'var', 'proc']:
    (root / directory).mkdir(parents=True, exist_ok=True)
def copy(source, dest):
    dest = root / dest.lstrip('/')
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, dest)
for binary, name in [(Path('target/debug/isolate-preview'), 'bin/isolate-preview'), (host, 'bin/isolate-host')]:
    copy(binary, name)
    for line in subprocess.check_output(['ldd', str(binary)], text=True).splitlines():
        words = line.split()
        path = words[2] if len(words) > 2 and words[1] == '=>' else words[0] if words else ''
        if path.startswith('/'):
            copy(path, path)
            if words[0].startswith("/"):
                copy(path, words[0])
copy('preview/fixtures/sdk-counter.js', 'app/worker.js')
shutil.copytree('build/host/licenses', root / 'usr/share/licenses/isolate-host', dirs_exist_ok=True)
for name in ['LICENSE', 'NOTICE', 'manifest.json']:
    copy('build/host/' + name, 'usr/share/licenses/isolate-host/' + name)
for source in Path('crates/isolate-preview/sdk').glob('LICENSE*'):
    copy(source, 'usr/share/licenses/isolate-preview/' + source.name)
copy('crates/isolate-preview/browser/provenance.json', 'usr/share/licenses/isolate-preview/browser-provenance.json')
copy('crates/isolate-preview/sdk/provenance.json', 'usr/share/licenses/isolate-preview/sdk-provenance.json')
shutil.copytree('crates/isolate-protocol/licenses', root / 'usr/share/licenses/isolate-protocol', dirs_exist_ok=True)
files = {}
for directory in ['bin', 'lib', 'lib64', 'usr/lib', 'usr/lib64', 'usr/share/licenses']:
    for path in sorted((root / directory).rglob('*')):
        if path.is_file():
            files[str(path.relative_to(root))] = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': path.stat().st_mode & 0o777}
fingerprint = hashlib.sha256()
for value in [pin['binarySha256'].encode(), b'2026-06-10', b'spki-native-bindings-v1', Path('crates/isolate-preview/sdk/modules.json').read_bytes(), Path('crates/isolate-preview/sdk/entry.js').read_bytes(), Path('crates/isolate-preview/browser/client.js').read_bytes(), b'']:
    fingerprint.update(len(value).to_bytes(8, 'little')); fingerprint.update(value)
(root / 'runtime.json').write_text(json.dumps({'formatVersion':1, 'target':'linux-x86_64', 'hostSha256':pin['binarySha256'], 'sdkRevision':fingerprint.hexdigest(), 'files':files}, indent=2)+'\n')
print(root.resolve())
