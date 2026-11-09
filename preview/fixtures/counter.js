// Uses the POC's existing STORAGE fetch binding; SDK injection comes later.
export default {
  async fetch(request, env) {
    const current = await env.STORAGE.fetch('http://storage/counter');
    let count = current.ok ? Number(await current.text()) : 0;
    if (new URL(request.url).pathname === '/increment') {
      count++;
      const result = await env.STORAGE.fetch('http://storage/counter', {method: 'PUT', body: String(count)});
      if (!result.ok) throw new Error('storage write failed');
    }
    return Response.json({count});
  }
};
