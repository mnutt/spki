#!/usr/bin/env python3
"""Build all runnable examples using only the standalone checkout and pinned tools."""
from pathlib import Path
import subprocess
root = Path(__file__).resolve().parent.parent
examples = root/'examples'
for config in sorted(examples.glob('*/sandstorm-pkgdef.capnp')):
    subprocess.run([str(root/'target/debug/spki'), '--project', str(config.parent),
                    'build', '--esbuild', str(root/'build/tools/esbuild')], check=True)
    print(f'PASS: {config.parent.name}', flush=True)
assert not list((examples/'pending').rglob('sandstorm-pkgdef.capnp'))
assert not list(examples.rglob('*.key')), 'Examples must not include signing keys'
print('PASS: pending examples are excluded and no signing keys are included')
