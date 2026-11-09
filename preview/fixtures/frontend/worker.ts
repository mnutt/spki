import {exportCapnp, sandstorm} from 'sandstorm:api';
import {BrowserCounter} from 'capnp:./counter.capnp';
let counter;
let value = 0;
async function exportCounter(api, sessionId = '') {
  return exportCapnp(api, BrowserCounter, {
    session: () => ({id: sessionId}),
    read: () => ({value}),
    increment: ({amount}) => ({value: value += amount}),
    reset: () => ({value: value = 0}),
    wait: () => new Promise(() => {}),
    async echo({stream}) { await stream.write({data: new Uint8Array([1, 2, 3])}); },
  });
}
export default {
  async connect(request: Request, env) {
    const sessionId = request.headers.get('x-sandstorm-session-id');
    if (!sessionId) throw new Error('Missing trusted session');
    if (request.headers.get('x-sandstorm-permissions') !== '') throw new Error('Preview permission leaked');
    return exportCounter(sandstorm(request, env), sessionId);
  },
  async fetch(request: Request, env) {
    const api = sandstorm(request, env);
    if (new URL(request.url).pathname === '/counter') {
      counter ??= await exportCounter(api);
      return Response.json(await counter.browserHandoff(request));
    }
    return new Response('Not Found', {status:404});
  }
};
