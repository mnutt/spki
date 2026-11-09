#!/usr/bin/env python3
"""Acquire the pinned standalone runtime. Optionally supply a cached release archive."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import urllib.request

root = Path(__file__).resolve().parent.parent
pin = json.loads((root / 'runtime/isolate-host.json').read_text())
cache = root / 'build'
cache.mkdir(exist_ok=True)
archive = cache / 'isolate-host.tar.xz'
if len(sys.argv) > 1:
    shutil.copyfile(sys.argv[1], archive)
elif not archive.exists():
    with urllib.request.urlopen(pin['url'], timeout=60) as response, archive.open('wb') as output:
        shutil.copyfileobj(response, output)
if hashlib.sha256(archive.read_bytes()).hexdigest() != pin['archiveSha256']:
    raise SystemExit('runtime archive hash mismatch; remove build/isolate-host.tar.xz and retry')
with tempfile.TemporaryDirectory(dir=cache) as temporary:
    with tarfile.open(archive) as package:
        package.extractall(temporary, filter='data')
    release = Path(temporary)
    binary = release / 'bin/isolate-host'
    if hashlib.sha256(binary.read_bytes()).hexdigest() != pin['binarySha256']:
        raise SystemExit('runtime binary hash mismatch')
    shutil.copytree(release, cache / 'host', dirs_exist_ok=True)
print(cache / 'host/bin/isolate-host')
