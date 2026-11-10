import {compileAll} from './dist/shared/capnp-es.cQtDh6nd.mjs';
import {relative, dirname} from './path-posix.mjs';

// The caller supplies an in-memory compiler request. No process, filesystem,
// module loader, or network access is exposed to the embedded generator.
globalThis.__spkiGenerate = input => JSON.stringify([...compileAll(input, {
  ts: true,
  moduleSpecifier(context) {
    if (context.kind === 'runtime') return '/capnp-es/index.mjs';
    const from = context.fromPath.replace(/\.ts$/, '.capnp.js');
    const to = context.toPath.replace(/\.ts$/, '.capnp.js');
    if (to.startsWith('sandstorm/')) return '/__sandstorm/capnp/' + to;
    const path = relative(dirname(from), to);
    return path.startsWith('.') ? path : './' + path;
  },
}).files]);
