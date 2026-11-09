// Controller-owned entrypoint. Dynamic import reports module initialization failures
// through HTTP instead of leaving an unresolved host request capability.
import {AsyncLocalStorage} from "node:async_hooks";
const invocation = new AsyncLocalStorage();
let application;
const sessionChannels = new Map();
export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const ready = url.pathname === '/__spki_internal_ready';
    if (url.pathname === '/__spki_internal_close_session') {
      for (const channel of sessionChannels.get(url.searchParams.get('id')) || []) channel.close();
      return new Response(null, {status: 204});
    }
    try {
      application ??= import('./application.js');
      const {default: app} = await application;
      if (typeof app?.fetch !== 'function') throw new TypeError('Worker must export default.fetch');
      if (ready) return new Response('ready');
      // A native channel receives callbacks in its originating request's IoContext.
      // Keep that invocation open while channels exist, rather than using waitUntil's
      // 30-second post-response deadline. The Rust adapter consumes the bounded body
      // using this private length header and retains the host request until EOF.
      const native = env.__SANDSTORM_NATIVE_CAPNP;
      const sessionId = request.headers.get('x-sandstorm-session-id') || '';
      const channels = new Set();
      const state = {sessionId, channels, finished: () => {}};
      const requestEnv = Object.create(env);
      requestEnv.__SANDSTORM_NATIVE_CAPNP = {
        open() {
          const current = invocation.getStore();
          if (!current) throw new Error("Native RPC requires an active worker request");
          const {sessionId, channels} = current;
          const channel = native.open();
          let active = true;
          const finish = () => {
            if (!active) return;
            active = false;
            channels.delete(wrapped);
            const session = sessionChannels.get(sessionId);
            session?.delete(wrapped);
            if (session?.size === 0) sessionChannels.delete(sessionId);
            if (channels.size === 0) current.finished();
          };
          const wrapped = {
            send(bytes) { return channel.send(bytes); },
            async receive() {
              try { return await channel.receive(); }
              catch (error) { finish(); throw error; }
            },
            close() { finish(); return channel.close(); },
          };
          channels.add(wrapped);
          if (!sessionChannels.has(sessionId)) sessionChannels.set(sessionId, new Set());
          sessionChannels.get(sessionId).add(wrapped);
          return wrapped;
        },
      };
      let response;
      try {
        response = await invocation.run(state, async () => {
          if (url.pathname === '/__spki_internal_connect') {
            if (typeof app.connect !== 'function') {
              return new Response('Application does not export connect()', {status: 404});
            }
            const exported = await app.connect(request, requestEnv, ctx);
            return Response.json(await exported.browserHandoff(request));
          }
          return app.fetch(request, requestEnv, ctx);
        });
      }
      catch (error) { for (const channel of channels) channel.close(); throw error; }
      if (channels.size === 0) return response;
      const closed = new Promise(resolve => { state.finished = resolve; });
      const reader = response.body?.getReader();
      const chunks = [];
      let size = 0;
      if (reader) {
        while (true) {
          const {value, done} = await reader.read();
          if (done) break;
          size += value.length;
          if (size > 1024 * 1024) {
            await reader.cancel();
            for (const channel of channels) channel.close();
            throw new Error('Native RPC handoff response exceeds 1 MiB');
          }
          chunks.push(value);
        }
      }
      if ([204, 205, 304].includes(response.status) || request.method === 'HEAD') {
        for (const channel of channels) channel.close();
        throw new Error('Native RPC handoff requires a response body');
      }
      const headers = new Headers(response.headers);
      headers.delete('content-length');
      headers.set('x-spki-native-body-size', String(size));
      const body = new ReadableStream({
        start(controller) { for (const chunk of chunks) controller.enqueue(chunk); },
        async pull(controller) { await closed; controller.close(); },
        cancel() { for (const channel of channels) channel.close(); },
      });
      return new Response(body, {status: response.status, headers});
    } catch (error) {
      const message = String(error?.stack || error).slice(0, 8192);
      console.error(message);
      return new Response(message, {
        status: ready ? 422 : 500,
        headers: {'content-type': 'text/plain; charset=utf-8'},
      });
    }
  }
};
