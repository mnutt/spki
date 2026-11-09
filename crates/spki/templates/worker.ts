import {greeting} from './greeting';
export default {
  async fetch(request: Request) {
    if (new URL(request.url).pathname === '/health') return Response.json({ok: true});
    return new Response(`<!doctype html><html><head><meta charset="utf-8"><title>My isolate</title></head><body><h1>${greeting}</h1><p>Edit src/greeting.ts and save to see your changes.</p></body></html>`, {
      headers: {'content-type': 'text/html; charset=utf-8'},
    });
  },
};
