#!/usr/bin/env python3
"""Host-only regressions. Requires a built spki binary, Python, and Node 22+."""
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parent.parent
for name in ['stage', 'project', 'compiler', 'embedded-bundler', 'frontend', 'examples']:
    print(f'Checking {name}…', flush=True)
    subprocess.run([sys.executable, str(root / f'scripts/check-{name}.py')], cwd=root, check=True)
