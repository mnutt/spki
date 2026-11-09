#!/usr/bin/env python3
"""Fetch the pinned standalone Linux x64 compiler; accepts a cached archive argument."""
import base64
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import urllib.request

root = Path(__file__).resolve().parent.parent
pin = json.loads((root / 'runtime/esbuild.json').read_text())
cache = root / 'build/tools'
cache.mkdir(parents=True, exist_ok=True)
archive = cache / 'esbuild.tgz'
if len(sys.argv) > 1:
    shutil.copyfile(sys.argv[1], archive)
elif not archive.exists():
    with urllib.request.urlopen(pin['url'], timeout=60) as response, archive.open('wb') as output:
        shutil.copyfileobj(response, output)
actual = 'sha512-' + base64.b64encode(hashlib.sha512(archive.read_bytes()).digest()).decode()
if actual != pin['integrity']:
    raise SystemExit('esbuild archive integrity mismatch')
with tempfile.TemporaryDirectory(dir=cache) as temporary:
    with tarfile.open(archive) as package:
        package.extractall(temporary, filter='data')
    binary = Path(temporary) / 'package/bin/esbuild'
    if hashlib.sha256(binary.read_bytes()).hexdigest() != pin['binarySha256']:
        raise SystemExit('esbuild binary integrity mismatch')
    shutil.copy2(binary, cache / 'esbuild')
    (cache / 'esbuild').chmod(0o755)
print(cache / 'esbuild')
