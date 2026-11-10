import {exportCapnp, sandstorm} from 'sandstorm:api';
import {Counter} from 'capnp:./schemas/counter.capnp';
import {greeting} from './greeting';

// Shared by browser sessions; resets when this worker restarts or is redeployed.
let value = 0;
let counter;
const observers = new Set();

function publish() {
  for (const observer of observers) {
    // A disconnected browser is removed when its callback fails.
    observer.changed({value}).catch(() => observers.delete(observer));
  }
  return {value};
}

export default {
  async connect(request: Request, env) {
    const api = sandstorm(request, env);
    counter ??= await exportCapnp(api, Counter, {
      read: () => ({value, greeting}),
      increment: () => { ++value; return publish(); },
      reset: () => { value = 0; return publish(); },
      async subscribe({observer}) {
        observers.add(observer);
        try {
          await observer.changed({value});
        } catch (error) {
          observers.delete(observer);
          throw error;
        }
      },
    });
    return counter;
  },
  async fetch(request: Request) {
    if (new URL(request.url).pathname === '/health') return Response.json({ok: true});
    return new Response('Not Found', {status: 404});
  },
};
