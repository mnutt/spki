#!/usr/bin/env python3
"""Exercise embedded bundling without helper executables, plus served browser assets."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
cli = root / 'target/debug/spki'

def run(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True, text=True, **kwargs)

with tempfile.TemporaryDirectory() as temporary:
    base = Path(temporary)
    entry = base / 'worker.ts'
    entry.write_text('import text from "./text.txt"; import data from "./data.json"; export default {fetch() { return new Response(text + data.value); }};')
    (base / 'text.txt').write_text('embedded:')
    (base / 'data.json').write_text('{"value": 42}')
    env = dict(os.environ, PATH='/no-external-tools', SPKI_ESBUILD='/not-installed')
    standalone = base / 'standalone.mjs'
    run(cli, 'build', '--entry', entry, '--output', standalone, env=env)
    assert 'embedded:' in standalone.read_text()
    assert '42' in standalone.read_text()

    package = base/'node_modules/conditional'
    package.mkdir(parents=True)
    (package/'package.json').write_text(json.dumps({'exports': {'workerd': './worker.js', 'default': './fallback.js'}}))
    (package/'worker.js').write_text('export default "worker-condition";')
    (package/'fallback.js').write_text('export default "wrong-condition";')
    entry.write_text('export {default} from "conditional";')
    conditional = base/'conditional.mjs'
    run(cli, 'build', '--entry', entry, '--output', conditional, env=env)
    assert 'worker-condition' in conditional.read_text() and 'wrong-condition' not in conditional.read_text()

    # Bundled literal CommonJS and dynamic imports work; unresolved runtime loading fails.
    (base / 'helper.cjs').write_text('module.exports = {answer: 42};')
    entry.write_text('import data from "./helper.cjs"; export default {fetch() { return Response.json(data); }};')
    run(cli, 'build', '--entry', entry, '--output', base/'cjs.mjs', env=env)
    entry.write_text('export default {async fetch() { return Response.json(await import("./helper.cjs")); }};')
    run(cli, 'build', '--entry', entry, '--output', base/'dynamic.mjs', env=env)
    for source in ['export default () => require(globalThis.name);', 'import x from "missing-package"; export default x;', 'export default () => import("node:fs");']:
        entry.write_text(source)
        result = subprocess.run([str(cli), 'build', '--entry', str(entry), '--output', str(base/'bad.mjs')], env=env, capture_output=True)
        assert result.returncode != 0, source
        assert not (base/'bad.mjs').exists()

    project = base/'frontend'
    run(cli, 'init', project)
    (project/'src/worker.ts').write_text('export default {fetch() { return new Response("missing", {status: 404}); }};')
    config = project/'sandstorm-pkgdef.capnp'
    config.write_text(config.read_text().replace('  compatibilityDate', '  ui = (entrypoints = ["ui/main.ts", "ui/other.ts"]),\n  compatibilityDate'))
    ui = project/'ui'
    ui.mkdir()
    (ui/'main.ts').write_text('import "./shared"; import logo from "./logo.svg"; globalThis.logo = logo; globalThis.lazy = () => import("./lazy");')
    (ui/'other.ts').write_text('import "./shared"; globalThis.other = true;')
    (ui/'shared.ts').write_text('import "./shared.css"; globalThis.shared = true;')
    (ui/'shared.css').write_text('@import "./base.css"; body { background: url("./logo.svg"); color: red; }')
    (ui/'base.css').write_text('body { color: blue; }')
    (ui/'lazy.ts').write_text('import "./lazy.css"; export const loaded = true;')
    (ui/'lazy.css').write_text('.lazy { color: green; }')
    logo = '<svg xmlns="http://www.w3.org/2000/svg"></svg>'
    (ui/'logo.svg').write_text(logo)
    output = base/'frontend.mjs'
    run(cli, '--project', project, 'build', '--output', output)
    first = output.read_bytes()
    assert str(base).encode() not in first
    # Check actual responses from the packaged asset router, not compiler output formatting.
    inspect = '''
import app from %s;
const get = async path => { const r = await app.fetch(new Request('https://test' + path), {}, {}); if (r.status !== 200) throw Error(path + ': ' + r.status); return r.text(); };
const main = await get('/assets/main.css');
const other = await get('/assets/other.css');
if (!main.includes('.lazy') || other.includes('.lazy')) throw Error('entry CSS reachability');
for (const css of [main, other]) {
  if (!(css.indexOf('blue') < css.indexOf('red'))) throw Error('CSS order');
  const url = css.match(/url\\(["']?([^"')]+)["']?\\)/)[1];
  if ((await get(url)) !== %s) throw Error('CSS asset bytes');
}
const js = await get('/assets/main.js');
const media = js.match(/\\/assets\\/media\\/[^"']+\\.svg/)[0];
if ((await get(media)) !== %s) throw Error('JS asset URL');
'''
    run('node', '--input-type=module', '-e', inspect % (json.dumps(output.as_uri()), json.dumps(logo), json.dumps(logo)))
    second = base/'same.mjs'
    run(cli, '--project', project, 'build', '--output', second)
    assert first == second.read_bytes(), 'embedded asset build is not deterministic'
    (ui/'base.css').write_text('body { color: purple; }')
    run(cli, '--project', project, 'build', '--output', base/'changed.mjs')
    assert first != (base/'changed.mjs').read_bytes()
print('PASS: no bundler executable, CJS/JSON/text, import rejection, shared/lazy CSS, CSS cascade, emitted asset URLs, deterministic builds')
