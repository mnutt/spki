export default {
  connect: app.connect?.bind(app),
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    let path;
    try { path = decodeURIComponent(url.pathname).slice(1); }
    catch { return new Response('Invalid URL', {status: 400}); }
    if (path.split('/').some(p => p === '.' || p === '..') || path.includes('\\')) {
      return new Response('Invalid URL', {status: 400});
    }
    if (path === '' || path.endsWith('/')) path += 'index.html';
    if (Object.hasOwn(assets, path)) {
      if (request.method !== 'GET' && request.method !== 'HEAD') {
        return new Response('Method Not Allowed', {status: 405, headers: {Allow: 'GET, HEAD'}});
      }
      const [mime, encoded] = assets[path];
      const body = request.method === 'HEAD' ? null : Uint8Array.from(atob(encoded), c => c.charCodeAt(0));
      return new Response(body, {headers: {
        'content-type': mime, 'cache-control': 'no-cache', 'x-content-type-options': 'nosniff',
      }});
    }
    return app.fetch(request, env, ctx);
  }
};
