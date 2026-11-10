#!/usr/bin/env python3
"""Create the compressed runtime payload embedded by release CLI builds."""
import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import tempfile
import tomllib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, default=Path('build/grain-root'))
parser.add_argument('--output', type=Path, default=Path('build/runtime.xz'))
args = parser.parse_args()
root = args.root.resolve(strict=True)
receipt = json.loads((root / 'runtime.json').read_text())
with open('Cargo.toml', 'rb') as file:
    receipt['spkiVersion'] = tomllib.load(file)['workspace']['package']['version']
for name, entry in receipt['files'].items():
    path = root / name
    assert path.is_file() and not path.is_symlink() and path.resolve().is_relative_to(root), name
    entry['size'] = path.stat().st_size
header = json.dumps(receipt, sort_keys=True, separators=(',', ':')).encode()
assert len(header) <= 1024 * 1024
args.output.parent.mkdir(parents=True, exist_ok=True)
with tempfile.NamedTemporaryFile(dir=args.output.parent) as temporary:
    with lzma.LZMAFile(temporary, 'w', preset=3) as output:
        output.write(b'SPKIRUN1')
        output.write(len(header).to_bytes(4, 'little'))
        output.write(header)
        for name, entry in sorted(receipt['files'].items()):
            digest = hashlib.sha256()
            count = 0
            with (root / name).open('rb') as source:
                while data := source.read(65536):
                    count += len(data)
                    digest.update(data)
                    output.write(data)
            assert count == entry['size'] and digest.hexdigest() == entry['sha256'], f'Runtime file changed: {name}'
    temporary.flush()
    os.fsync(temporary.fileno())
    # Publish a fresh artifact without overwriting one another build might be embedding.
    os.link(temporary.name, args.output)
print(f'{args.output}: {args.output.stat().st_size / 1024**2:.1f} MiB compressed')
