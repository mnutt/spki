// Maintainer-only reference for differential tests, not used by spki.
// Install with npm ci --prefix tools/schema-compiler. Pins match the vendored SDK.
import path from 'node:path';
import {compileAll} from '@mnutt/capnp-es/compiler';
const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const {files} = await compileAll(Buffer.concat(chunks), {
  js: true,
  tsconfig: {noCheck: true},
  moduleSpecifier(context) {
    if (context.kind === 'runtime') return '/capnp-es/index.mjs';
    const from = context.fromPath.replace(/\.ts$/, '.capnp.js');
    const to = context.toPath.replace(/\.ts$/, '.capnp.js');
    if (to.startsWith('sandstorm/')) return '/__sandstorm/capnp/' + to;
    const relative = path.posix.relative(path.posix.dirname(from), to);
    return relative.startsWith('.') ? relative : './' + relative;
  }
});
process.stdout.write(JSON.stringify([...files.entries()]));
