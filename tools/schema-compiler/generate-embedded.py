#!/usr/bin/env python3
"""Maintainer-only regeneration: npm ci --prefix tools/schema-compiler, then run this.
Normal cargo builds and installed spki need neither Node nor node_modules.
The pinned upstream generator retains all schema/RPC generation logic. Only its
TypeScript printer/compiler and Node path imports are replaced: Rust/Oxc handles
TS emission, and the path adapter operates on logical schema names.
"""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

root = Path(__file__).resolve().parents[2]
tool = root/'tools/schema-compiler'
package = tool/'node_modules/@mnutt/capnp-es'
assert json.loads((package/'package.json').read_text())['version'] == '0.3.0'
compiler = Path('dist/shared/capnp-es.cQtDh6nd.mjs')
original = (package/compiler).read_text()
expected = '5f05e8187f0ed78ea6b87080838def802586d179518ad02df6005bb644cfe6e0'
assert hashlib.sha256(original.encode()).hexdigest() == expected, 'upstream generator changed; review adaptation'
stage = root/'build/schema-compiler-vendor'
shutil.copytree(package/'dist', stage/'dist', dirs_exist_ok=True)
source = original.replace("import ts from 'typescript';\n", '').replace("from 'node:path/posix'", "from '../../path-posix.mjs'")
source = source.replace('async function compileAll(', 'function compileAll(')
a = source.index('  tsCompile(\n', source.index('function compileAll('))
b = source.index('  if (!opts?.ts)', a)
source = source[:a] + source[b:]
a = source.index('  const sourceFile = ts.createSourceFile(')
source = source[:a] + '  return SOURCE_COMMENT + ctx.codeParts.map(p => p.toString()).join("");\n}\n' + '\nexport { compileAll };\n'
assert 'ts.create' not in source and 'tsCompile(' not in source and "from 'typescript'" not in source
(stage/compiler).write_text(source)
shutil.copy2(tool/'path-posix.mjs', stage/'path-posix.mjs')
shutil.copy2(tool/'embedded-entry.mjs', stage/'entry.mjs')
destination = root/'crates/spki/schema-compiler'
subprocess.run(['cargo', 'run', '--locked', '-p', 'spki', '--example', 'bundle-schema-compiler', '--', str(stage/'entry.mjs'), str(destination/'compiler.js')], cwd=root, check=True)
shutil.copy2(package/'LICENSE', destination/'LICENSE')
# Record all bundled upstream modules, not just the compiler entrypoint.
inputs = {str(p.relative_to(package)): hashlib.sha256(p.read_bytes()).hexdigest()
          for p in sorted((package/'dist').rglob('*.mjs'))
          if p.parent.name == 'shared' or p == package/'dist/capnp/schema.mjs'}
(destination/'provenance.json').write_text(json.dumps({'package': '@mnutt/capnp-es', 'version': '0.3.0',
    'generatorSha256': expected, 'upstreamModules': inputs,
    'bundleSha256': hashlib.sha256((destination/'compiler.js').read_bytes()).hexdigest()}, indent=2)+'\n')
print(destination/'compiler.js')
