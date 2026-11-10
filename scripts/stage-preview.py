#!/usr/bin/env python3
"""Stage the Rust grain probe and pinned host; no Sandstorm checkout needed."""
from pathlib import Path
import hashlib, json, os, shutil, subprocess, tempfile
published = Path('build/grain-root')
published.parent.mkdir(parents=True, exist_ok=True)
# Build an isolated generation; only publish it after every copy and hash succeeds.
generation = tempfile.TemporaryDirectory(prefix='.grain-root-', dir=published.parent)
root = Path(generation.name) / "root"
root.mkdir()
files = {}
pin = json.loads(Path('runtime/isolate-host.json').read_text())
host = Path('build/host/bin/isolate-host')
assert hashlib.sha256(host.read_bytes()).hexdigest() == pin['binarySha256']
for directory in ['dev', 'tmp', 'var', 'proc']:
    (root / directory).mkdir(parents=True, exist_ok=True)
def copy(source, dest):
    dest = root / dest.lstrip('/')
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, dest)
    name = str(dest.relative_to(root))
    if name != 'app/worker.js':
        files[name] = {'sha256': hashlib.sha256(dest.read_bytes()).hexdigest(), 'mode': dest.stat().st_mode & 0o777}
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
def copy_tree(source, destination):
    for path in sorted(Path(source).rglob('*')):
        if path.is_file():
            copy(path, str(Path(destination) / path.relative_to(source)))
copy_tree('build/host/licenses', 'usr/share/licenses/isolate-host')
for name in ['LICENSE', 'NOTICE', 'manifest.json']:
    copy('build/host/' + name, 'usr/share/licenses/isolate-host/' + name)
for source in Path('crates/isolate-preview/sdk').glob('LICENSE*'):
    copy(source, 'usr/share/licenses/isolate-preview/' + source.name)
copy('crates/isolate-preview/browser/provenance.json', 'usr/share/licenses/isolate-preview/browser-provenance.json')
copy('crates/isolate-preview/sdk/provenance.json', 'usr/share/licenses/isolate-preview/sdk-provenance.json')
copy_tree('crates/isolate-protocol/licenses', 'usr/share/licenses/isolate-protocol')
fingerprint = hashlib.sha256()
for value in [pin['binarySha256'].encode(), b'2026-06-10', b'spki-workspace-v1', Path('crates/isolate-preview/sdk/modules.json').read_bytes(), Path('crates/isolate-preview/sdk/entry.js').read_bytes(), Path('crates/isolate-preview/browser/client.js').read_bytes(), b'']:
    fingerprint.update(len(value).to_bytes(8, 'little')); fingerprint.update(value)
(root / 'runtime.json').write_text(json.dumps({'formatVersion':1, 'target':'linux-x86_64', 'hostSha256':pin['binarySha256'], 'sdkRevision':fingerprint.hexdigest(), 'files':files}, indent=2)+'\n')
# Upgrade the old directory layout once, preserving it for readers already using it.
# Subsequent publications replace a symlink atomically; old generations remain in
# build/ until the build cache is cleaned, so concurrent packagers can finish.
complete = Path(generation.name + '-complete')
root.rename(complete)
root = complete
link = root.parent / (root.name + '-link')
link.symlink_to(root.name, target_is_directory=True)
if published.exists() and not published.is_symlink():
    legacy = root.parent / (root.name + '-legacy')
    published.rename(legacy)
    try:
        os.replace(link, published)
    except BaseException:
        legacy.rename(published)
        raise
else:
    os.replace(link, published)
generation.cleanup()
print(published.absolute())
