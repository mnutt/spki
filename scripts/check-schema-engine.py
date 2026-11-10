#!/usr/bin/env python3
"""Maintainer differential test. Node is used only as the old generator's oracle.
Install reference dependencies with npm ci --prefix tools/schema-compiler.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
cli = Path(os.environ.get('SPKI_TEST_BINARY', str(root/'target/debug/spki'))).resolve()
capnp = shutil.which('capnp')
runtime = (root/'tools/schema-compiler/node_modules/@mnutt/capnp-es/dist/index.mjs').as_uri()

def run(*args, **kwargs):
    result = subprocess.run([str(a) for a in args], capture_output=True, **kwargs)
    assert result.returncode == 0, result.stderr.decode()
    return result.stdout

with tempfile.TemporaryDirectory() as temporary:
    base = Path(temporary)
    project = base/'project'
    run(cli, 'init', project)
    (project/'nested').mkdir()
    (project/'nested/common.capnp').write_text('''@0xacbcb46e8e31a8fd;
struct Child { label @0 :Text = "déjà vu 🌍"; }
interface Base { ping @0 (value :UInt32) -> (result :UInt32); }
''')
    (project/'sample.capnp').write_text('''@0xbacde013089841ff;
using Common = import "nested/common.capnp";
enum Colour { red @0; green @1; blue @2; }
struct Record {
  title @0 :Text = "héllo 🌍";
  count @1 :Int64 = -9223372036854775807;
  colour @2 :Colour = green;
  numbers @3 :List(Int32);
  children @4 :List(Common.Child);
  bytes @5 :Data = 0x"00ff1234";
  union { nothing @6 :Void; enabled @7 :Bool; }
  details :group { score @8 :Float64 = 1.25; }
}
interface Service extends(Common.Base) {
  echo @0 (value :Record) -> (value :Record);
}
''')
    config = project/'sandstorm-pkgdef.capnp'
    config.write_text(config.read_text().replace('  compatibilityDate', '  schemas = ["sample.capnp"],\n  compatibilityDate'))
    (project/'src/worker.ts').write_text('export * from "capnp:./sample.capnp"; export default {fetch() { return new Response("ok"); }};')
    embedded = base/'embedded.mjs'
    # Neither capnp, Node, nor npm can be found by the CLI.
    env = dict(os.environ, PATH='/no-host-tools', CAPNP='/not-installed', SPKI_SCHEMA_COMPILER='/not-installed')
    run(cli, '--project', project, 'build', '--output', embedded, env=env)
    embedded.write_text(embedded.read_text().replace('capnp-es/index.mjs', runtime))

    request = run(capnp, 'compile', '-o-', '--src-prefix='+str(project),
                  project/'sample.capnp', project/'nested/common.capnp', cwd=project)
    reference = json.loads(run('node', root/'tools/schema-compiler/compile.mjs', input=request))
    old = base/'reference'
    old.mkdir()
    (old/'package.json').write_text('{"type":"module"}')
    for name, code in reference:
        # Generated imports end in .capnp.js, while compiler result keys end in .js.
        path = old/(name[:-3]+'.capnp.js')
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(code.replace('/capnp-es/index.mjs', runtime))
    inspect = r'''
import * as capnp from %s;
const mod = await import(process.argv[1]);
const normalize = (value, seen = new Set()) => {
  if (typeof value === 'bigint') return value.toString();
  if (typeof value === 'function') return value._capnp?.id ?? String(value.interfaceId ?? 'function');
  if (value && typeof value.toUint8Array === 'function') return Array.from(value.toUint8Array());
  if (value && typeof value === 'object') { if (seen.has(value)) return '<cycle>'; seen = new Set(seen); seen.add(value); }
  if (Array.isArray(value)) return value.map(v=>normalize(v, seen));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v, seen)]));
  return value;
};
const message = new capnp.Message();
const record = message.initRoot(mod.Record);
const defaults = [record.title, record.count.toString(), record.colour, record.details.score,
  Array.from(record.bytes.toUint8Array())];
record.title = 'new 🦀 text';
record.count = 9007199254740993n;
record.colour = mod.Colour.BLUE;
const numbers = record._initNumbers(3); [1,-2,2147483647].forEach((v,i)=>numbers.set(i,v));
record._initChildren(1).get(0).label = 'nested';
record.enabled = true;
record.details.score = 2.5;
const bytes = message.toArrayBuffer();
const read = new capnp.Message(bytes, false).getRoot(mod.Record);
const methods = mod.Service$Client.methods.map(method => normalize(method));
console.log(JSON.stringify({exports:Object.keys(mod).filter(k=>k!=='default').sort(),
  defaults, encoded:Buffer.from(bytes).toString('hex'),
  values:[read.title,read.count.toString(),read.colour,read.numbers.toArray(),read.children.get(0).label,read.enabled,read.details.score],
  which:read.which(), metadata:normalize(mod.Record._capnp), methods}));
'''
    snapshots = [json.loads(run('node', '--input-type=module', '-e', inspect % json.dumps(runtime), file.as_uri()))
                 for file in [old/'sample.capnp.js', embedded]]
    assert snapshots[0] == snapshots[1], (snapshots[0], snapshots[1])
    assert snapshots[1]['values'][0] == 'new 🦀 text'
print('PASS: no capnp or Node dependency; bindings match reference exports, defaults, Unicode, Int64, enums, lists, groups, unions, wire bytes, and inherited RPC metadata')
