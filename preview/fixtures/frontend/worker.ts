import {exportCapnp, sandstorm} from 'sandstorm:api';
import {BrowserCounter} from 'capnp:./counter.capnp';
let counter;
let value = 0;
export default {
  async fetch(request: Request, env) {
    const api = sandstorm(request, env);
    if (new URL(request.url).pathname === '/counter') {
      counter ??= await exportCapnp(api, BrowserCounter, {
        read: () => ({value}),
        increment: ({amount}) => ({value: value += amount}),
        reset: () => ({value: value = 0}),
        wait: () => new Promise(() => {}),
        async echo({stream}) { await stream.write({data: new Uint8Array([1, 2, 3])}); },
      });
      return Response.json(await counter.browserHandoff(request));
    }
    return new Response('Not Found', {status:404});
  }
};
