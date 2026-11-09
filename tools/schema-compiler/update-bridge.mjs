// Regenerate the locally extended bridge using the pinned capnp-es compiler.
// Run from the repository root after npm ci --prefix tools/schema-compiler.
import {spawnSync} from 'node:child_process';
import {readFileSync, writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {compileAll} from '@mnutt/capnp-es/compiler';

const schemaRoot = 'crates/isolate-protocol/schema';
const request = spawnSync('capnp', ['compile', `-I${schemaRoot}`, `--src-prefix=${schemaRoot}`,
  '-o-', `${schemaRoot}/sandstorm/isolate-bridge.capnp`]);
if (request.error) throw request.error;
if (request.status !== 0) throw new Error(request.stderr.toString());
const {files} = await compileAll(request.stdout, {
  js: true,
  tsconfig: {noCheck: true},
  moduleSpecifier(context) {
    if (context.kind === 'runtime') return '/capnp-es/index.mjs';
    return './' + context.toPath.split('/').at(-1).replace(/\.ts$/, '.capnp');
  },
});
const sdkRoot = 'crates/isolate-preview/sdk';
const modules = JSON.parse(readFileSync(`${sdkRoot}/modules.json`, 'utf8'));
const source = files.get('sandstorm/isolate-bridge.js');
if (!source) throw new Error('Missing generated bridge');
for (const key of Object.keys(modules)) {
  if (key.endsWith('sandstorm/isolate-bridge.capnp')) modules[key] = source;
}
writeFileSync(`${sdkRoot}/modules.json`, JSON.stringify(modules) + '\n');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const provenance = JSON.parse(readFileSync(`${sdkRoot}/provenance.json`, 'utf8'));
provenance.modules = Object.fromEntries(Object.entries(modules).map(([key, value]) => [key, hash(value)]));
writeFileSync(`${sdkRoot}/provenance.json`, JSON.stringify(provenance, null, 2) + '\n');
const protocol = JSON.parse(readFileSync(`${schemaRoot}/provenance.json`, 'utf8'));
protocol.sha256['sandstorm/isolate-bridge.capnp'] = hash(readFileSync(`${schemaRoot}/sandstorm/isolate-bridge.capnp`));
writeFileSync(`${schemaRoot}/provenance.json`, JSON.stringify(protocol, null, 2) + '\n');
