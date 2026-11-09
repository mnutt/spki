import {sandstorm} from 'sandstorm:api';
export default {
  async fetch(request, env) {
    const api = sandstorm(request, env);
    if (new URL(request.url).pathname === '/session') return Response.json(api.session());
    const store = api.storage();
    if (new URL(request.url).pathname === '/storage-check') {
      await store.putJson('sdk-test', {value: 42});
      const json = await store.getJson('sdk-test');
      const bytes = await store.getBytes('sdk-test');
      const head = await store.head('sdk-test');
      const index = await store.list();
      await store.delete('sdk-test');
      const missing = await store.get('sdk-test');
      return Response.json({ok: json.value === 42 && bytes.length === Number(head.bytes) && head.ok && index.keys.some(k => k.name === 'sdk-test') && missing === undefined});
    }
    let count = (await store.getJson('counter')) ?? 0;
    if (new URL(request.url).pathname === '/increment') {
      count++;
      const result = await store.putJson('counter', count);
      if (!result.ok) throw new Error('write failed');
    }
    return Response.json({count});
  }
};
